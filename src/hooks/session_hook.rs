//! Session hook for PeakBot.
//!
//! This module provides a PromptHook that emits events for tracking agent activity.
//! Events are streamed to handlers via async channels.

use anyhow::{Result, anyhow};
use chrono::Utc;
use rig_core::agent::{
    HookAction, InvalidToolCallContext, InvalidToolCallHookAction, PromptHook, ToolCallHookAction,
};
use rig_core::completion::message::AssistantContent;
use rig_core::completion::{CompletionModel, CompletionResponse, message::Message};
use rig_core::one_or_many::OneOrMany;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::mpsc;

use crate::hooks::events::AgentEvent;
use crate::hooks::events::SourcedEvent;
use crate::hooks::events::TokenUsage as EventTokenUsage;
use crate::ui::app_state::MessageSource;

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

/// One bucket per lane label (`"orchestrator"` or a sub-agent role). Every
/// field accumulates — unlike the flat totals, whose input/output are the last
/// request's. A lane's question is "what did this agent consume over the whole
/// session", not "how big was its last context".
#[derive(Clone, Debug, Default)]
pub struct LaneStats {
    /// Input tokens summed across every request on this lane.
    pub input_tokens: u64,
    /// Output tokens summed across every request on this lane.
    pub output_tokens: u64,
    /// API calls made on this lane.
    pub api_calls: u64,
    /// Accumulated cost (USD) on this lane.
    pub cost: f64,
}

/// Accumulated session statistics
///
/// NOTE: `total_input_tokens` and `total_output_tokens` are OVERWRITTEN per
/// request (not accumulated). They reflect the LAST request's token counts.
/// This mirrors rig's per-request usage reporting where input_tokens is the
/// full context size sent to the LLM. For the true cumulative sum across all
/// requests, use `cumulative_input_tokens()`.
#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    /// Input tokens from the LAST request (overwritten, not accumulated)
    pub total_input_tokens: u64,
    /// Output tokens from the LAST request (overwritten, not accumulated)
    pub total_output_tokens: u64,
    /// Total number of API calls
    pub total_api_calls: u64,
    /// Total cost in USD
    pub total_cost: f64,
    /// Per-request history for debugging
    requests: Vec<RequestStats>,
    /// Per-lane breakdown, keyed by lane label. Empty until the
    /// first lane-attributed request; the flat totals above are the
    /// authoritative grand total and are never derived from this map.
    lanes: std::collections::HashMap<String, LaneStats>,
    /// Input tokens of the last orchestrator-lane request.
    /// The compaction gate reads this; a sub-agent's request must not move it.
    /// `None` = no reading yet (gate falls through to the message-count heuristic).
    /// Deliberately NOT derived from `lanes`: that map is display state for
    /// `/stats`, and the compaction loop-guard has to zero this without lying
    /// to the user about what the orchestrator's last request cost.
    last_orchestrator_input_tokens: Option<u64>,
}

impl SessionStats {
    /// Create a new empty SessionStats
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a request's stats to the session, attributed to a lane.
    ///
    /// Token counts (input/output) are overwritten per request — only the most recent
    /// request's token usage is tracked. This mirrors rig's per-request usage reporting.
    /// API call count and total cost accumulate across all requests. The lane's
    /// bucket accumulates everything, tokens included.
    pub fn add_request(&mut self, lane: &str, input: u64, output: u64, cost: f64) {
        self.total_input_tokens = input;
        self.total_output_tokens = output;
        self.total_api_calls += 1;
        self.total_cost += cost;
        self.requests.push(RequestStats {
            input_tokens: input,
            output_tokens: output,
            cost,
        });

        let bucket = self.lanes.entry(lane.to_string()).or_default();
        bucket.input_tokens += input;
        bucket.output_tokens += output;
        bucket.api_calls += 1;
        bucket.cost += cost;

        if lane == crate::ui::app_state::ORCHESTRATOR_LANE {
            self.last_orchestrator_input_tokens = Some(input);
        }
    }

    /// Per-lane breakdown, sorted with `"orchestrator"` first then roles
    /// alphabetically — a stable order for the `/stats` breakdown.
    pub fn lanes_sorted(&self) -> Vec<(String, LaneStats)> {
        let mut rows: Vec<(String, LaneStats)> = self
            .lanes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        rows.sort_by(|a, b| match (a.0.as_str(), b.0.as_str()) {
            ("orchestrator", "orchestrator") => std::cmp::Ordering::Equal,
            ("orchestrator", _) => std::cmp::Ordering::Less,
            (_, "orchestrator") => std::cmp::Ordering::Greater,
            (x, y) => x.cmp(y),
        });
        rows
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
        self.lanes.clear();
    }

    /// Restore stats from a persisted snapshot (e.g. after `/load`).
    ///
    /// Replaces the current stats wholesale with the supplied values and seeds
    /// `requests` with a single synthetic entry so that `last_input_tokens()`
    /// — which the status bar uses as the live context-size indicator —
    /// returns the loaded conversation's last input count instead of `None`.
    pub fn restore(&mut self, input: u64, output: u64, api_calls: u64, cost: f64) {
        self.total_input_tokens = input;
        self.total_output_tokens = output;
        self.total_api_calls = api_calls;
        self.total_cost = cost;
        self.requests.clear();
        // Seed exactly one synthetic request so `last_input_tokens()` returns
        // the persisted value. We don't reconstruct full per-request history
        // (that detail isn't persisted) — just enough for the status bar.
        if api_calls > 0 || input > 0 || output > 0 {
            self.requests.push(RequestStats {
                input_tokens: input,
                output_tokens: output,
                cost,
            });
        }
        // /load must not blind the gate — seed it with the persisted input count.
        self.last_orchestrator_input_tokens = Some(input);
    }

