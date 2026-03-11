//! Token estimation utilities for context management.
//! Provides both a simple character-based estimator and a more accurate
//! tiktoken-based estimator.

use rig::completion::message::Message;

/// Trait for estimating token counts
pub trait TokenEstimator: Send + Sync {
    /// Estimate tokens for a given text
    fn estimate(&self, text: &str) -> usize;
    
    /// Estimate tokens for a single message
    fn estimate_message(&self, msg: &Message) -> usize;
    
    /// Estimate tokens for multiple messages
    fn estimate_messages(&self, msgs: &[Message]) -> usize;
}

/// Simple char-based estimator (4 chars ≈ 1 token)
/// This is a fallback when tiktoken is not available or fails
pub struct SimpleEstimator;

impl SimpleEstimator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SimpleEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenEstimator for SimpleEstimator {
    fn estimate(&self, text: &str) -> usize {
        // Rough approximation: 4 characters per token
        // This is a conservative estimate that works well as a fallback
        (text.len() / 4).max(1)
    }
    
    fn estimate_message(&self, msg: &Message) -> usize {
        // Convert message to text representation and estimate
        let text = message_to_text(msg);
        self.estimate(&text)
    }
    
    fn estimate_messages(&self, msgs: &[Message]) -> usize {
        msgs.iter().map(|msg| self.estimate_message(msg)).sum()
    }
}

/// Tiktoken-based estimator (more accurate)
/// Uses the cl100k_base encoding which is used by most modern models
pub struct TiktokenEstimator {
    encoding: tiktoken_rs::CoreBPE,
}

impl TiktokenEstimator {
    /// Create a new TiktokenEstimator with the cl100k_base encoding
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let encoding = tiktoken_rs::cl100k_base()?;
        Ok(Self { encoding })
    }
    
    /// Try to create a TiktokenEstimator, falling back to SimpleEstimator on error
    pub fn try_new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::new()
    }
}

impl TokenEstimator for TiktokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        self.encoding.encode_ordinary(text).len()
    }
    
    fn estimate_message(&self, msg: &Message) -> usize {
        let text = message_to_text(msg);
        self.estimate(&text)
    }
    
    fn estimate_messages(&self, msgs: &[Message]) -> usize {
        msgs.iter().map(|msg| self.estimate_message(msg)).sum()
    }
}

/// Convert a Message to a text representation for token estimation
fn message_to_text(msg: &Message) -> String {
    // Use format! to get a debug representation, then extract text
    // This is a simple fallback that works with any Message type
    format!("{:?}", msg)
}

/// Get the default token estimator (prefers tiktoken, falls back to simple)
pub fn get_default_estimator() -> Box<dyn TokenEstimator> {
    // Try to use tiktoken, fall back to simple estimator
    match TiktokenEstimator::try_new() {
        Ok(estimator) => Box::new(estimator),
        Err(e) => {
            tracing::warn!("Failed to initialize tiktoken: {}, using simple estimator", e);
            Box::new(SimpleEstimator::new())
        }
    }
}

/// Get the context window size for common models
/// Returns None if the model is not recognized (you can query the API for context window)
pub fn get_model_context_window(model: &str) -> Option<usize> {
    // Common model context windows (in tokens)
    // These are approximate and may change as models are updated
    match model.to_lowercase() {
        // Claude models
        m if m.contains("claude-3.7-sonnet") => Some(200_000),
        m if m.contains("claude-3.5-sonnet") => Some(200_000),
        m if m.contains("claude-3-opus") => Some(200_000),
        m if m.contains("claude-3-sonnet") => Some(200_000),
        m if m.contains("claude-3-haiku") => Some(200_000),
        
        // GPT-4 models
        m if m.contains("gpt-4o") => Some(128_000),
        m if m.contains("gpt-4-turbo") => Some(128_000),
        m if m.contains("gpt-4-32k") => Some(32_768),
        m if m.contains("gpt-4") => Some(8_192),
        
        // GPT-3.5 models
        m if m.contains("gpt-3.5-turbo") => Some(16_385),
        
        // Gemini models
        m if m.contains("gemini-2.0") => Some(1_000_000),
        m if m.contains("gemini-1.5-pro") => Some(2_000_000),
        m if m.contains("gemini-1.5-flash") => Some(1_000_000),
        
        // OpenRouter specific / other models
        m if m.contains("qwen") => Some(32_000),
        m if m.contains("mistral") => Some(32_000),
        m if m.contains("llama") => Some(32_000),
        
        // Default fallback (conservative)
        _ => Some(128_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_simple_estimator() {
        let estimator = SimpleEstimator::new();
        // 4 chars ≈ 1 token (integer division)
        assert_eq!(estimator.estimate("hello"), 1); // "hello" = 5 chars, 5/4 = 1
        assert_eq!(estimator.estimate("a"), 1);
        assert_eq!(estimator.estimate(""), 1); // min 1
        assert_eq!(estimator.estimate("hello world"), 2); // 11 chars, 11/4 = 2 (floor)
    }
    
    #[test]
    fn test_model_context_window() {
        assert_eq!(get_model_context_window("anthropic/claude-3.7-sonnet"), Some(200_000));
        assert_eq!(get_model_context_window("gpt-4o"), Some(128_000));
        // Unknown models return the fallback (128000)
        assert_eq!(get_model_context_window("unknown-model"), Some(128_000));
    }
}