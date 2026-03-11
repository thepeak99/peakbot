//! Token cost tracking hook for PeakBot.
//!
//! This module provides a PromptHook that tracks token usage and calculates costs
//! based on model pricing fetched from OpenRouter's API.

use anyhow::{Result, anyhow};
use rig::agent::{HookAction, PromptHook};
use rig::completion::{CompletionModel, CompletionResponse, message::Message};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// Pricing information for a model (cost per token)
#[derive(Clone, Debug)]
pub struct ModelPricing {
    /// Cost per input token (USD)
    pub input_per_token: f64,
    /// Cost per output token (USD)
    pub output_per_token: f64,
}

impl Default for ModelPricing {
    fn default() -> Self {
        // Default to Claude 3.7 Sonnet pricing (approx $3.00/M input, $15.00/M output)
        Self {
            input_per_token: 0.000003,
            output_per_token: 0.000015,
        }
    }
}

/// Statistics for a single API request
#[derive(Clone, Debug)]
pub struct RequestStats {
    /// Number of input tokens in this request
    pub input_tokens: u64,
    /// Number of output tokens in this request
    pub output_tokens: u64,
    /// Cost of this request in USD
    pub cost: f64,
}

/// Accumulated session statistics
#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    /// Total input tokens across all requests
    pub total_input_tokens: u64,
    /// Total output tokens across all requests
    pub total_output_tokens: u64,
    /// Total number of API calls
    pub total_api_calls: u64,
    /// Total cost in USD
    pub total_cost: f64,
    /// Per-request history for debugging
    requests: Vec<RequestStats>,
}

impl SessionStats {
    /// Create a new empty SessionStats
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a request's stats to the session
    pub fn add_request(&mut self, input: u64, output: u64, cost: f64) {
        self.total_input_tokens += input;
        self.total_output_tokens += output;
        self.total_api_calls += 1;
        self.total_cost += cost;
        self.requests.push(RequestStats {
            input_tokens: input,
            output_tokens: output,
            cost,
        });
    }

    /// Get a summary string of the session stats
    pub fn summary(&self) -> String {
        format!(
            "Total API Calls: {}\nTotal Input Tokens: {}\nTotal Output Tokens: {}\nTotal Tokens: {}\nTotal Cost: ${:.4}",
            self.total_api_calls,
            self.total_input_tokens,
            self.total_output_tokens,
            self.total_input_tokens + self.total_output_tokens,
            self.total_cost
        )
    }

    /// Format stats for a single request with running total
    pub fn format_per_request(&self, input: u64, output: u64, cost: f64) -> String {
        format!(
            "[Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}]",
            input, output, cost, self.total_cost
        )
    }

    /// Reset all statistics
    pub fn reset(&mut self) {
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_api_calls = 0;
        self.total_cost = 0.0;
        self.requests.clear();
    }

    /// Get the last request's stats
    pub fn last_request(&self) -> Option<RequestStats> {
        self.requests.last().cloned()
    }
}

// Manual implementation of Send + Sync for SessionStats since Mutex guards are not Send
unsafe impl Send for SessionStats {}
unsafe impl Sync for SessionStats {}

/// OpenRouter API response structure for models list
#[derive(Deserialize, Debug)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

/// Individual model from OpenRouter API
#[derive(Deserialize, Debug)]
struct OpenRouterModel {
    id: String,
    pricing: Pricing,
}

/// Pricing structure from OpenRouter API (3 fields: prompt=input, completion=output, web_search=ignored)
#[derive(Deserialize, Debug)]
struct Pricing {
    /// Cost per input token (as string, e.g., "0.00003" = $0.03/M)
    #[serde(rename = "prompt")]
    prompt: String,
    /// Cost per output token (as string, e.g., "0.00018" = $0.18/M)
    #[serde(rename = "completion")]
    completion: String,
    /// Cost for web search (not used for token cost calculation)
    #[allow(unused)] //We'll allow this for now
    #[serde(rename = "web_search")]
    #[serde(default)]
    web_search: Option<String>,
}