    /// Replace the per-lane breakdown wholesale (e.g. after `/load`). The flat
    /// totals restored by [`Self::restore`] stay authoritative; this only
    /// rehydrates the per-lane buckets so a resumed conversation can scope the
    /// Session panel to a sub-agent instead of reading zeros.
    pub fn restore_lanes(&mut self, lanes: impl IntoIterator<Item = (String, LaneStats)>) {
        self.lanes = lanes.into_iter().collect();
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

    /// Get the sum of all input tokens across all requests.
    /// Unlike total_input_tokens which stores the last request's value,
    /// this computes the cumulative sum of all requests.
    pub fn cumulative_input_tokens(&self) -> u64 {
        self.requests.iter().map(|r| r.input_tokens).sum()
    }

    /// Get all request stats for detailed inspection.
    pub fn all_requests(&self) -> Vec<RequestStats> {
        self.requests.clone()
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

    /// Input tokens of the last orchestrator-lane request.
    /// The compaction gate's reading — a sub-agent's request never moves it.
    /// `None` → gate falls back to the message-count heuristic.
    pub fn last_orchestrator_input_tokens(&self) -> Option<u64> {
        self.last_orchestrator_input_tokens
    }

    /// Reset the "live context size" signal to 0 *without* touching cumulative
    /// `total_*` bookkeeping or the API-call counter.
    ///
    /// Called from `StateManager::apply_compaction` so the next
    /// `last_input_tokens()` reads `Some(0)`, which makes
    /// `ContextManager::needs_compaction` skip its token-based branch and
    /// fall through to the message-count fallback. Without this clear, a
    /// terminate-and-restart cycle in `SessionHook::on_completion_call`
    /// reads the *pre-compaction* wire size from the last real request,
    /// re-fires the threshold, and infinite-loops — see `compactfuck.md`
    /// (Bug #2/#3) and the regression pin
    /// `force_compact_makes_needs_compaction_return_false`.
    ///
    /// Implementation: pushes a synthetic zero-token entry rather than
    /// popping/clearing real history. Symmetric with the seed trick in
    /// `restore()`: same one-extra-Vec-entry cost, no behaviour difference
    /// for `total_input_tokens` / `total_cost` / `total_api_calls` because
    /// none of those increment via direct `requests.push`.
    pub fn clear_last_input_tokens(&mut self) {
        self.requests.push(RequestStats {
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
        });
        // Loop guard, same reason: the gate must not re-fire post-compaction.
        self.last_orchestrator_input_tokens = None;
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
    let client = crate::http::client();
    let response = client
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("User-Agent", "PeakBot/1.0")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("OpenRouter API error: {}", response.status()));
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

    /// `input_context_tokens` must recover the TRUE prompt size across the two
    /// provider usage shapes, and degrade gracefully when `total_tokens` is unset.
    #[test]
    fn input_context_tokens_normalizes_across_providers() {
        let mut u = rig_core::completion::Usage::new();

        // Anthropic + caching: input_tokens is the uncached slice only;
        // total - output_tokens recovers the full prompt (4005, not 5).
        u.input_tokens = 5;
        u.cached_input_tokens = 3000;
        u.cache_creation_input_tokens = 1000;
        u.output_tokens = 200;
        u.total_tokens = 5 + 3000 + 1000 + 200;
        assert_eq!(input_context_tokens(&u), 4005);

        // OpenAI/OpenRouter: input_tokens already folds in cached tokens, so it
        // must NOT be double-counted. total = prompt + completion.
        let mut o = rig_core::completion::Usage::new();
        o.input_tokens = 4005; // includes the 3000 cached as a subset
        o.cached_input_tokens = 3000;
        o.output_tokens = 200;
        o.total_tokens = 4205;
        assert_eq!(input_context_tokens(&o), 4005);

        // Degenerate provider that leaves total_tokens at 0: fall back to the
        // raw input_tokens rather than reporting 0.
        let mut d = rig_core::completion::Usage::new();
        d.input_tokens = 1234;
        d.output_tokens = 56;
        d.total_tokens = 0;
        assert_eq!(input_context_tokens(&d), 1234);
    }

    /// A tool call must contribute nothing to the prose lane, whether or not
    /// the same turn also carried text.
    #[test]
    fn tool_call_alongside_prose_yields_prose_only() {
        let choice = OneOrMany::many([
            AssistantContent::text("Reading the file."),
            AssistantContent::tool_call("1", "file_read", serde_json::json!({"path": "a.txt"})),
        ])
        .unwrap();

        let (text, reasoning) = extract_content_from_response(&choice);
        assert_eq!(text, "Reading the file.");
        assert!(!text.contains("[tool call]"));
        assert!(reasoning.is_none());

        // Text split around a tool call joins with a single newline and leaves
        // no dangling separator where the tool call used to sit.
        let split = OneOrMany::many([
            AssistantContent::text("a"),
            AssistantContent::tool_call("1", "file_read", serde_json::json!({})),
            AssistantContent::text("b"),
        ])
        .unwrap();

        let (text, reasoning) = extract_content_from_response(&split);
        assert_eq!(text, "a\nb");
        assert!(reasoning.is_none());
    }

    /// A pure tool-call turn produces empty prose. Downstream,
    /// `process_event_for_ui`'s `!content.trim().is_empty()` guard (src/lib.rs)
    /// turns that into "no transcript bubble", while the 🔧 entry arrives
    /// separately via `AgentEvent::ToolCall`.
    #[test]
    fn tool_call_only_response_yields_empty_prose() {
        let choice = OneOrMany::one(AssistantContent::tool_call(
            "1",
            "file_read",
            serde_json::json!({"path": "a.txt"}),
        ));

        let (text, reasoning) = extract_content_from_response(&choice);
        assert!(text.is_empty());
        assert!(reasoning.is_none());
    }

    /// Assistant images are never rendered — `ChatMessage.attachments` is
    /// user-only — so they must not leak a placeholder into the prose lane.
    #[test]
    fn assistant_image_contributes_no_prose() {
        let choice = OneOrMany::many([
            AssistantContent::text("Here it is."),
            AssistantContent::image_base64("aGVsbG8=", None, None),
        ])
        .unwrap();

        let (text, reasoning) = extract_content_from_response(&choice);
        assert_eq!(text, "Here it is.");
        assert!(!text.contains("[image]"));
        assert!(reasoning.is_none());
    }

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

    /// `clear_last_input_tokens` makes `last_input_tokens()` return Some(0)
    /// so `ContextManager::needs_compaction` skips its token-based branch
    /// and falls through to the message-count fallback. Cumulative
    /// `total_*` fields and `total_api_calls` MUST be untouched —
    /// `/stats` stays honest across compactions.
    ///
    /// Pinned by the in-loop compaction plan: the previous attempt
    /// (`compactfuck.md`) infinite-looped because nothing reset the stale
    /// `last_input_tokens` between terminate-and-restart cycles. This is
    /// the loop guard.
    #[test]
    fn clear_last_input_tokens_zeroes_last_without_touching_cumulative() {
        let mut stats = SessionStats::new();
        stats.add_request("orchestrator", 500, 200, 0.05);
        stats.add_request("orchestrator", 800, 300, 0.08);

        // Sanity: pre-clear, last_input_tokens reflects the most recent request.
        assert_eq!(stats.last_input_tokens(), Some(800));
        assert_eq!(stats.total_input_tokens, 800);
        assert_eq!(stats.total_api_calls, 2);
        let cost_before = stats.total_cost;

        stats.clear_last_input_tokens();

        // last_input_tokens now reads 0 (or None) — the token branch in
        // needs_compaction skips and the message-count fallback takes over.
        assert!(
            stats.last_input_tokens().unwrap_or(0) == 0,
            "last_input_tokens must read 0 after clear, got {:?}",
            stats.last_input_tokens()
        );

        // Cumulative bookkeeping is preserved: /stats keeps reporting honestly.
        assert_eq!(stats.total_api_calls, 2, "api call count must not change");
        assert!(
            (stats.total_cost - cost_before).abs() < f64::EPSILON,
            "total cost must not change"
        );

        // The orchestrator gate scalar is also cleared — the compaction gate
        // must not re-fire from a stale orchestrator-lane reading.
        assert!(
            stats.last_orchestrator_input_tokens().is_none(),
            "orchestrator gate must be cleared (None) after clear_last_input_tokens"
        );
    }

    /// `restore` seeds the orchestrator gate so `/load` does not blind it.
    /// Without this, a resumed conversation would see `None` and the gate
    /// would fall through to the message-count heuristic indefinitely.
    #[test]
    fn restore_seeds_the_orchestrator_gate_reading() {
        let mut stats = SessionStats::new();
        stats.restore(5_000, 200, 10, 1.50);
        assert_eq!(
            stats.last_orchestrator_input_tokens(),
            Some(5_000),
            "restore must seed the orchestrator gate with the persisted input count"
        );
    }

    /// Per-lane aggregation keeps a bucket per lane while the flat
    /// grand total still accumulates across all lanes. Two lanes → two
    /// buckets, orchestrator sorted first.
    #[test]
    fn add_request_buckets_stats_per_lane() {
        let mut stats = SessionStats::new();
        stats.add_request("orchestrator", 100, 20, 0.01);
        stats.add_request("reviewer", 200, 40, 0.02);
        stats.add_request("reviewer", 300, 60, 0.03);

        // Flat grand total accumulates across lanes.
        assert_eq!(stats.total_api_calls, 3);
        assert!((stats.total_cost - 0.06).abs() < 1e-9);

        let lanes = stats.lanes_sorted();
        assert_eq!(lanes.len(), 2);
        // orchestrator sorts first.
        assert_eq!(lanes[0].0, "orchestrator");
        assert_eq!(lanes[0].1.api_calls, 1);
        assert!((lanes[0].1.cost - 0.01).abs() < 1e-9);
        // reviewer bucket: 2 calls, cost and tokens accumulate.
        assert_eq!(lanes[1].0, "reviewer");
        assert_eq!(lanes[1].1.api_calls, 2);
        assert!((lanes[1].1.cost - 0.05).abs() < 1e-9);
        assert_eq!(lanes[1].1.input_tokens, 500);
        assert_eq!(lanes[1].1.output_tokens, 100);
    }

    /// A resumed conversation must ADD to its persisted lane totals, not
    /// restart from zero and not double-count the restored base. This is the
    /// `/load`-then-keep-working path.
    #[test]
    fn add_request_accumulates_on_top_of_restored_lanes() {
        let mut stats = SessionStats::new();
        stats.restore_lanes([(
            "junior".to_string(),
            LaneStats {
                input_tokens: 10_000,
                output_tokens: 500,
                api_calls: 3,
                cost: 0.1,
            },
        )]);

        stats.add_request("junior", 2_000, 100, 0.02);
        stats.add_request("junior", 3_000, 150, 0.03);

        let (_, junior) = stats
            .lanes_sorted()
            .into_iter()
            .find(|(n, _)| n == "junior")
            .expect("junior lane present");
        assert_eq!(junior.input_tokens, 15_000, "10k base + 2k + 3k");
        assert_eq!(junior.output_tokens, 750, "500 base + 100 + 150");
        assert_eq!(junior.api_calls, 5, "3 base + 2 new");
        assert!((junior.cost - 0.15).abs() < 1e-9);
    }

    /// `restore_lanes` rehydrates the per-lane breakdown wholesale (the /load
    /// path) so a resumed conversation can scope the Session panel to a
    /// sub-agent instead of reading zeros.
    #[test]
    fn restore_lanes_rehydrates_breakdown() {
        let mut stats = SessionStats::new();
        assert!(stats.lanes_sorted().is_empty());

        stats.restore_lanes([
            (
                "reviewer".to_string(),
                LaneStats {
                    input_tokens: 500,
                    output_tokens: 40,
                    api_calls: 5,
                    cost: 0.05,
                },
            ),
            (
                "orchestrator".to_string(),
                LaneStats {
                    input_tokens: 100,
                    output_tokens: 20,
                    api_calls: 3,
                    cost: 0.01,
                },
            ),
        ]);

        let lanes = stats.lanes_sorted();
        assert_eq!(lanes.len(), 2);
        // lanes_sorted still orders orchestrator first regardless of input order.
        assert_eq!(lanes[0].0, "orchestrator");
        assert_eq!(lanes[1].0, "reviewer");
        assert_eq!(lanes[1].1.input_tokens, 500);
        assert_eq!(lanes[1].1.api_calls, 5);

        // Restoring again replaces (not merges) the breakdown.
        stats.restore_lanes([("developer".to_string(), LaneStats::default())]);
        let lanes = stats.lanes_sorted();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].0, "developer");
    }

