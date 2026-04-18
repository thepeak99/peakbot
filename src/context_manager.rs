//! Context management for handling long conversations.
//! Implements automatic context compaction when approaching the context window limit.
//! Uses actual token counts from the provider (via token_cost hook) instead of estimates.
//!
//! Compaction is an independent LLM call with no tools — text in, summary out.
//! Old messages are tagged `compacted = true` and skipped in get_agent_history(),
//! but kept in the ChatMessage array for UI display and persistence.

use crate::config::ContextConfig;
use crate::providers::CompactionModel;
use crate::state::StateManager;
use crate::ui::app_state::ChatMessage;
use anyhow::{Context as AnyhowContext, Result};
use std::sync::Arc;

/// Default context window size (128k tokens)
const DEFAULT_CONTEXT_WINDOW: usize = 128_000;
/// Estimated tokens per message for fallback calculations
const TOKENS_PER_MESSAGE: usize = 50;
/// Estimated tokens for a conversation summary
const SUMMARY_TOKENS: usize = 75;

/// Result of a context compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Number of messages before compaction
    pub original_count: usize,
    /// Number of messages after compaction (visible to LLM)
    pub compacted_count: usize,
    /// Estimated tokens saved
    pub tokens_saved: usize,
    /// Number of messages that were compacted
    pub num_discarded: usize,
}

/// Plan produced by compact() — describes what to do, doesn't do it.
/// StateManager::apply_compaction() executes this plan.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// The summary text produced by the CompactionModel
    pub summary: String,
    /// Messages at indices 0..boundary are candidates for compaction.
    /// apply_compaction() will mark them compacted except tool calls
    /// needed by ToolResults in the kept region.
    pub boundary: usize,
}

/// Manages context window usage and performs compaction when needed.
/// Uses actual token counts from StateManager.
#[derive(Clone)]
pub(crate) struct ContextManager {
    config: ContextConfig,
    context_window: usize,
    /// Reference to StateManager for stats
    state_manager: Arc<StateManager>,
    /// Tool-free model for summarization (independent call, no tools)
    compaction_model: Option<Arc<CompactionModel>>,
}

impl ContextManager {
    /// Create a new ContextManager with the given configuration.
    /// If context_window is 0 or None, attempts to auto-detect from model name.
    pub fn new(
        config: ContextConfig,
        model_name: &str,
        state_manager: Arc<StateManager>,
        compaction_model: Option<Arc<CompactionModel>>,
    ) -> Self {
        let context_window = config.context_window.unwrap_or_else(|| {
            match model_name.to_lowercase().as_str() {
                m if m.contains("claude-3.7-sonnet") => 200_000,
                m if m.contains("claude-3.5-sonnet") => 200_000,
                m if m.contains("claude-3-opus") => 200_000,
                m if m.contains("claude-3-sonnet") => 200_000,
                m if m.contains("claude-3-haiku") => 200_000,
                m if m.contains("gpt-4o") => 128_000,
                m if m.contains("gpt-4-turbo") => 128_000,
                m if m.contains("gpt-4-32k") => 32_768,
                m if m.contains("gpt-4") => 8_192,
                m if m.contains("gpt-3.5-turbo") => 16_385,
                m if m.contains("gemini-2.0") => 1_000_000,
                m if m.contains("gemini-1.5-pro") => 2_000_000,
                m if m.contains("gemini-1.5-flash") => 1_000_000,
                _ => DEFAULT_CONTEXT_WINDOW,
            }
        });

        Self {
            config,
            context_window,
            state_manager,
            compaction_model,
        }
    }

    /// Get the context window size
    #[allow(dead_code)]
    pub(crate) fn context_window(&self) -> usize {
        self.context_window
    }

    /// Get the compaction threshold (in tokens)
    pub fn threshold(&self) -> usize {
        ((self.context_window as f64) * self.config.threshold) as usize
    }

    /// Get current total tokens from actual API response.
    /// Uses the LAST request's input tokens (actual context size), not cumulative sum.
    pub fn get_current_tokens(&self) -> usize {
        let stats_arc = self.state_manager.stats_arc();
        let stats = stats_arc.lock().unwrap();
        stats.last_input_tokens().unwrap_or(0) as usize
    }

