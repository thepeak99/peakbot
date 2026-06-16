//! Provider abstraction layer for PeakBot.
//!
//! This module provides a unified interface for different LLM providers
//! (OpenRouter, Ollama, etc.) to make the codebase provider-independent.
//!
//! Clippy: provider constructors deliberately accept many configuration
//! arguments and return tuples of complex Rig types — refactoring would
//! force a builder pattern across the call sites for no real gain. Allow
//! `too_many_arguments` and `type_complexity` at module scope.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::config::{
    AnthropicCaching, AnthropicConfig, BashConfig, LlamaCppConfig, OllamaConfig, OpenAIConfig,
    OpenRouterConfig, ProviderConfig, SearXngConfig,
};
use crate::hooks::SessionHook;
use crate::hooks::events::AgentEvent;
#[cfg(feature = "mock")]
use crate::mock::MockCompletionModel;
use crate::state::StateManager;
use crate::tools::{
    BashBgTool, BashTool, FetchPageTool, FetchUrlTool, FileCreateTool, FileInsertTool,
    FileReadTool, FileStrReplaceTool, ListDirectoryTool, PowerShellTool, SearchTool, ShellKind,
    ThinkTool, TodoTool,
};
use anyhow::{Context, Result};
use rig_core::agent::{Agent, AgentBuilder};
use rig_core::client::completion::CompletionClient;
use rig_core::completion::Prompt;
use rig_core::completion::PromptError;
use rig_core::completion::message::Message;
use rig_core::providers::anthropic;
use rig_core::providers::ollama;
use rig_core::providers::openai;
use rig_core::providers::openrouter;
use rig_core::tool::ToolDyn;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Provider info - metadata about the current provider
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Provider name (e.g., "openrouter", "ollama")
    pub name: String,
    /// Model name
    pub model: String,
    /// Whether this provider supports pricing/cost tracking
    pub supports_pricing: bool,
    /// Whether this provider+model can accept image input.
    ///
    /// Set via [`supports_vision_for`]. Used by the dispatcher to block
    /// `[img:…]` submissions when the active model cannot see — emits a
    /// system error rather than silently dropping the images.
    pub supports_vision: bool,
}

/// Whether `[img:…]` attachments may flow to this provider+model. Anthropic
/// gates on transport (it carries images in user + tool-result channels), so
/// unknown model names are accepted there; other providers gate on the name.
pub fn supports_vision_for(provider_name: &str, model: &str) -> bool {
    provider_name == "anthropic" || crate::vision::model_supports_vision(model)
}

/// Apply an explicit `vision:` override on top of [`supports_vision_for`].
/// `Some(b)` forces; `None` auto-detects. Single point for both `[img:…]`
/// acceptance and (on Anthropic) `view_image` registration.
pub fn resolve_supports_vision(
    vision_override: Option<bool>,
    provider_name: &str,
    model: &str,
) -> bool {
    vision_override.unwrap_or_else(|| supports_vision_for(provider_name, model))
}

/// Tool-free completion model for compaction summarization.
#[derive(Clone)]
pub enum CompactionModel {
    OpenRouter(Agent<<openrouter::Client as CompletionClient>::CompletionModel, ()>),
    OpenAI(Agent<rig_core::providers::openai::responses_api::ResponsesCompletionModel, ()>),
    Anthropic(Agent<rig_core::providers::anthropic::completion::CompletionModel, ()>),
    LlamaCpp(Agent<rig_core::providers::openai::completion::CompletionModel, ()>),
    Ollama(Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>),
    #[cfg(feature = "mock")]
    Mock(Agent<MockCompletionModel, ()>),
}

impl CompactionModel {
    pub async fn summarize(&self, prompt: &str) -> Result<String, PromptError> {
        match self {
            CompactionModel::OpenRouter(a) => a.prompt(prompt).await,
            CompactionModel::OpenAI(a) => a.prompt(prompt).await,
            CompactionModel::Anthropic(a) => a.prompt(prompt).await,
            CompactionModel::LlamaCpp(a) => a.prompt(prompt).await,
            CompactionModel::Ollama(a) => a.prompt(prompt).await,
            #[cfg(feature = "mock")]
            CompactionModel::Mock(a) => a.prompt(prompt).await,
        }
    }
}

