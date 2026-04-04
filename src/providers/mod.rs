//! Provider abstraction layer for PeakBot.
//!
//! This module provides a unified interface for different LLM providers
//! (OpenRouter, Ollama, etc.) to make the codebase provider-independent.

use crate::config::{
    BashConfig, LlamaCppConfig, OllamaConfig, OpenAIConfig, OpenRouterConfig, ProviderConfig,
    SearXngConfig,
};
use crate::hooks::{SessionHook, SessionStats};
use crate::hooks::events::AgentEvent;
use tokio::sync::mpsc;
use crate::tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, SearchTool, ThinkTool,
    TodoList, TodoTool,
};
use anyhow::{Context, Result};
use rig::agent::Agent;
use rig::client::completion::CompletionClient;
use rig::completion::Prompt;
use rig::completion::PromptError;
use rig::completion::message::Message;
use rig::providers::ollama;
use rig::providers::openai;
use rig::providers::openrouter;
use rig::tool::ToolDyn;
use std::sync::{Arc, Mutex};

/// Provider info - metadata about the current provider
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Provider name (e.g., "openrouter", "ollama")
    pub name: String,
    /// Model name
    pub model: String,
    /// Whether this provider supports pricing/cost tracking
    pub supports_pricing: bool,
}

/// Cost tracking handle that provides access to session statistics
pub struct CostTracker {
    /// Session statistics for tracking tokens and costs
    stats: Arc<Mutex<SessionStats>>,
    /// Model pricing for cost calculation
    pricing: crate::hooks::ModelPricing,
}

impl CostTracker {
    /// Create a new cost tracker with the given pricing
    /// Note: Event processing is handled externally by the caller (e.g., AgentRunner)
    /// Use track_event() to process events for cost tracking
    pub fn new(pricing: crate::hooks::ModelPricing) -> Self {
        Self {
            stats: Arc::new(Mutex::new(SessionStats::new())),
            pricing,
        }
    }

    /// Process an event for cost tracking
    /// This should be called by the external event processor
    pub fn track_event(&self, event: &AgentEvent) {
        if let AgentEvent::CompletionResponse { usage, .. } = event {
            let cost = (usage.input_tokens as f64 * self.pricing.input_per_token)
                + (usage.output_tokens as f64 * self.pricing.output_per_token);

            let total_cost = if let Ok(mut stats) = self.stats.lock() {
                stats.add_request(usage.input_tokens, usage.output_tokens, cost);
                stats.total_cost
            } else {
                return;
            };

            // Log outside the lock to avoid holding the mutex during I/O
            tracing::info!(
                "Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}",
                usage.input_tokens,
                usage.output_tokens,
                cost,
                total_cost
            );
        }
    }

    /// Create a cost tracker with no tracking (for providers that don't support it)
    pub fn none() -> Self {
        Self {
            stats: Arc::new(Mutex::new(SessionStats::new())),
            pricing: crate::hooks::ModelPricing::default(),
        }
    }

    /// Update stats with token usage from a completion response
    pub fn track_completion(&self, input_tokens: u64, output_tokens: u64) {
        let cost = (input_tokens as f64 * self.pricing.input_per_token)
            + (output_tokens as f64 * self.pricing.output_per_token);
        
        if let Ok(mut stats) = self.stats.lock() {
            stats.add_request(input_tokens, output_tokens, cost);
            
            tracing::info!(
                "Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}",
                input_tokens,
                output_tokens,
                cost,
                stats.total_cost
            );
        }
    }

    /// Get stats for the last request
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