    /// Check if compaction is needed based on uncompacted message count and token usage.
    pub fn needs_compaction(&self, uncompacted_count: usize) -> bool {
        if !self.config.enabled {
            return false;
        }

        if uncompacted_count <= self.config.keep_recent {
            return false;
        }

        // Check actual token count if available
        let tokens = self.get_current_tokens();
        if tokens > 0 {
            return tokens > self.threshold();
        }

        // Fallback: message count (when no token data is available yet)
        let threshold_messages = (self.config.keep_recent * 3).max(10);
        uncompacted_count > threshold_messages
    }

    /// Get current token usage as a percentage (0.0 - 1.0)
    #[allow(dead_code)]
    pub(crate) fn usage_percentage(&self) -> f64 {
        let total = self.get_current_tokens();
        if total == 0 {
            return 0.0;
        }
        total as f64 / self.context_window as f64
    }

    /// Produce a CompactionPlan by summarizing older messages.
    /// This is a pure function — it reads messages but doesn't mutate them.
    /// The actual tagging is done by StateManager::apply_compaction().
    pub async fn compact(&self, messages: &[ChatMessage]) -> Result<CompactionPlan> {
        let model = self
            .compaction_model
            .as_ref()
            .context("No compaction model available for summarization")?;

        // Only consider uncompacted messages
        let uncompacted: Vec<(usize, &ChatMessage)> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.compacted)
            .collect();

        if uncompacted.len() <= self.config.keep_recent {
            anyhow::bail!("Not enough uncompacted messages to compact");
        }

        // The boundary: compact everything except the last keep_recent uncompacted messages
        let keep_count = self.config.keep_recent;
        let compact_count = uncompacted.len() - keep_count;
        // boundary is the original index of the first message to keep.
        // When keep_recent=0, compact everything — boundary is past the last message.
        let boundary = if compact_count >= uncompacted.len() {
            messages.len()
        } else {
            uncompacted[compact_count].0
        };

        // Format older messages for summarization (everything before boundary that isn't already compacted)
        let to_summarize: Vec<&ChatMessage> = messages[..boundary]
            .iter()
            .filter(|m| !m.compacted)
            .collect();

        let formatted = format_chat_messages_for_summary(&to_summarize);

        let prompt = format!(
            "Summarize this conversation concisely. Preserve: key decisions, important facts, \
            tool calls and their results, and any state needed to continue the conversation. \
            Be specific about what was done.\n\n{}\n\n\
            Provide a concise summary that captures the essential context:",
            formatted
        );

        let summary = model.summarize(&prompt).await
            .map_err(|e| anyhow::anyhow!("Compaction summarization failed: {}", e))?;

        Ok(CompactionPlan { summary, boundary })
    }

    /// How many messages would be compacted for a given plan
    pub fn estimate_compaction(&self, messages: &[ChatMessage], boundary: usize) -> CompactionResult {
        let original_uncompacted = messages.iter().filter(|m| !m.compacted).count();
        let would_compact = messages[..boundary]
            .iter()
            .filter(|m| !m.compacted)
            .count();
        // After compaction: 1 summary + (original_uncompacted - would_compact) kept
        let after = 1 + (original_uncompacted - would_compact);
        CompactionResult {
            original_count: original_uncompacted,
            compacted_count: after,
            tokens_saved: would_compact.saturating_mul(TOKENS_PER_MESSAGE).saturating_sub(SUMMARY_TOKENS),
            num_discarded: would_compact,
        }
    }

    /// Format context status for display
    #[allow(dead_code)]
    pub(crate) fn format_status(&self) -> String {
        let total_tokens = self.get_current_tokens();
        let usage_pct = self.usage_percentage();

        format!(
            "Context: {} / {} tokens ({:.1}%)\nCompaction threshold: {}% ({})\nEnabled: {}",
            total_tokens,
            self.context_window,
            usage_pct * 100.0,
            (self.config.threshold * 100.0) as usize,
            self.threshold(),
            if self.config.enabled { "yes" } else { "no" }
        )
    }
}

