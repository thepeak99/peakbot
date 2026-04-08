//! Context management for handling long conversations.
//! Implements automatic context compaction when approaching the context window limit.
//! Uses actual token counts from the provider (via token_cost hook) instead of estimates.

use crate::config::ContextConfig;
use crate::providers::DynAgent;
use crate::state::StateManager;
use anyhow::{Context as AnyhowContext, Result};
use rig::completion::message::{AssistantContent, Message, UserContent};
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
    /// Number of messages after compaction
    pub compacted_count: usize,
    /// Estimated tokens saved
    pub tokens_saved: usize,
    /// Number of messages that were discarded (truncated)
    pub num_discarded: usize,
}

/// Manages context window usage and performs compaction when needed
/// Uses actual token counts from StateManager
pub struct ContextManager {
    config: ContextConfig,
    context_window: usize,
    /// System prompt size (in tokens) - used to subtract from total
    system_prompt_tokens: usize,
    /// Reference to StateManager for stats
    state_manager: Arc<StateManager>,
    /// Reference to the LLM agent for summarization
    agent: Option<Arc<DynAgent>>,
}

impl ContextManager {
    /// Create a new ContextManager with the given configuration
    /// If context_window is 0 or None, attempts to auto-detect from model name
    pub fn new(
        config: ContextConfig,
        model_name: &str,
        state_manager: Arc<StateManager>,
        system_prompt_tokens: usize,
        agent: Option<Arc<DynAgent>>,
    ) -> Self {
        // Default context windows for common models
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
                _ => DEFAULT_CONTEXT_WINDOW, // Default fallback
            }
        });

        Self {
            config,
            context_window,
            system_prompt_tokens,
            state_manager,
            agent,
        }
    }

    /// Get the context window size
    pub fn context_window(&self) -> usize {
        self.context_window
    }

    /// Get the compaction threshold (in tokens)
    pub fn threshold(&self) -> usize {
        ((self.context_window as f64) * self.config.threshold) as usize
    }

    /// Get current total tokens from actual API response
    /// This is the EXACT token count from the provider, not an estimate
    pub fn get_current_tokens(&self) -> usize {
        let stats_arc = self.state_manager.stats_arc();
        let stats = stats_arc.lock().unwrap();
        stats.last_input_tokens().unwrap_or(0) as usize
    }

    /// Estimate total tokens for messages plus system prompt
    /// Now uses actual token counts from provider instead of estimation
    pub fn get_total_tokens(&self) -> usize {
        // Get actual input tokens from the last request
        // This includes: system prompt + conversation history + last user message
        let current_input = self.get_current_tokens();

        // Subtract system prompt to get conversation history size
        // (add a buffer since we can't know exact system prompt size)
        let history_tokens = current_input.saturating_sub(self.system_prompt_tokens);

        history_tokens
    }

    /// Check if compaction is needed based on current message count
    pub fn needs_compaction(&self, messages: &[Message]) -> bool {
        if !self.config.enabled {
            return false;
        }

        // Edge cases: no compaction needed for empty or very short history
        if messages.len() <= self.config.keep_recent {
            return false;
        }

        // Check actual token count if available
        let tokens = self.get_current_tokens();
        if tokens > 0 {
            return tokens > self.threshold();
        }

        // Fallback: check message count
        let threshold_messages = (self.threshold() / 100).max(10);
        messages.len() > threshold_messages
    }

    /// Check if compaction is needed based on actual token count from provider
    pub fn needs_compaction_by_tokens(&self, _messages: &[Message], _system_prompt: &str) -> bool {
        if !self.config.enabled {
            return false;
        }

        let total_tokens = self.get_current_tokens();
        total_tokens > self.threshold()
    }

    /// Get current token usage as a percentage (0.0 - 1.0)
    /// Uses actual token counts from provider
    pub fn usage_percentage(&self) -> f64 {
        let total = self.get_current_tokens();
        if total == 0 {
            return 0.0;
        }
        total as f64 / self.context_window as f64
    }

    /// Format messages for summarization prompt
    fn format_messages_for_summary(&self, messages: &[Message]) -> String {
        let mut output = String::new();
        output.push_str("Previous conversation:\n\n");

        for msg in messages {
            let (role, content) = match msg {
                Message::User { content } => {
                    // Extract text from User content
                    let text = content.iter()
                        .filter_map(|c| {
                            if let rig::completion::message::UserContent::Text(t) = c {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    ("User", text)
                }
                Message::Assistant { content, .. } => {
                    // Extract text from Assistant content
                    let text = content.iter()
                        .filter_map(|c| {
                            if let rig::completion::message::AssistantContent::Text(t) = c {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    ("Assistant", text)
                }
                // ToolResult and other variants - skip them for summarization
                #[allow(unreachable_patterns)]
                _ => continue,
            };
            if !content.is_empty() {
                output.push_str(&format!("{}: {}\n\n", role, content));
            }
        }

        output
    }

    /// Summarize messages using the LLM
    async fn summarize_messages(&self, messages: &[Message]) -> Result<String> {
        let agent = self.agent.as_ref()
            .context("No LLM agent available for summarization")?;

        let formatted = self.format_messages_for_summary(messages);

        let prompt = format!(
            "Please summarize the following conversation concisely, preserving the key information, decisions, and important details. \
            Focus on what matters for continuing the conversation:\n\n{}\n\n\
            Provide a concise summary (2-4 sentences) that captures the essential context:",

            formatted
        );

        let summary = agent.prompt(&prompt).await?;
        Ok(summary)
    }

    /// Perform context compaction
    /// Uses summarization approach: summarize older messages instead of just truncating
    /// Preserves tool call/result integrity by including ToolCall messages that are
    /// referenced by ToolResult messages in the kept region.
    pub async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        _system_prompt: &str,
    ) -> Result<CompactionResult> {
        if !self.config.enabled {
            return Ok(CompactionResult {
                original_count: messages.len(),
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_discarded: 0,
            });
        }

        let original_count = messages.len();

        // Edge cases
        if messages.len() <= self.config.keep_recent {
            return Ok(CompactionResult {
                original_count,
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_discarded: 0,
            });
        }

        // Get token count before compaction
        let tokens_before = self.get_current_tokens();

        // Split messages: keep recent, discard older
        let keep_start = messages.len().saturating_sub(self.config.keep_recent);

        // Find tool calls that are referenced by tool results in the keep region.
        // These must be preserved to maintain conversation integrity.
        let needed_tc_indices = find_needed_tool_calls(messages, keep_start);

        // Calculate which messages to discard (old messages minus needed tool calls)
        let needed_tc_set: std::collections::HashSet<usize> =
            needed_tc_indices.iter().cloned().collect();

        let num_to_discard = keep_start - needed_tc_indices.len();

        if num_to_discard == 0 && needed_tc_indices.is_empty() {
            return Ok(CompactionResult {
                original_count,
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_discarded: 0,
            });
        }

        // Get the messages to summarize (the older ones, excluding needed tool calls)
        let to_summarize: Vec<Message> = messages[0..keep_start]
            .iter()
            .enumerate()
            .filter(|(i, _)| !needed_tc_set.contains(i))
            .map(|(_, msg)| msg.clone())
            .collect();

        // Collect messages to keep: needed tool calls + the recent messages
        let mut to_keep: Vec<Message> = Vec::new();

        // Add needed tool calls (in order)
        for &idx in &needed_tc_indices {
            to_keep.push(messages[idx].clone());
        }

        // Add the recent messages (from keep_start to end)
        for i in keep_start..messages.len() {
            to_keep.push(messages[i].clone());
        }

        // Try to summarize the older messages if we have an agent
        let has_agent = self.agent.is_some();
        let summary_message = if has_agent {
            match self.summarize_messages(&to_summarize).await {
                Ok(summary) => {
                    // Create a user message with the summary as context
                    Some(Message::user(format!(
                        "[Previous conversation summary: {}]",
                        summary
                    )))
                }
                Err(e) => {
                    // If summarization fails, log and fall back to truncation
                    tracing::warn!("Failed to summarize messages: {}. Falling back to truncation.", e);
                    None
                }
            }
        } else {
            // No agent available, fall back to truncation
            None
        };

        // Clear all messages
        messages.clear();

        // Add summary message if we have one, otherwise just start fresh
        let summary_created = summary_message.is_some();
        if let Some(summary) = summary_message {
            messages.push(summary);
        }

        // Add back the messages we wanted to keep (tool calls + recent)
        messages.extend(to_keep);

        let compacted_count = messages.len();

        // Estimate tokens saved - summarization is much more efficient than truncation
        let tokens_saved = if summary_created {
            // Estimate: original had TOKENS_PER_MESSAGE tokens per message, summary is SUMMARY_TOKENS
            (num_to_discard * TOKENS_PER_MESSAGE).saturating_sub(SUMMARY_TOKENS)
        } else {
            // Fallback to truncation calculation
            tokens_before.saturating_sub(self.config.keep_recent * TOKENS_PER_MESSAGE)
        };

        Ok(CompactionResult {
            original_count,
            compacted_count,
            tokens_saved,
            num_discarded: num_to_discard,
        })
    }

    /// Format context status for display
    /// Uses actual token counts from provider
    pub fn format_status(&self) -> String {
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

/// Extract the call_id from a ToolResult message.
/// Returns the ID that links a ToolResult to its ToolCall.
fn extract_call_id_from_tool_result(msg: &Message) -> Option<String> {
    if let Message::User { content, .. } = msg {
        for item in content.iter() {
            if let UserContent::ToolResult(tr) = item {
                // Use the id field which is the primary identifier
                return Some(tr.id.clone());
            }
        }
    }
    None
}

/// Find a ToolCall message that has the given call_id.
/// Searches the full message list.
fn find_tool_call_by_id(messages: &[Message], call_id: &str) -> Option<usize> {
    for i in 0..messages.len() {
        if let Message::Assistant { content, .. } = &messages[i] {
            for item in content.iter() {
                if let AssistantContent::ToolCall(tc) = item {
                    if tc.id == call_id {
                        return Some(i);
                    }
                }
            }
        }
    }
    None
}

/// Find all ToolCall indices that are needed by ToolResults in the keep region.
/// These are ToolCalls that exist before keep_start but are referenced by
/// ToolResults that we want to keep.
fn find_needed_tool_calls(messages: &[Message], keep_start: usize) -> Vec<usize> {
    let mut needed_indices = Vec::new();
    let mut seen_call_ids = std::collections::HashSet::new();

    // Scan the keep region for ToolResults and find their corresponding ToolCalls
    for i in keep_start..messages.len() {
        if let Some(call_id) = extract_call_id_from_tool_result(&messages[i]) {
            // Skip if we've already found this call_id
            if seen_call_ids.contains(&call_id) {
                continue;
            }
            seen_call_ids.insert(call_id.clone());

            // Find the ToolCall for this result (search full list)
            if let Some(tc_index) = find_tool_call_by_id(messages, &call_id) {
                // Only add if the ToolCall is before keep_start (not already kept)
                if tc_index < keep_start {
                    needed_indices.push(tc_index);
                }
                // If tc_index >= keep_start, it's already in the kept region - skip
            } else {
                tracing::warn!(
                    "ToolResult references call_id '{}' but no matching ToolCall found",
                    call_id
                );
            }
        }
    }

    needed_indices
}

/// Get the default context config
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            context_window: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::message::{AssistantContent, ToolCall, ToolFunction, UserContent, ToolResult, ToolResultContent};
    use rig::one_or_many::OneOrMany;

    // ContextManager tests would need SessionStats mock
    // For now, just test the default config
    #[test]
    fn test_default_config() {
        let config = ContextConfig::default();
        assert_eq!(config.threshold, 0.8);
        assert_eq!(config.keep_recent, 5);
        assert!(config.enabled);
        assert!(config.context_window.is_none());
    }

    // Helper to create a ToolCall message
    fn make_tool_call(id: &str, tool_name: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                id.to_string(),
                ToolFunction::new(
                    tool_name.to_string(),
                    serde_json::json!({}),
                ),
            ))),
        }
    }

    // Helper to create a ToolResult message
    fn make_tool_result(id: &str, result: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                id: id.to_string(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::Text(
                    rig::completion::message::Text {
                        text: result.to_string(),
                    },
                )),
            })),
        }
    }

    // Helper to create a simple text message
    fn make_text_message(role: &str, text: &str) -> Message {
        match role {
            "user" => Message::User {
                content: OneOrMany::one(UserContent::Text(rig::completion::message::Text {
                    text: text.to_string(),
                })),
            },
            _ => Message::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::Text(
                    rig::completion::message::Text {
                        text: text.to_string(),
                    },
                )),
            },
        }
    }

    #[test]
    fn test_extract_call_id_from_tool_result() {
        let result_msg = make_tool_result("call_abc123", "output");
        assert_eq!(
            extract_call_id_from_tool_result(&result_msg),
            Some("call_abc123".to_string())
        );
    }

    #[test]
    fn test_extract_call_id_from_non_result_message() {
        let text_msg = make_text_message("user", "hello");
        assert_eq!(extract_call_id_from_tool_result(&text_msg), None);

        let tool_call_msg = make_tool_call("call_xyz", "bash");
        assert_eq!(extract_call_id_from_tool_result(&tool_call_msg), None);
    }

    #[test]
    fn test_find_tool_call_by_id() {
        let messages = vec![
            make_text_message("user", "hello"),
            make_tool_call("call_123", "bash"),
            make_text_message("assistant", "running"),
            make_tool_result("call_123", "ls output"),
        ];

        // Find the tool call for call_123 (should be at index 1)
        assert_eq!(
            find_tool_call_by_id(&messages, "call_123"),
            Some(1)
        );

        // Find non-existent call
        assert_eq!(
            find_tool_call_by_id(&messages, "call_nonexistent"),
            None
        );
    }

    #[test]
    fn test_find_needed_tool_calls_basic() {
        // Simulate: [user, tool_call, tool_result, assistant] with keep_start=2
        // So we keep tool_result and assistant, but need to find tool_call
        let messages = vec![
            make_text_message("user", "list files"),
            make_tool_call("call_abc", "bash"),
            make_tool_result("call_abc", "file1.txt\nfile2.txt"),
            make_text_message("assistant", "Here are your files"),
        ];

        let keep_start = 2; // Keep messages at index 2 and 3

        let needed = find_needed_tool_calls(&messages, keep_start);

        // Should find the tool call at index 1
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_find_needed_tool_calls_none_needed() {
        // No tool results in the keep region
        let messages = vec![
            make_text_message("user", "hello"),
            make_text_message("assistant", "hi"),
            make_text_message("user", "how are you?"),
            make_text_message("assistant", "fine thanks"),
        ];

        let keep_start = 2;

        let needed = find_needed_tool_calls(&messages, keep_start);

        assert!(needed.is_empty());
    }

    #[test]
    fn test_find_needed_tool_calls_already_kept() {
        // Tool call and result both in keep region
        let messages = vec![
            make_text_message("user", "old"),
            make_tool_call("call_abc", "bash"),
            make_tool_result("call_abc", "output"),
        ];

        let keep_start = 1; // Keep messages at index 1 and 2

        let needed = find_needed_tool_calls(&messages, keep_start);

        // Tool call is at index 1, which IS in the kept region (>= keep_start)
        // So no additional tool calls are needed
        assert!(needed.is_empty());
    }

    #[test]
    fn test_find_needed_tool_calls_multiple_results() {
        // Test case where we have tool calls/results in different positions
        // [user(0), tool_call_1(1), tool_result_1(2), tool_call_2(3), tool_result_2(4), assistant(5)]
        // keep_start = 3 (keep from index 3 onwards)
        // Keep region: tool_call_2(3), tool_result_2(4), assistant(5)
        // tool_result_2 references call_2 which is at index 3 (already kept)
        // But wait - we need call_1 for the summarize, which is at index 1
        // Actually, tool_result_1 is at index 2, which is NOT in keep region, so we don't scan it
        let messages = vec![
            make_text_message("user", "task 1"),
            make_tool_call("call_1", "bash"),      // index 1
            make_tool_result("call_1", "result 1"), // index 2
            make_tool_call("call_2", "read"),       // index 3
            make_tool_result("call_2", "result 2"), // index 4 - in keep region, references call_2
            make_text_message("assistant", "done"),  // index 5
        ];

        let keep_start = 3; // Keep from index 3 onwards

        let needed = find_needed_tool_calls(&messages, keep_start);

        // tool_result_2 (index 4) references call_2 which is at index 3 (in keep region)
        // So no additional tool calls needed
        assert!(needed.is_empty());
    }

    #[test]
    fn test_find_needed_tool_calls_two_needed() {
        // [user(0), tool_call_1(1), tool_result_1(2), tool_call_2(3), tool_result_2(4), assistant(5)]
        // keep_start = 4 (keep from index 4 onwards)
        // Keep region: tool_result_2(4), assistant(5)
        // tool_result_2 (index 4) references call_2 which is at index 3
        // 3 < 4, so index 3 is NOT in the keep region and MUST be included
        let messages = vec![
            make_text_message("user", "task 1"),
            make_tool_call("call_1", "bash"),       // index 1
            make_tool_result("call_1", "result 1"),  // index 2
            make_tool_call("call_2", "read"),        // index 3 - BEFORE keep_start
            make_tool_result("call_2", "result 2"),  // index 4 - IN keep region
            make_text_message("assistant", "done"),   // index 5
        ];

        let keep_start = 4; // Keep from index 4 onwards

        let needed = find_needed_tool_calls(&messages, keep_start);

        // tool_result_2 (index 4) references call_2 which is at index 3
        // index 3 < keep_start (4), so it must be included
        assert_eq!(needed, vec![3]);
    }

    #[test]
    fn test_find_needed_tool_calls_cross_boundary() {
        // This is the key scenario: tool call BEFORE keep_start, tool result IN keep_start
        // [user(0), tool_call(1), user(2), tool_result(3), assistant(4)]
        // keep_start = 2 (keep from index 2 onwards)
        // Keep region: user(2), tool_result(3), assistant(4)
        // tool_result(3) references call_1 which is at index 1 (BEFORE keep_start!)
        // This is the bug we were fixing - we MUST include index 1
        let messages = vec![
            make_text_message("user", "old question"),   // index 0
            make_tool_call("call_1", "bash"),           // index 1 - BEFORE keep_start
            make_text_message("user", "new question"),  // index 2 - IN keep region
            make_tool_result("call_1", "output"),       // index 3 - IN keep region, references call_1
            make_text_message("assistant", "answer"),    // index 4 - IN keep region
        ];

        let keep_start = 2; // Keep from index 2 onwards

        let needed = find_needed_tool_calls(&messages, keep_start);

        // tool_result (index 3) references call_1 which is at index 1 (before keep_start)
        // We MUST include this tool call
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_find_needed_tool_calls_duplicate_call_ids() {
        // Same call_id referenced multiple times (edge case)
        let messages = vec![
            make_text_message("user", "old"),
            make_tool_call("call_same", "bash"),
            make_tool_result("call_same", "first result"),
            make_tool_result("call_same", "second result"),
            make_text_message("assistant", "done"),
        ];

        let keep_start = 2; // Keep from index 2 onwards

        let needed = find_needed_tool_calls(&messages, keep_start);

        // Should only find the tool call once
        assert_eq!(needed, vec![1]);
    }

    #[test]
    fn test_find_needed_tool_calls_missing_tool_call() {
        // ToolResult references a call_id that doesn't exist
        let messages = vec![
            make_text_message("user", "old message"),
            make_tool_result("call_orphan", "result without call"),
            make_text_message("assistant", "response"),
        ];

        let keep_start = 1;

        let needed = find_needed_tool_calls(&messages, keep_start);

        // Should return empty since no tool call was found
        // (warning is logged but we handle it gracefully)
        assert!(needed.is_empty());
    }
}