    // ── Invalid tool-call recovery (ticket #223) ─────────────────────────────
    //
    // When the model emits a tool call for a name that isn't registered,
    // PeakBot should feed it a synthetic "unknown tool" result and let it
    // self-correct — instead of killing the turn with
    // `PromptError::UnknownToolCall`. The planned fix overrides
    // `PromptHook::on_invalid_tool_call` on `SessionHook` and returns
    // `InvalidToolCallHookAction::Skip { reason }` with a reason string that
    // (a) names the bad tool and (b) lists the registered tools so the
    // model can pick a real one on retry.
    //
    // These tests pin BOTH branches of the reason formatter:
    //   - empty available_tools → "(no tools available)"
    //   - non-empty available_tools → comma-separated list
    //
    // They will not compile until the fix lands (the hook method doesn't
    // exist on `SessionHook` yet), so the failure mode is a compile error
    // pointing at the missing `on_invalid_tool_call` override — the
    // intended RED.

    /// Helper: build a minimal `InvalidToolCallContext` for the
    /// reason-formatter tests. All fields on `InvalidToolCallContext` are
    /// `pub` (verified against the vendored rig-core 0.38.2 source at
    /// `~/.cargo/registry/src/.../rig-core-0.38.2/src/agent/prompt_request/hooks.rs`),
    /// so direct struct construction is the right shape.
    fn make_invalid_tool_call_context(
        tool_name: &str,
        available_tools: Vec<String>,
    ) -> rig_core::agent::InvalidToolCallContext {
        rig_core::agent::InvalidToolCallContext {
            tool_name: tool_name.to_string(),
            tool_call_id: Some("call_test_1".to_string()),
            internal_call_id: Some("internal_test_1".to_string()),
            args: Some("{}".to_string()),
            available_tools,
            allowed_tools: vec![],
            tool_choice: None,
            chat_history: vec![],
            is_streaming: false,
        }
    }