/// Format ChatMessages for the summarization prompt.
/// Includes all message types: user, agent, tool calls, and tool results.
fn format_chat_messages_for_summary(messages: &[&ChatMessage]) -> String {
    use crate::ui::app_state::MessageRole;

    let mut output = String::new();
    output.push_str("Previous conversation:\n\n");

    for msg in messages {
        match msg.role {
            MessageRole::User => {
                output.push_str(&format!("User: {}\n\n", msg.content));
            }
            MessageRole::Agent => {
                output.push_str(&format!("Assistant: {}\n\n", msg.content));
            }
            MessageRole::ToolCall => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
                let args = msg.tool_args.as_deref().unwrap_or("{}");
                // Truncate args to avoid blowing up the summary prompt
                let args_short = truncate_str(args, 200);
                output.push_str(&format!(
                    "Assistant [called {}({})]\n\n",
                    name, args_short
                ));
            }
            MessageRole::ToolResult => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
                let result = msg.tool_result.as_deref().unwrap_or("");
                let result_short = truncate_str(result, 500);
                output.push_str(&format!(
                    "Tool [{}] returned: {}\n\n",
                    name, result_short
                ));
            }
            MessageRole::Summary => {
                output.push_str(&format!("Previous summary: {}\n\n", msg.content));
            }
            MessageRole::System => {}
        }
    }

    output
}

/// Truncate a string to max_len, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else if max_len < 3 {
        &s[..max_len]
    } else {
        // Find a valid char boundary
        let end = s.floor_char_boundary(max_len.saturating_sub(3));
        &s[..end]
    }
}

/// Find indices of tool calls in messages[..boundary] that are needed by
/// tool results in messages[boundary..]. These must NOT be compacted.
pub(crate) fn find_needed_tool_calls_chat(messages: &[ChatMessage], boundary: usize) -> Vec<usize> {
    use crate::ui::app_state::MessageRole;
    use std::collections::HashSet;

    let mut needed = Vec::new();
    let mut seen_ids = HashSet::new();

    // Scan kept region for ToolResults, find their matching ToolCalls before boundary
    for msg in &messages[boundary..] {
        if msg.role == MessageRole::ToolResult
            && let Some(ref call_id) = msg.call_id
        {
            if seen_ids.contains(call_id) {
                continue;
            }
            seen_ids.insert(call_id.clone());

            // Find the ToolCall with this call_id before boundary
            for (i, m) in messages[..boundary].iter().enumerate() {
                if m.role == MessageRole::ToolCall && m.call_id.as_ref() == Some(call_id) {
                    needed.push(i);
                    break;
                }
            }
        }
    }

    needed
}

