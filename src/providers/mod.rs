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

pub mod retry;

use crate::config::{
    AnthropicCaching, AnthropicConfig, BashConfig, LlamaCppConfig, OllamaConfig, OpenAIConfig,
    OpenRouterConfig, ProviderConfig, RetryConfig, SearXngConfig, TimeoutsConfig,
};
use crate::hooks::SessionHook;
use crate::hooks::events::SourcedEvent;
#[cfg(feature = "mock")]
use crate::mock::MockCompletionModel;
use crate::state::StateManager;
use crate::tools::{
    BashBgTool, BashTool, FetchPageTool, FetchUrlTool, FileCreateTool, FileInsertTool,
    FileReadTool, FileStrReplaceTool, ListDirectoryTool, PdfReadTool, PowerShellTool, SearchTool,
    ShellKind, ThinkTool, TodoTool, ViewImageTool,
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
use std::sync::{Arc, Mutex};
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
    /// Resolved once at agent-build from
    /// `(model.preserve_reasoning, provider.preserve_reasoning)` via
    /// [`resolve_preserve_reasoning`]. Driving knob for the SessionHook
    /// capture seam (`with_preserve_reasoning`) and the StateManager
    /// wire-side gate together — when false, no thinking block survives
    /// capture or replay for this provider+model.
    pub preserve_reasoning: bool,
    /// Resolved counterpart of `preserve_reasoning` for the web
    /// transcript display path. Drives the server-side gate that
    /// decides whether `thinking` reaches the browser; defaults to
    /// false (wire-only / invisible).
    pub display_reasoning: bool,
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

/// Resolved decision for the `preserve_reasoning` capture knob.
///
/// Provider override wins over the model override (the provider block is the
/// coarser, more deliberate escape hatch — a deployment that 400s on thinking
/// wants one switch, not a per-model edit). `None` on both → `true`:
/// Anthropic replay needs the blocks to keep tool loops alive, and dropping
/// is what breaks them.
pub fn resolve_preserve_reasoning(
    model_override: Option<bool>,
    provider_override: Option<bool>,
) -> bool {
    provider_override.or(model_override).unwrap_or(true)
}

/// Same hierarchy as [`resolve_preserve_reasoning`], but the unset default
/// is `false` — thinking blocks are captured and replayed by default, but
/// invisible in the web transcript unless the user opts the model (or the
/// provider) in.
pub fn resolve_display_reasoning(
    model_override: Option<bool>,
    provider_override: Option<bool>,
) -> bool {
    provider_override.or(model_override).unwrap_or(false)
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    DynAgent,
    ProviderInfo,
    Option<mpsc::UnboundedReceiver<SourcedEvent>>,
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
                tools_filter,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
                skills,
                retry,
                timeouts,
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
                tools_filter,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
                skills,
                retry,
                timeouts,
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
                tools_filter,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
                skills,
                retry,
                timeouts,
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
                tools_filter,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
                skills,
                retry,
                timeouts,
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
                tools_filter,
                pipeline_registry,
                state_manager,
                shell_kind,
                vector_store,
                skills,
                retry,
                timeouts,
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
                .http_client(crate::http::client())
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
                .http_client(crate::http::client())
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
                .http_client(crate::http::client())
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
                .http_client(crate::http::client())
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
                .http_client(crate::http::client())
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Option<Arc<StateManager>>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    view_image: Option<ViewImageTool>,
    wire_bash_panel: bool,
    sub_agent_wiring: Option<SubAgentWiring>,
    timeouts: &TimeoutsConfig,
) -> rig_core::agent::AgentBuilder<M, P, rig_core::agent::WithBuilderTools>
where
    M: rig_core::completion::CompletionModel,
    P: rig_core::agent::PromptHook<M>,
{
    // Use provided tool or create a new one (with optional StateManager)
    let todo = todo_tool.unwrap_or_default();

    // Keep a clone for the delegate tool (built at the end) — `state_manager`
    // is moved into `bash_bg` below.
    let sm_for_delegate = state_manager.clone();

    // Path + shell tools resolve/spawn against the session cwd, owned by the
    // state manager (single source of truth). Without one (tests), fall back to
    // the process cwd — unchanged behaviour.
    let session_cwd: std::path::PathBuf = match state_manager.as_ref() {
        Some(sm) => sm.session_cwd(),
        None => std::env::current_dir().unwrap_or_default(),
    };

    // Register exactly ONE shell tool based on the detected environment.
    // The model only sees the tool that matches the actual shell available.
    // If no shell is detected (e.g. Windows with nothing installed), no
    // shell tool is registered at all.
    let shell_tool = match shell_kind {
        Some(ShellKind::PowerShell { path }) => Some(EitherTool::PowerShell(
            PowerShellTool::new(path.clone(), bash_config.env.clone())
                .with_session_cwd(session_cwd.clone()),
        )),
        Some(ShellKind::Bash { path }) => {
            // Wire the live panel (slice 3 of make-term-great-again.md)
            // when a state manager is available AND this agent owns the
            // panel. Sub-agents (`wire_bash_panel = false`) skip the panel
            // so their shell output never bleeds into the orchestrator's
            // bash panel — they still run PTY-backed against `session_cwd`.
            let bash = BashTool::new(path.clone(), bash_config.env.clone())
                .with_session_cwd(session_cwd.clone());
            Some(EitherTool::Bash(wire_bash_tool(
                bash,
                wire_bash_panel,
                state_manager.clone(),
            )))
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

    let mut tools: Vec<Box<dyn ToolDyn>> = vec![
        gate(Box::new(FileCreateTool::new(session_cwd.clone()))),
        gate(Box::new(FileStrReplaceTool::new(session_cwd.clone()))),
        gate(Box::new(FileInsertTool::new(session_cwd.clone()))),
        gate(Box::new(FileReadTool::new(session_cwd.clone()))),
        gate(Box::new(PdfReadTool::new(session_cwd.clone()))),
        gate(Box::new(bash_bg_tool)),
        gate(Box::new(ListDirectoryTool::new(session_cwd.clone()))),
        gate(Box::new(FetchUrlTool)),
        gate(Box::new(FetchPageTool)),
        // `todo` is ungated: the task text already carries the plan, so a
        // `thought` field is redundant — and some models (e.g. MiniMax)
        // structurally refuse it, tripping the gate's nudge on every call.
        Box::new(todo),
        // `think` is ungated: its `thought` IS the payload it echoes back,
        // not metadata — gating would strip the very thing it returns.
        Box::new(ThinkTool),
    ];

    // Add the single shell tool (bash OR powershell, never both, or none)
    if let Some(tool) = shell_tool {
        tools.push(match tool {
            EitherTool::Bash(t) => gate(Box::new(t)),
            EitherTool::PowerShell(t) => gate(Box::new(t)),
        });
    }

    // Conditionally add search tool if SearXNG is configured
    if let Some(config) = searxng_config {
        tools.push(gate(Box::new(SearchTool::new(config))));
    }
    // Conditionally add the vector tools when a store is configured.
    if let Some(store) = vector_store {
        tools.push(gate(Box::new(
            crate::tools::DocIndexTool::new(store.clone()).with_session_cwd(session_cwd.clone()),
        )));
        tools.push(gate(Box::new(
            crate::tools::DocSearchTool::new(store.clone()).with_session_cwd(session_cwd.clone()),
        )));
    }

    // `view_image` needs a tool-result channel that carries images — only
    // Anthropic. Other providers swap/err/drop the image, so registration is
    // gated by the caller passing `None`; `Some` implies both "registered"
    // and "configured" (§A6 — the ceiling can no longer go missing).
    if let Some(t) = view_image {
        tools.push(gate(Box::new(t)));
    }

    // Add DelegateTool if pipeline is enabled. It needs the same build
    // context the orchestrator had (to spawn fresh sub-agents), captured here
    // where that context is in scope. `sub_agent_wiring` carries the extra
    // pieces (event sink, max_turns) not already passed; the rest are cloned
    // from this call's args. Requires a `state_manager` (sub-agents need
    // session cwd + bg registry) — without one the delegate tool is simply
    // not registered.
    if let (Some(registry), Some(wiring), Some(sm)) =
        (pipeline_registry, sub_agent_wiring, sm_for_delegate)
    {
        let deps = crate::pipeline::SubAgentDeps {
            registry: Arc::new(registry.clone()),
            searxng: searxng_config.cloned(),
            bash_config: bash_config.clone(),
            tools_filter: tools_filter.clone(),
            state_manager: sm,
            shell_kind: shell_kind.cloned(),
            vector_store: vector_store.cloned(),
            max_turns: wiring.max_turns,
            skills: wiring.skills,
            event_sink: wiring.event_sink,
            retry: wiring.retry,
            timeouts: timeouts.clone(),
        };
        let delegate_tool = crate::pipeline::DelegateTool::new(Arc::new(deps));
        tools.push(gate(Box::new(delegate_tool)));
    }

    // Single filter seam: drop any built-in the `tools:` config excludes.
    // Keyed on the gated tool's name (the gate delegates `name()` to the
    // inner tool), so blocklist/allowlist address tools by their wire name.
    tools.retain(|t| tools_filter.allows(&t.name()));

    builder.tools(budget_all(tools, timeouts))
}

/// Attach the live bash panel iff this agent owns one. The orchestrator wires
/// its panel (`wire_panel = true`); sub-agents pass `false` so their shell
/// output never bleeds into the orchestrator's panel. Either way the tool runs
/// PTY-backed against the session cwd already set on `bash`.
fn wire_bash_tool(
    bash: BashTool,
    wire_panel: bool,
    state_manager: Option<Arc<StateManager>>,
) -> BashTool {
    match (wire_panel, state_manager) {
        (true, Some(sm)) => bash.with_state_manager(sm),
        _ => bash,
    }
}

/// The extra sub-agent build context threaded into [`add_builtin_tools`] that
/// it doesn't already receive as direct args: the orchestrator's event sink
/// (sub-agent events TEE here tagged `SubAgent`), `max_turns`, and the retry
/// policy.
pub(crate) struct SubAgentWiring {
    pub event_sink: Option<mpsc::UnboundedSender<SourcedEvent>>,
    pub max_turns: usize,
    /// The discovered skills — each delegation renders this through the
    /// role's filter into the sub-agent preamble.
    pub skills: crate::skills::SkillRegistry,
    /// Retry policy for a delegation's own wire calls — the same one the main
    /// run loop uses, so a sub-agent survives the blips a top-level turn does.
    pub retry: RetryConfig,
}

/// Internal enum to hold either a Bash or PowerShell tool for registration.
enum EitherTool {
    Bash(BashTool),
    PowerShell(PowerShellTool),
}

/// Wrap a tool in [`ThoughtGate`] so it gains the cross-cutting `thought`
/// field + soft nudge. Applied to every built-in and MCP tool.
fn gate(inner: Box<dyn ToolDyn>) -> Box<dyn ToolDyn> {
    Box::new(crate::tools::ThoughtGate::wrap(inner))
}

/// Wrap every tool in a wall-clock budget. Called at the two — and only two —
/// places tools enter the rig builder, so "every tool the model can call is
/// time-bounded" holds by construction. See docs/tool-time-budget-design.md.
fn budget_all(tools: Vec<Box<dyn ToolDyn>>, cfg: &TimeoutsConfig) -> Vec<Box<dyn ToolDyn>> {
    tools
        .into_iter()
        .map(|t| Box::new(crate::tools::TimeBudget::wrap(t, cfg)) as Box<dyn ToolDyn>)
        .collect()
}

/// Provider name + model for a sub-agent's sniff records. The five arms are
/// the same mapping `Config::provider_name`/`Config::model` make, but a
/// sub-agent is handed a bare `ProviderConfig`, not the whole `Config`.
fn wire_label_of(config: &ProviderConfig) -> (String, String) {
    match config {
        ProviderConfig::OpenRouter(c) => ("openrouter".to_string(), c.model.clone()),
        ProviderConfig::OpenAI(c) => ("openai".to_string(), c.model.clone()),
        ProviderConfig::Anthropic(c) => ("anthropic".to_string(), c.model.clone()),
        ProviderConfig::LlamaCpp(c) => ("llamacpp".to_string(), c.model.clone()),
        ProviderConfig::Ollama(c) => ("ollama".to_string(), c.model.clone()),
    }
}

/// Gate (`thought`) then budget (wall clock) a set of already-boxed MCP tools.
/// The budget is outermost so it also covers the gate's own JSON work and so
/// the timeout string is never mutated by the gate's nudge.
pub(crate) fn prepare_mcp_tools(
    tools: Vec<Box<dyn ToolDyn>>,
    cfg: &TimeoutsConfig,
) -> Vec<Box<dyn ToolDyn>> {
    budget_all(tools.into_iter().map(gate).collect(), cfg)
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    Agent<<openrouter::Client as CompletionClient>::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<SourcedEvent>,
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
        .http_client(crate::http::client())
        .api_key(&api_key)
        .build()
        .context("Failed to create OpenRouter client")?;

    let model = config.model.clone();

    // Get session stats from StateManager for context tracking
    let session_stats = state_manager.stats_arc();

    // Create session hook with stats tracking + compaction gate
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender.clone()), session_stats)
        .with_state_manager(&state_manager)
        .with_wire_label("openrouter".to_string(), model.clone());

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
        tools_filter,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        None,
        true,
        Some(SubAgentWiring {
            event_sink: Some(sender),
            retry: retry.clone(),
            max_turns,
            skills: skills.clone(),
        }),
        timeouts,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder
            .tools(prepare_mcp_tools(tools, timeouts))
            .build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openrouter".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "openrouter", &model),
        preserve_reasoning: false,
        display_reasoning: false,
    };

    Ok((agent, info, receiver, hook))
}

