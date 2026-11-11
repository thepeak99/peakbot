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

pub mod refusal;
pub mod retry;

use crate::config::resolve::{Member, Subagent};
use crate::config::{
    AnthropicCaching, AnthropicConfig, Env, LlamaCppConfig, OllamaConfig, OpenAIConfig,
    OpenRouterConfig, ProviderConfig, ResolvedModel, TimeoutsConfig,
};
use crate::hooks::SessionHook;
use crate::hooks::events::SourcedEvent;
#[cfg(feature = "mock")]
use crate::mock::MockCompletionModel;
use crate::pipeline::SubAgentDeps;
use crate::state::StateManager;
use crate::tools::env::{ToolEnv, builtin_tool_names};
use crate::tools::workspace::Workspace;
#[cfg(feature = "host")]
use crate::tools::{BashTool, PowerShellTool, ShellBgTool};
use crate::tools::{
    FetchPageTool, FetchUrlTool, SearchTool, ShellKind, ThinkTool, TodoTool, ViewImageTool,
};
use crate::tools::{
    FileCreateTool, FileInsertTool, FileReadTool, FileStrReplaceTool, ListDirectoryTool,
    PdfReadTool,
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
        history: &[Message],
    ) -> Result<String, PromptError> {
        // Own the Message once — then clone per match arm, since rig's
        // `prompt()` takes `impl Into<Message>` by value. `history` iterates as
        // `&Message`, which rig's `with_history` accepts via `Into<Message>`.
        let prompt: Message = prompt.into();
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

/// Build the main agent for `model` with `system_prompt` as its preamble.
///
/// Tools come from `tool_env` (the profile's built-ins, MCP servers, and
/// timeouts) with the bash env `tool_env.bash_env(member)`. `delegate` is
/// registered iff `delegate` is `Some` — callers pass it iff
/// `tools::env::delegate_enabled(agent)`; its `event_sink` is replaced by
/// this build's own channel. The delegation retry policy travels in
/// `SubAgentDeps.retry`. `model` is passed separately from `member` because
/// `/model` may run the agent on a model other than its configured one.
///
/// Returns the agent, provider info, event receiver, and shared session hook.
/// Stats are managed by the provided StateManager.
pub fn create_provider(
    model: &ResolvedModel,
    system_prompt: &str,
    tool_env: &ToolEnv,
    member: &Member,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
) -> Result<(
    DynAgent,
    ProviderInfo,
    Option<mpsc::UnboundedReceiver<SourcedEvent>>,
    Arc<SessionHook>,
)> {
    let bash_env = tool_env.bash_env(member);
    match &model.provider_config {
        ProviderConfig::OpenRouter(c) => {
            let (agent, info, receiver, hook) = create_openrouter_agent(
                c,
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
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
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
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
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
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
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
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
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
            )?;
            Ok((
                DynAgent::Ollama(agent),
                info,
                None,                             // No event channel for Ollama
                Arc::new(SessionHook::new(None)), // Empty hook for Ollama
            ))
        }
        #[cfg(feature = "mock")]
        ProviderConfig::Mock => {
            let (agent, info, receiver, hook) = create_fixed_mock_agent(
                system_prompt,
                tool_env,
                bash_env,
                delegate,
                todo_tool,
                state_manager,
                shell_kind,
            );
            Ok((DynAgent::Mock(agent), info, Some(receiver), Arc::new(hook)))
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
        #[cfg(feature = "mock")]
        ProviderConfig::Mock => {
            let model = MockCompletionModel::replying(crate::mock::MOCK_REPLY);
            let agent = AgentBuilder::new(model)
                .preamble(COMPACTION_PREAMBLE)
                .build();
            Ok(CompactionModel::Mock(agent))
        }
    }
}

/// The `type: mock` provider's main agent: a [`MockCompletionModel`] that
/// answers every prompt with [`crate::mock::MOCK_REPLY`], wired exactly like a
/// real provider (context-tracking hook, the planned built-ins, `delegate`)
/// so an end-to-end session runs the production path minus the network.
#[cfg(feature = "mock")]
fn create_fixed_mock_agent(
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
) -> (
    Agent<MockCompletionModel, SessionHook>,
    ProviderInfo,
    mpsc::UnboundedReceiver<SourcedEvent>,
    SessionHook,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let hook = SessionHook::with_context_tracking(Some(sender.clone()), state_manager.stats_arc())
        .with_state_manager(&state_manager)
        .with_wire_label("mock".to_string(), "mock".to_string());
    let agent_builder = AgentBuilder::new(MockCompletionModel::replying(crate::mock::MOCK_REPLY))
        .preamble(system_prompt)
        .default_max_turns(tool_env.max_turns)
        .hook(hook.clone());
    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(Some(sender))));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager),
        shell_kind,
        None,
        true,
    )
    .build();
    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock".to_string(),
        supports_vision: false,
        preserve_reasoning: false,
        display_reasoning: false,
    };
    (agent, info, receiver, hook)
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

/// Register an agent's tools on `builder`: the built-ins
/// [`builtin_tool_names`] plans (constructed here, by name — so the plan is
/// the single source of truth), then `delegate` iff `delegate` is `Some`
/// (never filterable), then the profile's MCP tools. Main agents and
/// subagents both come through here, so they get the same built-ins and
/// the same MCP tools by construction (spec §6.2).
///
/// `shell_kind` picks the ONE shell tool and its bg tool (`bash` +
/// `bash_bg` or `powershell` + `powershell_bg`); no shell ⇒ neither.
/// `view_image` is `Some` only for providers whose tool-result channel
/// carries images. `todo_tool` defaults to a fresh standalone list.
fn add_builtin_tools<M, P>(
    builder: rig_core::agent::AgentBuilder<M, P, rig_core::agent::NoToolConfig>,
    tool_env: &ToolEnv,
    bash_env: Env,
    todo_tool: Option<TodoTool>,
    delegate: Option<Arc<SubAgentDeps>>,
    state_manager: Option<Arc<StateManager>>,
    shell_kind: Option<&ShellKind>,
    view_image: Option<ViewImageTool>,
    wire_bash_panel: bool,
) -> rig_core::agent::AgentBuilder<M, P, rig_core::agent::WithBuilderTools>
where
    M: rig_core::completion::CompletionModel,
    P: rig_core::agent::PromptHook<M>,
{
    // Path + shell tools resolve/spawn against the session cwd, owned by the
    // state manager (single source of truth). Without one (tests), fall back to
    // the process cwd — unchanged behaviour.
    #[cfg(feature = "host")]
    let session_cwd: std::path::PathBuf = match state_manager.as_ref() {
        Some(sm) => sm.session_cwd(),
        None => std::env::current_dir().unwrap_or_default(),
    };
    // One root for all six file tools, shared by main agents and sub-agents
    // (they receive the same `StateManager`). Local: confined to the
    // conversation's upload folder, read per call.
    #[cfg(feature = "host")]
    let workspace: Workspace = Workspace::Open(session_cwd.clone());
    #[cfg(not(feature = "host"))]
    let workspace: Workspace = Workspace::Confined(state_manager.clone());
    #[cfg(feature = "host")]
    let shell_env = shell_env(&bash_env);
    // The Local edition plans no tool that touches the session cwd or a shell.
    #[cfg(not(feature = "host"))]
    let _ = (bash_env, wire_bash_panel);

    // The delegate tool's outer TimeBudget must not expire while the
    // sub-agent is parked — it awaits the same session gate as the hook and
    // the inner loop.
    let delegate_pause = delegate.as_ref().map(|d| d.state_manager.pause_gate());

    let mut todo_tool = Some(todo_tool.unwrap_or_default());
    let mut view_image = view_image;
    let planned = builtin_tool_names(tool_env, shell_kind, view_image.is_some());
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(planned.len() + 1);
    for name in planned {
        let tool: Box<dyn ToolDyn> = match name {
            "file_create" => gate(Box::new(FileCreateTool::new(workspace.clone()))),
            "file_str_replace" => gate(Box::new(FileStrReplaceTool::new(workspace.clone()))),
            "file_insert" => gate(Box::new(FileInsertTool::new(workspace.clone()))),
            "file_read" => gate(Box::new(FileReadTool::new(workspace.clone()))),
            "pdf_read" => gate(Box::new(PdfReadTool::new(workspace.clone()))),
            "list_directory" => gate(Box::new(ListDirectoryTool::new(workspace.clone()))),
            "fetch_url" => gate(Box::new(FetchUrlTool::new(&tool_env.fetch_url))),
            "fetch_page" => gate(Box::new(FetchPageTool)),
            // `todo` is ungated: the task text already carries the plan, so a
            // `thought` field is redundant — and some models (e.g. MiniMax)
            // structurally refuse it, tripping the gate's nudge on every call.
            "todo" => Box::new(todo_tool.take().unwrap_or_default()),
            // `think` is ungated: its `thought` IS the payload it echoes back,
            // not metadata — gating would strip the very thing it returns.
            "think" => Box::new(ThinkTool),
            // The bg tool always spawns with the shell the model sees. No
            // state manager (test paths) ⇒ it still registers but answers
            // every call with the `NoStateManager` coach message.
            #[cfg(feature = "host")]
            "bash_bg" | "powershell_bg" => gate(Box::new(ShellBgTool::new(
                planned_shell(shell_kind).clone(),
                state_manager.clone(),
                shell_env.clone(),
            ))),
            #[cfg(feature = "host")]
            "bash" => {
                // Sub-agents (`wire_bash_panel = false`) skip the live panel
                // so their shell output never bleeds into the main agent's
                // bash panel — they still run PTY-backed against
                // `session_cwd`.
                let bash = BashTool::new(
                    planned_shell(shell_kind).executable().to_string(),
                    shell_env.clone(),
                )
                .with_session_cwd(session_cwd.clone());
                gate(Box::new(wire_bash_tool(
                    bash,
                    wire_bash_panel,
                    state_manager.clone(),
                )))
            }
            #[cfg(feature = "host")]
            "powershell" => gate(Box::new(
                PowerShellTool::new(
                    planned_shell(shell_kind).executable().to_string(),
                    shell_env.clone(),
                )
                .with_session_cwd(session_cwd.clone()),
            )),
            "web_search" => {
                let search = tool_env
                    .search
                    .as_ref()
                    .expect("web_search is planned only when general.search is set");
                gate(Box::new(SearchTool::new(search)))
            }
            #[cfg(feature = "host")]
            "doc_index" => gate(Box::new(
                crate::tools::DocIndexTool::new(planned_vector(tool_env).clone())
                    .with_session_cwd(session_cwd.clone()),
            )),
            #[cfg(feature = "host")]
            "doc_search" => gate(Box::new(
                crate::tools::DocSearchTool::new(planned_vector(tool_env).clone())
                    .with_session_cwd(session_cwd.clone()),
            )),
            "view_image" => {
                gate(Box::new(view_image.take().expect(
                    "view_image is planned only when the caller passed the tool",
                )))
            }
            other => unreachable!("builtin_tool_names planned an unknown tool: {other}"),
        };
        tools.push(tool);
    }

    if let Some(deps) = delegate {
        tools.push(gate(Box::new(crate::pipeline::DelegateTool::new(deps))));
    }

    builder
        .tools(budget_all(tools, &tool_env.timeouts, delegate_pause))
        .tools(prepare_mcp_tools(tool_env.mcp.tools(), &tool_env.timeouts))
}

