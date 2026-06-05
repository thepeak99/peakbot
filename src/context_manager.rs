//! Context management for handling long conversations.
//! Implements automatic context compaction when approaching the context window limit.
//! Uses actual token counts from the provider (via token_cost hook) instead of estimates.
//!
//! Compaction is an independent LLM call with no tools — text in, summary out.
//! Old messages are tagged `compacted = true` and skipped in get_agent_history(),
//! but kept in the ChatMessage array for UI display and persistence.

use crate::config::ContextConfig;
use crate::providers::CompactionModel;
use crate::ui::app_state::ChatMessage;
use crate::utils::truncate_to_char_boundary;
use anyhow::{Context as AnyhowContext, Result};
use std::sync::Arc;

/// Default context size (128k tokens)
pub(crate) const DEFAULT_CONTEXT_SIZE: usize = 128_000;
/// Estimated tokens per message for fallback calculations
const TOKENS_PER_MESSAGE: usize = 50;
/// Estimated tokens for a conversation summary
const SUMMARY_TOKENS: usize = 75;

/// Single source of truth for the model-name → context-window mapping.
///
/// Used by both the legacy single-provider boot path (`main.rs`) and
/// the multi-model registry build (`config::model_registry`). Until
/// this helper existed, the same `match` block was duplicated in three
/// places — drift between them was a real correctness liability.
/// *(necessarily same — they really are the same lookup table)*
pub fn auto_detect_context_size(model_name: &str) -> usize {
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
        _ => DEFAULT_CONTEXT_SIZE,
    }
}

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

/// Manages context size usage and performs compaction when needed.
///
/// **Stateless by design.** The manager owns no back-reference to
/// `StateManager` — every method that depends on runtime state (token
/// counts, message lists) takes that state as an explicit argument.
/// This is what kept the previous design honest:
///
/// 1. **No cyclic Arc.** `StateManager` owns `ContextManager` directly;
///    `ContextManager` does not own `StateManager`. The previous
///    `Arc<StateManager>` back-ref formed a strong-count cycle that
///    leaked at shutdown.
/// 2. **No deadlock.** Holding a lock on the StateManager-owned
///    `ContextManager` slot across `.await` (as `force_compact` used
///    to do) was a deadlock waiting to fire — the awaited future
///    re-entered StateManager through the back-reference. With the
///    back-ref gone, callers clone the manager out under a brief read
///    guard, drop the guard, and `.await` on the clone.
#[derive(Clone)]
pub(crate) struct ContextManager {
    config: ContextConfig,
    context_size: usize,
    /// Tool-free model for summarization (independent call, no tools)
    compaction_model: Option<Arc<CompactionModel>>,
}

impl ContextManager {
    /// Create a new ContextManager with a pre-resolved context size.
    ///
    /// Resolution (per-model override OR auto-detect against the wire id)
    /// happens upstream at the active-model boundary — this constructor
    /// is the single consumer and stores the value verbatim. See
    /// `auto_detect_context_size` for the shared lookup helper.
    pub fn new(
        config: ContextConfig,
        context_size: usize,
        compaction_model: Option<Arc<CompactionModel>>,
    ) -> Self {
        Self {
            config,
            context_size,
            compaction_model,
        }
    }

    /// Get the context size
    #[allow(dead_code)]
    pub(crate) fn context_size(&self) -> usize {
        self.context_size
    }

    /// Whether automatic compaction is enabled
    pub(crate) fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Compaction threshold as a fraction (0.0 – 1.0)
    pub(crate) fn threshold_fraction(&self) -> f64 {
        self.config.threshold
    }

    /// Get the compaction threshold (in tokens)
    pub fn threshold(&self) -> usize {
        ((self.context_size as f64) * self.config.threshold) as usize
    }

    /// Check if compaction is needed based on uncompacted message count and token usage.
    ///
    /// `current_tokens` is the most recent API-reported input-token count.
    /// Pass `0` when not yet available — the message-count fallback then
    /// kicks in. Typically: `SessionStats::last_input_tokens().unwrap_or(0) as usize`.
    pub fn needs_compaction(&self, uncompacted_count: usize, current_tokens: usize) -> bool {
        if !self.config.enabled {
            return false;
        }

        if uncompacted_count <= self.config.keep_recent {
            return false;
        }

        if current_tokens > 0 {
            return current_tokens > self.threshold();
        }

        // Fallback: message count (when no token data is available yet)
        let threshold_messages = (self.config.keep_recent * 3).max(10);
        uncompacted_count > threshold_messages
    }