    /// Branch 1: when `available_tools` is empty, the synthetic reason
    /// must name the bad tool AND include the literal
    /// "(no tools available)". The model needs both pieces — the tool
    /// name so it can correct itself, and the empty-list marker so it
    /// doesn't keep retrying with a hallucinated tool.
    #[tokio::test]
    async fn invalid_tool_call_reason_names_bad_tool_when_no_tools_available() {
        // Hook needs an event channel so SessionHook::new is happy — but
        // we don't assert anything on events here, only on the returned
        // reason. We pin `M = MockCompletionModel` via turbofish so the
        // compiler can resolve which `PromptHook<M>` impl block we're
        // invoking (production code never has this problem because the
        // hook is always paired with a concrete completion model at
        // agent-construction time).
        let hook = SessionHook::new(None);
        let ctx = make_invalid_tool_call_context("nope", vec![]);

        // The trait `PromptHook<M>` is generic over `M` even though the
        // method itself is not — turbofish on the trait disambiguates
        // which impl block we're invoking. Production code never has
        // this problem because the hook is always paired with a
        // concrete completion model at agent-construction time.
        let action =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_invalid_tool_call(
                &hook, &ctx,
            )
            .await;

        // The fix MUST return Skip (not Fail, Retry, or Repair) so
        // rig-core feeds the synthetic reason back as a ToolResult and
        // loops. Asserting the exact variant pins the contract.
        match action {
            rig_core::agent::InvalidToolCallHookAction::Skip { reason } => {
                assert!(
                    reason.contains("unknown tool `nope`"),
                    "reason must name the bad tool with backticks, got: {reason:?}"
                );
                assert!(
                    reason.contains("(no tools available)"),
                    "reason must include the empty-list marker, got: {reason:?}"
                );
            }
            other => panic!(
                "expected Skip {{ reason }}, got {other:?} — \
                 SessionHook::on_invalid_tool_call must not fail-fast on unknown tools \
                 (ticket #223: this would abort the turn with PromptError::UnknownToolCall)"
            ),
        }
    }