#[cfg(feature = "host")]
/// The shell a planned `bash`/`powershell`/bg tool runs on. The plan only
/// names a shell tool when `shell_kind` is `Some`.
fn planned_shell(shell_kind: Option<&ShellKind>) -> &ShellKind {
    shell_kind.expect("shell tools are planned only when a shell is detected")
}

#[cfg(feature = "host")]
/// The store a planned `doc_*` tool uses. The plan only names them when the
/// profile has a vector store.
fn planned_vector(tool_env: &ToolEnv) -> &crate::vector::VectorStore {
    tool_env
        .vector
        .as_ref()
        .expect("doc tools are planned only when the profile has a vector store")
}

#[cfg(feature = "host")]
/// The shell tools' env shape. An empty overlay is `None` — the shell then
/// runs with the inherited process env, exactly as with no `env:` at all.
fn shell_env(env: &Env) -> Option<std::collections::HashMap<String, String>> {
    (!env.is_empty()).then(|| env.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// The `view_image` image budget for a provider whose tool-result channel
/// carries images and whose model can see — only Anthropic. `None` ⇒ no
/// `view_image` tool. One decision for the real build and the name
/// projections (`tools::env::main_agent_tool_names`).
pub(crate) fn view_image_budget(config: &ProviderConfig) -> Option<usize> {
    match config {
        ProviderConfig::Anthropic(c) => anthropic_view_image_budget(c),
        _ => None,
    }
}

/// [`view_image_budget`] for an Anthropic config: the same vision decision
/// that drives `[img:…]` acceptance.
fn anthropic_view_image_budget(c: &AnthropicConfig) -> Option<usize> {
    resolve_supports_vision(c.vision, "anthropic", &c.model).then_some(c.max_image_base64_bytes)
}

#[cfg(feature = "host")]
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

/// Wrap a tool in [`ThoughtGate`] so it gains the cross-cutting `thought`
/// field + soft nudge. Applied to every built-in and MCP tool.
fn gate(inner: Box<dyn ToolDyn>) -> Box<dyn ToolDyn> {
    Box::new(crate::tools::ThoughtGate::wrap(inner))
}

/// Wrap every tool in a wall-clock budget and a panic boundary. Called at the
/// two — and only two — places tools enter the rig builder, so "every tool the
/// model can call is time-bounded and panic-caught" holds by construction.
/// See docs/tool-time-budget-design.md.
///
/// Only `delegate` gets the session's pause gate: it is the only tool that
/// can park mid-call (its sub-agent's hook awaits the gate), so it is the
/// only one whose deadline must exclude parked time. MCP tools never park
/// mid-call — pass `None`.
fn budget_all(
    tools: Vec<Box<dyn ToolDyn>>,
    cfg: &TimeoutsConfig,
    delegate_pause: Option<Arc<crate::state::PauseGate>>,
) -> Vec<Box<dyn ToolDyn>> {
    tools
        .into_iter()
        .map(|t| {
            let budgeted = crate::tools::TimeBudget::wrap(t, cfg);
            let budgeted = match &delegate_pause {
                Some(gate) if budgeted.name() == "delegate" => {
                    budgeted.with_pause_gate(gate.clone())
                }
                _ => budgeted,
            };
            Box::new(budgeted) as Box<dyn ToolDyn>
        })
        .collect()
}

/// Provider name + model for a sub-agent's sniff records, from its bare
/// `ProviderConfig`.
fn wire_label_of(config: &ProviderConfig) -> (String, String) {
    match config {
        ProviderConfig::OpenRouter(c) => ("openrouter".to_string(), c.model.clone()),
        ProviderConfig::OpenAI(c) => ("openai".to_string(), c.model.clone()),
        ProviderConfig::Anthropic(c) => ("anthropic".to_string(), c.model.clone()),
        ProviderConfig::LlamaCpp(c) => ("llamacpp".to_string(), c.model.clone()),
        ProviderConfig::Ollama(c) => ("ollama".to_string(), c.model.clone()),
        #[cfg(feature = "mock")]
        ProviderConfig::Mock => ("mock".to_string(), "mock".to_string()),
    }
}

/// Gate (`thought`) then budget (wall clock) a set of already-boxed MCP tools.
/// The budget is outermost so it also covers the gate's own JSON work and so
/// the timeout string is never mutated by the gate's nudge.
pub(crate) fn prepare_mcp_tools(
    tools: Vec<Box<dyn ToolDyn>>,
    cfg: &TimeoutsConfig,
) -> Vec<Box<dyn ToolDyn>> {
    // MCP tools never park mid-call — no pause gate.
    budget_all(tools.into_iter().map(gate).collect(), cfg, None)
}

/// Create OpenRouter agent and info
fn create_openrouter_agent(
    config: &OpenRouterConfig,
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
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
        .default_max_turns(tool_env.max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(Some(sender))));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager.clone()),
        shell_kind,
        None,
        true,
    )
    .build();

    let info = ProviderInfo {
        name: "openrouter".to_string(),
        model: model.clone(),
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
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
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
        .default_max_turns(tool_env.max_turns)
        .hook(hook.clone());

    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(Some(sender))));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager.clone()),
        shell_kind,
        anthropic_view_image_budget(config).map(ViewImageTool::new),
        true,
    )
    .build();

    let info = ProviderInfo {
        name: "anthropic".to_string(),
        model: model.clone(),
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
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
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
        .default_max_turns(tool_env.max_turns);

    if let Some(temp) = config.temperature {
        agent_builder = agent_builder.temperature(temp as f64);
    }

    // Add built-in tools (including optional SearchTool and TodoTool).
    // Ollama has no event channel, so delegated sub-agents can't TEE their
    // events here — `event_sink` is `None`.
    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(None)));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager.clone()),
        shell_kind,
        None,
        true,
    )
    .build();

    let info = ProviderInfo {
        name: "ollama".to_string(),
        model: model.clone(),
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
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
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
        .default_max_turns(tool_env.max_turns)
        .hook(hook.clone());

    // Add built-in tools (including optional SearchTool and TodoTool)
    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(Some(sender))));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager.clone()),
        shell_kind,
        None,
        true,
    )
    .build();

    let info = ProviderInfo {
        name: "openai".to_string(),
        model: model.clone(),
        supports_vision: resolve_supports_vision(config.vision, "openai", &model),
        preserve_reasoning: false,
        display_reasoning: false,
    };

    Ok((agent, info, receiver, hook))
}