/// A dynamic agent type that can work with any provider
/// This allows us to abstract over different provider agent types at runtime
pub enum DynAgent {
    /// OpenRouter agent with session hook
    OpenRouter(Agent<<openrouter::Client as CompletionClient>::CompletionModel, SessionHook>),
    /// OpenAI agent (uses modern responses API)
    OpenAI(
        Agent<rig_core::providers::openai::responses_api::ResponsesCompletionModel, SessionHook>,
    ),
    /// Anthropic agent (Messages API — carries images in tool results;
    /// works against Claude or a local Anthropic-compatible server)
    Anthropic(Agent<rig_core::providers::anthropic::completion::CompletionModel, SessionHook>),
    /// LlamaCpp agent (uses completions API for compatibility with llama.cpp)
    LlamaCpp(Agent<rig_core::providers::openai::completion::CompletionModel, SessionHook>),
    /// Ollama agent (no hook for local models)
    Ollama(Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>),
    /// Mock agent for testing (uses MockCompletionModel with session hook)
    #[cfg(feature = "mock")]
    Mock(Agent<MockCompletionModel, SessionHook>),
}

impl DynAgent {
    /// Prompt the agent with a single message
    pub async fn prompt(&self, prompt: &str) -> Result<String, PromptError> {
        match self {
            DynAgent::OpenRouter(agent) => agent.prompt(prompt).await,
            DynAgent::OpenAI(agent) => agent.prompt(prompt).await,
            DynAgent::Anthropic(agent) => agent.prompt(prompt).await,
            DynAgent::LlamaCpp(agent) => agent.prompt(prompt).await,
            DynAgent::Ollama(agent) => agent.prompt(prompt).await,
            #[cfg(feature = "mock")]
            DynAgent::Mock(agent) => agent.prompt(prompt).await,
        }
    }

    /// Prompt the agent with chat history.
    ///
    /// Accepts any `impl Into<Message>`. `&str` and `String` are text turns;
    /// a full `Message::User` with mixed content drives a vision turn. The
    /// same tool loop fires in both cases — rig's `PromptRequest` doesn't
    /// distinguish.
    pub async fn prompt_with_history(
        &self,
        prompt: impl Into<Message>,
        history: &mut Vec<Message>,
    ) -> Result<String, PromptError> {
        // Own the Message once — then clone per match arm, since rig's
        // `prompt()` takes `impl Into<Message>` by value.
        //
        // rig's `with_history` takes `IntoIterator<Item: Into<Message>>`.
        // `&mut Vec<Message>` iterates as `&mut Message`, which doesn't impl
        // `Into<Message>`. Reborrow as `&Vec<Message>` (yields `&Message`,
        // which DOES impl `Into<Message>` via blanket clone).
        let prompt: Message = prompt.into();
        let history: &Vec<Message> = &*history;
        match self {
            DynAgent::OpenRouter(agent) => agent.prompt(prompt.clone()).with_history(history).await,
            DynAgent::OpenAI(agent) => agent.prompt(prompt.clone()).with_history(history).await,
            DynAgent::Anthropic(agent) => agent.prompt(prompt.clone()).with_history(history).await,
            DynAgent::LlamaCpp(agent) => agent.prompt(prompt.clone()).with_history(history).await,
            DynAgent::Ollama(agent) => agent.prompt(prompt.clone()).with_history(history).await,
            #[cfg(feature = "mock")]
            DynAgent::Mock(agent) => agent.prompt(prompt).with_history(history).await,
        }
    }

    /// Check if this is a mock agent
    pub fn is_mock(&self) -> bool {
        #[cfg(feature = "mock")]
        {
            matches!(self, DynAgent::Mock(_))
        }
        #[cfg(not(feature = "mock"))]
        {
            false
        }
    }
}