/// Build the Anthropic `SessionHook` with `preserve_reasoning` already
/// applied. `AgentBuilder::hook` stores the value by `Clone`, so any
/// configuration on the returned hook must happen *before* it reaches the
/// rig agent — patching it afterwards leaves the agent's embedded hook on
/// `preserve_reasoning: false` forever.
pub fn build_anthropic_session_hook(
    sender: tokio::sync::mpsc::UnboundedSender<crate::hooks::events::SourcedEvent>,
    session_stats: Arc<Mutex<crate::hooks::SessionStats>>,
    state_manager: &Arc<StateManager>,
    preserve_reasoning: bool,
) -> SessionHook {
    SessionHook::with_context_tracking(Some(sender), session_stats)
        .with_state_manager(state_manager)
        .with_preserve_reasoning(preserve_reasoning)
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    Agent<rig_core::providers::anthropic::completion::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<SourcedEvent>,
    SessionHook,
)> {
    // API key is optional — local Anthropic-compatible servers often need none.
    let api_key = config.api_key.clone().unwrap_or_default();

    if config.model.is_empty() {
        anyhow::bail!("Anthropic model not specified");
    }

    let client = anthropic::Client::builder()
        .http_client(crate::http::client())
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create Anthropic client")?;

    let model = config.model.clone();

    let session_stats = state_manager.stats_arc();

    // One decision feeds both `[img:…]` acceptance and `view_image` registration.
    let supports_vision = resolve_supports_vision(config.vision, "anthropic", &model);

    // Resolve `preserve_reasoning` before the rig agent captures the hook
    // by clone — `with_preserve_reasoning` on a hook already handed to
    // `.hook(...)` is a no-op for the agent's embedded copy.
    let preserve_reasoning = resolve_preserve_reasoning(config.preserve_reasoning, None);

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = build_anthropic_session_hook(
        sender.clone(),
        session_stats,
        &state_manager,
        preserve_reasoning,
    )
    .with_wire_label("anthropic".to_string(), model.clone());

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
        tools_filter,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        supports_vision.then(|| ViewImageTool::new(config.max_image_base64_bytes)),
        true,
        Some(SubAgentWiring {
            event_sink: Some(sender),
            retry: retry.clone(),
            max_turns,
            skills: skills.clone(),
        }),
        timeouts,
    );

    let agent = if let Some(tools) = mcp_tools {
        agent_builder
            .tools(prepare_mcp_tools(tools, timeouts))
            .build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "anthropic".to_string(),
        model: model.clone(),
        supports_pricing: false,
        supports_vision,
        // `build_provider_config` already resolved the model vs provider
        // overrides into `AnthropicConfig.preserve_reasoning`. Reuse the
        // local computed above so the value that drives the hook and the
        // value on `ProviderInfo` cannot diverge.
        preserve_reasoning,
        display_reasoning: resolve_display_reasoning(config.display_reasoning, None),
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    Agent<<ollama::Client as CompletionClient>::CompletionModel, ()>,
    ProviderInfo,
)> {
    if config.model.is_empty() {
        anyhow::bail!("Ollama model not specified");
    }

    // Use Nothing as API key since Ollama doesn't require one
    let client = ollama::Client::builder()
        .http_client(crate::http::client())
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

    // Add built-in tools (including optional SearchTool and TodoTool).
    // Ollama has no event channel, so delegated sub-agents can't TEE their
    // events here — `event_sink` is `None`.
    let agent_builder = add_builtin_tools(
        agent_builder,
        searxng_config,
        todo_tool,
        bash_config,
        tools_filter,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        None,
        true,
        Some(SubAgentWiring {
            event_sink: None,
            retry: retry.clone(),
            max_turns,
            skills: skills.clone(),
        }),
        timeouts,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder
            .tools(prepare_mcp_tools(tools, timeouts))
            .build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "ollama".to_string(),
        model: model.clone(),
        supports_pricing: false,
        supports_vision: resolve_supports_vision(config.vision, "ollama", &model),
        // Non-Anthropic provider: no Anthropic-style thinking blocks flow
        // through the wire. The StateManager provider gate is what keeps a
        // reloaded Claude transcript from leaking reasoning into this
        // provider's turn.
        preserve_reasoning: false,
        display_reasoning: false,
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    Agent<rig_core::providers::openai::responses_api::ResponsesCompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<SourcedEvent>,
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
        .http_client(crate::http::client())
        .api_key(&api_key)
        .base_url(&config.base_url)
        .build()
        .context("Failed to create OpenAI client")?;

    let model = config.model.clone();

    // Get session stats from StateManager for context tracking
    let session_stats = state_manager.stats_arc();

    // Create session hook with stats tracking + compaction gate
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender.clone()), session_stats)
        .with_state_manager(&state_manager)
        .with_wire_label("openai".to_string(), model.clone());

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
        tools_filter,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        None,
        true,
        Some(SubAgentWiring {
            event_sink: Some(sender),
            retry: retry.clone(),
            max_turns,
            skills: skills.clone(),
        }),
        timeouts,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder
            .tools(prepare_mcp_tools(tools, timeouts))
            .build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "openai".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "openai", &model),
        preserve_reasoning: false,
        display_reasoning: false,
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
    tools_filter: &crate::config::ToolsConfig,
    pipeline_registry: Option<&crate::pipeline::SubAgentRegistry>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    skills: &crate::skills::SkillRegistry,
    retry: &RetryConfig,
    timeouts: &TimeoutsConfig,
) -> Result<(
    Agent<rig_core::providers::openai::completion::CompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<SourcedEvent>,
    SessionHook,
)> {
    // API key is optional for local llama.cpp instances
    let api_key = config.api_key.clone().unwrap_or_default();

    if config.model.is_empty() {
        anyhow::bail!("LlamaCpp model not specified");
    }

    // Build the OpenAI client with completions API for llama.cpp compatibility
    let client = openai::Client::builder()
        .http_client(crate::http::client())
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
    let hook = SessionHook::with_context_tracking(Some(sender.clone()), session_stats)
        .with_state_manager(&state_manager)
        .with_wire_label("llamacpp".to_string(), model.clone());

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
        tools_filter,
        pipeline_registry,
        Some(state_manager.clone()),
        shell_kind,
        vector_store,
        None,
        true,
        Some(SubAgentWiring {
            event_sink: Some(sender),
            retry: retry.clone(),
            max_turns,
            skills: skills.clone(),
        }),
        timeouts,
    );

    // Add MCP tools and build
    let agent = if let Some(tools) = mcp_tools {
        agent_builder
            .tools(prepare_mcp_tools(tools, timeouts))
            .build()
    } else {
        agent_builder.build()
    };

    let info = ProviderInfo {
        name: "llamacpp".to_string(),
        model: model.clone(),
        supports_pricing: true,
        supports_vision: resolve_supports_vision(config.vision, "llamacpp", &model),
        preserve_reasoning: false,
        display_reasoning: false,
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
    mpsc::UnboundedReceiver<SourcedEvent>,
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
        .with_state_manager(&state_manager)
        .with_wire_label("mock".to_string(), "mock-model".to_string());

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
        .tool(FileCreateTool::default())
        .tool(FileStrReplaceTool::default())
        .tool(FileInsertTool::default())
        .tool(FileReadTool::default())
        .tool(bash_tool)
        .tool(ListDirectoryTool::default())
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
        // Mock's purpose-built for tests exercising reasoning flows; default
        // preserves the integration with the capture/wire seams.
        preserve_reasoning: true,
        display_reasoning: false,
    };

    Ok((
        DynAgent::Mock(agent),
        info,
        receiver,
        Arc::new(hook),
        model_clone,
    ))
}

