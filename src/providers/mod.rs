//! Provider abstraction layer for PeakBot.
//!
//! This module provides a unified interface for different LLM providers
//! (OpenRouter, Ollama, etc.) to make the codebase provider-independent.

use crate::config::{BashConfig, OllamaConfig, OpenAIConfig, OpenRouterConfig, ProviderConfig, SearXngConfig};
use crate::hooks::{TokenCostHook, ToolEventBuffer};
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
#[derive(Clone)]
pub struct CostTracker {
    /// The hook that tracks token costs (only for OpenRouter)
    hook: Option<TokenCostHook>,
}

impl CostTracker {
    /// Create a new cost tracker
    fn new(hook: Option<TokenCostHook>) -> Self {
        Self { hook }
    }

    /// Create a cost tracker with no tracking (for providers that don't support it)
    pub fn none() -> Self {
        Self { hook: None }
    }

    /// Get stats for the last request
    pub fn get_last_request_stats(&self) -> Option<String> {
        self.hook.as_ref()?.get_last_request_stats()
    }

    /// Get session summary
    pub fn get_session_summary(&self) -> Option<String> {
        self.hook.as_ref()?.get_session_summary()
    }

    /// Reset session stats
    pub fn reset_stats(&self) {
        if let Some(ref hook) = self.hook {
            hook.reset_stats();
        }
    }

    /// Get the underlying SessionStats for context tracking
    pub fn get_session_stats(&self) -> Option<Arc<Mutex<crate::hooks::SessionStats>>> {
        self.hook.as_ref().map(|h| h.get_stats())
    }

    /// Get total tokens used in this session
    pub fn get_total_tokens(&self) -> u64 {
        if let Some(ref hook) = self.hook {
            if let Ok(stats) = hook.get_stats().lock() {
                return stats.total_tokens();
            }
        }
        0
    }

    /// Get total cost for this session
    pub fn get_total_cost(&self) -> f64 {
        if let Some(ref hook) = self.hook {
            if let Ok(stats) = hook.get_stats().lock() {
                return stats.total_cost;
            }
        }
        0.0
    }
}

/// A dynamic agent type that can work with any provider
/// This allows us to abstract over different provider agent types at runtime
pub enum DynAgent {
    /// OpenRouter agent with cost tracking hook
    OpenRouter(Agent<<openrouter::Client as CompletionClient>::CompletionModel, TokenCostHook>),
    /// OpenAI agent (no cost tracking by default, but supports it)
    OpenAI(Agent<<openai::Client as CompletionClient>::CompletionModel, TokenCostHook>),
    /// Ollama agent (no cost tracking for local models)
    Ollama(Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>),
}

impl DynAgent {
    /// Prompt the agent with a single message
    pub async fn prompt(&self, prompt: &str) -> Result<String, PromptError> {
        match self {
            DynAgent::OpenRouter(agent) => agent.prompt(prompt).await,
            DynAgent::OpenAI(agent) => agent.prompt(prompt).await,
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
            DynAgent::Ollama(agent) => agent.prompt(prompt).with_history(history).await,
        }
    }
}

/// Create a provider client and agent from the configuration
///
/// If mcp_tools is provided, they will be added to the agent along with built-in tools.
/// The system_prompt is used as the agent's preamble.
/// For OpenRouter, cost tracking is enabled automatically.
/// If todo_tool is provided, it will be used (otherwise a default one is created).
/// If tool_buffer is provided, tool calls will be tracked for persistence.
/// Returns the todo state Arc for user visibility.
pub fn create_provider(
    config: &ProviderConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    tool_buffer: Option<ToolEventBuffer>,
) -> Result<(DynAgent, ProviderInfo, CostTracker, Arc<Mutex<TodoList>>)> {
    match config {
        ProviderConfig::OpenRouter(c) => {
            let (agent, info, hook, todo_state) = create_openrouter_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                tool_buffer,
            )?;
            Ok((
                DynAgent::OpenRouter(agent),
                info,
                CostTracker::new(Some(hook)),
                todo_state,
            ))
        }
        ProviderConfig::OpenAI(c) => {
            let (agent, info, hook, todo_state) = create_openai_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                tool_buffer,
            )?;
            Ok((
                DynAgent::OpenAI(agent),
                info,
                CostTracker::new(Some(hook)),
                todo_state,
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
            )?;
            Ok((
                DynAgent::Ollama(agent),
                info,
                CostTracker::none(),
                todo_state,
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

    (builder, todo_state)
}

/// Create OpenRouter agent and info with cost tracking
fn create_openrouter_agent(
    config: &OpenRouterConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    tool_buffer: Option<ToolEventBuffer>,
) -> Result<(
    Agent<<openrouter::Client as CompletionClient>::CompletionModel, TokenCostHook>,
    ProviderInfo,
    TokenCostHook,
    Arc<Mutex<TodoList>>,
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

    // Create the cost tracking hook with optional tool buffer
    let hook = TokenCostHook::new(
        model.clone(),
        crate::hooks::ModelPricing::default(),
        tool_buffer,
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
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config);

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

    Ok((agent, info, hook, todo_state))
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

    // Add built-in tools (including optional SearchTool and TodoTool)
    let (agent_builder, todo_state) =
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config);

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
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    tool_buffer: Option<ToolEventBuffer>,
) -> Result<(
    Agent<<openai::Client as CompletionClient>::CompletionModel, TokenCostHook>,
    ProviderInfo,
    TokenCostHook,
    Arc<Mutex<TodoList>>,
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
    let client = openai::Client::builder()
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create OpenAI client")?;

    let model = config.model.clone();

    // Create the cost tracking hook with optional tool buffer
    let hook = TokenCostHook::new(
        model.clone(),
        crate::hooks::ModelPricing::default(),
        tool_buffer,
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
        add_builtin_tools(agent_builder, searxng_config, todo_tool, bash_config);

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

    Ok((agent, info, hook, todo_state))
}
