//! Session hook for PeakBot.
//!
//! This module provides a PromptHook that emits events for tracking agent activity.
//! Events are streamed to handlers via async channels.

use anyhow::{Result, anyhow};
use chrono::Utc;
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, message::Message};
use rig::completion::message::AssistantContent;
use rig::one_or_many::OneOrMany;
use serde::Deserialize;
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::hooks::events::AgentEvent;
use crate::hooks::events::TokenUsage as EventTokenUsage;

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

    /// Get total input tokens used so far (approximates context size)
    /// This is the sum of all input tokens from all requests
    pub fn total_input_tokens(&self) -> u64 {
        self.total_input_tokens
    }

    /// Get total output tokens used so far
    pub fn total_output_tokens(&self) -> u64 {
        self.total_output_tokens
    }

    /// Get total tokens (input + output)
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }

    /// Get the input tokens from the most recent request
    /// This approximates the current context size (history + new message)
    pub fn last_input_tokens(&self) -> Option<u64> {
        self.requests.last().map(|r| r.input_tokens)
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
        let models: Vec<OpenRouterModel> =
            serde_json::from_str(json_data).expect("Failed to parse JSON test data");

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

#[derive(Clone)]
pub struct SessionHook {
    /// Channel sender for streaming events
    event_sender: Option<mpsc::UnboundedSender<AgentEvent>>,
    /// Reference to session stats for token tracking
    stats: Arc<Mutex<SessionStats>>,
    /// Context window size in tokens
    context_window: u64,
    /// Compaction threshold (0.0-1.0)
    threshold: f64,
}

impl SessionHook {
    /// Create a new minimal session hook (no context checking)
    pub fn new(event_sender: Option<mpsc::UnboundedSender<AgentEvent>>) -> Self {
        Self {
            event_sender,
            stats: Arc::new(Mutex::new(SessionStats::new())),
            context_window: 128_000,
            threshold: 0.8,
        }
    }

    /// Create a new session hook with full context checking
    pub fn with_context_tracking(
        event_sender: Option<mpsc::UnboundedSender<AgentEvent>>,
        stats: Arc<Mutex<SessionStats>>,
        context_window: u64,
        threshold: f64,
    ) -> Self {
        Self {
            event_sender,
            stats,
            context_window,
            threshold,
        }
    }

    /// Create a new session hook with a new event channel (backward compatible)
    pub fn with_channel() -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                event_sender: Some(sender),
                stats: Arc::new(Mutex::new(SessionStats::new())),
                context_window: 128_000,
                threshold: 0.8,
            },
            receiver,
        )
    }
}

/// Extract text and reasoning from the response choice
/// Simplified extraction - just gets basic info without worrying about all edge cases
fn extract_content_from_response(choice: &OneOrMany<AssistantContent>) -> (String, Option<String>) {
    let mut text = String::new();
    let mut reasoning = None;

    // Try to iterate over the contents
    for item in choice.iter() {
        match item {
            AssistantContent::Text(t) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t.to_string());
            }
            AssistantContent::Reasoning(r) => {
                // Extract reasoning text from the Reasoning struct
                let mut reasonings = String::new();
                for rc in &r.content {
                    match rc {
                        rig::completion::message::ReasoningContent::Text { text: t, .. } => {
                            if !reasonings.is_empty() {
                                reasonings.push('\n');
                            }
                            reasonings.push_str(t);
                        }
                        rig::completion::message::ReasoningContent::Summary(s) => {
                            if !reasonings.is_empty() {
                                reasonings.push('\n');
                            }
                            reasonings.push_str(s);
                        }
                        _ => {}
                    }
                }
                if !reasonings.is_empty() {
                    reasoning = Some(reasonings);
                }
            }
            AssistantContent::ToolCall(_tc) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("[tool call]");
            }
            AssistantContent::Image(_) => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("[image]");
            }
        }
    }

    (text, reasoning)
}

impl<M: CompletionModel> PromptHook<M> for SessionHook {
    /// Called before the prompt is sent - just emit an event
    async fn on_completion_call(&self, _prompt: &Message, history: &[Message]) -> HookAction {
        if let Some(ref sender) = self.event_sender {
            let event = AgentEvent::CompletionRequest {
                message_count: history.len() + 1,
                estimated_tokens: None,
                timestamp: Utc::now(),
            };
            let _ = sender.send(event);
        }
        HookAction::Continue
    }

    /// Called after response - emit event with raw token data (no calculation)
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        if let Some(ref sender) = self.event_sender {
            let usage = &response.usage;

            // Extract content and reasoning from response using rig's API
            let (content, reasoning) = extract_content_from_response(&response.choice);

            // Emit event with RAW token counts - cost calculation happens in CostHandler
            let event = AgentEvent::CompletionResponse {
                content,
                reasoning,
                usage: EventTokenUsage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens: usage.input_tokens + usage.output_tokens,
                    cost: 0.0, // Cost = 0 here, calculated by CostHandler
                },
                timestamp: Utc::now(),
            };
            let _ = sender.send(event);
        }
        HookAction::Continue
    }

    /// Called before tool invocation - emit event
    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        if let Some(ref sender) = self.event_sender {
            let event = AgentEvent::ToolCall {
                tool_name: tool_name.to_string(),
                arguments: args.to_string(),
                call_id: tool_call_id.or(Some(internal_call_id.to_string())),
                timestamp: Utc::now(),
            };
            let _ = sender.send(event);
        }
        ToolCallHookAction::Continue
    }

    /// Called after tool result - emit event and check for context overflow
    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        if let Some(ref sender) = self.event_sender {
            let event = AgentEvent::ToolResult {
                tool_name: tool_name.to_string(),
                arguments: args.to_string(),
                result: result.to_string(),
                success: !result.starts_with("Error:"),
                call_id: tool_call_id.or(Some(internal_call_id.to_string())),
                timestamp: Utc::now(),
            };
            let _ = sender.send(event);
        }

        // Check context usage and terminate if needed
        if let Ok(stats) = self.stats.lock() {
            if let Some(current_tokens) = stats.last_input_tokens() {
                let threshold_tokens = (self.context_window as f64 * self.threshold) as u64;
                if current_tokens > threshold_tokens {
                    tracing::info!(
                        "Context at {}% ({} tokens), triggering compact and resume",
                        (current_tokens as f64 / self.context_window as f64) * 100.0,
                        current_tokens
                    );
                    return HookAction::terminate("Context limit reached, compacting");
                }
            }
        }

        HookAction::Continue
    }
}