/// Create a provider client and agent from the configuration
///
/// If mcp_tools is provided, they will be added to the agent along with built-in tools.
/// The system_prompt is used as the agent's preamble.
/// Returns the agent, provider info, event receiver, and shared session hook.
/// Stats are managed by the provided StateManager.
pub fn create_provider(
    config: &ProviderConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    DynAgent,
    ProviderInfo,
    Option<mpsc::UnboundedReceiver<AgentEvent>>,
    Arc<SessionHook>,
)> {
    match config {
        ProviderConfig::OpenRouter(c) => {
            let (agent, info, receiver, hook) = create_openrouter_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
            )?;
            Ok((
                DynAgent::OpenRouter(agent),
                info,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::OpenAI(c) => {
            let (agent, info, receiver, hook) = create_openai_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
            )?;
            Ok((
                DynAgent::OpenAI(agent),
                info,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::Anthropic(c) => {
            let (agent, info, receiver, hook) = create_anthropic_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
            )?;
            Ok((
                DynAgent::Anthropic(agent),
                info,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::LlamaCpp(c) => {
            let (agent, info, receiver, hook) = create_llamacpp_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
            )?;
            Ok((
                DynAgent::LlamaCpp(agent),
                info,
                Some(receiver),
                Arc::new(hook),
            ))
        }
        ProviderConfig::Ollama(c) => {
            let (agent, info) = create_ollama_agent(
                c,
                mcp_tools,
                system_prompt,
                searxng_config,
                max_turns,
                todo_tool,
                bash_config,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
            )?;
            Ok((
                DynAgent::Ollama(agent),
                info,
                None,                             // No event channel for Ollama
                Arc::new(SessionHook::new(None)), // Empty hook for Ollama
            ))
        }
    }
}

const COMPACTION_PREAMBLE: &str = "\
You are a conversation summarizer. Given a conversation transcript, produce a concise summary \
that preserves: key decisions made, important facts and context, tool calls and their results, \
and any state needed to continue the conversation. Be specific about what was done, not vague.";

/// Create a tool-free CompactionModel from the provider config.
/// Uses `model_override` if set, otherwise the provider's default model.
pub fn create_compaction_model(
    config: &ProviderConfig,
    model_override: Option<&str>,
) -> Result<CompactionModel> {
    match config {
        ProviderConfig::OpenRouter(c) => {
            let api_key = c
                .api_key
                .clone()
                .context("OpenRouter API key not configured")?;
            let client = openrouter::Client::builder()
                .api_key(&api_key)
                .build()
                .context("Failed to create OpenRouter client for compaction")?;
            let model = model_override.unwrap_or(&c.model);
            let agent = client.agent(model).preamble(COMPACTION_PREAMBLE).build();
            Ok(CompactionModel::OpenRouter(agent))
        }
        ProviderConfig::OpenAI(c) => {
            let api_key = c.api_key.clone().context("OpenAI API key not configured")?;
            let client = openai::Client::builder()
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create OpenAI client for compaction")?;
            let model = model_override.unwrap_or(&c.model);
            let agent = client.agent(model).preamble(COMPACTION_PREAMBLE).build();
            Ok(CompactionModel::OpenAI(agent))
        }
        ProviderConfig::Anthropic(c) => {
            let api_key = c.api_key.clone().unwrap_or_default();
            let client = anthropic::Client::builder()
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create Anthropic client for compaction")?;
            let model = model_override.unwrap_or(&c.model);
            // Anthropic hard-requires max_tokens (rig errors locally if it's
            // unset), and rig's per-model default is None for any non-Claude
            // name — so compaction/title generation silently failed on
            // gateway models like `minimax/MiniMax-M3`. Set it explicitly.
            let agent = client
                .agent(model)
                .preamble(COMPACTION_PREAMBLE)
                .max_tokens(c.max_tokens)
                .build();
            Ok(CompactionModel::Anthropic(agent))
        }
        ProviderConfig::LlamaCpp(c) => {
            let api_key = c.api_key.clone().unwrap_or_default();
            let client = openai::Client::builder()
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create LlamaCpp client for compaction")?
                .completions_api();
            let model = model_override.unwrap_or(&c.model);
            let agent = client.agent(model).preamble(COMPACTION_PREAMBLE).build();
            Ok(CompactionModel::LlamaCpp(agent))
        }
        ProviderConfig::Ollama(c) => {
            let client = ollama::Client::builder()
                .base_url(&c.base_url)
                .api_key(rig_core::client::Nothing)
                .build()
                .context("Failed to create Ollama client for compaction")?;
            let model = model_override.unwrap_or(&c.model);
            let agent = client.agent(model).preamble(COMPACTION_PREAMBLE).build();
            Ok(CompactionModel::Ollama(agent))
        }
    }
}

/// Create a mock CompactionModel for testing
#[cfg(feature = "mock")]
pub fn create_mock_compaction_model() -> (CompactionModel, MockCompletionModel) {
    use rig_core::agent::AgentBuilder;
    let mock_model = MockCompletionModel::new();
    let model_clone = mock_model.clone();
    let agent = AgentBuilder::new(mock_model)
        .preamble(COMPACTION_PREAMBLE)
        .build();
    (CompactionModel::Mock(agent), model_clone)
}

/// Get built-in tools for PeakBot (excluding SearchTool which requires config)
/// If todo_tool is provided, uses it; otherwise creates a new one.
///
/// `shell_kind` determines which shell tool is exposed to the model:
/// - `ShellKind::Bash` → registers `bash` tool
/// - `ShellKind::PowerShell` → registers `powershell` tool
fn add_builtin_tools<M, P>(
    builder: rig_core::agent::AgentBuilder<M, P, rig_core::agent::NoToolConfig>,
    searxng_config: Option<&SearXngConfig>,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Option<Arc<StateManager>>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    register_view_image: bool,
) -> rig_core::agent::AgentBuilder<M, P, rig_core::agent::WithBuilderTools>
where
    M: rig_core::completion::CompletionModel,
    P: rig_core::agent::PromptHook<M>,
{
    // Use provided tool or create a new one (with optional StateManager)
    let todo = todo_tool.unwrap_or_default();

    // Register exactly ONE shell tool based on the detected environment.
    // The model only sees the tool that matches the actual shell available.
    // If no shell is detected (e.g. Windows with nothing installed), no
    // shell tool is registered at all.
    let shell_tool = match shell_kind {
        Some(ShellKind::PowerShell { path }) => Some(EitherTool::PowerShell(PowerShellTool::new(
            path.clone(),
            bash_config.env.clone(),
        ))),
        Some(ShellKind::Bash { path }) => {
            // Wire the live panel (slice 3 of make-term-great-again.md)
            // when a state manager is available. Without one, the tool
            // still runs PTY-backed but skips the UI side-effects —
            // the same shape `BashBgTool` uses.
            let bash = BashTool::new(path.clone(), bash_config.env.clone());
            let bash = match state_manager.clone() {
                Some(sm) => bash.with_state_manager(sm),
                None => bash,
            };
            Some(EitherTool::Bash(bash))
        }
        None => None,
    };

    // `bash_bg` requires StateManager — it has no state of its own
    // (registry lives on StateManager). When `state_manager` is `None`
    // (test paths that exercise providers without a state manager),
    // fall back to the `Default` impl, which returns
    // `BashBgError::NoStateManager` on every call. The error is a
    // coach message rather than a panic, matching `TodoTool`'s same-
    // shape pattern.
    let bash_bg_tool = match state_manager {
        Some(sm) => BashBgTool::new_with_env(sm, bash_config.env.clone()),
        None => BashBgTool::default(),
    };

    let mut builder = builder
        .tool(FileCreateTool)
        .tool(FileStrReplaceTool)
        .tool(FileInsertTool)
        .tool(FileReadTool)
        .tool(bash_bg_tool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(FetchPageTool)
        .tool(ThinkTool)
        .tool(todo);

    // Add the single shell tool (bash OR powershell, never both, or none)
    if let Some(tool) = shell_tool {
        builder = match tool {
            EitherTool::Bash(t) => builder.tool(t),
            EitherTool::PowerShell(t) => builder.tool(t),
        };
    }

    // Conditionally add search tool if SearXNG is configured
    if let Some(config) = searxng_config {
        builder = builder.tool(SearchTool::new(config));
    }

    // Conditionally add the vector tools when a store is configured.
    if let Some(store) = vector_store {
        builder = builder
            .tool(crate::tools::DocIndexTool::new(store.clone()))
            .tool(crate::tools::DocSearchTool::new(store.clone()));
    }

    // `view_image` needs a tool-result channel that carries images — only
    // Anthropic. Other providers swap/err/drop the image, so registration is gated.
    if register_view_image {
        builder = builder.tool(crate::tools::ViewImageTool);
    }

    // Add DelegateTool if pipeline is enabled
    if let Some(registry) = pipeline_registry {
        let delegate_tool = crate::pipeline::DelegateTool::new(Arc::new(registry.clone()));
        builder = builder.tool(delegate_tool);
    }

    builder
}

/// Internal enum to hold either a Bash or PowerShell tool for registration.
enum EitherTool {
    Bash(BashTool),
    PowerShell(PowerShellTool),
}

/// Create OpenRouter agent and info
fn create_openrouter_agent(
    config: &OpenRouterConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    Agent<<openrouter::Client as CompletionClient>::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    let api_key = config
        .api_key
        .clone()
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

    // Get session stats from StateManager for context tracking
    let session_stats = state_manager.stats_arc();

    // Create session hook with stats tracking + compaction gate
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(&state_manager);

    // Build agent with system prompt, hook, and built-in tools
    let agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        false,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openrouter".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "openrouter", &model),
    };

    Ok((agent, info, receiver, hook))
}

/// Create Anthropic agent and info. `base_url` fronts hosted Claude or a
/// local Anthropic-compatible server (e.g. llama-server `/v1/messages`);
/// the tool-result channel carries images, hence `view_image` is here.
fn create_anthropic_agent(
    config: &AnthropicConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    Agent<rig_core::providers::anthropic::completion::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    // API key is optional — local Anthropic-compatible servers often need none.
    let api_key = config.api_key.clone().unwrap_or_default();

    if config.model.is_empty() {
        anyhow::bail!("Anthropic model not specified");
    }

    let client = anthropic::Client::builder()
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create Anthropic client")?;

    let model = config.model.clone();

    let session_stats = state_manager.stats_arc();

    // One decision feeds both `[img:…]` acceptance and `view_image` registration.
    let supports_vision = resolve_supports_vision(config.vision, "anthropic", &model);

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(&state_manager);

    let completion_model = client.completion_model(&model);
    // Build the model explicitly so prompt caching can be toggled — `client.agent()`
    // hides the model and exposes no hook for the caching flags.
    let completion_model = match config.prompt_caching {
        AnthropicCaching::Off => completion_model,
        AnthropicCaching::Manual => completion_model.with_prompt_caching(),
        AnthropicCaching::Auto => completion_model.with_automatic_caching(),
        AnthropicCaching::Auto1h => completion_model.with_automatic_caching_1h(),
    };

    let agent_builder = AgentBuilder::new(completion_model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        supports_vision,
    );

    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "anthropic".to_string(),
        model: model.clone(),
        supports_pricing: false,
        supports_vision,
    };

    Ok((agent, info, receiver, hook))
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
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>,
    ProviderInfo,
)> {
    if config.model.is_empty() {
        anyhow::bail!("Ollama model not specified");
    }

    // Use Nothing as API key since Ollama doesn't require one
    let client = ollama::Client::builder()
        .base_url(&config.base_url)
        .api_key(rig_core::client::Nothing)
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

    // Add built-in tools (including optional SearchTool and TodoTool)
    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        false,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "ollama".to_string(),
        model: model.clone(),
        supports_pricing: false,
        supports_vision: resolve_supports_vision(config.vision, "ollama", &model),
    };

    Ok((agent, info))
}

/// Create OpenAI agent and info
fn create_openai_agent(
    config: &OpenAIConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    Agent<rig_core::providers::openai::responses_api::ResponsesCompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    let api_key = config
        .api_key
        .clone()
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

    // Get session stats from StateManager for context tracking
    let session_stats = state_manager.stats_arc();

    // Create session hook with stats tracking + compaction gate
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(&state_manager);

    // Build agent with system prompt, hook, and built-in tools
    let agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        false,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openai".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "openai", &model),
    };

    Ok((agent, info, receiver, hook))
}

/// Create LlamaCpp agent and info (uses completions API for compatibility)
fn create_llamacpp_agent(
    config: &LlamaCppConfig,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
    system_prompt: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    todo_tool: Option<TodoTool>,
    bash_config: &BashConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
) -> Result<(
    Agent<rig_core::providers::openai::completion::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<AgentEvent>,
    SessionHook,
)> {
    // API key is optional for local llama.cpp instances
    let api_key = config.api_key.clone().unwrap_or_default();

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

    // Get session stats from StateManager for context tracking
    let session_stats = state_manager.stats_arc();

    // Create session hook with stats tracking + compaction gate
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(&state_manager);

    // Build agent with system prompt, hook, and built-in tools
    let mut agent_builder = client
        .agent(&model)
        .preamble(system_prompt)
        .max_tokens(config.max_tokens)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Merge user-supplied extra params (e.g. {"no-log": true} for LiteLLM
    // proxies). rig flattens this JSON into every chat-completions request
    // body, so any vendor-specific top-level field passes through unchanged.
    if let Some(extra) = config.extra_params.clone() {
        agent_builder = agent_builder.additional_params(extra);
    }

    // Add built-in tools (including optional SearchTool and TodoTool)
    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        false,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder.tools(tools).build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "llamacpp".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "llamacpp", &model),
    };

    Ok((agent, info, receiver, hook))
}

/// Create a mock agent for testing with MockCompletionModel and SessionHook
///
/// This is only available when the "mock" feature is enabled and allows the test harness to create
/// a DynAgent::Mock variant that can be used with AgentRunner.
#[cfg(feature = "mock")]
pub fn create_mock_agent(
    system_prompt: &str,
    max_turns: usize,
    state_manager: Arc<StateManager>,
) -> Result<(
    DynAgent,
    ProviderInfo,
    mpsc::UnboundedReceiver<AgentEvent>,
    Arc<SessionHook>,
    MockCompletionModel,
)> {
    use rig_core::agent::AgentBuilder;

    let mock_model = MockCompletionModel::new();
    let model_clone = mock_model.clone();

    // Create session hook with stats tracking + compaction gate (using context_tracking for full functionality)
    let session_stats = state_manager.stats_arc();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(&state_manager);

    // Build agent with mock model, session hook, and built-in tools
    let agent_builder = AgentBuilder::new(mock_model)
        .preamble(system_prompt)
        .max_tokens(1024)
        .default_max_turns(max_turns)
        .hook(hook.clone());

    // Add built-in tools (simplified for testing)
    let bash_tool = BashTool::default();
    let todo = TodoTool::new(state_manager.clone());

    let agent = agent_builder
        .tool(FileCreateTool)
        .tool(FileStrReplaceTool)
        .tool(FileInsertTool)
        .tool(FileReadTool)
        .tool(bash_tool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(FetchPageTool)
        .tool(ThinkTool)
        .tool(todo)
        .build();

    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock-model".to_string(),
        supports_pricing: true,
        // Mock is used by integration tests — keep vision enabled so those
        // tests can exercise both code paths.
        supports_vision: true,
    };

    Ok((
        DynAgent::Mock(agent),
        info,
        receiver,
        Arc::new(hook),
        model_clone,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_info_supports_vision_pins_detection_for_known_patterns() {
        // Pinning test for the `supports_vision` flag — this is the boundary
        // where a vision-capable model must be recognised, and vice versa.
        let vision_ok = ProviderInfo {
            name: "openrouter".into(),
            model: "anthropic/claude-3.5-sonnet".into(),
            supports_pricing: true,
            supports_vision: crate::vision::model_supports_vision("anthropic/claude-3.5-sonnet"),
        };
        let vision_no = ProviderInfo {
            name: "openrouter".into(),
            model: "qwen/qwq-32b".into(),
            supports_pricing: true,
            supports_vision: crate::vision::model_supports_vision("qwen/qwq-32b"),
        };
        assert!(vision_ok.supports_vision);
        assert!(!vision_no.supports_vision);
    }

    #[test]
    fn supports_vision_for_gates_anthropic_on_transport_not_model_name() {
        // Regression: `[img:…]` was blocked on Anthropic for any unknown name
        // (e.g. local GGUF), while `view_image` worked there — must gate alike.
        assert!(supports_vision_for("anthropic", "minimax/MiniMax-M3"));
        assert!(supports_vision_for("anthropic", "some-local-gguf"));
        assert!(supports_vision_for("anthropic", "claude-3.5-sonnet"));

        // Other providers still rely on name-based detection.
        assert!(supports_vision_for(
            "openrouter",
            "anthropic/claude-3.5-sonnet"
        ));
        assert!(!supports_vision_for("openrouter", "qwen/qwq-32b"));
        assert!(!supports_vision_for("ollama", "some-local-gguf"));
    }

    #[test]
    fn resolve_supports_vision_override_beats_auto_detection() {
        // None → auto-detection (unchanged behaviour).
        assert!(resolve_supports_vision(
            None,
            "anthropic",
            "some-local-gguf"
        ));
        assert!(!resolve_supports_vision(None, "openrouter", "qwen/qwq-32b"));

        // Some(true) forces ON even for an unrecognised name on a provider whose
        // auto-detection would say no (the user's `vision: true` case).
        assert!(resolve_supports_vision(
            Some(true),
            "ollama",
            "some-local-gguf"
        ));
        assert!(resolve_supports_vision(
            Some(true),
            "openrouter",
            "qwen/qwq-32b"
        ));

        // Some(false) forces OFF even for a model auto-detection would accept.
        assert!(!resolve_supports_vision(
            Some(false),
            "anthropic",
            "claude-3.5-sonnet"
        ));
        assert!(!resolve_supports_vision(
            Some(false),
            "openrouter",
            "gpt-4o"
        ));
    }

    #[test]
    fn prompt_with_history_signature_accepts_string_and_message() {
        // Compile-only pin: if this builds, both a `&str` and a
        // `rig_core::Message::User` with multimodal content satisfy the
        // `impl Into<Message>` bound on `prompt_with_history`. We don't
        // invoke the call because it would require a real agent; the type
        // bound is the contract we're pinning.
        use rig_core::OneOrMany;
        use rig_core::completion::message::{
            DocumentSourceKind, Image, ImageMediaType, Message, Text, UserContent,
        };

        fn _takes_into_message<T: Into<Message>>(_t: T) {}

        _takes_into_message("hello");
        _takes_into_message(String::from("hello"));

        let multimodal = Message::User {
            content: OneOrMany::many([
                UserContent::Image(Image {
                    data: DocumentSourceKind::Base64("x".into()),
                    media_type: Some(ImageMediaType::PNG),
                    detail: None,
                    additional_params: None,
                }),
                UserContent::Text(Text::new("what is this?")),
            ])
            .expect("non-empty"),
        };
        _takes_into_message(multimodal);
    }

    // -----------------------------------------------------------------
    // Layer 1 — boot-time validation pins (see unfuck-compact.md).
    //
    // The boot path at `lib.rs:325` and the model-switch path at
    // `lib.rs:1015` rely on `create_compaction_model` returning `Err`
    // when the provider can't be constructed. These pins lock that
    // contract for the providers where the failure mode is testable
    // without a network round-trip (i.e. missing API key).
    // -----------------------------------------------------------------

    #[test]
    fn create_compaction_model_fails_when_openai_api_key_missing() {
        // OpenAI explicitly checks `api_key.is_some()` and returns
        // `"OpenAI API key not configured"`. With Layer 1 in place, this
        // error propagates up `lib.rs:339` (`?` after `with_context`) and
        // aborts boot when `context.enabled == true`.
        let cfg = ProviderConfig::OpenAI(crate::config::OpenAIConfig {
            api_key: None,
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o".to_string(),
            max_tokens: 1024,
            vision: None,
        });
        let result = create_compaction_model(&cfg, None);
        assert!(
            result.is_err(),
            "missing api_key must surface as an error, not a silent default"
        );
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("OpenAI API key not configured"),
            "error chain should mention the missing key; got: {msg}"
        );
    }

    #[test]
    fn create_compaction_model_fails_when_openrouter_api_key_missing() {
        // Parallel pin for OpenRouter. Same boot-path contract.
        let cfg = ProviderConfig::OpenRouter(crate::config::OpenRouterConfig {
            api_key: None,
            model: "anthropic/claude-3.5-sonnet".to_string(),
            max_tokens: 1024,
            vision: None,
        });
        let result = create_compaction_model(&cfg, None);
        assert!(result.is_err(), "missing api_key must surface as an error");
        let msg = format!("{:#}", result.err().unwrap());
        assert!(
            msg.contains("OpenRouter API key not configured"),
            "error chain should mention the missing key; got: {msg}"
        );
    }

    #[test]
    fn create_compaction_model_honours_model_override() {
        // Sanity pin: the `model_override` param is what `lib.rs` threads
        // through as `config.context.compaction_model`. If it's silently
        // ignored, the user's "use a cheaper model for compaction"
        // contract breaks. We can't observe the wire payload here, but we
        // can confirm the override path doesn't error on a well-formed
        // Ollama config (no API key required → succeeds even offline).
        let cfg = ProviderConfig::Ollama(crate::config::OllamaConfig {
            base_url: "http://localhost:11434".to_string(),
            model: "llama3".to_string(),
            temperature: None,
            vision: None,
        });
        let result = create_compaction_model(&cfg, Some("qwen2.5-coder:7b"));
        assert!(
            result.is_ok(),
            "Ollama construction shouldn't fail on a well-formed config (offline)"
        );
    }

    /// Regression: the Anthropic compaction model must set `max_tokens`.
    /// rig errors *locally* with "max_tokens must be set for Anthropic"
    /// when it's unset, and its per-model default is None for any
    /// non-Claude name (e.g. a gateway model like `minimax/MiniMax-M3`).
    /// That swallowed the title/compaction LLM call silently. We can't
    /// read the built agent's private fields, but we can drive
    /// `summarize` against an unroutable host: with `max_tokens` set the
    /// failure is a *transport* error, NOT the local max_tokens guard.
    #[tokio::test]
    async fn anthropic_compaction_model_sets_max_tokens_for_non_claude_model() {
        let cfg = ProviderConfig::Anthropic(crate::config::AnthropicConfig {
            api_key: Some("test-key".to_string()),
            // Reserved TEST-NET-1 (RFC 5737) — guaranteed unroutable, so
            // the call fails at transport, never reaching a real server.
            base_url: "http://192.0.2.1:1/v1/messages".to_string(),
            model: "minimax/MiniMax-M3".to_string(),
            max_tokens: 64,
            prompt_caching: crate::config::AnthropicCaching::Off,
            vision: None,
        });
        let model = create_compaction_model(&cfg, None).expect("construction must succeed");
        let err = model
            .summarize("title this")
            .await
            .expect_err("call to an unroutable host must fail");
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            !msg.contains("max_tokens"),
            "max_tokens must be set so rig doesn't reject locally; got: {msg}"
        );
    }
}