/// Create LlamaCpp agent and info (uses completions API for compatibility)
fn create_llamacpp_agent(
    config: &LlamaCppConfig,
    system_prompt: &str,
    tool_env: &ToolEnv,
    bash_env: Env,
    delegate: Option<Arc<SubAgentDeps>>,
    todo_tool: Option<TodoTool>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
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
        .default_max_turns(tool_env.max_turns)
        .hook(hook.clone());

    // Merge user-supplied extra params (e.g. {"no-log": true} for LiteLLM
    // proxies). rig flattens this JSON into every chat-completions request
    // body, so any vendor-specific top-level field passes through unchanged.
    if let Some(extra) = config.extra_params.clone() {
        agent_builder = agent_builder.additional_params(extra);
    }

    // Add built-in tools (including optional SearchTool and TodoTool)
    let delegate = delegate.map(|d| Arc::new(d.with_event_sink(Some(sender))));
    let agent = add_builtin_tools(
        agent_builder,
        tool_env,
        bash_env,
        todo_tool,
        delegate,
        Some(state_manager.clone()),
        shell_kind,
        None,
        true,
    )
    .build();

    let info = ProviderInfo {
        name: "llamacpp".to_string(),
        model: model.clone(),
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
    let todo = TodoTool::new(state_manager.clone());

    #[cfg(feature = "host")]
    let agent_builder = agent_builder
        .tool(FileCreateTool::default())
        .tool(FileStrReplaceTool::default())
        .tool(FileInsertTool::default())
        .tool(FileReadTool::default())
        .tool(BashTool::default())
        .tool(ListDirectoryTool::default());
    let agent = agent_builder
        .tool(FetchUrlTool::default())
        .tool(FetchPageTool)
        .tool(ThinkTool)
        .tool(todo)
        .build();

    let info = ProviderInfo {
        name: "mock".to_string(),
        model: "mock-model".to_string(),
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

/// Build a fresh sub-agent [`DynAgent`] for a delegation on `model` — the
/// delegation's EFFECTIVE model (`SubAgentDeps::model_for`), which may differ
/// from `sub.member.model` (an override, or an inherited orchestrator model).
///
/// Shares [`add_builtin_tools`] with the main agent's build — a subagent
/// gets the **same built-ins and the same MCP tools** from the profile's
/// `tool_env` (session-cwd + bg registry come from the shared
/// `state_manager`), *minus* `delegate` itself (no nested delegation), with
/// the bash env `profile.env ⊕ sub.member.env`. MCP servers are shared with
/// the main agent: stateful servers carry their state across a delegation.
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
    tool_env: &ToolEnv,
    sub: &Subagent,
    model: &ResolvedModel,
    preamble: &str,
    sink: Option<mpsc::UnboundedSender<SourcedEvent>>,
    state_manager: Arc<StateManager>,
    shell_kind: Option<&ShellKind>,
) -> Result<(DynAgent, Arc<SessionHook>)> {
    let config = &model.provider_config;
    let role = sub.member.name.as_str();
    let max_turns = tool_env.max_turns;
    // Sized against THIS model, not the main agent's: subagents routinely
    // run on smaller-context models.
    let context_budget = state_manager
        .compaction_threshold()
        .map(|fraction| (fraction * model.context_size as f64) as usize);
    // Events-only lane-tagged hook. No compaction gate (fresh context) — the
    // sub-agent gate terminates instead of compacting. The pause gate is the
    // session's single one (owned by the StateManager): the hook's
    // checkpoints, the delegate loop's deadline, and the delegate TimeBudget
    // all await the same gate. Ollama never attaches this hook to its agent
    // (hookless by type), so its gate is never awaited — it is not pausable.
    let (wire_provider, wire_model) = wire_label_of(config);
    let hook = SessionHook::new(sink)
        .with_source(crate::ui::app_state::MessageSource::SubAgent {
            role: role.to_string(),
        })
        .with_wire_label(wire_provider, wire_model)
        .with_sub_agent_gate(context_budget)
        .with_pause(state_manager.pause_gate());

    // A subagent never sees `delegate` (no nested delegation). Its `todo`
    // is `TodoTool::default()` — a working *standalone* backend with its
    // own isolated list that drives no panel (the visible panel is a
    // main-agent affordance).
    let bash_env = tool_env.bash_env(&sub.member);

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
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                None,
                false,
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
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                None,
                false,
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
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                anthropic_view_image_budget(c).map(ViewImageTool::new),
                false,
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
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                None,
                false,
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
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                None,
                false,
            );
            Ok((DynAgent::Ollama(builder.build()), Arc::new(hook)))
        }
        #[cfg(feature = "mock")]
        ProviderConfig::Mock => {
            let builder = AgentBuilder::new(MockCompletionModel::replying(crate::mock::MOCK_REPLY))
                .preamble(preamble)
                .default_max_turns(max_turns)
                .hook(hook.clone());
            let builder = add_builtin_tools(
                builder,
                tool_env,
                bash_env,
                None,
                None,
                Some(state_manager),
                shell_kind,
                None,
                false,
            );
            Ok((DynAgent::Mock(builder.build()), Arc::new(hook)))
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

    #[cfg(feature = "host")]
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
            supports_vision: crate::vision::model_supports_vision("anthropic/claude-3.5-sonnet"),
            preserve_reasoning: false,
            display_reasoning: false,
        };
        let vision_no = ProviderInfo {
            name: "openrouter".into(),
            model: "qwen/qwq-32b".into(),
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
        let budgeted = budget_all(tools, &TimeoutsConfig::default(), None);

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

    /// Per-instance-named inner tool that parks at the given gate's
    /// checkpoint when called. `Tool::NAME` is a const, so a per-instance
    /// name needs a hand-rolled `ToolDyn` impl.
    struct ParkingTool {
        name: &'static str,
        gate: Arc<crate::state::PauseGate>,
    }

    impl ToolDyn for ParkingTool {
        fn name(&self) -> String {
            self.name.to_string()
        }

        fn definition(
            &self,
            _prompt: String,
        ) -> rig_core::wasm_compat::WasmBoxedFuture<'_, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: self.name.to_string(),
                    description: "named".to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                }
            })
        }

        fn call(
            &self,
            _args: String,
        ) -> rig_core::wasm_compat::WasmBoxedFuture<'_, Result<String, rig_core::tool::ToolError>>
        {
            let gate = self.gate.clone();
            Box::pin(async move {
                gate.checkpoint().await;
                // Post-park work: under virtual time the resume and a plain
                // timeout's deadline land in the same clock jump, so without
                // this the timeout and the inner's completion would race.
                // With it, the deadline (t+1s) is always behind the inner's
                // completion (t+park+100ms) when the park is long.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok("done".to_string())
            })
        }
    }

    /// Park the gate for one virtual hour, then resume. Blocks on the
    /// `parked` watch (never a yield-spin — that would suppress the virtual
    /// clock advance, see `pause.rs`'s tests for the full explanation).
    fn hour_park_then_resume(gate: Arc<crate::state::PauseGate>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut rx = gate.subscribe_parked();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    return; // gate dropped
                }
            }
            tokio::time::advance(std::time::Duration::from_secs(3600)).await;
            gate.resume();
        })
    }

    /// `budget_all` must make the deadline pause-aware for `delegate` ONLY —
    /// it is the only tool that can park mid-call. Behavioural pin: with a
    /// 1 s budget, an inner future parked in the gate for 1 virtual hour must
    /// survive for `delegate` and must time out for every other tool.
    #[tokio::test(start_paused = true)]
    async fn budget_all_makes_only_delegate_pause_aware() {
        let gate = Arc::new(crate::state::PauseGate::new());
        let cfg = TimeoutsConfig {
            tool_secs: 1,
            delegate_secs: 3_600,
        };

        let tools: Vec<Box<dyn ToolDyn>> = vec![
            Box::new(ParkingTool {
                name: "delegate",
                gate: gate.clone(),
            }),
            // `fetch_page` — a generic-row tool that gets `tool_secs` (1 s)
            // verbatim. (Not `bash`: the shell floor would outlast the park.)
            Box::new(ParkingTool {
                name: "fetch_page",
                gate: gate.clone(),
            }),
        ];
        let budgeted = budget_all(tools, &cfg, Some(gate.clone()));

        // delegate: parked time does not count — the 1-hour park survives.
        gate.request_pause();
        let waiter = hour_park_then_resume(gate.clone());
        let out = budgeted[0].call("{}".to_string()).await;
        assert_eq!(
            out.expect("delegate's budget must not fire while parked"),
            "done",
            "delegate's TimeBudget must exclude parked time"
        );
        waiter.await.expect("waiter task must not panic");

        // fetch_page: plain timeout — the same 1-hour park trips its 1 s budget.
        gate.request_pause();
        let waiter = hour_park_then_resume(gate.clone());
        let out = budgeted[1].call("{}".to_string()).await;
        let s = out.expect("timeout must be Ok(String), not Err(ToolError)");
        assert!(
            s.contains("⏱ TIMEOUT"),
            "non-delegate tools must keep a plain timeout — only delegate can park mid-call; got: {s}"
        );
        waiter.await.expect("waiter task must not panic");
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
        let budgeted = budget_all(tools, &cfg, None);
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

    // ── the consistency pin, tools surface ─────────────────────────────────
    //
    // Companion to the `tools_filter_*` / `skill_filter_*` tests in
    // `config::tests`. The tools surface consumes `NameFilter`
    // through `add_builtin_tools`'s single filter seam:
    // `tools.retain(|t| tools_filter.allows(&t.name()))`. That function is
    // private to this module, so the pin's per-seam proof lives here: the
    // REAL `add_builtin_tools` is driven with the mock model (no network)
    // and the surviving tool set is read back through rig's
    // `ToolServerHandle::get_tool_defs`. The full 8-row table is pinned on
    // the shared predicate in the config module; here we prove the
    // master-switch short-circuit, the allowlist, and the blocklist each
    // reach the real seam.

    /// A `ToolEnv` whose built-in filter is `filter` and which has no
    /// optional extras (no search, no vector store, no MCP servers).
    #[cfg(feature = "mock")]
    fn tool_env_with(filter: &crate::config::NameFilter) -> ToolEnv {
        ToolEnv {
            tools: filter.clone(),
            fetch_url: crate::config::FetchUrlConfig::default(),
            search: None,
            vector: None,
            mcp: crate::tools::env::McpSource::for_test(&[], crate::config::NameFilter::default()),
            base_env: Env::new(),
            timeouts: TimeoutsConfig::default(),
            max_turns: 0,
        }
    }

    /// The real build registers exactly what `builtin_tool_names` plans —
    /// the single source of truth `main_agent_tool_names` projects.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn add_builtin_tools_registers_exactly_the_planned_names() {
        let filter = crate::config::NameFilter {
            disabled: vec!["think".into()],
            ..Default::default()
        };
        for shell in [
            None,
            Some(ShellKind::Bash {
                path: "/bin/sh".to_string(),
            }),
            Some(ShellKind::PowerShell {
                path: "pwsh.exe".to_string(),
            }),
        ] {
            let planned: Vec<String> =
                builtin_tool_names(&tool_env_with(&filter), shell.as_ref(), false)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
            let built = builtin_tool_names_under(&filter, shell).await;
            assert_eq!(built, planned, "the build must register exactly the plan");
        }
    }

    /// Drive the real `add_builtin_tools` filter seam with `filter` and
    /// `shell_kind` and return the wire names of the tools that survive,
    /// in registration order. No optional extras — the built-in core set
    /// only.
    ///
    /// PR2: the helper takes a shell kind because the shell tools are
    /// shell-dependent: `None` registers NEITHER `bash`/`powershell` NOR
    /// bg tool (the bg tool is constructed inside the same
    /// `match shell_kind` arm as the shell tool); `Bash` registers
    /// `bash` + `bash_bg`; `PowerShell` registers `powershell` +
    /// `powershell_bg`.
    #[cfg(feature = "mock")]
    async fn builtin_tool_names_under(
        filter: &crate::config::NameFilter,
        shell_kind: Option<ShellKind>,
    ) -> Vec<String> {
        let model = MockCompletionModel::new();
        let agent = add_builtin_tools(
            AgentBuilder::new(model),
            &tool_env_with(filter),
            Env::new(),
            None, // todo
            None, // delegate
            None, // state manager
            shell_kind.as_ref(),
            None,  // view_image
            false, // wire bash panel
        )
        .build();
        let defs = agent
            .tool_server_handle
            .get_tool_defs(None)
            .await
            .expect("tool defs");
        defs.into_iter().map(|d| d.name).collect()
    }

    /// Drive the real `add_builtin_tools` with `filter` and `shell_kind`
    /// and return the FULL serialized tool definitions (name +
    /// description + parameters) that survive, in registration order.
    /// PR3 companion to [`builtin_tool_names_under`]: the name list is
    /// checked by the registration tests; this one checks everything the
    /// MODEL sees, including descriptions and parameter schemas.
    #[cfg(feature = "mock")]
    async fn builtin_tool_defs_under(
        filter: &crate::config::NameFilter,
        shell_kind: Option<ShellKind>,
    ) -> Vec<serde_json::Value> {
        let model = MockCompletionModel::new();
        let agent = add_builtin_tools(
            AgentBuilder::new(model),
            &tool_env_with(filter),
            Env::new(),
            None, // todo
            None, // delegate
            None, // state manager
            shell_kind.as_ref(),
            None,  // view_image
            false, // wire bash panel
        )
        .build();
        let defs = agent
            .tool_server_handle
            .get_tool_defs(None)
            .await
            .expect("tool defs");
        defs.into_iter()
            .map(|d| serde_json::to_value(&d).expect("ToolDefinition is Serialize"))
            .collect()
    }

    /// PR3 SWEEP: on a PowerShell host, NO built-in tool definition —
    /// name, description, or parameter schema — may contain "bash"
    /// (case-insensitive). This is the machine-checked version of "the
    /// model never sees bash on a PowerShell box": it catches leaks in
    /// any tool's description (file-edit "OVER BASH", the bg tool's
    /// bash-flavoured coaching, parameter schemas, …), not just the
    /// shell tools.
    ///
    /// Note: `view_image` is not registered in this helper (it needs the
    /// Anthropic tool-result channel), so it is out of sweep scope — its
    /// description is checked by the file-level tests if it ever gains a
    /// bash reference.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn builtin_tool_definitions_are_bash_free_under_powershell() {
        let filter = crate::config::NameFilter::default();
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let defs = builtin_tool_defs_under(&filter, Some(ps)).await;
        assert!(
            !defs.is_empty(),
            "sweep needs the built-in core set; got no definitions"
        );
        let offenders: Vec<String> = defs
            .iter()
            .filter(|d| d.to_string().to_lowercase().contains("bash"))
            .map(|d| d["name"].as_str().unwrap_or("?").to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "built-in tool definitions must be bash-free on a PowerShell host; offenders: {offenders:?}"
        );
    }

    /// PR3 companion pin for the sweep: on a Bash host the bg tool is
    /// `bash_bg` — the sweep's inverse. (The Bash host legitimately
    /// contains "bash", so this only checks the expected name is present.)
    #[cfg(feature = "mock")]
    #[cfg(feature = "host")]
    #[tokio::test]
    async fn builtin_tool_definitions_register_bash_bg_under_bash() {
        let filter = crate::config::NameFilter::default();
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let defs = builtin_tool_defs_under(&filter, Some(bash)).await;
        let names: Vec<&str> = defs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(
            names.contains(&"bash_bg"),
            "Bash host must register the bash_bg definition; got {names:?}"
        );
    }

    /// The tools seam honours the locked `NameFilter` semantics:
    /// `enabled: false` drops EVERY built-in tool (the master-switch
    /// short-circuit reaches the real seam); `only` is an allowlist;
    /// `disabled` is a blocklist.
    ///
    /// PR2: driven with a Bash shell kind — with no shell, `bash_bg` is
    /// no longer registered (see `builtin_shell_and_bg_tools_follow_
    /// shell_kind`), so the "rest of the built-ins" assertion below needs
    /// a shell to have a `bash_bg` to keep.
    #[cfg(feature = "mock")]
    #[cfg(feature = "host")]
    #[tokio::test]
    async fn tools_seam_honours_name_filter_enabled_only_and_disabled() {
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        // Master switch: `enabled: false` ⇒ NOTHING survives.
        let off = crate::config::NameFilter {
            enabled: false,
            ..Default::default()
        };
        let names = builtin_tool_names_under(&off, Some(bash.clone())).await;
        assert!(
            names.is_empty(),
            "enabled: false must drop every built-in tool; got {names:?}"
        );

        // Allowlist: exactly the named tool survives.
        let only_think = crate::config::NameFilter {
            disabled: vec![],
            only: vec!["think".into()],
            ..Default::default()
        };
        let names = builtin_tool_names_under(&only_think, Some(bash.clone())).await;
        assert_eq!(
            names,
            vec!["think".to_string()],
            "only: [think] must leave exactly think"
        );

        // Blocklist: everything but the named tool survives.
        let no_think = crate::config::NameFilter {
            disabled: vec!["think".into()],
            only: vec![],
            ..Default::default()
        };
        let names = builtin_tool_names_under(&no_think, Some(bash)).await;
        assert!(
            !names.contains(&"think".to_string()),
            "disabled: [think] must drop think; got {names:?}"
        );
        assert!(
            names.contains(&"file_read".to_string()) && names.contains(&"bash_bg".to_string()),
            "disabled: [think] must keep the rest of the built-ins; got {names:?}"
        );
    }

    /// PR2 (updated for PR3): the shell tools follow the detected shell
    /// kind — and the bg tool with them. The bg tool's wire name is now
    /// shell-derived (`ShellKind::bg_tool_name()`): Bash registers
    /// `bash` + `bash_bg`; PowerShell registers `powershell` +
    /// `powershell_bg`; no shell registers neither shell NOR bg tool (the
    /// bg tool is constructed inside the same `match shell_kind` arm as
    /// the shell tool, so a Windows box with no shell at all gets no
    /// shell surface, not a broken one).
    #[cfg(feature = "mock")]
    #[cfg(feature = "host")]
    #[tokio::test]
    async fn builtin_shell_and_bg_tools_follow_shell_kind() {
        let filter = crate::config::NameFilter::default();

        // No shell → no `bash`, no `powershell`, no `bash_bg`, no `powershell_bg`.
        let names = builtin_tool_names_under(&filter, None).await;
        assert!(
            !names.contains(&"bash".to_string()),
            "no shell ⇒ no `bash` tool; got {names:?}"
        );
        assert!(
            !names.contains(&"powershell".to_string()),
            "no shell ⇒ no `powershell` tool; got {names:?}"
        );
        assert!(
            !names.contains(&"bash_bg".to_string()),
            "no shell ⇒ no `bash_bg` tool (bg tool lives in the shell arm); got {names:?}"
        );
        assert!(
            !names.contains(&"powershell_bg".to_string()),
            "no shell ⇒ no `powershell_bg` tool; got {names:?}"
        );

        // Bash → `bash` + `bash_bg`, no `powershell`, no `powershell_bg`.
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let names = builtin_tool_names_under(&filter, Some(bash)).await;
        assert!(
            names.contains(&"bash".to_string()),
            "Bash shell ⇒ `bash` registered; got {names:?}"
        );
        assert!(
            names.contains(&"bash_bg".to_string()),
            "Bash shell ⇒ `bash_bg` registered; got {names:?}"
        );
        assert!(
            !names.contains(&"powershell".to_string()),
            "Bash shell ⇒ no `powershell` tool; got {names:?}"
        );
        assert!(
            !names.contains(&"powershell_bg".to_string()),
            "Bash shell ⇒ no `powershell_bg` tool; got {names:?}"
        );

        // PowerShell (legacy 5.1 path) → `powershell` + `powershell_bg`,
        // no `bash` and no `bash_bg` (PR3: the bg tool is renamed per shell).
        let ps = ShellKind::PowerShell {
            path: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
        };
        let names = builtin_tool_names_under(&filter, Some(ps)).await;
        assert!(
            names.contains(&"powershell".to_string()),
            "PowerShell shell ⇒ `powershell` registered; got {names:?}"
        );
        assert!(
            names.contains(&"powershell_bg".to_string()),
            "PowerShell shell ⇒ `powershell_bg` registered (PR3: shell-derived name); got {names:?}"
        );
        assert!(
            !names.contains(&"bash".to_string()),
            "PowerShell shell ⇒ no `bash` tool; got {names:?}"
        );
        assert!(
            !names.contains(&"bash_bg".to_string()),
            "PowerShell shell ⇒ no `bash_bg` tool (PR3: renamed to `powershell_bg`); got {names:?}"
        );
    }

    /// PR3: `bash_bg` and `powershell_bg` are SEPARATE config-addressable
    /// names (human decision) — disabling one never disables the other,
    /// and each only addresses the tool its own shell registers.
    #[cfg(feature = "mock")]
    #[cfg(feature = "host")]
    #[tokio::test]
    async fn powershell_bg_and_bash_bg_are_separately_disablable() {
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let no_powershell_bg = crate::config::NameFilter {
            disabled: vec!["powershell_bg".into()],
            ..Default::default()
        };
        let no_bash_bg = crate::config::NameFilter {
            disabled: vec!["bash_bg".into()],
            ..Default::default()
        };

        // PowerShell host + `disabled: [powershell_bg]` → the bg tool is
        // gone, the shell tool survives.
        let names = builtin_tool_names_under(&no_powershell_bg, Some(ps.clone())).await;
        assert!(
            !names.contains(&"powershell_bg".to_string()),
            "disabled: [powershell_bg] must drop powershell_bg on a PowerShell host; got {names:?}"
        );
        assert!(
            names.contains(&"powershell".to_string()),
            "disabling powershell_bg must NOT disable the powershell shell tool; got {names:?}"
        );

        // PowerShell host + `disabled: [bash_bg]` → the SEPARATE name
        // addresses nothing here: powershell_bg stays registered.
        let names = builtin_tool_names_under(&no_bash_bg, Some(ps.clone())).await;
        assert!(
            names.contains(&"powershell_bg".to_string()),
            "disabled: [bash_bg] must NOT drop powershell_bg (separate names); got {names:?}"
        );
        assert!(
            names.contains(&"powershell".to_string()),
            "disabled: [bash_bg] must not affect the powershell shell tool; got {names:?}"
        );

        // Bash host + `disabled: [bash_bg]` → bash_bg is gone, bash stays.
        let names = builtin_tool_names_under(&no_bash_bg, Some(bash.clone())).await;
        assert!(
            !names.contains(&"bash_bg".to_string()),
            "disabled: [bash_bg] must drop bash_bg on a Bash host; got {names:?}"
        );
        assert!(
            names.contains(&"bash".to_string()),
            "disabling bash_bg must NOT disable the bash shell tool; got {names:?}"
        );

        // Bash host + `disabled: [powershell_bg]` → addresses nothing:
        // bash_bg stays registered.
        let names = builtin_tool_names_under(&no_powershell_bg, Some(bash)).await;
        assert!(
            names.contains(&"bash_bg".to_string()),
            "disabled: [powershell_bg] must NOT drop bash_bg (separate names); got {names:?}"
        );
    }
}