    /// Reset session stats
    pub fn reset_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.reset();
        }
    }

    /// Get the underlying SessionStats for context tracking
    pub fn get_session_stats(&self) -> Arc<Mutex<SessionStats>> {
        self.stats.clone()
    }

    /// Get the pricing model
    pub fn get_pricing(&self) -> &crate::hooks::ModelPricing {
        &self.pricing
    }

    /// Get total tokens used in this session
    pub fn get_total_tokens(&self) -> u64 {
        if let Ok(stats) = self.stats.lock() {
            return stats.total_tokens();
        }
        0
    }

    /// Get total cost for this session
    pub fn get_total_cost(&self) -> f64 {
        if let Ok(stats) = self.stats.lock() {
            return stats.total_cost;
        }
        0.0
    }
}

/// A dynamic agent type that can work with any provider
/// This allows us to abstract over different provider agent types at runtime
pub enum DynAgent {
    /// OpenRouter agent with session hook
    OpenRouter(Agent<<openrouter::Client as CompletionClient>::CompletionModel, SessionHook>),
    /// OpenAI agent (uses modern responses API)
    OpenAI(Agent<rig::providers::openai::responses_api::ResponsesCompletionModel, SessionHook>),
    /// LlamaCpp agent (uses completions API for compatibility with llama.cpp)
    LlamaCpp(Agent<rig::providers::openai::completion::CompletionModel, SessionHook>),
    /// Ollama agent (no hook for local models)
    Ollama(Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>),
}

impl DynAgent {
    /// Prompt the agent with a single message
    pub async fn prompt(&self, prompt: &str) -> Result<String, PromptError> {
        match self {
            DynAgent::OpenRouter(agent) => agent.prompt(prompt).await,
            DynAgent::OpenAI(agent) => agent.prompt(prompt).await,
            DynAgent::LlamaCpp(agent) => agent.prompt(prompt).await,
            DynAgent::Ollama(agent) => agent.prompt(prompt).await,
        }
    }

    /// Prompt the agent with chat history
    pub async fn prompt_with_history(
        &self,
        prompt: &str,
        history: &mut Vec<Message>,
    ) -> Result<String, PromptError> {
        match self {
            DynAgent::OpenRouter(agent) => agent.prompt(prompt).with_history(history).await,
            DynAgent::OpenAI(agent) => agent.prompt(prompt).with_history(history).await,
            DynAgent::LlamaCpp(agent) => agent.prompt(prompt).with_history(history).await,
            DynAgent::Ollama(agent) => agent.prompt(prompt).with_history(history).await,
        }
    }
}