    /// Branch 2: when `available_tools` is non-empty, the synthetic
    /// reason must name the bad tool AND every available tool (so the
    /// model can pick a real one). We check three representative tool
    /// names so the test pins ordering/comma-separation, not just
    /// membership of a single token.
    #[tokio::test]
    async fn invalid_tool_call_reason_lists_every_available_tool() {
        let hook = SessionHook::new(None);
        let available = vec![
            "bash".to_string(),
            "file_read".to_string(),
            "think".to_string(),
        ];
        let ctx = make_invalid_tool_call_context("totally_made_up", available.clone());

        // See the empty-list test for the turbofish rationale.
        let action =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_invalid_tool_call(
                &hook, &ctx,
            )
            .await;

        match action {
            rig_core::agent::InvalidToolCallHookAction::Skip { reason } => {
                assert!(
                    reason.contains("unknown tool `totally_made_up`"),
                    "reason must name the bad tool with backticks, got: {reason:?}"
                );
                for tool in &available {
                    assert!(
                        reason.contains(tool),
                        "reason must mention available tool `{tool}`, got: {reason:?}"
                    );
                }
                // And — crucially — it must NOT use the empty-list marker
                // when we have tools to list.
                assert!(
                    !reason.contains("(no tools available)"),
                    "reason must NOT say 'no tools available' when tools ARE available, got: {reason:?}"
                );
            }
            other => panic!(
                "expected Skip {{ reason }}, got {other:?} — \
                 SessionHook::on_invalid_tool_call must not fail-fast on unknown tools"
            ),
        }
    }

    /// A sub-agent's hook is the same `SessionHook` with a lane + budget gate,
    /// so a hallucinated tool name must be skipped there too — a delegation
    /// dying on an unknown tool would reach `handoff::classify` as a failure
    /// instead of letting the sub-agent self-correct.
    #[tokio::test]
    async fn sub_agent_hook_skips_unknown_tool_call_like_the_orchestrator() {
        let hook = SessionHook::new(None)
            .with_source(MessageSource::SubAgent {
                role: "reviewer".to_string(),
            })
            .with_sub_agent_gate(Some(1_000));
        let ctx = make_invalid_tool_call_context("teleport", vec!["bash".to_string()]);

        // See the empty-list test for the turbofish rationale.
        let action =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_invalid_tool_call(
                &hook, &ctx,
            )
            .await;

        match action {
            rig_core::agent::InvalidToolCallHookAction::Skip { reason } => {
                assert!(
                    reason.contains("unknown tool `teleport`") && reason.contains("bash"),
                    "reason must name the bad tool and the real ones, got: {reason:?}"
                );
            }
            other => panic!(
                "expected Skip {{ reason }}, got {other:?} — a sub-agent must recover \
                 from an unknown tool call exactly like the orchestrator does"
            ),
        }
    }

    // ── Sub-agent gate ──────────────────────────────────────────────────────

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    /// The snapshot must be `history ++ [prompt]` — exactly what rig would put
    /// in a `PromptCancelled` at that instant — and it must be visible through
    /// a *clone* of the hook, since the agent holds one clone and the delegate
    /// tool holds another.
    #[tokio::test]
    async fn sub_agent_gate_snapshots_history_through_clones() {
        let hook = SessionHook::new(None).with_sub_agent_gate(None);
        let clone = hook.clone();
        assert!(
            hook.history_snapshot().is_empty(),
            "empty before any request"
        );

        let history = vec![user_msg("task"), Message::assistant("working")];
        let prompt = user_msg("next");
        let action =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_completion_call(
                &hook, &prompt, &history,
            )
            .await;

        assert!(
            matches!(action, HookAction::Continue),
            "no budget = no gate"
        );
        let snap = clone.history_snapshot();
        assert_eq!(snap.len(), 3, "snapshot must be history ++ [prompt]");
        assert_eq!(snap[2], prompt);
    }