/// Fetches model pricing from OpenRouter API.
///
/// # Arguments
/// * `api_key` - OpenRouter API key
/// * `model` - Model ID (e.g., "anthropic/claude-3.7-sonnet")
///
/// # Returns
/// * `Ok(ModelPricing)` - The pricing information for the model
/// * `Err` - If the API call fails or model is not found
pub async fn fetch_model_pricing(api_key: &str, model: &str) -> Result<ModelPricing> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("User-Agent", "PeakBot/1.0")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("OpenRouter API error: {}", response.status()).into());
    }

    let models: OpenRouterModelsResponse = response.json().await?;
    get_model_pricing(models.data, model)
}

fn get_model_pricing(models: Vec<OpenRouterModel>, model: &str) -> Result<ModelPricing> {
    // Find our model (API returns average price already per model)
    if let Some(model_info) = models.iter().find(|m| m.id == model) {
        // Parse the pricing fields directly - API already returns average price
        // prompt = input price, completion = output price
        let input_per_token = model_info
            .pricing
            .prompt
            .parse::<f64>()
            .map_err(|_| anyhow!("Failed to parse input price"))?;
        let output_per_token = model_info
            .pricing
            .completion
            .parse::<f64>()
            .map_err(|_| anyhow!("Failed to parse output price"))?;

        // OpenRouter returns prices per token (e.g., 0.000003 = $3.00 per million tokens)
        Ok(ModelPricing {
            input_per_token,
            output_per_token,
        })
    } else {
        // Fallback to defaults if model not found
        tracing::warn!(
            "Model {} not found in OpenRouter, using default pricing",
            model
        );
        Ok(ModelPricing::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_model_pricing_minimax() {
        // JSON data from OpenRouter API for MiniMax M2.5 model
        let json_data = r#"[
            {
                "id": "minimax/minimax-m2.5",
                "canonical_slug": "minimax/minimax-m2.5-20260211",
                "hugging_face_id": "MiniMaxAI/MiniMax-M2.5",
                "name": "MiniMax: MiniMax M2.5",
                "created": 1770908502,
                "description": "MiniMax-M2.5 is a SOTA large language model",
                "context_length": 196608,
                "architecture": {
                    "modality": "text->text",
                    "input_modalities": ["text"],
                    "output_modalities": ["text"],
                    "tokenizer": "Other",
                    "instruct_type": null
                },
                "pricing": {
                    "prompt": "0.000000295",
                    "completion": "0.0000012",
                    "input_cache_read": "0.00000003"
                },
                "top_provider": {
                    "context_length": 196608,
                    "max_completion_tokens": 196608,
                    "is_moderated": false
                },
                "per_request_limits": null,
                "supported_parameters": ["frequency_penalty", "include_reasoning"],
                "default_parameters": {
                    "temperature": 1,
                    "top_p": 0.95,
                    "frequency_penalty": null
                },
                "expiration_date": null
            }
        ]"#;

        // Parse JSON into Vec<OpenRouterModel>
        let models: Vec<OpenRouterModel> = serde_json::from_str(json_data)
            .expect("Failed to parse JSON test data");

        // Verify we parsed one model
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "minimax/minimax-m2.5");

        // Call get_model_pricing
        let pricing = get_model_pricing(models, "minimax/minimax-m2.5")
            .expect("get_model_pricing should succeed");

        // Verify output pricing values
        assert_eq!(pricing.input_per_token, 0.000000295);
        assert_eq!(pricing.output_per_token, 0.0000012);
    }

    #[test]
    fn test_get_model_pricing_not_found() {
        // Empty models list - model not found
        let models: Vec<OpenRouterModel> = vec![];

        // Call get_model_pricing with non-existent model
        let pricing = get_model_pricing(models, "nonexistent/model")
            .expect("get_model_pricing should succeed even when model not found");

        // Should return default pricing
        assert_eq!(pricing.input_per_token, 0.000003);
        assert_eq!(pricing.output_per_token, 0.000015);
    }
}