// ─── Local edition file tools (mobile-attach part 1, RED) ────────────────
//
// TDD tests written BEFORE the implementation; Local + mock builds only.
// The contract (design part 1): the Local edition registers the EXISTING
// six file tools — `file_create`, `file_str_replace`, `file_insert`,
// `file_read`, `pdf_read`, `list_directory` — confined to the
// conversation's upload folder, while bash and doc tools stay absent.
//
// They drive the REAL rig seam (`add_builtin_tools` +
// `tool_server_handle.call_tool`) and name no tool constructors, so they
// COMPILE before the tools are un-gated in Local: today
// `call_tool("file_read")` fails at runtime with "tool not found" — the
// right RED (feature absent, not a typo). The design's test plan puts the
// first two in `src/state/state_manager.rs`; they live here instead
// because `add_builtin_tools` (the registration seam under test) is
// private to this module.

#[cfg(all(test, not(feature = "host"), feature = "mock"))]
mod local_file_tools_tests {
    use super::*;
    use crate::attachments::{AttachmentKind, UploadStore, model_note};
    use crate::config::{NameFilter, UploadsConfig};

    const FILE_TOOLS: [&str; 6] = [
        "file_create",
        "file_str_replace",
        "file_insert",
        "file_read",
        "pdf_read",
        "list_directory",
    ];