/// Build a fresh sub-agent [`DynAgent`] for a delegation.
///
/// Shares [`add_builtin_tools`] with the orchestrator path — a sub-agent gets
/// the **full built-in toolset** (session-cwd + bg registry come from the
/// shared `state_manager`), *minus* `delegate` itself (`pipeline_registry` is
/// always `None` — no nested delegation in v1).
///
/// The hook is **events-only** (`SessionHook::new(sink).with_source(...)`) —
/// no `with_context_tracking`/`with_state_manager`, so it neither tracks the
/// orchestrator's context size nor gates in-loop compaction (the sub-agent
/// runs on a fresh, concise-by-prompt history). Its `ToolCall`/`ToolResult`/
/// `CompletionResponse` events flow to the orchestrator's shared receiver
/// tagged `SubAgent { role }`, so cost rolls up and turns are TEE'd to the
/// transcript. Returns the hook so the caller can register it as the active
/// sub-agent hook.
///
/// **Ollama is hookless by type** (`DynAgent::Ollama` carries `()`), so a role
/// on an Ollama model ignores `sink`/`source`: no event TEE, no cost roll-up
/// for that lane. This matches Ollama's existing no-cost-tracking
/// behaviour — a documented degradation, not a silent surprise. The same
/// applies to `context_budget`: an Ollama sub-agent gets neither the proactive
/// gate nor a history snapshot, so an interrupted run is summarised only when
/// the error itself carries history (`MaxTurnsError`, `PromptCancelled`);
/// a bare `CompletionError` yields a header-only handoff.
pub(crate) fn build_sub_agent(
    config: &ProviderConfig,
    preamble: &str,
    sink: Option<mpsc::UnboundedSender<SourcedEvent>>,
    role: &str,
    searxng_config: Option<&SearXngConfig>,
    max_turns: usize,
    bash_config: &BashConfig,
    tools_filter: &crate::config::ToolsConfig,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
    vector_store: Option<&crate::vector::VectorStore>,
    context_budget: Option<usize>,
    timeouts: &TimeoutsConfig,
) -> Result<(DynAgent, Arc<SessionHook>)> {
    // Events-only lane-tagged hook. No compaction gate (fresh context) — the
    // sub-agent gate terminates instead of compacting.
    let (wire_provider, wire_model) = wire_label_of(config);
    let hook = SessionHook::new(sink)
        .with_source(crate::ui::app_state::MessageSource::SubAgent {
            role: role.to_string(),
        })
        .with_wire_label(wire_provider, wire_model)
        .with_sub_agent_gate(context_budget);

    // A sub-agent never sees `delegate` (no nested delegation). It reaches the
    // `todo` tool via `add_builtin_tools`'s `todo_tool.unwrap_or_default()`,
    // and `TodoTool::default()` is now a working *standalone* backend with its
    // own isolated list — so each sub-agent gets a fresh, functional todo that
    // drives no panel (the visible panel is an orchestrator-only affordance).
    let no_pipeline: Option<&crate::pipeline::SubAgentRegistry> = None;

    match config {
        ProviderConfig::OpenRouter(c) => {
            let api_key = c
                .api_key
                .clone()
                .context("OpenRouter API key not configured for sub-agent")?;
            let client = openrouter::Client::builder()
                .http_client(crate::http::client())
                .api_key(&api_key)
                .build()
                .context("Failed to create OpenRouter client for sub-agent")?;
            let builder = client
                .agent(&c.model)
                .preamble(preamble)
                .max_tokens(c.max_tokens)
                .default_max_turns(max_turns)
                .hook(hook.clone());
            let builder = add_builtin_tools(
                builder,
                searxng_config,
                None,
                bash_config,
                tools_filter,
                no_pipeline,
                Some(state_manager),
                shell_kind,
                vector_store,
                None,
                false,
                None,
                timeouts,
            );
            Ok((DynAgent::OpenRouter(builder.build()), Arc::new(hook)))
        }
        ProviderConfig::OpenAI(c) => {
            let api_key = c
                .api_key
                .clone()
                .context("OpenAI API key not configured for sub-agent")?;
            let client = openai::Client::builder()
                .http_client(crate::http::client())
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create OpenAI client for sub-agent")?;
            let builder = client
                .agent(&c.model)
                .preamble(preamble)
                .max_tokens(c.max_tokens)
                .default_max_turns(max_turns)
                .hook(hook.clone());
            let builder = add_builtin_tools(
                builder,
                searxng_config,
                None,
                bash_config,
                tools_filter,
                no_pipeline,
                Some(state_manager),
                shell_kind,
                vector_store,
                None,
                false,
                None,
                timeouts,
            );
            Ok((DynAgent::OpenAI(builder.build()), Arc::new(hook)))
        }
        ProviderConfig::Anthropic(c) => {
            let api_key = c.api_key.clone().unwrap_or_default();
            let client = anthropic::Client::builder()
                .http_client(crate::http::client())
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create Anthropic client for sub-agent")?;
            let supports_vision = resolve_supports_vision(c.vision, "anthropic", &c.model);
            let completion_model = client.completion_model(&c.model);
            let completion_model = match c.prompt_caching {
                AnthropicCaching::Off => completion_model,
                AnthropicCaching::Manual => completion_model.with_prompt_caching(),
                AnthropicCaching::Auto => completion_model.with_automatic_caching(),
                AnthropicCaching::Auto1h => completion_model.with_automatic_caching_1h(),
            };
            let builder = AgentBuilder::new(completion_model)
                .preamble(preamble)
                .max_tokens(c.max_tokens)
                .default_max_turns(max_turns)
                .hook(hook.clone());
            let builder = add_builtin_tools(
                builder,
                searxng_config,
                None,
                bash_config,
                tools_filter,
                no_pipeline,
                Some(state_manager),
                shell_kind,
                vector_store,
                supports_vision.then(|| ViewImageTool::new(c.max_image_base64_bytes)),
                false,
                None,
                timeouts,
            );
            Ok((DynAgent::Anthropic(builder.build()), Arc::new(hook)))
        }
        ProviderConfig::LlamaCpp(c) => {
            let api_key = c.api_key.clone().unwrap_or_default();
            let client = openai::Client::builder()
                .http_client(crate::http::client())
                .api_key(&api_key)
                .base_url(&c.base_url)
                .build()
                .context("Failed to create LlamaCpp client for sub-agent")?
                .completions_api();
            let mut builder = client
                .agent(&c.model)
                .preamble(preamble)
                .max_tokens(c.max_tokens)
                .default_max_turns(max_turns)
                .hook(hook.clone());
            if let Some(extra) = c.extra_params.clone() {
                builder = builder.additional_params(extra);
            }
            let builder = add_builtin_tools(
                builder,
                searxng_config,
                None,
                bash_config,
                tools_filter,
                no_pipeline,
                Some(state_manager),
                shell_kind,
                vector_store,
                None,
                false,
                None,
                timeouts,
            );
            Ok((DynAgent::LlamaCpp(builder.build()), Arc::new(hook)))
        }
        ProviderConfig::Ollama(c) => {
            // Ollama's DynAgent variant carries no hook — `sink`/`source` are
            // dropped for this lane (see the fn doc). The agent still runs and
            // returns its output; only TEE/cost are unavailable.
            let client = ollama::Client::builder()
                .http_client(crate::http::client())
                .base_url(&c.base_url)
                .api_key(rig_core::client::Nothing)
                .build()
                .context("Failed to create Ollama client for sub-agent")?;
            let mut builder = client
                .agent(&c.model)
                .preamble(preamble)
                .default_max_turns(max_turns);
            if let Some(temp) = c.temperature {
                builder = builder.temperature(temp as f64);
            }
            let builder = add_builtin_tools(
                builder,
                searxng_config,
                None,
                bash_config,
                tools_filter,
                no_pipeline,
                Some(state_manager),
                shell_kind,
                vector_store,
                None,
                false,
                None,
                timeouts,
            );
            Ok((DynAgent::Ollama(builder.build()), Arc::new(hook)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::ToolDefinition;
    use rig_core::tool::Tool;
    use serde_json::{Value, json};
    use std::time::Duration;

    #[test]
    fn wire_bash_tool_severs_sub_agent_panel() {
        // The orchestrator (`wire_panel = true`) drives the live bash
        // panel; a sub-agent (`wire_panel = false`) must not — its shell
        // output would otherwise bleed into the orchestrator's panel.
        let path = "/bin/bash".to_string();
        let mk = || BashTool::new(path.clone(), None);
        let sm = Arc::new(StateManager::new());

        // Orchestrator: panel wired.
        assert!(wire_bash_tool(mk(), true, Some(sm.clone())).has_state_manager());
        // Sub-agent: panel severed even with a state manager present.
        assert!(!wire_bash_tool(mk(), false, Some(sm.clone())).has_state_manager());
        // No state manager (test paths): nothing to wire, regardless of flag.
        assert!(!wire_bash_tool(mk(), true, None).has_state_manager());
    }

    #[test]
    fn provider_info_supports_vision_pins_detection_for_known_patterns() {
        // Pinning test for the `supports_vision` flag — this is the boundary
        // where a vision-capable model must be recognised, and vice versa.
        let vision_ok = ProviderInfo {
            name: "openrouter".into(),
            model: "anthropic/claude-3.5-sonnet".into(),
            supports_pricing: true,
            supports_vision: crate::vision::model_supports_vision("anthropic/claude-3.5-sonnet"),
            preserve_reasoning: false,
            display_reasoning: false,
        };
        let vision_no = ProviderInfo {
            name: "openrouter".into(),
            model: "qwen/qwq-32b".into(),
            supports_pricing: true,
            supports_vision: crate::vision::model_supports_vision("qwen/qwq-32b"),
            preserve_reasoning: false,
            display_reasoning: false,
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
            preserve_reasoning: None,
            display_reasoning: None,
            max_image_base64_bytes: 5 * 1024 * 1024,
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

    // ── registration invariant (B) ─────────────────────────────────────────
    //
    // The two seams — `add_builtin_tools`'s `builder.tools(...)` and the
    // renamed `prepare_mcp_tools` (ex-`gate_all`) — are the only two places
    // tools enter the rig builder. These tests pin that both seams wrap
    // every tool in `TimeBudget` AND (for MCP) `ThoughtGate`, so "an
    // unbudgeted tool" stays unrepresentable. The functions don't exist
    // yet — these tests are RED.

    /// Minimal inner tool with a fixed name; used to verify the decorator
    /// wrappers preserve `name()` and `definition()` byte-for-byte.
    struct InnerEcho;

    impl Tool for InnerEcho {
        const NAME: &'static str = "echo";
        type Error = std::convert::Infallible;
        type Args = Value;
        type Output = String;

        async fn definition(&self, _p: String) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_string(),
                description: "echo".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": { "x": { "type": "string" } },
                    "required": ["x"]
                }),
            }
        }

        async fn call(&self, args: Value) -> Result<String, Self::Error> {
            Ok(args.to_string())
        }
    }

    /// `budget_all` must be a structural pass-through on the surface area
    /// `ToolDyn` exposes: name preserved verbatim, definition body preserved
    /// verbatim (no `thought` injection — that's `ThoughtGate`'s job; if
    /// `TimeBudget` injected it, the gate would double-inject).
    ///
    /// Updated for the configurable-budget surface: `budget_all` now
    /// threads `cfg: &TimeoutsConfig` through so every tool's budget resolves
    /// from the operator's config, not the hard-coded constant.
    #[tokio::test]
    async fn budget_all_preserves_name_and_definition() {
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(InnerEcho)];
        let budgeted = budget_all(tools, &TimeoutsConfig::default());

        assert_eq!(budgeted.len(), 1, "budget_all must preserve count");
        assert_eq!(budgeted[0].name(), "echo", "name must pass through");

        let def = budgeted[0].definition(String::new()).await;
        assert_eq!(def.name, "echo", "definition().name must pass through");

        // The exact property set from InnerEcho.definition() — no `thought`.
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(
            !props.contains_key("thought"),
            "TimeBudget must NOT inject `thought` — that's ThoughtGate's job; got schema: {}",
            def.parameters
        );
    }

    /// The MCP seam (`prepare_mcp_tools`) must produce tools with BOTH the
    /// `thought` injection (from `ThoughtGate`) AND a budget (from
    /// `TimeBudget`). The first is observable through `definition()`; the
    /// second is observable only as "the schema wasn't touched by TimeBudget
    /// alone" — together, the schema is `ThoughtGate(InnerEcho)` with the
    /// TimeBudget wrapper sitting around the gate.
    ///
    /// Updated for the configurable-budget surface: same threading as
    /// `budget_all` — `cfg: &TimeoutsConfig` for the budget resolution.
    #[tokio::test]
    async fn prepare_mcp_tools_injects_thought_and_preserves_name() {
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(InnerEcho)];
        let mcp = prepare_mcp_tools(tools, &TimeoutsConfig::default());

        assert_eq!(mcp.len(), 1, "prepare_mcp_tools must preserve count");
        assert_eq!(mcp[0].name(), "echo", "name must pass through");

        let def = mcp[0].definition(String::new()).await;
        assert_eq!(def.name, "echo", "definition().name must pass through");

        // ThoughtGate effect: `thought` must appear as a required property.
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(
            props.contains_key("thought"),
            "ThoughtGate must inject `thought` for every MCP tool; got schema: {}",
            def.parameters
        );
        let required = def.parameters["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v == "thought"),
            "`thought` must be in the required list (so the model is forced to provide it); got: {required:?}"
        );
        // The inner tool's own property must survive the gate.
        assert!(
            props.contains_key("x"),
            "the inner tool's `x` property must survive both decorators; got: {props:?}"
        );
    }

    /// End-to-end pin for the configurable-budget surface: a tool that
    /// hangs forever, wrapped by `budget_all` with the *configured* budget
    /// set to 1 second, must come back as a `⏱ TIMEOUT` string inside a
    /// few seconds of real wall clock. Mirrors the same real-short-budget
    /// technique the `time_budget` self-test uses (`tokio::start_paused`
    /// needs `test-util`, not enabled in this crate).
    ///
    /// RED state: this test will fail to compile before the implementer
    /// threads `cfg: &TimeoutsConfig` into `budget_all` AND adds
    /// `TimeoutsConfig` to the providers module's `crate::config` import
    /// block. Once the surface is in place, this test is the load-bearing
    /// end-to-end pin: a tool resolving its budget from the operator's
    /// config, not from a constant. Pre-implementation: zero sites can
    /// pass it.
    #[tokio::test]
    async fn budget_all_applies_the_configured_seconds() {
        // `tool_secs: 1` is the minimum legal config value; nothing smaller
        // gets past `TimeoutsConfig::validate()`. The decorator arms exactly
        // `Duration::from_secs(1)` because `budget_for("always_pending", &cfg)`
        // for an unknown tool returns `cfg.tool_secs` verbatim — no clamping
        // — so this test verifies the pass-through works.
        let cfg = TimeoutsConfig {
            tool_secs: 1,
            delegate_secs: 3_600,
        };

        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(AlwaysPending)];
        let budgeted = budget_all(tools, &cfg);
        assert_eq!(budgeted.len(), 1, "budget_all must preserve count");
        assert_eq!(
            budgeted[0].name(),
            "always_pending",
            "name must pass through"
        );

        let started = tokio::time::Instant::now();
        let result = budgeted[0].call("{}".to_string()).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "configured budget (1s) must fire within seconds; took {:?}",
            elapsed
        );

        let s = result.expect("timeout must be Ok(String), not Err(ToolError)");
        assert!(s.contains("⏱ TIMEOUT"), "timeout marker missing from: {s}");
        assert!(s.contains("always_pending"), "tool name missing from: {s}");
        assert!(
            s.contains("1s"),
            "configured budget seconds (1s) must appear in the timeout message; got: {s}"
        );
    }

    /// Local fixture used only by `budget_all_applies_the_configured_seconds`:
    /// a `ToolDyn` whose `call` returns a never-completing future, so
    /// `TimeBudget` is forced to arm the wall-clock boundary. Inlined here
    /// (rather than reaching for `time_budget::Named` which is a private
    /// sibling-module type) to keep both tests independent — if the
    /// decorator-side fixture changes shape, this test shouldn't move with it.
    struct AlwaysPending;

    impl Tool for AlwaysPending {
        const NAME: &'static str = "always_pending";
        type Error = std::convert::Infallible;
        type Args = Value;
        type Output = String;

        async fn definition(&self, _p: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "hangs forever".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                }),
            }
        }

        async fn call(&self, _args: Value) -> Result<String, Self::Error> {
            std::future::pending::<()>().await;
            unreachable!("never returns");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Resolution helpers (design §2.1 / §8 — Task T2).
    //
    // `resolve_preserve_reasoning` and `resolve_display_reasoning` apply
    // the design's resolution rules:
    //   - provider_override wins over model_override (provider is the
    //     coarser, more deliberate escape hatch).
    //   - `None` here defaults to `true` for preserve and `false` for
    //     display (the wire-only / invisible defaults).
    // ─────────────────────────────────────────────────────────────────────────

    /// `resolve_preserve_reasoning`: provider wins over model; unset
    /// defaults to `true`.
    #[test]
    fn resolve_preserve_reasoning_hierarchy() {
        // Both unset → true (the design's default-on).
        assert!(resolve_preserve_reasoning(None, None));

        // Model says on, provider silent → true.
        assert!(resolve_preserve_reasoning(Some(true), None));

        // Model says off, provider silent → false.
        assert!(!resolve_preserve_reasoning(Some(false), None));

        // Model says on, provider overrules off → false (provider wins).
        assert!(!resolve_preserve_reasoning(Some(true), Some(false)));

        // Model says off, provider overrules on → true (provider wins).
        assert!(resolve_preserve_reasoning(Some(false), Some(true)));
    }

    /// `resolve_display_reasoning`: same hierarchy, but the unset default
    /// is `false` (wire-only / invisible by default).
    #[test]
    fn resolve_display_reasoning_hierarchy() {
        // Both unset → false (the design's default-off).
        assert!(!resolve_display_reasoning(None, None));

        // Model says on, provider silent → true.
        assert!(resolve_display_reasoning(Some(true), None));

        // Model says off, provider silent → false.
        assert!(!resolve_display_reasoning(Some(false), None));

        // Provider overrules off → false.
        assert!(!resolve_display_reasoning(Some(true), Some(false)));

        // Provider overrules on → true.
        assert!(resolve_display_reasoning(Some(false), Some(true)));
    }
}