    /// The gate fires on the request *after* the one that blew the budget —
    /// terminating before the oversized request is what makes it proactive.
    #[tokio::test]
    async fn sub_agent_gate_terminates_over_budget() {
        let stats = Arc::new(Mutex::new(SessionStats::new()));
        let hook = SessionHook::with_context_tracking(None, stats.clone())
            .with_sub_agent_gate(Some(1_000));

        let prompt = user_msg("next");
        let under =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_completion_call(
                &hook,
                &prompt,
                &[],
            )
            .await;
        assert!(
            matches!(under, HookAction::Continue),
            "no usage reported yet must not trip the gate"
        );

        stats
            .lock()
            .unwrap()
            .add_request("reviewer", 1_001, 10, 0.0);
        let over =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_completion_call(
                &hook,
                &prompt,
                &[],
            )
            .await;
        match over {
            HookAction::Terminate { reason } => assert_eq!(reason, "subagent-context"),
            other => panic!("expected terminate(subagent-context), got {other:?}"),
        }
        assert_eq!(
            hook.history_snapshot().len(),
            1,
            "the gate must still snapshot before firing"
        );
    }

    /// An orchestrator hook has no gate at all: no snapshot, no terminate.
    #[tokio::test]
    async fn orchestrator_hook_has_no_sub_agent_gate() {
        let stats = Arc::new(Mutex::new(SessionStats::new()));
        stats
            .lock()
            .unwrap()
            .add_request("orchestrator", 999_999, 10, 0.0);
        let hook = SessionHook::with_context_tracking(None, stats);

        let prompt = user_msg("next");
        let action =
            rig_core::agent::PromptHook::<crate::mock::MockCompletionModel>::on_completion_call(
                &hook,
                &prompt,
                &[],
            )
            .await;
        assert!(matches!(action, HookAction::Continue));
        assert!(hook.history_snapshot().is_empty());
    }
}

/// Sub-agent-only hook state. `SessionHook: Clone` shallow-copies Arc handles,
/// so `last_history` must stay behind an `Arc` — otherwise the clone handed to
/// the agent and the `Arc<SessionHook>` handed to the caller would diverge.
#[derive(Clone)]
struct SubAgentGate {
    /// Terminate before a request whose predecessor already reported more
    /// input tokens than this. `None` = gate disabled; snapshotting still happens.
    budget: Option<usize>,
    /// History as of the most recent `on_completion_call` — exactly the full
    /// history rig would carry in a `PromptCancelled` at that instant. Empty
    /// until the first call (and forever under hookless Ollama).
    last_history: Arc<Mutex<Vec<Message>>>,
}

#[derive(Clone)] // NOTE: Clone only shallow-copies the Arc handles
pub struct SessionHook {
    /// Channel sender for streaming events
    event_sender: Option<mpsc::UnboundedSender<SourcedEvent>>,
    /// The lane every emitted event is stamped with. Defaults to
    /// [`MessageSource::Human`] (the orchestrator); a sub-agent's hook sets
    /// [`MessageSource::SubAgent`] via [`SessionHook::with_source`] so its
    /// events reach the shared receiver tagged with the role.
    source: MessageSource,
    /// Reference to session stats for token tracking
    #[allow(dead_code)]
    stats: Arc<Mutex<SessionStats>>,
    /// User requested stop
    stop_requested: Arc<AtomicBool>,
    /// Weak reference to the `StateManager`, used by `on_completion_call` to
    /// gate in-loop compaction (see `mid-compaction.md` § 3 Step 1). Weak so
    /// the hook never extends the manager's lifetime — the agent owns the
    /// hook, and the manager owns the agent indirectly via the registry, so
    /// an `Arc` here would cycle.
    state_manager: Option<Weak<crate::state::StateManager>>,
    /// Set only on a sub-agent's hook (by `build_sub_agent`): the context
    /// budget gate plus the per-request history snapshot the delegate tool
    /// reads to summarise an interrupted delegation.
    sub_agent: Option<SubAgentGate>,
}

impl SessionHook {
    /// Create a new session hook
    pub fn new(event_sender: Option<mpsc::UnboundedSender<SourcedEvent>>) -> Self {
        Self {
            event_sender,
            source: MessageSource::Human,
            stats: Arc::new(Mutex::new(SessionStats::new())),
            stop_requested: Arc::new(AtomicBool::new(false)),
            state_manager: None,
            sub_agent: None,
        }
    }

    /// Create a new session hook with shared stats tracking
    pub fn with_context_tracking(
        event_sender: Option<mpsc::UnboundedSender<SourcedEvent>>,
        stats: Arc<Mutex<SessionStats>>,
    ) -> Self {
        Self {
            event_sender,
            source: MessageSource::Human,
            stats,
            stop_requested: Arc::new(AtomicBool::new(false)),
            state_manager: None,
            sub_agent: None,
        }
    }

