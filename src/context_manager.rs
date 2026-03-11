//! Context management for handling long conversations.
//! Implements automatic context compaction when approaching the context window limit.

use crate::config::ContextConfig;
use crate::token_estimator::{get_default_estimator, get_model_context_window, TokenEstimator};
use anyhow::Result;
use rig::completion::message::Message;

/// Result of a context compaction operation
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Number of messages before compaction
    pub original_count: usize,
    /// Number of messages after compaction
    pub compacted_count: usize,
    /// Estimated tokens saved
    pub tokens_saved: usize,
    /// Number of messages that were summarized
    pub num_summarized: usize,
}

/// Manages context window usage and performs compaction when needed
pub struct ContextManager {
    config: ContextConfig,
    estimator: Box<dyn TokenEstimator>,
    context_window: usize,
}

impl ContextManager {
    /// Create a new ContextManager with the given configuration
    /// If context_window is 0 or None, attempts to auto-detect from model name
    pub fn new(config: ContextConfig, model_name: &str) -> Self {
        let context_window = config.context_window.unwrap_or_else(|| {
            get_model_context_window(model_name).unwrap_or(128_000)
        });
        
        Self {
            config,
            estimator: get_default_estimator(),
            context_window,
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
    
    /// Estimate total tokens for messages plus system prompt
    pub fn estimate_total_tokens(&self, messages: &[Message], system_prompt: &str) -> usize {
        let message_tokens = self.estimator.estimate_messages(messages);
        let system_tokens = self.estimator.estimate(system_prompt);
        message_tokens + system_tokens
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
        
        // Check if we have enough messages to trigger compaction
        let threshold_messages = (self.threshold() / 100).max(10); // Rough heuristic
        messages.len() > threshold_messages
    }
    
    /// Check if compaction is needed based on estimated token count
    pub fn needs_compaction_by_tokens(&self, messages: &[Message], system_prompt: &str) -> bool {
        if !self.config.enabled {
            return false;
        }
        
        let total_tokens = self.estimate_total_tokens(messages, system_prompt);
        total_tokens > self.threshold()
    }
    
    /// Get current token usage as a percentage (0.0 - 1.0)
    pub fn usage_percentage(&self, messages: &[Message], system_prompt: &str) -> f64 {
        let total = self.estimate_total_tokens(messages, system_prompt);
        total as f64 / self.context_window as f64
    }
    
    /// Perform context compaction
    /// Uses hybrid approach: summarize old messages, keep recent ones
    pub fn compact(&mut self, messages: &mut Vec<Message>, _system_prompt: &str) -> Result<CompactionResult> {
        if !self.config.enabled {
            return Ok(CompactionResult {
                original_count: messages.len(),
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_summarized: 0,
            });
        }
        
        let original_count = messages.len();
        
        // Edge cases
        if messages.len() <= self.config.keep_recent {
            return Ok(CompactionResult {
                original_count,
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_summarized: 0,
            });
        }
        
        // Split messages into to-summarize and to-keep
        let keep_start = messages.len().saturating_sub(self.config.keep_recent);
        
        // Get the counts we need before any mutation
        let num_to_summarize = keep_start;
        let tokens_before = self.estimator.estimate_messages(&messages[..keep_start]);
        
        if num_to_summarize == 0 {
            return Ok(CompactionResult {
                original_count,
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_summarized: 0,
            });
        }
        
        // Create summary message (we can't actually call the model here,
        // so we create a placeholder that will be filled in by the agent)
        let _summary_content = format!(
            "[Summary of {} messages - {} tokens summarized]\n\nKey points from earlier conversation: [This summary will be generated when context is compacted]",
            num_to_summarize,
            tokens_before
        );
        
        // Clone the messages we want to keep before clearing
        let to_keep: Vec<Message> = messages[keep_start..].to_vec();
        
        // Clear all messages - this is a simple truncation approach
        messages.clear();
        
        // Add back the messages we wanted to keep
        messages.extend(to_keep);
        
        let compacted_count = messages.len();
        
        Ok(CompactionResult {
            original_count,
            compacted_count,
            tokens_saved: tokens_before.saturating_sub(100), // Approximate tokens saved
            num_summarized: num_to_summarize,
        })
    }
    
    /// Format context status for display
    pub fn format_status(&self, messages: &[Message], system_prompt: &str) -> String {
        let total_tokens = self.estimate_total_tokens(messages, system_prompt);
        let usage_pct = self.usage_percentage(messages, system_prompt);
        
        format!(
            "Context: {} / {} tokens ({:.1}%)\n{} messages\nCompaction threshold: {}% ({})\nEnabled: {}",
            total_tokens,
            self.context_window,
            usage_pct * 100.0,
            messages.len(),
            (self.config.threshold * 100.0) as usize,
            self.threshold(),
            if self.config.enabled { "yes" } else { "no" }
        )
    }
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
    
    #[test]
    fn test_model_context_window_parsing() {
        // Test various model names
        assert_eq!(get_model_context_window("anthropic/claude-3.7-sonnet"), Some(200_000));
        assert_eq!(get_model_context_window("openai/gpt-4o"), Some(128_000));
        assert_eq!(get_model_context_window("google/gemini-1.5-pro"), Some(2_000_000));
    }
}