/// Get the default context config
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            context_window: None,
            compaction_model: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::MessageRole;

    #[test]
    fn test_default_config() {
        let config = ContextConfig::default();
        assert_eq!(config.threshold, 0.8);
        assert_eq!(config.keep_recent, 5);
        assert!(config.enabled);
        assert!(config.context_window.is_none());
        assert!(config.compaction_model.is_none());
    }

    #[test]
    fn test_message_count_fallback_threshold_is_sane() {
        let keep_recent: usize = 5;
        let threshold_messages = (keep_recent * 3).max(10);
        assert_eq!(threshold_messages, 15);
        assert!(
            threshold_messages <= 100,
            "Message count fallback threshold should be reachable, got {}",
            threshold_messages
        );

        let small_keep = 1_usize;
        let small_threshold = (small_keep * 3).max(10);
        assert_eq!(small_threshold, 10);

        let large_keep = 50_usize;
        let large_threshold = (large_keep * 3).max(10);
        assert_eq!(large_threshold, 150);
    }

    #[test]
    fn test_format_chat_messages_includes_tool_calls() {
        let messages = vec![
            ChatMessage::user("List my files".to_string()),
            ChatMessage::tool_call("bash", r#"{"command":"ls"}"#, Some("call_1".to_string())),
            ChatMessage::tool_result("bash", r#"{"command":"ls"}"#, "file1.txt\nfile2.txt", Some("call_1".to_string())),
            ChatMessage::agent("Here are your files: file1.txt and file2.txt".to_string()),
        ];

        let refs: Vec<&ChatMessage> = messages.iter().collect();
        let formatted = format_chat_messages_for_summary(&refs);

        assert!(formatted.contains("User: List my files"));
        assert!(formatted.contains("Assistant [called bash("));
        assert!(formatted.contains("Tool [bash] returned:"));
        assert!(formatted.contains("file1.txt"));
        assert!(formatted.contains("Assistant: Here are your files"));
    }

    #[test]
    fn test_format_chat_messages_includes_summary() {
        let messages = vec![
            ChatMessage::summary("Previous work: set up the project".to_string()),
            ChatMessage::user("Continue from where we left off".to_string()),
        ];

        let refs: Vec<&ChatMessage> = messages.iter().collect();
        let formatted = format_chat_messages_for_summary(&refs);

        assert!(formatted.contains("Previous summary: Previous work: set up the project"));
        assert!(formatted.contains("User: Continue from where we left off"));
    }

    #[test]
    fn test_find_needed_tool_calls_chat_basic() {
        let messages = vec![
            ChatMessage::user("list files".to_string()),
            ChatMessage::tool_call("bash", r#"{"cmd":"ls"}"#, Some("call_1".to_string())),
            ChatMessage::tool_result("bash", r#"{"cmd":"ls"}"#, "file1.txt", Some("call_1".to_string())),
            ChatMessage::agent("Here are your files".to_string()),
        ];

        // boundary=2: keep messages at index 2,3; compact 0,1
        let needed = find_needed_tool_calls_chat(&messages, 2);
        // tool_result at index 2 references call_1, which is at index 1 (before boundary)
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_find_needed_tool_calls_chat_none_needed() {
        let messages = vec![
            ChatMessage::user("hello".to_string()),
            ChatMessage::agent("hi".to_string()),
            ChatMessage::user("how are you?".to_string()),
            ChatMessage::agent("fine".to_string()),
        ];

        let needed = find_needed_tool_calls_chat(&messages, 2);
        assert!(needed.is_empty());
    }

    #[test]
    fn test_find_needed_tool_calls_chat_already_kept() {
        let messages = vec![
            ChatMessage::user("old".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("call_1".to_string())),
            ChatMessage::tool_result("bash", "{}", "output", Some("call_1".to_string())),
        ];

        // boundary=1: both tool_call and tool_result are in the kept region
        let needed = find_needed_tool_calls_chat(&messages, 1);
        assert!(needed.is_empty());
    }

    #[test]
    fn test_find_needed_tool_calls_chat_cross_boundary() {
        let messages = vec![
            ChatMessage::user("old question".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("call_1".to_string())),
            ChatMessage::user("new question".to_string()),
            ChatMessage::tool_result("bash", "{}", "output", Some("call_1".to_string())),
            ChatMessage::agent("answer".to_string()),
        ];

        let needed = find_needed_tool_calls_chat(&messages, 2);
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_find_needed_tool_calls_chat_duplicate_ids() {
        let messages = vec![
            ChatMessage::user("old".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("call_same".to_string())),
            ChatMessage::tool_result("bash", "{}", "first", Some("call_same".to_string())),
            ChatMessage::tool_result("bash", "{}", "second", Some("call_same".to_string())),
            ChatMessage::agent("done".to_string()),
        ];

        let needed = find_needed_tool_calls_chat(&messages, 2);
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "he");
        assert_eq!(truncate_str("hi", 1), "h");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn test_compacted_messages_skipped_in_format() {
        let mut msg = ChatMessage::user("old message".to_string());
        msg.compacted = true;
        let new_msg = ChatMessage::user("new message".to_string());

        let messages = vec![&msg, &new_msg];

        // format_chat_messages_for_summary doesn't filter compacted — that's the caller's job.
        // But verify it formats both if passed both.
        let formatted = format_chat_messages_for_summary(&messages);
        assert!(formatted.contains("old message"));
        assert!(formatted.contains("new message"));
    }

    #[test]
    fn test_summary_message_constructor() {
        let msg = ChatMessage::summary("Test summary content".to_string());
        assert_eq!(msg.role, MessageRole::Summary);
        assert_eq!(msg.content, "Test summary content");
        assert!(!msg.compacted);
    }
}