    /// Get current token usage as a percentage (0.0 - 1.0).
    /// Caller passes the live token count — see `needs_compaction`.
    #[allow(dead_code)]
    pub(crate) fn usage_percentage(&self, current_tokens: usize) -> f64 {
        if current_tokens == 0 {
            return 0.0;
        }
        current_tokens as f64 / self.context_size as f64
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

        // Snap the boundary off any ToolResult so the inserted summary can't
        // split a tool_use/tool_result pair (see snap_boundary_past_tool_results).
        let boundary = snap_boundary_past_tool_results(messages, boundary);

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

        let summary = model
            .summarize(&prompt)
            .await
            .map_err(|e| anyhow::anyhow!("Compaction summarization failed: {}", e))?;

        Ok(CompactionPlan { summary, boundary })
    }

    /// How many messages would be compacted for a given plan
    pub fn estimate_compaction(
        &self,
        messages: &[ChatMessage],
        boundary: usize,
    ) -> CompactionResult {
        let original_uncompacted = messages.iter().filter(|m| !m.compacted).count();
        let would_compact = messages[..boundary].iter().filter(|m| !m.compacted).count();
        // After compaction: 1 summary + (original_uncompacted - would_compact) kept
        let after = 1 + (original_uncompacted - would_compact);
        CompactionResult {
            original_count: original_uncompacted,
            compacted_count: after,
            tokens_saved: would_compact
                .saturating_mul(TOKENS_PER_MESSAGE)
                .saturating_sub(SUMMARY_TOKENS),
            num_discarded: would_compact,
        }
    }

    /// Format context status for display
    #[allow(dead_code)]
    pub(crate) fn format_status(&self, current_tokens: usize) -> String {
        let usage_pct = self.usage_percentage(current_tokens);

        format!(
            "Context: {} / {} tokens ({:.1}%)\nCompaction threshold: {}% ({})\nEnabled: {}",
            current_tokens,
            self.context_size,
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
                // Truncate args to avoid blowing up the summary prompt.
                // UTF-8 safe — see crate::utils::strings.
                let args_short = truncate_to_char_boundary(args, 200);
                output.push_str(&format!("Assistant [called {}({})]\n\n", name, args_short));
            }
            MessageRole::ToolResult => {
                let name = msg.tool_name.as_deref().unwrap_or("unknown");
                let result = msg.tool_result.as_deref().unwrap_or("");
                let result_short = truncate_to_char_boundary(result, 500);
                output.push_str(&format!("Tool [{}] returned: {}\n\n", name, result_short));
            }
            MessageRole::Summary => {
                output.push_str(&format!("Previous summary: {}\n\n", msg.content));
            }
            MessageRole::System => {}
        }
    }

    output
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

/// Advance a compaction boundary forward so it never lands *on* a `ToolResult`.
///
/// The summary is inserted at the boundary; a boundary on a `ToolResult` whose
/// `ToolCall` was rescued just before it wedges the summary between the pair,
/// which Anthropic rejects (`tool_use ids were found without tool_result blocks
/// immediately after`). Snapping past leading `ToolResult`s keeps each pair
/// together. Returns the boundary unchanged when it already points at a
/// non-`ToolResult` or the end of the list.
pub(crate) fn snap_boundary_past_tool_results(messages: &[ChatMessage], boundary: usize) -> usize {
    use crate::ui::app_state::MessageRole;

    let mut b = boundary;
    while b < messages.len() && messages[b].role == MessageRole::ToolResult {
        b += 1;
    }
    b
}

