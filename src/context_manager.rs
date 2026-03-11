//! Context management for handling long conversations.
//! Implements automatic context compaction when approaching the context window limit.
//! Uses actual token counts from the provider (via token_cost hook) instead of estimates.

use crate::config::ContextConfig;
use crate::hooks::token_cost::SessionStats;
use anyhow::Result;
use rig::completion::message::Message;
use std::sync::{Arc, Mutex};

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
/// Uses actual token counts from the provider via SessionStats
pub struct ContextManager {
    config: ContextConfig,
    context_window: usize,
    /// System prompt size (in tokens) - used to subtract from total
    system_prompt_tokens: usize,
    /// Reference to session stats for actual token counts
    stats: Arc<Mutex<SessionStats>>,
}

impl ContextManager {
    /// Create a new ContextManager with the given configuration
    /// If context_window is 0 or None, attempts to auto-detect from model name
    pub fn new(config: ContextConfig, model_name: &str, stats: Arc<Mutex<SessionStats>>, system_prompt_tokens: usize) -> Self {
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
                _ => 128_000, // Default fallback
            }
        });
        
        Self {
            config,
            context_window,
            system_prompt_tokens,
            stats,
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
        let stats = self.stats.lock().unwrap();
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
    
    /// Perform context compaction
    /// Uses truncation approach: keep recent messages, discard older ones
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
        
        // Get token count before compaction
        let tokens_before = self.get_current_tokens();
        
        // Split messages: keep recent, discard older
        let keep_start = messages.len().saturating_sub(self.config.keep_recent);
        let num_to_summarize = keep_start;
        
        if num_to_summarize == 0 {
            return Ok(CompactionResult {
                original_count,
                compacted_count: messages.len(),
                tokens_saved: 0,
                num_summarized: 0,
            });
        }
        
        // Clone the messages we want to keep before clearing
        let to_keep: Vec<Message> = messages[keep_start..].to_vec();
        
        // Clear all messages - simple truncation approach
        messages.clear();
        
        // Add back the messages we wanted to keep
        messages.extend(to_keep);
        
        let compacted_count = messages.len();
        
        // Estimate tokens saved (rough approximation)
        let tokens_saved = tokens_before.saturating_sub(self.config.keep_recent * 50); // ~50 tokens per message
        
        Ok(CompactionResult {
            original_count,
            compacted_count,
            tokens_saved,
            num_summarized: num_to_summarize,
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
}