    /// A `ToolEnv` with no optional extras (no search, no vector store,
    /// no MCP servers) — the Local edition has no vector store at all.
    fn local_tool_env() -> ToolEnv {
        ToolEnv {
            tools: NameFilter::default(),
            fetch_url: crate::config::FetchUrlConfig::default(),
            search: None,
            vector: None,
            mcp: crate::tools::env::McpSource::for_test(&[], NameFilter::default()),
            base_env: Env::new(),
            timeouts: TimeoutsConfig::default(),
            max_turns: 0,
        }
    }

    type LocalAgent = rig_core::agent::Agent<MockCompletionModel, ()>;

    /// The real Local build: `add_builtin_tools` with the given env and
    /// state manager (the confinement root is read from it per call).
    fn local_agent(tool_env: &ToolEnv, sm: Option<Arc<StateManager>>) -> LocalAgent {
        add_builtin_tools(
            AgentBuilder::new(MockCompletionModel::new()),
            tool_env,
            Env::new(),
            None, // todo
            None, // delegate
            sm,
            None,  // shell kind
            None,  // view_image
            false, // wire bash panel
        )
        .build()
    }

    async fn tool_names(handle: &rig_core::tool::server::ToolServerHandle) -> Vec<String> {
        handle
            .get_tool_defs(None)
            .await
            .expect("tool defs")
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    #[tokio::test]
    async fn local_build_registers_the_six_file_tools() {
        let env = local_tool_env();
        let agent = local_agent(&env, None);
        let names = tool_names(&agent.tool_server_handle).await;
        for tool in FILE_TOOLS {
            assert!(
                names.iter().any(|n| n == tool),
                "the Local build must register `{tool}`; registered: {names:?}"
            );
        }
        // The five existing Local built-ins stay.
        for tool in ["fetch_url", "fetch_page", "todo", "think"] {
            assert!(
                names.iter().any(|n| n == tool),
                "the Local build keeps `{tool}`; registered: {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn local_build_tools_only_filter_drops_the_file_tools() {
        let mut env = local_tool_env();
        env.tools = NameFilter {
            only: vec!["think".into()],
            ..Default::default()
        };
        let agent = local_agent(&env, None);
        let names = tool_names(&agent.tool_server_handle).await;
        for tool in FILE_TOOLS {
            assert!(
                !names.iter().any(|n| n == tool),
                "`tools: {{ only: [think] }}` must drop `{tool}`; registered: {names:?}"
            );
        }
        assert!(
            names.iter().any(|n| n == "think"),
            "the allowlisted tool stays; registered: {names:?}"
        );
    }

    #[tokio::test]
    async fn local_build_keeps_shell_and_doc_tools_absent() {
        let env = local_tool_env();
        let agent = local_agent(&env, None);
        let names = tool_names(&agent.tool_server_handle).await;
        for tool in [
            "bash",
            "bash_bg",
            "powershell",
            "powershell_bg",
            "doc_index",
            "doc_search",
            "view_image",
        ] {
            assert!(
                !names.iter().any(|n| n == tool),
                "the Local build must NOT register `{tool}`; registered: {names:?}"
            );
        }
    }

    /// The model-facing contract end to end: the path printed by
    /// `attachments::model_note` for a stored NON-IMAGE attachment is
    /// readable by the Local build's `file_read`.
    #[tokio::test]
    async fn local_file_read_reads_a_model_note_path_of_a_stored_attachment() {
        let root = tempfile::tempdir().unwrap();
        let store = UploadStore::new(root.path().to_path_buf(), UploadsConfig::default());
        let sm = StateManager::new_arc_for_session(None, store.clone());
        let env = local_tool_env();
        let agent = local_agent(&env, Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        sm.create_conversation("c".into(), "p".into(), "m".into(), String::new());
        let convo = sm.get_current_conversation_id().expect("minted");
        let att = store
            .store(convo, "invoice.txt", &b"invoice contents"[..])
            .await
            .expect("store the attachment");
        assert_eq!(
            att.kind,
            AttachmentKind::File,
            "the fixture must be a non-image attachment"
        );

        // The absolute path the model is told to read.
        let note = model_note(&store, std::slice::from_ref(&att));
        let path = note
            .lines()
            .nth(1)
            .expect("one attachment line")
            .rsplit_once(": ")
            .expect("path after ': '")
            .1
            .to_string();
        assert!(
            path.starts_with(root.path().to_string_lossy().as_ref()),
            "model_note points into the store: {path}"
        );

        let out = handle
            .call_tool(
                "file_read",
                &format!(
                    "{{\"path\":{}}}",
                    serde_json::to_string(&path).expect("json")
                ),
            )
            .await
            .expect("the Local build registers file_read and reads the model_note path");
        assert!(
            out.contains("invoice contents"),
            "file_read must return the attachment content, got: {out}"
        );
    }

    /// Confinement root is read PER CALL: a conversation minted AFTER the
    /// tools were built (the `/new` + `/model` rebuild shape) is the one
    /// the tools resolve against.
    #[tokio::test]
    async fn a_conversation_minted_after_the_tools_were_built_is_the_one_resolved_against() {
        let root = tempfile::tempdir().unwrap();
        let store = UploadStore::new(root.path().to_path_buf(), UploadsConfig::default());
        let sm = StateManager::new_arc_for_session(None, store);
        let env = local_tool_env();
        // The tools are built while NO conversation exists…
        let agent = local_agent(&env, Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();
        assert!(sm.get_current_conversation_id().is_none(), "fixture");

        // …and only then is the conversation minted.
        sm.create_conversation("c".into(), "p".into(), "m".into(), String::new());
        let convo = sm.get_current_conversation_id().expect("minted");
        let work = root.path().join(convo.to_string()).join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("late.txt"), "late file").unwrap();

        let out = handle
            .call_tool("file_read", r#"{"path":"late.txt"}"#)
            .await
            .expect("a relative path must resolve against the post-build conversation");
        assert!(
            out.contains("late file"),
            "the post-build conversation must be the one resolved against, got: {out}"
        );
    }

    /// Main agent and sub-agent share the confinement root: a file the
    /// main agent `file_create`s is the one the sub-agent's `file_read`
    /// sees (design part 1 §5: `build_sub_agent` passes the same
    /// `StateManager`).
    #[tokio::test]
    async fn main_agent_file_create_is_visible_to_the_sub_agent_file_read() {
        let root = tempfile::tempdir().unwrap();
        let store = UploadStore::new(root.path().to_path_buf(), UploadsConfig::default());
        let sm = StateManager::new_arc_for_session(None, store);
        sm.create_conversation("c".into(), "p".into(), "m".into(), String::new());

        let model = ResolvedModel {
            alias: "mock".to_string(),
            model_name: "mock-model".to_string(),
            provider_name: "mock".to_string(),
            provider_kind: crate::config::ProviderType::Mock,
            provider_config: crate::config::ProviderConfig::Mock,
            context_size: 100_000,
        };
        let env = local_tool_env();
        let member = Member {
            name: "main".to_string(),
            model: model.clone(),
            skills: NameFilter::default(),
            agents_md: false,
            env: Default::default(),
        };
        let (main, _info, _events, _hook) =
            create_provider(&model, "p", &env, &member, None, None, sm.clone(), None)
                .expect("main agent builds");
        let sub = Subagent {
            member: Member {
                name: "worker".to_string(),
                model: model.clone(),
                skills: NameFilter::default(),
                agents_md: false,
                env: Default::default(),
            },
            prompt: "you are a worker".to_string(),
        };
        let (sub_agent, _hook) = build_sub_agent(&env, &sub, &model, "p", None, sm.clone(), None)
            .expect("sub agent builds");

        let main_handle = match &main {
            DynAgent::Mock(a) => a.tool_server_handle.clone(),
            _ => panic!("expected the mock main agent"),
        };
        let sub_handle = match &sub_agent {
            DynAgent::Mock(a) => a.tool_server_handle.clone(),
            _ => panic!("expected the mock sub agent"),
        };

        let names = tool_names(&main_handle).await;
        assert!(
            names.iter().any(|n| n == "file_create"),
            "the main agent must have file_create; registered: {names:?}"
        );
        main_handle
            .call_tool("file_create", r#"{"path":"a.txt","file_text":"shared"}"#)
            .await
            .expect("main agent file_create");

        let out = sub_handle
            .call_tool("file_read", r#"{"path":"a.txt"}"#)
            .await
            .expect("the sub agent's file_read must see the main agent's file (same root)");
        assert!(out.contains("shared"), "got: {out}");
        let _ = root;
    }
    // ─── End-to-end confinement: real tools through the tool server ──────
    //
    // Refusals surface as `Err(ToolServerError)` from `call_tool` (rig maps
    // a tool's `Err` into `ToolSetError::ToolCallError`); the message the
    // model sees is carried inside that error's Display.

    /// A fresh uploads root + state manager with one minted conversation.
    fn confinement_layout() -> (
        tempfile::TempDir,
        crate::attachments::UploadStore,
        Arc<StateManager>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let store = UploadStore::new(root.path().to_path_buf(), UploadsConfig::default());
        let sm = StateManager::new_arc_for_session(None, store.clone());
        sm.create_conversation("c".into(), "p".into(), "m".into(), String::new());
        (root, store, sm)
    }

    /// The absolute attachment path the attached-files note prints.
    fn attachment_path(store: &UploadStore, att: &crate::attachments::Attachment) -> String {
        let note = model_note(store, std::slice::from_ref(att));
        note.lines()
            .nth(1)
            .expect("one attachment line")
            .rsplit_once(": ")
            .expect("path after ': '")
            .1
            .to_string()
    }

    /// The file `store` puts `name` at inside conversation `convo`.
    fn stored_file(
        root: &tempfile::TempDir,
        convo: &str,
        att: &crate::attachments::Attachment,
    ) -> std::path::PathBuf {
        root.path()
            .join(convo)
            .join(att.id.to_string())
            .join(&att.name)
    }

    /// `file_create` with `../../escape.txt`, an absolute path outside the
    /// uploads root, and `../<upload-id>/new.txt` beside an attachment are
    /// each refused with the locked outside message, and nothing is created
    /// at the target.
    #[tokio::test]
    async fn file_create_refuses_every_escape_and_writes_nothing() {
        let (root, store, sm) = confinement_layout();
        let convo = sm.get_current_conversation_id().expect("minted");
        let convo_s = convo.to_string();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        let att = store
            .store(convo, "doc.txt", &b"attachment"[..])
            .await
            .expect("store");
        let beside = root
            .path()
            .join(&convo_s)
            .join(att.id.to_string())
            .join("new.txt");

        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("escape-abs.txt");

        // (path, fragment of the refusal the model must see). Beside an
        // attachment is inside the read boundary, so it is read-only.
        for (raw, want) in [
            (
                "../../escape.txt".to_string(),
                "is outside this conversation's folder",
            ),
            (
                outside_file.to_str().unwrap().to_string(),
                "is outside this conversation's folder",
            ),
            (
                format!("../{}/new.txt", att.id),
                "is read-only: attached files cannot be changed",
            ),
        ] {
            let args = format!(
                "{{\"path\":{},\"file_text\":\"pwned\"}}",
                serde_json::to_string(&raw).expect("json")
            );
            let err = handle
                .call_tool("file_create", &args)
                .await
                .expect_err("the escape must be refused")
                .to_string();
            assert!(
                err.contains(want),
                "file_create({raw:?}) must be refused with `{want}`, got: {err}"
            );
        }
        assert!(
            !root.path().join("escape.txt").exists(),
            "no file at the uploads root"
        );
        assert!(!outside_file.exists(), "no file outside the uploads root");
        assert!(!beside.exists(), "no file beside the attachment");
        let _ = root;
    }

    /// `file_str_replace` and `file_insert` on an attachment's absolute
    /// path are refused as read-only, and the attachment's bytes are
    /// unchanged.
    #[tokio::test]
    async fn attachment_is_read_only_for_the_edit_tools() {
        let (root, store, sm) = confinement_layout();
        let convo = sm.get_current_conversation_id().expect("minted");
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        let att = store
            .store(convo, "note.txt", &b"original bytes"[..])
            .await
            .expect("store");
        let path = attachment_path(&store, &att);
        let file = stored_file(&root, &convo.to_string(), &att);
        assert_eq!(std::fs::read(&file).unwrap(), b"original bytes", "fixture");

        let args = format!(
            "{{\"path\":{},\"old_str\":\"original\",\"new_str\":\"tampered\"}}",
            serde_json::to_string(&path).expect("json")
        );
        let err = handle
            .call_tool("file_str_replace", &args)
            .await
            .expect_err("editing an attachment must be refused")
            .to_string();
        assert!(
            err.contains("is read-only: attached files cannot be changed"),
            "file_str_replace on the attachment must be the read-only refusal, got: {err}"
        );

        let args = format!(
            "{{\"path\":{},\"insert_line\":0,\"insert_text\":\"tampered\"}}",
            serde_json::to_string(&path).expect("json")
        );
        let err = handle
            .call_tool("file_insert", &args)
            .await
            .expect_err("editing an attachment must be refused")
            .to_string();
        assert!(
            err.contains("is read-only: attached files cannot be changed"),
            "file_insert on the attachment must be the read-only refusal, got: {err}"
        );

        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"original bytes",
            "the attachment's bytes must be unchanged"
        );
        let _ = root;
    }

    /// `file_read` and `list_directory` on another conversation's folder
    /// (relative and absolute) and on a real outside file are refused, and
    /// none of the outside content appears in the result.
    #[tokio::test]
    async fn other_conversation_and_real_files_are_unreadable() {
        let (root, _store, sm) = confinement_layout();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        // The conversation minted by the fixture becomes the OTHER one (it
        // holds a secret); a second one is minted and is the current one.
        let other = sm
            .get_current_conversation_id()
            .expect("minted")
            .to_string();
        sm.create_conversation("current".into(), "p".into(), "m".into(), String::new());
        assert_ne!(
            sm.get_current_conversation_id()
                .expect("minted")
                .to_string(),
            other,
            "fixture: the current conversation is a different one"
        );
        let other_file = root.path().join(&other).join("work").join("secret.txt");
        std::fs::create_dir_all(other_file.parent().unwrap()).unwrap();
        std::fs::write(&other_file, "their-convo-secret").unwrap();

        // A real file outside the uploads root, with distinctive content.
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("hostname-like.txt");
        std::fs::write(&outside_file, "real-outside-content").unwrap();

        let want = "is outside this conversation's folder";
        for raw in [
            format!("../../{other}/work/secret.txt"),
            other_file.to_str().unwrap().to_string(),
            format!("../../{other}"),
            root.path().join(&other).to_str().unwrap().to_string(),
            outside_file.to_str().unwrap().to_string(),
            "/etc/hostname".to_string(),
            "/etc".to_string(),
        ] {
            let args = format!(
                "{{\"path\":{}}}",
                serde_json::to_string(&raw).expect("json")
            );
            for tool in ["file_read", "list_directory"] {
                let msg = match handle.call_tool(tool, &args).await {
                    Ok(body) => panic!("{tool}({raw:?}) must be refused, got: {body}"),
                    Err(e) => e.to_string(),
                };
                assert!(
                    msg.contains(want),
                    "{tool}({raw:?}) must be the outside refusal, got: {msg}"
                );
                for leaked in ["their-convo-secret", "real-outside-content"] {
                    assert!(
                        !msg.contains(leaked),
                        "{tool}({raw:?}) leaked `{leaked}`: {msg}"
                    );
                }
            }
        }
        let _ = root;
    }

    /// The tool server reports a refusal as `Err`, and the exact
    /// model-facing message is carried inside it.
    #[tokio::test]
    async fn refusal_surfaces_as_an_error_carrying_the_locked_message() {
        let (_root, _store, sm) = confinement_layout();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();
        let raw = "../../escape.txt";
        let err = handle
            .call_tool(
                "file_read",
                &format!("{{\"path\":{}}}", serde_json::to_string(raw).unwrap()),
            )
            .await
            .expect_err("a refusal is an error, not a result string")
            .to_string();
        let expected = format!(
            "Path '{raw}' is outside this conversation's folder. Use a relative path, or a path from the attached-files list."
        );
        assert!(
            err.contains(&expected),
            "the exact outside message must reach the caller, got: {err}"
        );
    }

    /// Positive cases: `file_create notes.md` then `file_read notes.md`
    /// round-trips; `list_directory .` shows it.
    #[tokio::test]
    async fn work_folder_round_trips_create_read_and_list() {
        let (_root, _store, sm) = confinement_layout();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        handle
            .call_tool(
                "file_create",
                r#"{"path":"notes.md","file_text":"round-trip content"}"#,
            )
            .await
            .expect("creating in work/ must succeed");

        let out = handle
            .call_tool("file_read", r#"{"path":"notes.md"}"#)
            .await
            .expect("reading the created file must succeed");
        assert!(
            out.contains("round-trip content"),
            "file_read must return the created content, got: {out}"
        );

        let out = handle
            .call_tool("list_directory", r#"{"path":"."}"#)
            .await
            .expect("listing work/ must succeed");
        assert!(
            out.contains("notes.md"),
            "list_directory . must show notes.md, got: {out}"
        );
    }

    /// A stored attachment read by the absolute path that
    /// `attachments::model_note` prints returns its content, and a stored
    /// PDF read with `pdf_read` returns its text.
    #[tokio::test]
    async fn stored_attachments_are_readable_by_their_model_note_paths() {
        let (_root, store, sm) = confinement_layout();
        let convo = sm.get_current_conversation_id().expect("minted");
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        let txt = store
            .store(convo, "plain.txt", &b"attachment body 123"[..])
            .await
            .expect("store");
        let path = attachment_path(&store, &txt);
        let out = handle
            .call_tool(
                "file_read",
                &format!("{{\"path\":{}}}", serde_json::to_string(&path).unwrap()),
            )
            .await
            .expect("reading a stored attachment must succeed");
        assert!(
            out.contains("attachment body 123"),
            "file_read must return the attachment content, got: {out}"
        );

        let pdf = store
            .store(convo, "hello.pdf", HELLO_PDF)
            .await
            .expect("store");
        let path = attachment_path(&store, &pdf);
        let out = handle
            .call_tool(
                "pdf_read",
                &format!("{{\"path\":{}}}", serde_json::to_string(&path).unwrap()),
            )
            .await
            .expect("pdf_read of a stored PDF must succeed");
        assert!(
            out.contains("Hello PDF"),
            "pdf_read must return the PDF text, got: {out}"
        );
    }

    /// A minimal valid single-page PDF whose only text is "Hello PDF" —
    /// the same hand-built fixture the `pdf_read` unit tests use.
    const HELLO_PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n\
4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n\
5 0 obj\n<< /Length 40 >>\nstream\nBT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET\nendstream\nendobj\n\
xref\n0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
0000000241 00000 n \n\
0000000311 00000 n \n\
trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n401\n%%EOF\n";

    /// A symlink planted in `work/` pointing outside the uploads root:
    /// `file_read` through it is refused, and `file_create` through it is
    /// refused with the outside target never created.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_in_work_pointing_outside_is_refused() {
        let (root, _store, sm) = confinement_layout();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("leak.txt");
        std::fs::write(&outside_file, "outside secret").unwrap();

        let convo = sm
            .get_current_conversation_id()
            .expect("minted")
            .to_string();
        let work = root.path().join(&convo).join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&outside_file, work.join("link")).unwrap();

        let err = handle
            .call_tool("file_read", r#"{"path":"link"}"#)
            .await
            .expect_err("reading through the symlink must be refused")
            .to_string();
        assert!(
            err.contains("is outside this conversation's folder"),
            "file_read through the symlink must be the outside refusal, got: {err}"
        );
        assert!(
            !err.contains("outside secret"),
            "the refusal must not leak the target content: {err}"
        );

        let err = handle
            .call_tool(
                "file_create",
                r#"{"path":"link/new.txt","file_text":"pwned"}"#,
            )
            .await
            .expect_err("creating through the symlink must be refused")
            .to_string();
        assert!(
            err.contains("is outside this conversation's folder"),
            "file_create through the symlink must be the outside refusal, got: {err}"
        );
        assert!(
            !outside_file.join("new.txt").exists(),
            "no file created outside"
        );
        let _ = (root, outside);
    }

    /// A symlinked DIRECTORY in `work/` pointing outside: reading and
    /// listing through it are refused (plain and recursive), the recursive
    /// listing of `.` never descends into it, and an in-place edit through
    /// a symlink to an outside FILE leaves that file's bytes unchanged.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_dir_and_file_in_work_never_expose_or_change_outside_content() {
        let (root, _store, sm) = confinement_layout();
        let convo = sm
            .get_current_conversation_id()
            .expect("minted")
            .to_string();
        let agent = local_agent(&local_tool_env(), Some(sm.clone()));
        let handle = agent.tool_server_handle.clone();

        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("deep")).unwrap();
        std::fs::write(
            outside.path().join("deep").join("leaked-name.txt"),
            "dir-secret",
        )
        .unwrap();
        let outside_file = outside.path().join("edit-me.txt");
        std::fs::write(&outside_file, "line one\n").unwrap();

        let work = root.path().join(&convo).join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(outside.path(), work.join("dirlink")).unwrap();
        std::os::unix::fs::symlink(&outside_file, work.join("filelink")).unwrap();
        std::fs::write(work.join("inside.txt"), "inside").unwrap();

        let want = "is outside this conversation's folder";
        for (tool, args) in [
            ("list_directory", r#"{"path":"dirlink"}"#),
            ("list_directory", r#"{"path":"dirlink","recursive":true}"#),
            ("file_read", r#"{"path":"dirlink/deep/leaked-name.txt"}"#),
            ("file_read", r#"{"path":"filelink"}"#),
            (
                "file_create",
                r#"{"path":"dirlink/deep/new.txt","file_text":"x"}"#,
            ),
            (
                "file_str_replace",
                r#"{"path":"filelink","old_str":"one","new_str":"PWNED"}"#,
            ),
            (
                "file_insert",
                r#"{"path":"filelink","insert_line":0,"insert_text":"PWNED"}"#,
            ),
        ] {
            let msg = match handle.call_tool(tool, args).await {
                Ok(body) => panic!("{tool} {args} must be refused, got: {body}"),
                Err(e) => e.to_string(),
            };
            assert!(
                msg.contains(want),
                "{tool} {args}: expected outside refusal, got: {msg}"
            );
            assert!(
                !msg.contains("dir-secret"),
                "{tool} {args} leaked content: {msg}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&outside_file).unwrap(),
            "line one\n"
        );
        assert!(!outside.path().join("deep").join("new.txt").exists());

        // Recursive listing of `.` must not walk through the dir symlink.
        let listing = handle
            .call_tool("list_directory", r#"{"path":".","recursive":true}"#)
            .await
            .expect("listing work/ must succeed");
        assert!(listing.contains("inside.txt"), "got: {listing}");
        assert!(
            !listing.contains("leaked-name.txt") && !listing.contains("edit-me.txt"),
            "recursive list_directory followed a symlink out of the folder: {listing}"
        );
    }
}