/// Get the default context config
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
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
        let messages = [
            ChatMessage::user("List my files".to_string()),
            ChatMessage::tool_call("bash", r#"{"command":"ls"}"#, Some("call_1".to_string())),
            ChatMessage::tool_result(
                "bash",
                r#"{"command":"ls"}"#,
                "file1.txt\nfile2.txt",
                Some("call_1".to_string()),
            ),
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
        let messages = [
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
            ChatMessage::tool_result(
                "bash",
                r#"{"cmd":"ls"}"#,
                "file1.txt",
                Some("call_1".to_string()),
            ),
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

    /// Regression: a boundary on a ToolResult must snap forward, else the
    /// inserted summary orphans the rescued ToolCall and Anthropic rejects it.
    #[test]
    fn snap_boundary_advances_past_tool_result() {
        let messages = vec![
            ChatMessage::user("q".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "out", Some("c1".to_string())),
            ChatMessage::agent("a".to_string()),
        ];

        // Boundary points at the ToolResult (index 2) — the bug trigger.
        assert_eq!(snap_boundary_past_tool_results(&messages, 2), 3);
    }

    #[test]
    fn snap_boundary_leaves_non_tool_result_untouched() {
        let messages = vec![
            ChatMessage::user("q".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "out", Some("c1".to_string())),
            ChatMessage::tool_call("bash", "{}", Some("c2".to_string())),
        ];

        // Boundary on a ToolCall: kept call+result stay together, no snap needed.
        assert_eq!(snap_boundary_past_tool_results(&messages, 3), 3);
        // Boundary on a User message: untouched.
        assert_eq!(snap_boundary_past_tool_results(&messages, 0), 0);
    }

    #[test]
    fn snap_boundary_skips_consecutive_tool_results() {
        // A ToolCall whose call_id has two results (duplicate-id case) — snap
        // must skip BOTH so neither gets orphaned across the summary insert.
        let messages = vec![
            ChatMessage::tool_call("bash", "{}", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "r1", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "r2", Some("c1".to_string())),
            ChatMessage::agent("done".to_string()),
        ];

        assert_eq!(snap_boundary_past_tool_results(&messages, 1), 3);
    }

    #[test]
    fn snap_boundary_clamps_at_end() {
        let messages = vec![
            ChatMessage::tool_call("bash", "{}", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "r1", Some("c1".to_string())),
        ];

        // Boundary at the trailing ToolResult would snap to len() (compact all).
        assert_eq!(snap_boundary_past_tool_results(&messages, 1), 2);
        // Boundary already at end is a no-op.
        assert_eq!(snap_boundary_past_tool_results(&messages, 2), 2);
    }

    /// After the snap, every kept ToolCall is immediately followed by its
    /// ToolResult even with the summary inserted at the boundary.
    #[test]
    fn snap_prevents_summary_splitting_a_pair() {
        use crate::ui::app_state::MessageRole;

        let messages = vec![
            ChatMessage::user("q".to_string()),
            ChatMessage::tool_call("bash", "{}", Some("c1".to_string())),
            ChatMessage::tool_result("bash", "{}", "out", Some("c1".to_string())),
            ChatMessage::agent("a".to_string()),
        ];

        // Raw boundary bisects the pair (points at the ToolResult).
        let raw = 2;
        let snapped = snap_boundary_past_tool_results(&messages, raw);

        // Simulate apply_compaction: rescue calls before boundary whose result is
        // at/after boundary, then insert a summary at the boundary.
        let needed: std::collections::HashSet<usize> =
            find_needed_tool_calls_chat(&messages, snapped)
                .into_iter()
                .collect();
        let mut seq: Vec<(MessageRole, Option<String>)> = Vec::new();
        for (i, m) in messages.iter().enumerate() {
            if i == snapped {
                seq.push((MessageRole::Summary, None));
            }
            if i >= snapped || needed.contains(&i) {
                seq.push((m.role, m.call_id.clone()));
            }
        }
        if snapped >= messages.len() {
            seq.push((MessageRole::Summary, None));
        }

        // Invariant: every kept ToolCall is immediately followed by its result.
        for w in seq.windows(2) {
            if w[0].0 == MessageRole::ToolCall {
                assert_eq!(
                    w[1].0,
                    MessageRole::ToolResult,
                    "ToolCall must be immediately followed by ToolResult, got {:?}",
                    w[1]
                );
                assert_eq!(w[0].1, w[1].1, "call_id mismatch across the pair");
            }
        }
    }

    #[test]
    fn test_truncate_at_call_sites() {
        // The two call sites in `build_summary_prompt` rely on
        // `crate::utils::truncate_to_char_boundary` to stay UTF-8 safe.
        // Full behavioural tests live in `crate::utils::strings::tests`;
        // this is just a smoke-pin that the helper resolves and the
        // call-site budgets are sane.
        assert_eq!(truncate_to_char_boundary("hello", 200), "hello");
        assert_eq!(truncate_to_char_boundary("hello world", 5), "hello");
        // Multi-byte content at the 200/500-byte cuts used in this file
        // must never panic — regression for the same bug class as #9.
        let multibyte = format!("a{}", "🦀".repeat(75)); // 301 bytes, boundary 297
        let cut = truncate_to_char_boundary(&multibyte, 200);
        assert!(multibyte.is_char_boundary(cut.len()));
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

    /// Regression pin for the cancelled-Step-4 fukup
    /// (see `context-fukup.md`).
    ///
    /// `ContextManager::new` must store the `context_size: usize` it
    /// receives verbatim, with no fallback / auto-detect / global-config
    /// override path. Resolution is the caller's job; the manager is
    /// the single consumer. If this test fails, somebody re-introduced
    /// a per-model-vs-global tug-of-war upstream.
    #[test]
    fn context_size_is_stored_verbatim_and_drives_threshold() {
        let cfg = ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            compaction_model: None,
        };
        let cm = ContextManager::new(cfg, 50_000, None);
        assert_eq!(cm.context_size(), 50_000);
        // 50_000 × 0.8 = 40_000
        assert_eq!(cm.threshold(), 40_000);
    }
}