/// Token cost tracking hook
///
/// This hook intercepts completion responses to track token usage
/// and calculate costs based on model pricing.
#[derive(Clone)]
pub struct TokenCostHook {
    /// Name of the model being used
    #[allow(dead_code)]
    model_name: String,
    /// Shared session statistics (using Arc<Mutex> for interior mutability)
    stats: Arc<Mutex<SessionStats>>,
    /// Model pricing (fetched from API or default)
    pricing: Arc<ModelPricing>,
}

impl TokenCostHook {
    /// Create a new hook with pre-configured pricing
    pub fn new(model_name: String, pricing: ModelPricing) -> Self {
        Self {
            model_name,
            stats: Arc::new(Mutex::new(SessionStats::new())),
            pricing: Arc::new(pricing),
        }
    }

    /// Create a new hook and fetch pricing from OpenRouter API
    pub async fn with_api_pricing(model_name: String, api_key: &str) -> Self {
        let pricing = fetch_model_pricing(api_key, &model_name)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to fetch model pricing: {}, using defaults", e);
                ModelPricing::default()
            });

        Self {
            model_name,
            stats: Arc::new(Mutex::new(SessionStats::new())),
            pricing: Arc::new(pricing),
        }
    }

    /// Calculate cost based on token usage and pricing
    /// Prices are already per-token from OpenRouter API
    fn calculate_cost(&self, input: u64, output: u64) -> f64 {
        let pricing = &self.pricing;
        (input as f64 * pricing.input_per_token) + (output as f64 * pricing.output_per_token)
    }

    /// Get a reference to the stats
    pub fn get_stats(&self) -> Arc<Mutex<SessionStats>> {
        self.stats.clone()
    }

    /// Get a formatted string for the last request
    pub fn get_last_request_stats(&self) -> Option<String> {
        let stats = self.stats.lock().ok()?;
        let last = stats.last_request()?;
        Some(stats.format_per_request(last.input_tokens, last.output_tokens, last.cost))
    }

    /// Get session summary
    pub fn get_session_summary(&self) -> Option<String> {
        let stats = self.stats.lock().ok()?;
        Some(stats.summary())
    }

    /// Reset the session stats
    pub fn reset_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.reset();
        }
    }
}

impl<M: CompletionModel> PromptHook<M> for TokenCostHook {
    /// Called before the prompt is sent to the model
    async fn on_completion_call(&self, _prompt: &Message, _history: &[Message]) -> HookAction {
        tracing::debug!("TokenCostHook: Starting completion call");
        HookAction::Continue
    }

    /// Called after the response is received - extract token usage here
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let usage = &response.usage;
        let input = usage.input_tokens;
        let output = usage.output_tokens;

        // Calculate cost using our pricing
        let cost = self.calculate_cost(input, output);

        // Update stats
        if let Ok(mut stats) = self.stats.lock() {
            stats.add_request(input, output, cost);

            // Log the stats
            tracing::info!(
                "Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}",
                input,
                output,
                cost,
                stats.total_cost
            );
        }

        HookAction::Continue
    }
}

/// Trait for accessing cost statistics from a hook
pub trait CostTrackingStats {
    /// Get formatted stats for the last request
    fn get_last_request_stats(&self) -> Option<String>;
    /// Get session summary
    fn get_session_summary(&self) -> Option<String>;
    /// Reset the session stats
    fn reset_stats(&self);
}

impl CostTrackingStats for TokenCostHook {
    fn get_last_request_stats(&self) -> Option<String> {
        let stats = self.stats.lock().ok()?;
        let last = stats.last_request()?;
        Some(stats.format_per_request(last.input_tokens, last.output_tokens, last.cost))
    }

    fn get_session_summary(&self) -> Option<String> {
        let stats = self.stats.lock().ok()?;
        Some(stats.summary())
    }

    fn reset_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.reset();
        }
    }
}