    /// Stamp this hook's emitted events with a lane. Builder-style; returns
    /// `self` for chaining. Defaults to [`MessageSource::Human`]; a sub-agent
    /// hook sets [`MessageSource::SubAgent`] so its `ToolCall`/`ToolResult`/
    /// `CompletionResponse` events reach the shared receiver tagged with the
    /// role.
    pub fn with_source(mut self, source: MessageSource) -> Self {
        self.source = source;
        self
    }

    /// Wire this hook to a `StateManager` so it can gate in-loop compaction
    /// from `on_completion_call`. Builder-style; returns `self` for chaining.
    ///
    /// The hook stores a `Weak`, so passing a fresh `Arc` is safe — the hook
    /// will silently no-op if the manager has been dropped.
    pub fn with_state_manager(mut self, sm: &Arc<crate::state::StateManager>) -> Self {
        self.state_manager = Some(Arc::downgrade(sm));
        self
    }

    /// Mark this hook as a sub-agent's and give it a context budget in input
    /// tokens. Called only from `build_sub_agent`. A `None` budget disables the
    /// proactive gate; history snapshotting happens either way.
    pub fn with_sub_agent_gate(mut self, budget: Option<usize>) -> Self {
        self.sub_agent = Some(SubAgentGate {
            budget,
            last_history: Arc::new(Mutex::new(Vec::new())),
        });
        self
    }

    /// The history as of the last `on_completion_call`. Empty for an
    /// orchestrator hook, for a hookless (Ollama) sub-agent, and before the
    /// first request.
    pub fn history_snapshot(&self) -> Vec<Message> {
        self.sub_agent
            .as_ref()
            .map(|gate| gate.last_history.lock().unwrap().clone())
            .unwrap_or_default()
    }

    /// Request the agent to stop
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    /// Whether a stop has been requested and not yet consumed by the loop.
    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    /// Drop a stop signal the hook never got to read.
    ///
    /// Background: with the #183 design, turn cancellation now unwinds the
    /// turn without ever reaching a hook boundary (`process_message_internal`'s
    /// `select!` resolves on `cancel.cancelled()` and returns). That means
    /// `stop_requested` (this hook's `AtomicBool`) is set in
    /// `request_stop_and_drain` but **never consumed** by the hook — and would
    /// survive into the *next* turn, terminating it spuriously the moment the
    /// hook is polled. This method is the explicit reset the drain arm calls
    /// after the turn has unwound (design §4 step 11).
    ///
    /// #183: stub — currently a no-op. The implementation task will
    /// `store(false, Ordering::SeqCst)`.
    pub fn clear_stop(&self) {
        // #183: stub — implementation lands in the implementation task.
        // Real body: self.stop_requested.store(false, Ordering::SeqCst);
    }

    /// The lane this hook stamps on its emitted events.
    pub fn source(&self) -> &MessageSource {
        &self.source
    }

    /// Get a clone of the current session stats
    pub fn get_stats(&self) -> SessionStats {
        self.stats.lock().unwrap().clone()
    }

    /// Create a new session hook with a new event channel (backward compatible)
    pub fn with_channel() -> (Self, mpsc::UnboundedReceiver<SourcedEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                event_sender: Some(sender),
                source: MessageSource::Human,
                stats: Arc::new(Mutex::new(SessionStats::new())),
                stop_requested: Arc::new(AtomicBool::new(false)),
                state_manager: None,
                sub_agent: None,
            },
            receiver,
        )
    }
}

/// Provider-agnostic input token count. Under caching, `input_tokens` is
/// unreliable (Anthropic = uncached only; OpenAI = folded). Use
/// `total_tokens - output_tokens`, falling back to `input_tokens` if unset.
fn input_context_tokens(usage: &rig_core::completion::Usage) -> u64 {
    usage
        .total_tokens
        .saturating_sub(usage.output_tokens)
        .max(usage.input_tokens)
}

/// Extract text and reasoning from the response choice
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
                        rig_core::completion::message::ReasoningContent::Text {
                            text: t, ..
                        } => {
                            if !reasonings.is_empty() {
                                reasonings.push('\n');
                            }
                            reasonings.push_str(t);
                        }
                        rig_core::completion::message::ReasoningContent::Summary(s) => {
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
            // Tool calls and images are not prose — they reach the UI as their
            // own ChatMessage (or not at all); synthesising a text placeholder
            // here leaks a literal into the transcript.
            AssistantContent::ToolCall(_) | AssistantContent::Image(_) => {}
        }
    }

    (text, reasoning)
}