/// Create a provider client and agent from the configuration
///
/// If mcp_tools is provided, they will be added to the agent along with built-in tools.
/// The system_prompt is used as the agent's preamble.
/// Create the provider agent with all tools and hooks.
/// Returns the agent, provider info, cost tracker, todo state, event receiver, and shared session hook.
pub fn create_provider(
    config: &ProviderConfig,
    context_config: &crate::config::ContextConfig,
    context_window: usize,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
) -> Result<(DynAgent, ProviderInfo, CostTracker, Arc<Mutex<TodoList>>, Option<mpsc::UnboundedReceiver<AgentEvent>>, Arc<SessionHook>)> {
    match config {
        ProviderConfig::OpenRouter(c) => {
            let (agent, info, cost_tracker, todo_state, receiver, hook) = create_openrouter_agent(
                c,
                context_config,
                context_window,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
            )?;
            Ok((
                DynAgent::OpenRouter(agent),
                info,
                cost_tracker,
                todo_state,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::OpenAI(c) => {
            let (agent, info, cost_tracker, todo_state, receiver, hook) = create_openai_agent(
                c,
                context_config,
                context_window,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
            )?;
            Ok((
                DynAgent::OpenAI(agent),
                info,
                cost_tracker,
                todo_state,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::LlamaCpp(c) => {
            let (agent, info, cost_tracker, todo_state, receiver, hook) = create_llamacpp_agent(
                c,
                context_config,
                context_window,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
            )?;
            Ok((
                DynAgent::LlamaCpp(agent),
                info,
                cost_tracker,
                todo_state,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::Ollama(c) => {
            let (agent, info, todo_state) = create_ollama_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
            )?;
            Ok((
                DynAgent::Ollama(agent),
                info,
                CostTracker::none(),
                todo_state,
                None, // No event channel for Ollama
                Arc::new(SessionHook::new(None)), // Empty hook for Ollama (no injection support)
            ))
        }
    }
}

/// Get built-in tools for PeakBot (excluding SearchTool which requires config)
/// If todo_tool is provided, uses it; otherwise creates a new one
fn add_builtin_tools<M, P>(
    builder: rig::agent::AgentBuilder<M, P, rig::agent::NoToolConfig>,
    searxng_config: Option<&SearXngConfig>,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    cost_tracker: Arc<Mutex<crate::hooks::SessionStats>>,
) -> (
    rig::agent::AgentBuilder<M, P, rig::agent::WithBuilderTools>,
    Arc<Mutex<TodoList>>,
)
where
    M: rig::completion::CompletionModel,
    P: rig::agent::PromptHook<M>,
{
    // Use provided tool or create a new one
    let todo = todo_tool.unwrap_or_default();
    let todo_state = todo.get_state();

    // Create BashTool with configured environment variables
    let bash_tool = BashTool::new(bash_config.env.clone());

    let mut builder = builder
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(bash_tool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(ThinkTool)
        .tool(todo);

    // Conditionally add search tool if SearXNG is configured
    if let Some(config) = searxng_config {
        builder = builder.tool(SearchTool::new(config));
    }

    // Add DelegateTool if pipeline is enabled
    if let Some(registry) = pipeline_registry {
        let delegate_tool = crate::pipeline::DelegateTool::new(
            Arc::new(registry.clone()),
            cost_tracker,
        );
        builder = builder.tool(delegate_tool);
    }

    (builder, todo_state)
}

/// Create OpenRouter agent and info with cost tracking
fn create_openrouter_agent(
    config: &OpenRouterConfig,
    context_config: &crate::config::ContextConfig,
    context_window: usize,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
) -> Result<(
    Agent<<openrouter::Client as CompletionClient>::CompletionModel, SessionHook>,
    ProviderInfo,
    CostTracker,
    Arc<Mutex<TodoList>>,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    let api_key = config
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
        .context("OpenRouter API key not configured")?;

    if api_key.is_empty() {
        anyhow::bail!("OpenRouter API key not configured");
    }
    if config.model.is_empty() {
        anyhow::bail!("OpenRouter model not specified");
    }

    let client = openrouter::Client::builder()
        .api_key(&api_key)
        .build()
        .context("Failed to create OpenRouter client")?;

    let model = config.model.clone();

    // Create cost tracker first (needed for session hook stats)
    let cost_tracker = CostTracker::new(crate::hooks::ModelPricing::default());
    let cost_tracker_stats = cost_tracker.get_session_stats();

    // Create session hook with context tracking
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(
        Some(sender),
        cost_tracker_stats.clone(),
        context_window as u64,
        context_config.threshold,
    );

    // Build agent with system prompt, hook, and built-in tools
    let agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let (agent_builder, todo_state) =
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config, pipeline_registry, cost_tracker_stats);

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openrouter".to_string(),
        model,
        supports_pricing: true,
    };

    Ok((agent, info, cost_tracker, todo_state, receiver, hook))
}

/// Create Ollama agent and info (no cost tracking for local models)
fn create_ollama_agent(
    config: &OllamaConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
) -> Result<(
    Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>,
    ProviderInfo,
    Arc<Mutex<TodoList>>,
)> {
    if config.model.is_empty() {
        anyhow::bail!("Ollama model not specified");
    }

    // Use Nothing as API key since Ollama doesn't require one
    let client = ollama::Client::builder()
        .base_url(&config.base_url)
        .api_key(rig::client::Nothing)
        .build()
        .context(format!(
            "Failed to create Ollama client at {}",
            config.base_url
        ))?;

    let model = config.model.clone();

    // Build agent with system prompt
    let mut agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .default_max_turns(max_turns);

    if let Some(temp) = config.temperature {
        agent_builder = agent_builder.temperature(temp as f64);
    }
    if let Some(num_ctx) = config.num_ctx {
        // Set num_ctx via additional_params for Ollama
        let params = serde_json::json!({
            "num_ctx": num_ctx
        });
        agent_builder = agent_builder.additional_params(params);
    }

    // Ollama doesn't have cost tracking, so use a dummy stats tracker
    let dummy_stats = Arc::new(Mutex::new(crate::hooks::SessionStats::new()));
    
    // Add built-in tools (including optional SearchTool and TodoTool)
    let (agent_builder, todo_state) =
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config, pipeline_registry, dummy_stats);

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "ollama".to_string(),
        model,
        supports_pricing: false,
    };

    Ok((agent, info, todo_state))
}

/// Create OpenAI agent and info with cost tracking
fn create_openai_agent(
    config: &OpenAIConfig,
    context_config: &crate::config::ContextConfig,
    context_window: usize,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
) -> Result<(
    Agent<rig::providers::openai::responses_api::ResponsesCompletionModel, SessionHook>,
    ProviderInfo,
    CostTracker,
    Arc<Mutex<TodoList>>,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    let api_key = config
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .context("OpenAI API key not configured")?;

    if api_key.is_empty() {
        anyhow::bail!("OpenAI API key not configured");
    }
    if config.model.is_empty() {
        anyhow::bail!("OpenAI model not specified");
    }

    // Build the OpenAI client with configurable base URL
    // Note: Using default responses API (not completions_api) for modern OpenAI compatibility
    let client = openai::Client::builder()
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create OpenAI client")?;

    let model = config.model.clone();

    // Create cost tracker first (needed for session hook stats)
    let cost_tracker = CostTracker::new(crate::hooks::ModelPricing::default());
    let cost_tracker_stats = cost_tracker.get_session_stats();

    // Create session hook with context tracking
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(
        Some(sender),
        cost_tracker_stats.clone(),
        context_window as u64,
        context_config.threshold,
    );

    // Build agent with system prompt, hook, and built-in tools
    let agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let (agent_builder, todo_state) =
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config, pipeline_registry, cost_tracker_stats);

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openai".to_string(),
        model,
        supports_pricing: true,
    };

    Ok((agent, info, cost_tracker, todo_state, receiver, hook))
}

/// Create LlamaCpp agent and info (uses completions API for compatibility)
fn create_llamacpp_agent(
    config: &LlamaCppConfig,
    context_config: &crate::config::ContextConfig,
    context_window: usize,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
) -> Result<(
    Agent<rig::providers::openai::completion::CompletionModel, SessionHook>,
    ProviderInfo,
    CostTracker,
    Arc<Mutex<TodoList>>,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    // API key is optional for local llama.cpp instances
    let api_key = config
        .api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .unwrap_or_default();

    if config.model.is_empty() {
        anyhow::bail!("LlamaCpp model not specified");
    }

    // Build the OpenAI client with completions API for llama.cpp compatibility
    let client = openai::Client::builder()
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create LlamaCpp client")?
        .completions_api();

    let model = config.model.clone();

    // Create cost tracker first (needed for session hook stats)
    let cost_tracker = CostTracker::new(crate::hooks::ModelPricing::default());
    let cost_tracker_stats = cost_tracker.get_session_stats();

    // Create session hook with context tracking
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(
        Some(sender),
        cost_tracker_stats.clone(),
        context_window as u64,
        context_config.threshold,
    );

    // Build agent with system prompt, hook, and built-in tools
    let agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let (agent_builder, todo_state) =
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config, pipeline_registry, cost_tracker_stats);

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "llamacpp".to_string(),
        model,
        supports_pricing: true,
    };

    Ok((agent, info, cost_tracker, todo_state, receiver, hook))
}