impl<M: CompletionModel> PromptHook<M> for SessionHook {
    /// Called before the prompt is sent. Three responsibilities:
    /// 1. Emit a `CompletionRequest` event for observers (cost tracker, UI).
    /// 2. **Sub-agent gate** (sub-agent hooks only): snapshot the history rig
    ///    is about to send — the delegate tool summarises it if the run fails —
    ///    and terminate with reason `"subagent-context"` when the previous
    ///    request already blew the role's context budget.
    /// 3. **Gate in-loop compaction**: if the wired `StateManager` says
    ///    `needs_compaction()`, terminate the agentic loop with reason
    ///    `"compact"`. The caller (`process_message_internal`) catches this,
    ///    runs `force_compact().await` synchronously, and re-enters
    ///    `prompt_with_history` with the compacted state.
    ///
    /// This is the right boundary because `on_completion_call` fires
    /// *immediately before each wire request* — the request that's about to
    /// blow the context. The loop guard against infinite terminate-restart
    /// cycles lives in `apply_compaction` (which clears
    /// `last_input_tokens`), not here. See `mid-compaction.md` for the full
    /// design.
    async fn on_completion_call(&self, prompt: &Message, history: &[Message]) -> HookAction {
        if let Some(ref sender) = self.event_sender {
            let event = AgentEvent::CompletionRequest {
                message_count: history.len() + 1,
                estimated_tokens: None,
                timestamp: Utc::now(),
            };
            let _ = sender.send(SourcedEvent {
                source: self.source.clone(),
                event,
            });
        }

        // Sub-agent budget gate. `history ++ [prompt]` is exactly what rig
        // would put in a `PromptCancelled` here, so snapshot before deciding —
        // the gate-fire and the error paths then see identical history.
        if let Some(gate) = &self.sub_agent {
            let mut full = history.to_vec();
            full.push(prompt.clone());
            *gate.last_history.lock().unwrap() = full;

            let last_input = self.stats.lock().unwrap().last_input_tokens().unwrap_or(0) as usize;
            if let Some(budget) = gate.budget
                && last_input > budget
            {
                tracing::info!(
                    last_input,
                    budget,
                    "Sub-agent context budget crossed, terminating before the oversized request"
                );
                return HookAction::terminate("subagent-context");
            }
        }

        // In-loop compaction gate. Only fires when a StateManager is wired.
        if let Some(weak) = self.state_manager.as_ref()
            && let Some(sm) = weak.upgrade()
            && sm.needs_compaction()
        {
            tracing::info!("Compaction threshold crossed mid-loop, terminating to compact");
            return HookAction::terminate("compact");
        }

        HookAction::Continue
    }

    /// Called after response - emit event and check for interruptions.
    /// This is the right place for interruption checks because we have usage data here
    /// and it fires at LLM boundaries (before tool execution).
    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let usage = &response.usage;
        let input_tokens = input_context_tokens(usage);

        // Update the session stats with this request's usage
        // This is critical for ContextManager to track actual token counts
        if let Ok(mut stats) = self.stats.lock() {
            stats.add_request(
                self.source.lane_label(),
                input_tokens,
                usage.output_tokens,
                0.0, // Cost is calculated externally
            );
        }

        if let Some(ref sender) = self.event_sender {
            // Extract content and reasoning from response using rig's API
            let (content, reasoning) = extract_content_from_response(&response.choice);

            // Emit event with per-request token counts; cost is computed in CostHandler.
            let event = AgentEvent::CompletionResponse {
                content,
                reasoning,
                usage: EventTokenUsage {
                    input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens: input_tokens + usage.output_tokens,
                    cost: 0.0, // Cost = 0 here, calculated by CostHandler
                },
                timestamp: Utc::now(),
            };
            let _ = sender.send(SourcedEvent {
                source: self.source.clone(),
                event,
            });
        }

        // Check for interruptions at LLM boundary (before tool execution)

        // Stop flag check — only interruption handled here.
        // Context compaction is handled by ContextManager at the top of the
        // AgentRunner loop (before the next LLM call), not here. Terminating
        // mid-response would throw away the LLM's output and previously caused
        // an infinite retry loop (see compactfuck.md Bug #2 and #3).
        if self.stop_requested.load(Ordering::SeqCst) {
            self.stop_requested.store(false, Ordering::SeqCst);
            tracing::info!("Stop requested, terminating");
            return HookAction::terminate("stop");
        }

        HookAction::Continue
    }

    /// A hallucinated tool name must not abort the whole turn (#223): hand the
    /// model a synthetic error naming the real tools so it can self-correct.
    /// `Skip` degrades to `Fail` under `ToolChoice::None`, which PeakBot never sets.
    async fn on_invalid_tool_call(
        &self,
        ctx: &InvalidToolCallContext,
    ) -> InvalidToolCallHookAction {
        let available = if ctx.available_tools.is_empty() {
            "(no tools available)".to_string()
        } else {
            ctx.available_tools.join(", ")
        };
        let reason = format!(
            "Error: unknown tool `{}`. Available tools: {}. Pick one of those and retry.",
            ctx.tool_name, available
        );

        // Surface it in the transcript — otherwise the recovery is invisible and
        // the wasted round-trip looks like the model stalling.
        if let Some(weak) = self.state_manager.as_ref()
            && let Some(sm) = weak.upgrade()
        {
            sm.add_system_message(format!(
                "⚠️  Model called unknown tool `{}` — sent a synthetic error and asked it to retry.",
                ctx.tool_name
            ));
        }

        InvalidToolCallHookAction::skip(reason)
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
            let _ = sender.send(SourcedEvent {
                source: self.source.clone(),
                event,
            });
        }
        ToolCallHookAction::Continue
    }

    /// Called after tool result - just emit event, no interruption logic here.
    /// All interruption checks happen in on_completion_response.
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
            let _ = sender.send(SourcedEvent {
                source: self.source.clone(),
                event,
            });
        }
        HookAction::Continue
    }
}
