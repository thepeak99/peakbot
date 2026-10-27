//! PeakBot library - Core functionality for connecting to MCP servers and managing tools.

/// PeakBot binary version, baked in at compile time. The single source of truth
/// for "what is this?" — the TUI welcome banner, the system prompt the model
/// sees, and the `WelcomeState` payload sent over the WebSocket wire all read
/// from this one constant. Adding a duplicate `env!("CARGO_PKG_VERSION")`
/// literal anywhere else in the crate is a regression: any of the three sites
/// could drift from the binary the user actually runs.
pub const PEAKBOT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod attachments;
pub mod bg_processes;
pub mod config;
mod context_manager;
mod conversation;
mod conversation_manager;
mod conversation_title;
mod hooks;
pub mod http;
pub mod image_cache;
pub mod install;
mod mcp_auth;
mod memory_compaction;
#[cfg(feature = "mock")]
pub mod mock;
pub mod pipeline;
mod providers;
pub mod pty_runner;
pub mod reasoning;
pub mod selection;
pub mod session;
pub mod skills;
pub mod sniff;
pub mod state;
pub mod storage;
pub mod test_runner;
mod tool_use_validator;
pub mod tools;
pub mod ui;
pub mod utils;
pub mod vector;
pub mod vision;

pub use config::{
    AnthropicConfig, Bound, Config, ContextConfig, ConversationConfig, McpServerConfig,
    McpTransportType, ModelEntry, ModelRegistry, OllamaConfig, OpenRouterConfig, ProviderConfig,
    ProviderEntry, ProviderType, RegistryError, RepoMode, ResolvedModel, RetryConfig, SearchConfig,
    VectorDbConfig, get_config_file_path,
};
use context_manager::ContextManager;
pub use context_manager::{CompactionResult, auto_detect_context_size};
pub use conversation::{
    Conversation, ConversationMetadata, ConversationSummary, Message as ConversationMessage,
};
pub use conversation_manager::{ConversationManager, ConversationManagerConfig};
pub use hooks::{
    AgentEvent, ModelPricing, SessionHook, SessionStats, SourcedEvent, TokenUsage,
    fetch_model_pricing,
};
pub use pipeline::{DelegateTool, SubAgentDeps};
#[cfg(feature = "mock")]
pub use providers::create_mock_agent;
pub use providers::{
    CompactionModel, DynAgent, ProviderInfo, build_anthropic_session_hook, create_compaction_model,
    create_provider,
};

use rig_core::completion::{Message, PromptError};
use rig_core::tool::ToolDyn;
use rig_core::tool::rmcp::McpTool;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{TokioChildProcess, streamable_http_client::StreamableHttpClientTransport};
pub use session::{Session, SessionDeps, create_session};
pub use skills::{SkillRegistry, load_default_skills};
pub use state::StateManager;
pub use storage::{ConversationStorage, FileStorage};
pub use test_runner::{CompactionInfo, TestRunner};
pub use tools::{
    BashTool, FetchPageTool, FetchUrlTool, FileCreateTool, FileInsertTool, FileReadTool,
    FileStrReplaceTool, ListDirectoryTool, PowerShellTool, SearchTool, ShellKind, ThinkTool,
    TodoArgs, TodoItem, TodoStatus, TodoTool, print_no_shell_warning,
};
pub use ui::{Ui, UiAction};

use anyhow::{Context, Result, anyhow};
use futures::FutureExt;
use rmcp::service::{RoleClient, RunningService, ServiceExt};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

/// Message types for internal queue between event loop and agent loop.
///
/// **Single-writer invariant** (see `make-flow-great-again.md`):
/// `UserMessage` carries the buffered text + attachments forward through
/// the channel. The `add_user_message{,_with_attachments}` write into
/// `StateManager` happens in the **agent loop** at dequeue time — never in
/// the event loop. This is what guarantees user-typed text only ever
/// lands between agent turns, never inside one (specifically, never
/// between an in-flight `ToolCall` and its `ToolResult`).
///
/// **Exception:** `run_bg_synthetic_turn_if_any` →
/// `add_user_message_from_background` is a second writer that bypasses
/// this invariant by injecting a `[bg output]` user message directly into
/// the transcript. This is the seam where a bg message can wedge between
/// a `ToolCall` and its `ToolResult`.
enum QueueMessage {
    UserMessage {
        text: String,
        attachments: Vec<crate::attachments::Attachment>,
    },
    Command(String),
    /// Stop was requested. The message to post lives in the controller's
    /// `pending_stop` cell, so an escalating second press can rewrite it
    /// before the agent loop reaches this marker (#183: one message per stop).
    StopMarker,
    /// Switch the active model and restart on a fresh conversation.
    /// Carries the validated alias (validation happens in the View
    /// before the action is sent). The agent loop dequeues this between
    /// turns and runs `rebuild_agent_for_alias`.
    SwitchModel(String),
    /// Change the working directory and restart on a fresh conversation.
    /// Carries the validated, canonicalised path (validation happens in
    /// the View before the action is sent). Dequeued between turns and
    /// handled by `handle_change_cwd`. Same rebuild seam as SwitchModel,
    /// different axis. See ticket #124.
    ChangeCwd(String),
    /// `/profile`, `/agent` or a picker selection. Dequeued between turns and
    /// handled by `handle_select`: lists are rendered, a selection is
    /// refused once the conversation has turns (locked) or under a pin, else
    /// the bound is swapped, the agent rebuilt and the selection persisted.
    Select(SelectRequest),
    /// One or more `bash_bg` processes have output ready. Payload is
    /// empty — the agent loop drains every bg buffer in one pass.
    /// Multiple notifications coalesce naturally (the drain returns
    /// `None` if buffers were already cleared). Pushed by per-process
    /// reader threads (debounced inside the reader) and consumed
    /// between turns. See `bash-background.md` § "Wiring into the agent loop".
    BackgroundOutputReady,
}

/// How should a submitted input buffer be routed by the event loop?
///
/// The REPL emits every Enter-submission as `UiAction::SendMessage { text, .. }` —
/// the popup path and the plain-typed path go through the same action.
/// `classify_submission` is the single chokepoint that decides whether the
/// text is a slash command (dispatched internally) or a user turn (sent to
/// the LLM).
///
/// Regression guard: before this existed, every slash command (`/new`,
/// `/help`, …) was appended as a user message and billed as an LLM turn.
/// See `allehailmenu.md`.
#[derive(Debug)]
enum SubmitKind {
    /// `/stop` — interrupts the running agent instead of queueing.
    StopCommand,
    /// `/pause` — asks the running sub-agent to park at its next
    /// checkpoint. Immediate like `/stop` (works mid-turn); no-op with a
    /// short system note when no sub-agent is running.
    PauseCommand,
    /// `/resume` — wakes a paused (or pausing) sub-agent. Immediate like
    /// `/stop` (works mid-turn); no-op with a short system note when no
    /// sub-agent is running.
    ResumeCommand,
    /// Any other `/xxx` — routed to `process_command_internal`.
    Command(String),
    /// Plain chat content — sent to the LLM. `image_tokens` are the raw
    /// `[img:…]` token bodies (grammar-checked only; `attachments::collect`
    /// resolves them at enqueue).
    UserMessage {
        text: String,
        image_tokens: Vec<String>,
    },
    /// `[img:…]` grammar error (`data:` URI, unknown scheme, bare word).
    /// Surfaces as a system error; does not reach the LLM.
    InvalidAttachment(crate::attachments::AttachError),
    /// `/profile [name]` or `/agent [name]` — a per-conversation selection,
    /// not an LLM turn.
    Select(crate::selection::SelectSubmission),
    /// `/model <alias>` — switch the orchestrator model (new conversation).
    SwitchModel(String),
}

fn classify_submission(msg: &str) -> SubmitKind {
    let trimmed = msg.trim();
    if trimmed == "/stop" {
        return SubmitKind::StopCommand;
    }
    // `/pause` / `/resume` — sub-agent pause controls. Like `/stop` they
    // are handled immediately in the event loop (not queued behind the
    // running turn), so they work mid-delegation.
    if trimmed == "/pause" {
        return SubmitKind::PauseCommand;
    }
    if trimmed == "/resume" {
        return SubmitKind::ResumeCommand;
    }
    // `/profile` and `/agent` are per-conversation selections, not LLM turns.
    if let Some(sub) = crate::selection::classify_select(trimmed) {
        return SubmitKind::Select(sub);
    }
    // `/model <alias>` switches the model (web/stdio parity with the TUI,
    // which intercepts it before sending). Bare `/model` and `/models` stay
    // commands.
    if let Some(rest) = trimmed.strip_prefix("/model")
        && rest.starts_with(char::is_whitespace)
        && !rest.trim().is_empty()
    {
        return SubmitKind::SwitchModel(rest.trim().to_string());
    }
    // Preserve whitespace on commands — leading whitespace in "/ quit"
    // should not be stripped before the command handler sees it.
    if msg.starts_with('/') {
        return SubmitKind::Command(msg.to_string());
    }
    // Grammar only — no I/O. Tokens are resolved (and copied into the
    // store) by `attachments::collect` at enqueue.
    match crate::attachments::parse_image_tokens(msg) {
        Ok((text, image_tokens)) if !image_tokens.is_empty() => SubmitKind::UserMessage {
            text: text.trim().to_string(),
            image_tokens,
        },
        Ok(_) => SubmitKind::UserMessage {
            text: msg.to_string(),
            image_tokens: Vec::new(),
        },
        Err(e) => SubmitKind::InvalidAttachment(e),
    }
}

/// Render a [`crate::state::StopTally`] into the system-message string the
/// drain arm prints. Contract (byte-exact):
///
/// | `StopTally`  | Output                                                |
/// |--------------|-------------------------------------------------------|
/// | `{ shell }`  | `Agent stopped by user (killed 1 shell process)`      |
/// | `default()`  | `Agent stopped by user`                               |
///
/// The shell clause is omitted when no foreground shell was running, so an
/// idle stop reads exactly as it did before #183. Pure function so the T4
/// test pins it byte-exact and so neither stop arm can disagree on wording.
fn stop_message(t: crate::state::StopTally) -> String {
    if t.shell {
        "Agent stopped by user (killed 1 shell process)".to_string()
    } else {
        "Agent stopped by user".to_string()
    }
}

/// System message for a graceful (first-press) Stop that was not escalated.
/// Wire-only user message sent once when the final reply came back blank.
/// Never persisted to the transcript.
const EMPTY_REPLY_NUDGE: &str =
    "Your last reply was empty. Reply to the user now with your complete answer as plain text.";
/// System row shown when the model stays blank even after the nudge.
const EMPTY_REPLY_WARNING: &str = "⚠ The model returned an empty reply, even after being asked again. Try rephrasing or asking again.";

const GRACEFUL_STOP_MESSAGE: &str = "Agent stopped by user (after the current tool call)";

/// The stop handshake between the two loops. `Some(msg)` = a stop is in
/// flight: the agent loop discards queued input until the `StopMarker`, then
/// posts `msg`. Lock order: this cell, then the `StateManager` state.
type PendingStop = Arc<std::sync::Mutex<Option<String>>>;

/// A selection request as it reaches the agent loop (the sole owner of the
/// bound and the agent handle). Both shapes end in `handle_select`, so the
/// lock / pin / unknown-name rules (`crate::selection`) exist exactly once.
#[derive(Debug)]
enum SelectRequest {
    /// Typed `/profile [name]` or `/agent [name]` — bare forms list.
    Command(crate::selection::SelectSubmission),
    /// `UiAction::Select` from the Agents panel / `{"type":"select"}` frame.
    /// `model` is the requested orchestrator alias (absent = the agent's
    /// configured model); the selection carries the subagent overrides.
    Pair {
        selection: crate::config::resolve::Selection,
        model: Option<String>,
    },
}

/// Completion result sent from agent loop back to event loop
#[derive(Clone)]
enum CompletionResult {
    Success,
    Stopped,
    Error,
    CommandDone,
}

/// Render a caught panic's payload as text — shared by every `catch_unwind`
/// boundary in the crate (the `agent_loop` turn-level backstop here, and
/// `TimeBudget::call`'s per-tool boundary in `src/tools/time_budget.rs`) so
/// "what does a caught panic look like to the model/operator" has exactly
/// one answer.
///
/// `panic!("literal")` payloads downcast to `&str`; `panic!("{}", x)` and
/// `panic!("...{x}...")` payloads downcast to `String` (the format macros
/// always produce an owned `String`). Anything else (`std::panic::panic_any`
/// with a non-string payload) has no sensible text — "unknown panic" is the
/// answer, not a crash trying to render it.
///
/// Takes the payload by value so a `&Box<dyn Any + Send>` (a `&Box`) can't be
/// passed in place of the owned box.
pub(crate) fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

const SYSTEM_PROMPT_PERSONA: &str = include_str!("system_prompt_persona.txt");
const SYSTEM_PROMPT_CORE: &str = include_str!("system_prompt_core.txt");
const SYSTEM_PROMPT_SHELL: &str = include_str!("system_prompt_shell.txt");
const MEMORY_PROMPT_SECTION: &str = include_str!("system_prompt_memory.txt");

/// The shell-specific tool guidance, rendered for the detected shell. Lives
/// apart from the core so a host never reads another shell's tool names.
fn shell_section(shell: &ShellKind) -> String {
    SYSTEM_PROMPT_SHELL
        .replace("{shell_tool}", shell.name())
        .replace("{bg_tool}", shell.bg_tool_name())
        .replace(
            "{output_dir}",
            &crate::tools::shell_output_dir().display().to_string(),
        )
}

/// The core tool guidance, plus the shell section when a shell was detected.
/// A `Head::Whole` agent skips both (its persona replaces the whole static
/// head).
fn core_guidance(shell: Option<&ShellKind>) -> String {
    let mut out = String::from(SYSTEM_PROMPT_CORE);
    if let Some(shell) = shell {
        // The core ends without a trailing blank line; keep sections apart.
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&shell_section(shell));
    }
    out
}

/// The `# Environment Information` block (cwd, time, version, OS, binary,
/// shell). Derived fresh at each call so the time is live — shared by the
/// orchestrator prompt and every sub-agent preamble.
pub(crate) fn env_block(shell_kind: Option<&ShellKind>, cwd: &std::path::Path) -> String {
    let current_time = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string();
    let binary_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown".to_string());

    format!(
        "\n# Environment Information\n\n- **Current Working Directory**: {}\n- **Current Time**: {}\n- **PeakBot Version**: {}\n- **Operating System**: {}\n- **PeakBot Binary Path**: {}\n{}",
        cwd.to_string_lossy(),
        current_time,
        PEAKBOT_VERSION,
        std::env::consts::OS,
        binary_path,
        shell_line(shell_kind),
    )
}

/// Load `agents.md` from `cwd` (case-insensitive), wrapped as a prompt
/// section. Empty when absent. Injected into the agentless/orchestrator
/// prompt unconditionally; a sub-agent role gets it only when it opts in
/// via `agents_md: true` (see `build_sub_agent_preamble`).
pub(crate) fn agents_md_section(cwd: &std::path::Path) -> String {
    std::fs::read_dir(cwd)
        .ok()
        .and_then(|entries| {
            entries.filter_map(|e| e.ok()).find(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| name.to_lowercase() == "agents.md")
                    .unwrap_or(false)
            })
        })
        .and_then(|entry| std::fs::read_to_string(entry.path()).ok())
        .map(|content| format!("\n# Agents.md Content\n\n--------------------------------------------------------\n{}\n", content.trim()))
        .unwrap_or_default()
}

/// Build the main agent's system prompt from a resolved `(profile, agent)`
/// (spec §6.1). Sections in output order: static head, core guidance (unless
/// [`Head::Whole`]), memory.md workflow, skills, env block, agents.md,
/// `# Instructions`.
///
/// There is no orchestrator special case: a [`Head::BuiltIn`] agent leads
/// with the built-in persona whether or not it has subagents, and
/// `agent.prompt` renders as `# Instructions` for single agents too. The main
/// agent's skills are filtered by `agent.main.skills`.
///
/// [`Head::Whole`]: crate::config::resolve::Head::Whole
/// [`Head::BuiltIn`]: crate::config::resolve::Head::BuiltIn
pub fn build_agent_prompt(
    agent: &crate::config::resolve::ResolvedAgent,
    profile: &crate::config::resolve::ResolvedProfile,
    skills: &SkillRegistry,
    shell: Option<ShellKind>,
    cwd: &std::path::Path,
) -> String {
    use crate::config::resolve::Head;

    let shell = shell.as_ref();
    let mut prompt = String::new();

    // `Config::parse` already trimmed the persona (I4). The built-in persona
    // file ends with its own single `\n`, which a configured persona mirrors so
    // the core starts at the same byte boundary.
    match &agent.head {
        Head::BuiltIn => {
            prompt.push_str(SYSTEM_PROMPT_PERSONA);
            prompt.push_str(&core_guidance(shell));
        }
        Head::Persona(persona) => {
            prompt.push_str(persona);
            prompt.push('\n');
            prompt.push_str(&core_guidance(shell));
        }
        Head::Whole(persona) => {
            prompt.push_str(persona);
            prompt.push('\n');
        }
    }

    if profile.memory.enabled {
        prompt.push_str(MEMORY_PROMPT_SECTION);
    }
    prompt.push_str(&skills.to_system_prompt_section_filtered(&agent.main.skills));
    prompt.push_str(&env_block(shell, cwd));
    if agent.main.agents_md {
        prompt.push_str(&agents_md_section(cwd));
    }

    // Parse guarantees non-blank but not trimmed; trim like the old builder
    // so the section's byte layout is unchanged.
    if let Some(instructions) = &agent.prompt {
        prompt.push_str(&format!("\n# Instructions\n\n{}\n", instructions.trim()));
    }

    debug!("System prompt:\n {}", prompt);

    prompt
}

/// The prompt half of the agent rebuild seam that every session verb
/// (`/model`, `/cd`, `/new`, `/load`, a selection) funnels through.
///
/// `model` is the model the agent is being rebuilt on — it may differ from
/// `bound.agent().main.model` after a `/model` switch. The model is free
/// (spec §6.5): no `(profile, agent)` ever refuses a switch, a
/// subagent-bearing one included, so this never fails for selection reasons.
/// The prompt itself is model-agnostic: it is exactly [`build_agent_prompt`]
/// for the bound. `Result` keeps the seam's error channel, which the provider
/// half ([`AgentRunner`]'s rebuild) uses for build failures.
pub fn rebuild_agent_for_resolved(
    model: &ResolvedModel,
    bound: &Bound,
    skills: &SkillRegistry,
    shell: Option<ShellKind>,
    cwd: &std::path::Path,
) -> Result<String, String> {
    debug!(
        "rebuilding agent prompt for {} › {} on model {}",
        bound.profile.name,
        bound.agent().main.name,
        model.alias
    );
    Ok(build_agent_prompt(
        bound.agent(),
        &bound.profile,
        skills,
        shell,
        cwd,
    ))
}

/// The `**Shell**:` env-block line, derived from the detected shell. It
/// names both shell tools so sub-agent preambles (which carry the env block
/// but not the shell section) still learn them.
/// Empty when no shell was detected (Windows with nothing installed).
fn shell_line(shell_kind: Option<&ShellKind>) -> String {
    match shell_kind {
        Some(ShellKind::PowerShell { path }) => format!(
            "- **Shell**: powershell ({path}) — your shell tools are `powershell` and \
            `powershell_bg`. Write PowerShell syntax (e.g. `Select-String`, `Get-ChildItem`, \
            `Get-Content`), not POSIX syntax or tools (`grep`, `sed`, `ls -la`).\n"
        ),
        Some(ShellKind::Bash { path }) => format!(
            "- **Shell**: bash ({path}) — your shell tools are `bash` and `bash_bg`. \
            Use POSIX/bash syntax.\n"
        ),
        None => String::new(),
    }
}

/// Convert stored conversation messages to rig Messages for LLM chat history
pub fn convert_conversation_to_rig_messages(conv: &Conversation) -> Vec<Message> {
    use crate::conversation::Message as StoredMessage;
    use rig_core::completion::message::{
        AssistantContent, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    };
    use rig_core::one_or_many::OneOrMany;

    let mut messages = Vec::new();

    for msg in &conv.messages {
        match msg {
            StoredMessage::User { content, .. } => {
                messages.push(Message::user(content.clone()));
            }
            StoredMessage::Assistant { content, .. } => {
                messages.push(Message::assistant(content.clone()));
            }
            StoredMessage::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                let args = serde_json::from_str(arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let id = call_id.clone().unwrap_or_else(|| tool_name.clone());
                messages.push(Message::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                        id,
                        ToolFunction::new(tool_name.clone(), args),
                    ))),
                });
            }
            StoredMessage::ToolResult {
                tool_name,
                result,
                call_id,
                ..
            } => {
                let id = call_id.clone().unwrap_or_else(|| tool_name.clone());
                messages.push(Message::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id,
                        call_id: None,
                        // Image-JSON results (`view_image`) reconstruct as Image;
                        // plain results stay text. See get_agent_history.
                        content: ToolResultContent::from_tool_output(result.clone()),
                    })),
                });
            }
            StoredMessage::Summary { content, .. } => {
                messages.push(Message::user(format!("[Conversation summary] {content}")));
            }
        }
    }

    messages
}

/// Everything the agent loop needs to rebuild `DynAgent` on a session verb
/// (`/model`, `/cd`, `/new`, `/load`, a selection). Only the agent itself is
/// replaced; `mcp_handles` is kept explicitly so MCP subprocesses survive a
/// rebuild (fresh tool list, same processes).
///
/// `config` and `bound` are the session's live truth: a reload swaps
/// `config` for the fresh parse and re-binds `bound` (spec §6.7); every
/// rebuild derives prompt, tools and roster from them. Production boot
/// always attaches one; without it, the rebuild verbs are inert.
pub struct RebuildContext {
    pub config: Arc<Config>,
    /// The session's ONE `(profile, agent)` (I2).
    pub bound: Bound,
    pub mcp_handles: Arc<Vec<McpServerHandle>>,
    /// Detected shell kind — OS-level, so it persists across rebuilds.
    pub shell_kind: Option<ShellKind>,
    /// Skill registry — rescanned on reload; the prompt embeds a filtered
    /// skills section.
    pub skills: SkillRegistry,
    pub todo_tool: Option<TodoTool>,
    /// The boot `--profile` pin, if any. While set, runtime profile selection
    /// is refused and the repo file is ignored on every reload.
    pub profile_pin: Option<String>,
}

impl RebuildContext {
    /// How a reload reads the repo file: a pinned profile is a global profile,
    /// so the repo file is ignored entirely under a pin (spec §6.5).
    pub fn repo_mode(&self) -> RepoMode {
        if self.profile_pin.is_some() {
            RepoMode::Ignore
        } else {
            RepoMode::Include
        }
    }
}

/// How a reload re-binds the session's `Bound` (spec §6.7 step 6).
enum Rebind<'a> {
    /// `/new`, `/model`: keep the bound while the conversation has turns,
    /// re-resolve it while it has none.
    Session,
    /// `/cd`: rebind for the target tree, ignoring the turn count — `/cd`
    /// always starts a fresh conversation, so the target repo's agent is
    /// adopted (the locked rule lives in `selection::rebind_for_cd`).
    ChangeCwd,
    /// `/load`: a new binding from the loaded conversation's saved names
    /// (§6.6 `resolve_saved_selection`), including its saved overrides.
    Saved {
        profile: Option<&'a str>,
        agent: Option<&'a str>,
        subagent_models: &'a std::collections::BTreeMap<String, String>,
    },
}

/// Re-read the config + skills for a live session (spec §6.7 steps 1–7) so
/// edits take effect on a session verb without a restart. `target_cwd` is
/// the session's cwd — `/new` and `/model` don't move it; `/cd` has its
/// own seam, [`reload_for_cd`].
///
/// Mutates `ctx` (`skills`, `config`, `bound`) and `sm` (catalog, selection,
/// visible tabs). It does NOT rebuild the agent — the caller does (step 8).
/// Returns the system messages to surface rather than emitting them, so a
/// following `reset_conversation_state()` (as `/model` and `/cd` do) can't
/// wipe them from the chat.
pub fn reload_session_config(
    ctx: &mut RebuildContext,
    sm: &Arc<StateManager>,
    target_cwd: &std::path::Path,
) -> Vec<String> {
    reload_config(ctx, sm, target_cwd, Rebind::Session)
}

/// Re-read the config + skills for a `/cd` to `target_cwd` and rebind with
/// the `/cd` rule (cd-rebind): the target tree's repo profiles win, so the
/// bound re-resolves in the target catalog even while the conversation has
/// turns. Mirrors [`reload_session_config`] (same steps 1–7), rebind step
/// 6 replaced by `selection::rebind_for_cd`.
///
/// When the pair changes, the messages include exactly
/// `ℹ Switched to <p> › <a> for <target>.`; when the pair is unchanged, no
/// such line.
pub fn reload_for_cd(
    ctx: &mut RebuildContext,
    sm: &Arc<StateManager>,
    target_cwd: &std::path::Path,
) -> Vec<String> {
    reload_config(ctx, sm, target_cwd, Rebind::ChangeCwd)
}

/// The model a `/cd` rebuilds on (cd-rebind): the SAME pair keeps the
/// session's ACTIVE model (the active alias, else the wire id); a CHANGED
/// pair starts on the new agent's configured model.
fn model_after_cd(
    previous: &Bound,
    ctx: &RebuildContext,
    sm: &StateManager,
) -> Result<ResolvedModel, String> {
    if !previous.same_pair(&ctx.bound) {
        return Ok(ctx.bound.agent().main.model.clone());
    }
    // The active alias is the session's live choice (a `/model` switch
    // included); the wire id is the fallback when the alias was removed
    // from config.
    ctx.config
        .registry
        .resolve(&sm.get_model_alias())
        .or_else(|| {
            ctx.config
                .registry
                .find_by_wire_id(&sm.get_provider_name(), &sm.get_model())
        })
        .cloned()
        .ok_or_else(|| "current model not found in registry".to_string())
}

fn reload_config(
    ctx: &mut RebuildContext,
    sm: &Arc<StateManager>,
    target_cwd: &std::path::Path,
    rebind: Rebind<'_>,
) -> Vec<String> {
    let mut messages = Vec::new();

    // 1. Skills for the target tree.
    let (skills, skill_warnings) = load_default_skills(target_cwd);
    ctx.skills = skills;
    messages.extend(skill_warnings);

    // 2. Repo-file migration (a pin means repo files are never read).
    let mut repo_mode = ctx.repo_mode();
    if repo_mode == RepoMode::Include
        && let Some(config_dir) = crate::config::get_config_dir()
    {
        match crate::config::auto_migrate_repo(&config_dir, target_cwd) {
            Ok(Some(migrated)) => messages.extend(migrated.notices()),
            Ok(None) => {}
            // Step 3 reports the global problem itself.
            Err(e)
                if matches!(
                    e.reason,
                    crate::config::migrate::AutoMigrateFailure::GlobalNotUsable(_)
                ) => {}
            Err(e) => {
                messages.push(format!("⚠ {e} — ignoring this repo's profiles."));
                repo_mode = RepoMode::Ignore;
            }
        }
    }

    // 3. Load; any global failure keeps the previous config.
    let Some(fresh) = load_fresh_config(target_cwd, repo_mode, &ctx.skills, &mut messages) else {
        return messages;
    };
    if let Some(pin) = ctx.profile_pin.as_deref()
        && fresh.catalog.start_bound(Some(pin)).is_err()
    {
        messages.push(format!(
            "⚠ --profile '{pin}' is no longer configured — keeping the previous config."
        ));
        return messages;
    }

    // 4. Boot-only blocks keep their boot value until a restart.
    for key in crate::config::resolve::boot_only_diff(&ctx.config, &fresh) {
        messages.push(format!("⚠ {key} change ignored — restart to apply."));
    }

    // 5. Adopt.
    ctx.config = Arc::new(fresh);
    sm.set_catalog(ctx.config.catalog.infos());

    // 6. Rebind.
    let catalog = &ctx.config.catalog;
    let pin = ctx.profile_pin.as_deref();
    match rebind {
        Rebind::ChangeCwd => {
            let (bound, warnings) =
                crate::selection::rebind_for_cd(catalog, &ctx.config.registry, &ctx.bound);
            // The one user-visible line a /cd switch emits (byte-exact).
            if !bound.same_pair(&ctx.bound) {
                messages.push(format!(
                    "ℹ Switched to {} › {} for {}.",
                    bound.profile.name,
                    bound.agent().main.name,
                    target_cwd.display()
                ));
            }
            messages.extend(warnings);
            ctx.bound = bound;
        }
        Rebind::Saved {
            profile,
            agent,
            subagent_models,
        } => {
            let (bound, warnings) = crate::selection::resolve_saved_selection(
                catalog,
                &ctx.config.registry,
                profile,
                agent,
                subagent_models,
                pin,
            );
            ctx.bound = bound;
            messages.extend(warnings);
        }
        // In-flight conversations keep their definitions; only the
        // cwd-derived prompt parts refresh in the caller's rebuild.
        Rebind::Session if sm.conversation_has_turns() => {
            let name = &ctx.bound.profile.name;
            if catalog.profile(name).is_none() {
                messages.push(format!(
                    "⚠ Profile '{name}' is not configured in {}; this conversation keeps it until /new.",
                    target_cwd.display()
                ));
            }
        }
        Rebind::Session => {
            match catalog.resolve(&ctx.bound.profile.name, &ctx.bound.agent().main.name) {
                Some(bound) => {
                    // The pair re-resolved: re-validate the overrides
                    // leniently — a vanished entry drops with its ⚠ and the
                    // sub keeps its default (plan §4.4).
                    let (bound, warnings) =
                        crate::selection::carry_overrides(bound, &ctx.bound, &ctx.config.registry);
                    messages.extend(warnings);
                    ctx.bound = bound;
                }
                None => {
                    // Under a pin the fallback stays inside the pinned profile.
                    let bound = catalog
                        .start_bound(pin)
                        .expect("the pin was checked against the fresh catalog above");
                    let sel = bound.selection();
                    messages.push(format!(
                        "ℹ Using {} › {} (default here).",
                        sel.profile, sel.agent
                    ));
                    ctx.bound = bound;
                }
            }
        }
    }

    // 7. Publish what the session is bound to.
    publish_bound(sm, &ctx.bound);

    messages
}

/// Publish the session's `Bound` to the UI state. The selection and the
/// visible tabs always travel together: the tabs (including `settings`, which
/// gates the gear) belong to the bound profile, so every rebind must refresh
/// both — a picker profile switch included.
pub(crate) fn publish_bound(sm: &StateManager, bound: &Bound) {
    sm.set_selection(bound.selection());
    sm.set_visible_tabs(
        bound
            .profile
            .tabs
            .iter()
            .map(|tab| tab.to_string())
            .collect(),
    );
}

/// Step 3 of §6.7: `Config::load`, with a broken repo file degrading to the
/// global-only config. `None` means "keep the previous config" (the reason
/// is already in `messages`).
fn load_fresh_config(
    target_cwd: &std::path::Path,
    mut repo_mode: RepoMode,
    skills: &SkillRegistry,
    messages: &mut Vec<String>,
) -> Option<Config> {
    use crate::config::resolve::LoadError;

    let known_skills = skills.names();
    let parse_ctx = crate::config::resolve::ParseCtx {
        known_skills: &known_skills,
    };
    let repo_path = target_cwd.join(".peakbot").join("config.yaml");
    loop {
        let repo_failure = match Config::load(target_cwd, repo_mode, &parse_ctx) {
            Ok(fresh) => return Some(fresh),
            Err(LoadError::Invalid(errors))
                if repo_mode == RepoMode::Include
                    && errors
                        .0
                        .iter()
                        .all(|e| e.file == crate::config::ConfigFileKind::Repo) =>
            {
                config_errors_summary(&errors)
            }
            Err(LoadError::Io(path, e)) if repo_mode == RepoMode::Include && path == repo_path => {
                format!("cannot read it: {e}")
            }
            Err(LoadError::Invalid(errors)) => {
                messages.push(format!(
                    "⚠ config reload failed: {} — keeping previous config.",
                    config_errors_summary(&errors)
                ));
                return None;
            }
            // Never migrated here: the user may have restored the 0.23 file
            // on purpose while PeakBot was running.
            Err(LoadError::Legacy(path)) => {
                messages.push(format!(
                    "⚠ {}: {}; keeping the previous config.",
                    path.display(),
                    crate::config::resolve::L1_MESSAGE
                ));
                return None;
            }
            Err(LoadError::Missing(path)) => {
                messages.push(format!(
                    "⚠ config reload failed: {} does not exist — keeping previous config.",
                    path.display()
                ));
                return None;
            }
            Err(LoadError::Io(path, e)) => {
                messages.push(format!(
                    "⚠ config reload failed: cannot read {}: {e} — keeping previous config.",
                    path.display()
                ));
                return None;
            }
        };
        messages.push(format!(
            "⚠ {}: {repo_failure} — ignoring this repo's profiles.",
            repo_path.display()
        ));
        repo_mode = RepoMode::Ignore;
    }
}

/// `<first error> (+N more)` — one chat line for a whole error list.
fn config_errors_summary(errors: &crate::config::ConfigErrors) -> String {
    let Some(first) = errors.0.first() else {
        return "invalid config".to_string();
    };
    let path = first.dotted_path();
    let mut out = if path.is_empty() {
        first.message.clone()
    } else {
        format!("{path}: {}", first.message)
    };
    if errors.0.len() > 1 {
        out.push_str(&format!(" (+{} more)", errors.0.len() - 1));
    }
    out
}

/// What a provider build yields: the agent, its provider facts, the event
/// stream (`None` for providers without hooks) and its session hook.
pub(crate) type BuiltAgent = (
    DynAgent,
    ProviderInfo,
    Option<mpsc::UnboundedReceiver<SourcedEvent>>,
    Arc<SessionHook>,
);

/// The main agent built from `ctx.bound` on `model` — the one recipe for
/// session boot and every rebuild verb. The prompt is built for the
/// session's current cwd; the profile's [`ToolEnv`] is rebuilt from the
/// long-lived MCP handles (no subprocess restarts; the vector store reuses
/// the process-wide DB handle); `delegate` exists iff the agent has
/// subagents.
///
/// [`ToolEnv`]: crate::tools::env::ToolEnv
pub(crate) fn build_bound_agent(
    model: &ResolvedModel,
    ctx: &RebuildContext,
    sm: &Arc<StateManager>,
) -> Result<BuiltAgent, String> {
    use crate::tools::env::{ToolEnv, delegate_enabled};

    let system_prompt = rebuild_agent_for_resolved(
        model,
        &ctx.bound,
        &ctx.skills,
        ctx.shell_kind.clone(),
        &sm.session_cwd(),
    )?;
    let tool_env = Arc::new(ToolEnv::for_profile(
        &ctx.config,
        &ctx.bound.profile,
        &ctx.mcp_handles,
    ));
    let agent = ctx.bound.agent();
    let delegate = delegate_enabled(agent).then(|| {
        Arc::new(SubAgentDeps {
            bound: ctx.bound.clone(),
            orchestrator_model: model.clone(),
            tool_env: Arc::clone(&tool_env),
            state_manager: Arc::clone(sm),
            shell_kind: ctx.shell_kind.clone(),
            skills: ctx.skills.clone(),
            event_sink: None,
            retry: ctx.config.general.retry.clone(),
        })
    });
    crate::providers::create_provider(
        model,
        &system_prompt,
        &tool_env,
        &agent.main,
        delegate,
        ctx.todo_tool.clone(),
        Arc::clone(sm),
        ctx.shell_kind.as_ref(),
    )
    .map_err(|e| format!("failed to build agent for `{}`: {e}", model.alias))
}

/// Shared cell holding the *currently active* SessionHook. Replaced
/// in-place by the agent loop on `/model` rebuild; the event loop's
/// `/stop` path reads through the cell so it always cancels the
/// active prompt's hook, never a stale predecessor. Reads clone the
/// inner Arc immediately so the guard never spans an await point.
pub type SharedSessionHook = Arc<std::sync::RwLock<Arc<SessionHook>>>;

/// The tool-free compaction (and title) model for `model` — same provider
/// config, so the same credentials as the main model. With compaction
/// enabled a construction failure is an error the caller must surface; with
/// it disabled the model is best-effort (it still powers conversation
/// titles) and a failure just means `None`.
fn compaction_model_for(
    model: &ResolvedModel,
    context: &ContextConfig,
) -> Result<Option<Arc<CompactionModel>>> {
    let built = crate::providers::create_compaction_model(
        &model.provider_config,
        context.compaction_model.as_deref(),
    );
    if context.enabled {
        Ok(Some(Arc::new(built?)))
    } else {
        Ok(built.ok().map(Arc::new))
    }
}

/// AgentRunner — the Controller in MVC.
///
/// Receives input (UiAction) from Views, calls the agent, writes results to
/// StateManager (Model). Never reads stdin or prints directly.
pub struct AgentRunner {
    agent: Arc<DynAgent>,
    config: Arc<Config>,
    /// Alias of the model the runner was built on — the boot conversation
    /// is stamped with it.
    model_alias: String,
    #[allow(unused)]
    skills: SkillRegistry,
    state_manager: Option<Arc<StateManager>>,
    // Shared session hook for interrupt/queue state
    session_hook: Arc<SessionHook>,
    // Retained for streaming output handler (view concern, set up by main.rs)
    event_receiver: Option<mpsc::UnboundedReceiver<SourcedEvent>>,
    /// Optional rebuild deps for `/model` switching. Set by
    /// [`AgentRunner::with_rebuild_context`] from `main.rs` after
    /// construction. When `None`, `/model` is a no-op.
    rebuild_ctx: Option<RebuildContext>,
    /// Tool-free compaction model shared with ContextManager.
    /// Kept for historical/API reasons; memory compaction now fetches
    /// the live model from StateManager via get_title_model() so it sees
    /// updates on every agent rebuild.
    #[allow(dead_code)]
    compaction_model: Option<Arc<CompactionModel>>,
}

impl AgentRunner {
    /// `model` is the model `agent` was built on: its provider config feeds
    /// the compaction model and its context size the compaction thresholds.
    #[allow(clippy::too_many_arguments)] // mirrors the provider build's outputs; callers pass them straight through
    pub fn new(
        agent: DynAgent,
        config: Arc<Config>,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        event_receiver: Option<mpsc::UnboundedReceiver<SourcedEvent>>,
        state_manager: Option<Arc<StateManager>>,
        session_hook: Arc<SessionHook>,
        model: &ResolvedModel,
    ) -> anyhow::Result<Self> {
        let agent = Arc::new(agent);

        // When compaction is enabled, a construction failure is fatal —
        // refuse to boot rather than silently degrade into the death
        // spiral.  See unfuck-compact.md Layer 1.
        let compaction_model =
            compaction_model_for(model, &config.general.context).with_context(|| {
                "Failed to construct compaction model for the active provider. \
                 The active provider config is what's read for compaction (same \
                 credentials as the main model). To boot without compaction, set \
                 `general.context.enabled: false` in config.yaml."
            })?;

        // Initialize ContextManager inside StateManager (StateManager owns it)
        if let Some(ref sm) = state_manager {
            // Idempotent with the boot stamp in `create_session`; covers
            // callers that build the runner directly (test harnesses).
            sm.set_supports_vision(provider_info.supports_vision);
            let cm = ContextManager::new(
                config.general.context.clone(),
                model.context_size,
                compaction_model.clone(),
            );
            sm.init_context_manager(cm);
            // The title model is the same as the compaction model — reuse the Arc
            if let Some(model) = compaction_model.clone() {
                sm.init_title_model(model);
            }
        }

        Ok(Self {
            agent,
            config,
            model_alias: model.alias.clone(),
            skills,
            state_manager,
            session_hook,
            event_receiver,
            rebuild_ctx: None,
            compaction_model,
        })
    }

    /// Attach a [`RebuildContext`] so `/model` switches can rebuild
    /// the agent in-place between turns. Builder-style — returns
    /// `self` for chaining off `AgentRunner::new(…)`.
    pub fn with_rebuild_context(mut self, ctx: RebuildContext) -> Self {
        self.rebuild_ctx = Some(ctx);
        self
    }

    /// Force context compaction
    pub async fn force_compact(&mut self) {
        if let Some(ref sm) = self.state_manager {
            match sm.force_compact().await {
                Some(result) => {
                    sm.add_system_message(format!(
                        "Context compacted: {} → {} messages, {} discarded",
                        result.original_count, result.compacted_count, result.num_discarded
                    ));
                }
                None => {
                    sm.add_system_message("Nothing to compact.".to_string());
                }
            }
        }
    }

    /// Message types for internal queue between event loop and agent loop
    pub const _QUEUE_PLACEHOLDER: () = ();

    /// The controller loop — spawns two loops: event loop (receives from View) and
    /// agent loop (processes messages). This allows /stop to interrupt the agent.
    pub async fn run_loop(&mut self, action_receiver: mpsc::UnboundedReceiver<UiAction>) {
        // Channel between event loop and agent loop
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<QueueMessage>(32);

        // Background-process notification bridge: per-process reader
        // threads push `()` pings here (debounced inside the reader);
        // a small forwarder task translates each ping into a
        // `QueueMessage::BackgroundOutputReady` and ships it through
        // the same agent-loop queue user messages flow on. The
        // `Sender<()>` is handed to `StateManager` so the `bash_bg`
        // tool's `start` verb can hand a clone to each reader. See
        // `bash-background.md` § "Wiring into the agent loop".
        let (bg_notify_tx, mut bg_notify_rx) = mpsc::unbounded_channel::<()>();
        let bg_sm = self.state_manager.clone();
        if let Some(sm) = bg_sm.as_ref() {
            sm.attach_bg_notify(bg_notify_tx);
        }
        let bg_bridge_handle = {
            let msg_tx = msg_tx.clone();
            tokio::spawn(async move {
                loop {
                    // A process may be coalescing output behind its
                    // cooldown; arm a wakeup at the soonest window expiry
                    // so a buffer that goes quiet still flushes on time.
                    let deadline = bg_sm.as_ref().and_then(|sm| sm.next_bg_poke_deadline());
                    let flush = async {
                        match deadline {
                            Some(d) => tokio::time::sleep_until(d.into()).await,
                            // No pending cooldown → park this arm forever;
                            // the next reader ping re-evaluates the deadline.
                            None => std::future::pending::<()>().await,
                        }
                    };
                    tokio::select! {
                        biased;
                        ping = bg_notify_rx.recv() => {
                            if ping.is_none() {
                                break; // sender dropped → loop torn down
                            }
                            let _ = msg_tx.send(QueueMessage::BackgroundOutputReady).await;
                        }
                        _ = flush => {
                            let _ = msg_tx.send(QueueMessage::BackgroundOutputReady).await;
                        }
                    }
                }
            })
        };

        // Set by the event loop on Stop, consumed by the agent loop at the
        // matching StopMarker. See `make-flow-great-again.md`: /stop == stop,
        // queued follow-ups are discarded along with the in-flight turn.
        let pending_stop: PendingStop = Arc::default();

        // Completion notifications back to event loop
        let (completion_tx, _completion_rx) =
            tokio::sync::broadcast::channel::<CompletionResult>(8);

        // Extract fields we need to pass to spawned loops
        let state_manager = self.state_manager.clone();
        let config_model = self.model_alias.clone();
        let agent = self.agent.clone();
        let state_manager_for_agent = self.state_manager.clone();
        let config_for_agent = Arc::clone(&self.config);
        let event_receiver = self.event_receiver.take();
        // Shared cell for state that swaps on `/model` rebuild. The event
        // loop reads through it so `/stop` always cancels the active
        // prompt. See [`SharedSessionHook`].
        let session_hook_cell: SharedSessionHook =
            Arc::new(std::sync::RwLock::new(self.session_hook.clone()));
        // The rebuild context is consumed once into the agent loop. If
        // it's `None`, `/model` switches are inert (the loop emits a
        // system-message error if a `SwitchModel` action somehow
        // arrives).
        let rebuild_ctx = self.rebuild_ctx.take();

        // Spawn the two loops.
        //
        // Note: the per-event processor task used to live here (it
        // consumed `event_receiver` and forwarded events to the
        // `StateManager`). It now runs *inside* the agent loop so it
        // can be torn down and respawned with the fresh receiver
        // produced by a `/model` rebuild. See `agent_loop`.
        let event_handle = tokio::spawn({
            let msg_tx = msg_tx.clone();
            let completion_tx = completion_tx.clone();
            let pending_stop = pending_stop.clone();
            let session_hook_cell = session_hook_cell.clone();

            async move {
                Self::event_loop(
                    action_receiver,
                    msg_tx,
                    completion_tx,
                    state_manager,
                    session_hook_cell,
                    config_model,
                    pending_stop,
                )
                .await;
            }
        });

        let agent_handle = tokio::spawn({
            let msg_rx = tokio::sync::Mutex::new(msg_rx);
            let completion_tx = completion_tx.clone();
            let pending_stop = pending_stop.clone();
            let session_hook_cell = session_hook_cell.clone();

            async move {
                Self::agent_loop(
                    msg_rx,
                    completion_tx,
                    state_manager_for_agent,
                    agent,
                    config_for_agent,
                    pending_stop,
                    event_receiver,
                    session_hook_cell,
                    rebuild_ctx,
                )
                .await;
            }
        });

        // Wait for event loop to exit (View closed)
        event_handle.await.ok();
        agent_handle.abort();
        // The abort drops the in-flight turn's future exactly as Stop does;
        // await so the drop has landed before the seam below reads state.
        agent_handle.await.ok();
        // Drop the bg notify sender so the bridge wakes and exits.
        if let Some(sm) = self.state_manager.as_ref() {
            // Answer the in-flight tool call, if any, before teardown.
            sm.close_interrupted_tool_call();
            sm.detach_bg_notify();
            // Kill any still-running bg processes before tearing down.
            sm.clear_bg();
        }
        bg_bridge_handle.abort();
    }

    /// Event loop - receives UiActions from View, queues messages for agent loop.
    ///
    /// **Single-writer invariant** (see `make-flow-great-again.md`): this loop
    /// **never** calls `add_user_message{,_with_attachments}`. User-typed text
    /// only enters `state.chat.messages` from the agent loop, at dequeue time,
    /// immediately before its turn fires. That structurally prevents user-text
    /// from wedging between an in-flight `ToolCall` and its `ToolResult`.
    ///
    /// **Exception:** `run_bg_synthetic_turn_if_any` →
    /// `add_user_message_from_background` is a second writer that bypasses
    /// this invariant by injecting a `[bg output]` user message directly into
    /// the transcript. This is the seam where a bg message can wedge between
    /// a `ToolCall` and its `ToolResult`.
    ///
    /// Typing during a busy turn is therefore a queued **follow-up**, not an
    /// interrupt. The explicit interrupt is `/stop` (or `Esc` →
    /// `UiAction::RequestStop`), which arms `pending_stop`, sends a
    /// `StopMarker`, and the agent loop discards every queued message until
    /// the marker is seen. /stop means stop.
    #[allow(clippy::too_many_arguments)] // event_loop coordinates many handles; refactoring loses clarity
    async fn event_loop(
        mut action_receiver: mpsc::UnboundedReceiver<UiAction>,
        msg_tx: tokio::sync::mpsc::Sender<QueueMessage>,
        _completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        _session_hook_cell: SharedSessionHook,
        config_model: String,
        pending_stop: PendingStop,
    ) {
        use crate::state::StopStep;

        // Helper: one Stop press. The first press is graceful (the hook ends
        // the turn at its next boundary); the second aborts through the
        // per-turn `CancellationToken` (`StateManager::request_stop`). Either
        // way the queue is drained and exactly one marker is sent per stop —
        // escalation only rewrites the message in the cell. Background
        // (`bash_bg`) processes are deliberately spared — they survive Stop.
        let request_stop = |state_manager: &Option<Arc<StateManager>>,
                            msg_tx: &tokio::sync::mpsc::Sender<QueueMessage>,
                            pending_stop: &PendingStop| {
            let sm = state_manager.clone();
            let msg_tx = msg_tx.clone();
            let pending_stop = pending_stop.clone();
            async move {
                let Some(sm) = sm else { return };
                // Holding the cell across `request_stop` means the agent loop
                // can never dequeue between the abort and the drain arming.
                let first = {
                    let mut cell = pending_stop.lock().unwrap();
                    let first = cell.is_none();
                    match sm.request_stop() {
                        StopStep::Noop => return,
                        StopStep::Graceful => *cell = Some(GRACEFUL_STOP_MESSAGE.to_string()),
                        StopStep::Immediate(tally) => {
                            *cell = Some(stop_message(tally));
                            sm.set_status(Some("Stop requested...".to_string()));
                        }
                    }
                    sm.set_pending_input_count(0);
                    first
                };
                if first {
                    msg_tx.send(QueueMessage::StopMarker).await.ok();
                }
            }
        };
        // Ensure a boot conversation exists. Idempotent: `create_session`
        // already minted (fresh) or loaded (resume) one synchronously before
        // spawning this loop, so this only fires for callers that bypass the
        // factory (test harnesses). The active wire identity (provider_name +
        // model) was stamped on the StateManager before run_loop; we read it
        // back so saved conversations carry the right re-activation key.
        // `cwd` is the SM-owned session cwd (set in `create_session` before
        // run_loop is spawned; for the test-harness bypass path the SM
        // seeds it from `current_dir()` at construction).
        if let Some(ref sm) = state_manager {
            sm.ensure_boot_conversation(&sm.session_cwd(), &config_model);
        }

        while let Some(action) = action_receiver.recv().await {
            match action {
                UiAction::SendMessage {
                    text: msg,
                    attachments,
                } => {
                    // Route by shape of the text. Slash commands must NOT
                    // be sent to the LLM, and must NOT be appended as user
                    // messages — their handlers emit their own system
                    // output. See `classify_submission` docs.
                    let kind = classify_submission(&msg);
                    // Attachments only ride a user turn; a command never
                    // consumes them, so refuse loudly instead of dropping.
                    if !attachments.is_empty()
                        && !matches!(
                            kind,
                            SubmitKind::UserMessage { .. } | SubmitKind::InvalidAttachment(_)
                        )
                    {
                        if let Some(ref sm) = state_manager {
                            sm.add_system_message(
                                "❌ Attachments can't be sent with a /command.".to_string(),
                            );
                        }
                        continue;
                    }
                    match kind {
                        SubmitKind::StopCommand => {
                            request_stop(&state_manager, &msg_tx, &pending_stop).await;
                        }
                        // `/pause` / `/resume` — immediate, like `/stop`: they
                        // must work mid-turn (that's the whole point of
                        // pausing a running sub-agent), so they are NOT
                        // queued behind the in-flight turn. Both are
                        // idempotent no-ops in StateManager; the only extra
                        // here is a short note when there is no sub-agent to
                        // act on, so the command never fails silently.
                        SubmitKind::PauseCommand => {
                            if let Some(ref sm) = state_manager {
                                if sm.get_state().sub_agent.is_none() {
                                    sm.add_system_message("No sub-agent running.".to_string());
                                } else {
                                    sm.request_pause();
                                }
                            }
                        }
                        SubmitKind::ResumeCommand => {
                            if let Some(ref sm) = state_manager {
                                if sm.get_state().sub_agent.is_none() {
                                    sm.add_system_message("No sub-agent running.".to_string());
                                } else {
                                    sm.resume();
                                }
                            }
                        }
                        SubmitKind::Command(cmd) => {
                            // Dispatched by agent_loop via process_command_internal.
                            msg_tx.send(QueueMessage::Command(cmd)).await.ok();
                        }
                        SubmitKind::UserMessage { text, image_tokens } => {
                            // Resolve ids + `[img:]` tokens into stored
                            // attachments for the CURRENT conversation. Any
                            // failure is a ❌ system message and nothing is
                            // queued (the model is never called).
                            let resolved = if image_tokens.is_empty() && attachments.is_empty() {
                                Vec::new()
                            } else {
                                // No SM ⇒ no store and no conversation to resolve against.
                                let Some(ref sm) = state_manager else {
                                    continue;
                                };
                                let Some(convo) = sm.get_current_conversation_id() else {
                                    sm.add_system_message(
                                        "❌ No active conversation to attach files to.".to_string(),
                                    );
                                    continue;
                                };
                                match crate::attachments::collect(
                                    sm.uploads(),
                                    convo,
                                    &sm.session_cwd(),
                                    image_tokens,
                                    attachments,
                                )
                                .await
                                {
                                    Ok(atts) => atts,
                                    Err(e) => {
                                        sm.add_system_message(format!("❌ {e}"));
                                        continue;
                                    }
                                }
                            };
                            // Single-writer invariant: do NOT call
                            // add_user_message here. The agent loop appends
                            // user input to chat at dequeue time, between
                            // turns, where it can never wedge between a
                            // ToolCall and its ToolResult.
                            if let Some(ref sm) = state_manager {
                                sm.increment_pending_input();
                            }
                            msg_tx
                                .send(QueueMessage::UserMessage {
                                    text,
                                    attachments: resolved,
                                })
                                .await
                                .ok();
                        }
                        SubmitKind::InvalidAttachment(e) => {
                            if let Some(ref sm) = state_manager {
                                sm.add_system_message(format!("❌ {e}"));
                            }
                            // Do not enqueue — the model is never called.
                        }
                        SubmitKind::SwitchModel(alias) => {
                            // Same as `UiAction::SwitchModel`: the agent loop
                            // owns the agent handle and validates the alias.
                            if let Some(ref sm) = state_manager {
                                sm.increment_pending_input();
                            }
                            msg_tx.send(QueueMessage::SwitchModel(alias)).await.ok();
                        }
                        SubmitKind::Select(sub) => {
                            // Forward to the agent loop: it owns the bound,
                            // the catalog, the pin and the agent handle (a
                            // selection rebuilds it; even the lists read the
                            // live bound). Pending until it is handled.
                            if let Some(ref sm) = state_manager {
                                sm.increment_pending_input();
                            }
                            msg_tx
                                .send(QueueMessage::Select(SelectRequest::Command(sub)))
                                .await
                                .ok();
                        }
                    }
                }

                UiAction::RequestStop => {
                    // Esc key — same shape as /stop. Stop means stop, queue is dropped.
                    request_stop(&state_manager, &msg_tx, &pending_stop).await;
                }

                UiAction::SwitchModel(alias) => {
                    // Forward the validated alias to the agent loop. The
                    // agent loop is the single owner of the agent handle,
                    // so the rebuild has to happen there. Counts as a
                    // pending input so the status bar shows activity until
                    // the new agent is up. (`decrement_pending_input` is
                    // called after the rebuild completes.)
                    if let Some(ref sm) = state_manager {
                        sm.increment_pending_input();
                    }
                    msg_tx.send(QueueMessage::SwitchModel(alias)).await.ok();
                }

                UiAction::ChangeCwd(path) => {
                    // Same shape as SwitchModel — forward the validated
                    // path to the agent loop (sole owner of the agent
                    // handle) and count it as pending input so the status
                    // bar shows activity until the rebuild completes.
                    if let Some(ref sm) = state_manager {
                        sm.increment_pending_input();
                    }
                    msg_tx.send(QueueMessage::ChangeCwd(path)).await.ok();
                }

                UiAction::Select { selection, model } => {
                    // Forward to the agent loop (sole owner of the bound and
                    // the agent handle — the selection rebuilds it). Counts as
                    // pending input so the status bar shows activity until the
                    // rebuild completes.
                    if let Some(ref sm) = state_manager {
                        sm.increment_pending_input();
                    }
                    msg_tx
                        .send(QueueMessage::Select(SelectRequest::Pair {
                            selection,
                            model,
                        }))
                        .await
                        .ok();
                }

                UiAction::PauseSubAgent => {
                    // Immediate, like RequestStop — not queued behind the
                    // running turn. All it does is arm the pause gate; the
                    // sub-agent parks itself at its next checkpoint.
                    // No-op (idempotent) when no pausable sub-agent runs.
                    if let Some(ref sm) = state_manager {
                        sm.request_pause();
                    }
                }

                UiAction::ResumeSubAgent => {
                    // Immediate, like RequestStop — clears the gate so the
                    // parked sub-agent wakes at its next checkpoint.
                    // No-op (idempotent) when no sub-agent is paused.
                    if let Some(ref sm) = state_manager {
                        sm.resume();
                    }
                }
            }
        }
    }

    /// Compact the session's `memory.md` if it exceeds `threshold_bytes`.
    ///
    /// Extracted from `agent_loop` so the path resolution is testable: the
    /// path is joined onto `sm.session_cwd()`, **not** the process cwd. The
    /// process cwd is frozen at startup (see
    /// `tests/scenarios/no_set_current_dir.rs`); in web mode the
    /// session lives in a project directory that is not where the binary was
    /// launched from. Resolving relatively silently compacted the wrong file
    /// — i.e. never compacted at all.
    ///
    /// The compaction model is read live from `StateManager` so it reflects
    /// every agent rebuild. Failures are logged, never fatal.
    async fn maybe_compact_memory(sm: &StateManager, threshold_bytes: usize) {
        let path = sm.session_cwd().join("memory.md");
        let Some(content) = crate::memory_compaction::read_if_oversized(&path, threshold_bytes)
        else {
            return;
        };
        let Some(model) = sm.get_title_model() else {
            tracing::warn!(
                "memory.md is {} bytes (over the {} byte threshold) but no compaction \
                 model is available — skipping compaction",
                content.len(),
                threshold_bytes
            );
            return;
        };

        sm.set_status(Some("Compacting memory.md...".to_string()));
        let size_before = content.len();
        match crate::memory_compaction::compact_memory(&content, &model).await {
            Ok(compacted) => {
                if let Err(e) = std::fs::write(&path, compacted) {
                    tracing::warn!("Failed to write compacted memory.md: {}", e);
                } else {
                    sm.add_system_message(format!(
                        "memory.md compacted (was {} bytes)",
                        size_before
                    ));
                    tracing::info!(
                        "memory.md compacted: {} -> {} bytes",
                        size_before,
                        std::fs::metadata(&path)
                            .map(|m| m.len() as usize)
                            .unwrap_or(0)
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Memory compaction failed: {}", e);
            }
        }
        sm.set_status(None);
    }

    /// Agent loop - processes messages from event loop, sends completions back.
    ///
    /// **Single-writer for user input.** This loop is the *only* place that
    /// calls `add_user_message{,_with_attachments}` — see
    /// `make-flow-great-again.md`. The write happens at dequeue time, before
    /// the turn is built and sent to the model, so the message lands strictly
    /// between agent turns. This makes it structurally impossible for
    /// user-typed text to wedge between an in-flight `ToolCall` and its
    /// `ToolResult`.
    ///
    /// **Drain on /stop.** While `pending_stop` holds a message, all queued
    /// items are discarded until the matching `StopMarker` takes and posts
    /// it. /stop = stop, queued follow-ups are dropped along with the turn.
    ///
    /// A panic while handling a message aborts that turn like Stop and the
    /// loop keeps serving.
    #[allow(clippy::too_many_arguments)] // agent_loop owns rebuild + event-processor lifecycle
    async fn agent_loop(
        msg_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<QueueMessage>>,
        completion_tx: tokio::sync::broadcast::Sender<CompletionResult>,
        state_manager: Option<Arc<StateManager>>,
        initial_agent: Arc<DynAgent>,
        mut config: Arc<Config>,
        pending_stop: PendingStop,
        initial_event_receiver: Option<mpsc::UnboundedReceiver<SourcedEvent>>,
        session_hook_cell: SharedSessionHook,
        mut rebuild_ctx: Option<RebuildContext>,
    ) {
        // The agent reference is `mut` here — `/model` rebuilds it
        // in-place between turns. `session_hook_cell` is mirrored into the
        // live event_loop.
        let mut agent = initial_agent;

        // Memory compaction is deferred to the first user message so
        // startup stays instant and the spinner provides visual feedback.
        let mut memory_compaction_done = false;

        // The event-processor task pulls AgentEvents off the
        // receiver and forwards them into StateManager. It used to
        // live in `run_loop` but now lives here so we can tear it
        // down and respawn it whenever a `/model` rebuild produces a
        // fresh receiver. The handle is `Some` while a processor is
        // active.
        let mut event_processor: Option<tokio::task::JoinHandle<()>> =
            initial_event_receiver.map(|rx| {
                let sm = state_manager.clone();
                tokio::spawn(async move {
                    let mut rx = rx;
                    while let Some(event) = rx.recv().await {
                        Self::process_event_for_ui(&sm, event);
                    }
                })
            });

        loop {
            // Wait for a message. The `MutexGuard` must drop before
            // dispatch: a handler that re-enters the queue would
            // deadlock on it.
            let msg = msg_rx.lock().await.recv().await;

            // Channel closed: abort the event processor and exit.
            let Some(msg) = msg else {
                if let Some(h) = event_processor.take() {
                    h.abort();
                }
                break;
            };

            // Drain mode: while a stop is in flight, discard everything up to
            // its StopMarker. The event loop has already zeroed the pending
            // counter. `BackgroundOutputReady` is discarded too: /stop == stop,
            // *including* auto-injection — buffers keep filling in the
            // registry and flush in one synthetic turn on the next ping.
            if !matches!(msg, QueueMessage::StopMarker) && pending_stop.lock().unwrap().is_some() {
                continue;
            }

            let dispatch = async {
                match msg {
                    QueueMessage::UserMessage { text, attachments } => {
                        // Single-writer point: append to chat *now*, between turns.
                        if let Some(ref sm) = state_manager {
                            if attachments.is_empty() {
                                sm.add_user_message(text.clone());
                            } else {
                                sm.add_user_message_with_attachments(text.clone(), attachments);
                            }
                            // Escape hatch: an interrupted conversation (provisional
                            // or untitled) gets its definitive title now from
                            // whatever transcript exists. The gate no-ops on a
                            // fresh conversation's opening message; fire-and-forget.
                            sm.maybe_generate_title();
                            sm.decrement_pending_input();
                            // A real human turn clears all bg cooldowns, so any
                            // buffered background output flushes on the next drain
                            // alongside the user's message.
                            sm.reset_bg_cooldowns();
                            sm.set_running(true);
                        }

                        // Lazy memory compaction: run once, on the first user
                        // message, so startup stays instant and the spinner gives
                        // visual feedback while we work. The compaction model is
                        // read live from StateManager inside the helper, so it
                        // sees `/model` and selection rebuilds. memory.md is a
                        // profile setting, so it follows the session's Bound.
                        let memory_threshold = rebuild_ctx
                            .as_ref()
                            .map(|ctx| &ctx.bound.profile.memory)
                            .filter(|memory| memory.enabled)
                            .map(|memory| memory.threshold_bytes);
                        if !memory_compaction_done && let Some(threshold) = memory_threshold {
                            memory_compaction_done = true;
                            if let Some(sm) = state_manager.as_ref() {
                                Self::maybe_compact_memory(sm, threshold).await;
                            }
                        }

                        // Build the current-turn `Message` — attachments (if any)
                        // are read from state, so both text and vision turns use
                        // the same dispatch path.
                        let current_turn = state_manager
                            .as_ref()
                            .and_then(|sm| sm.build_current_turn_message())
                            .unwrap_or_else(|| {
                                // Fallback: no state manager (test-only paths) —
                                // pass the String through as a text-only Message.
                                rig_core::completion::message::Message::from(text.as_str())
                            });

                        let result = Self::process_message_internal(
                            current_turn,
                            &state_manager,
                            &agent,
                            &config,
                        )
                        .await;

                        // Mark as done — snapshot run_started_at BEFORE set_running(false) clears
                        // it, then emit a "worked for MM:SS" system message (reuses the spinner
                        // formatter so the post-run figure matches the live indicator).
                        if let Some(ref sm) = state_manager {
                            let started_at = sm.get_state().run_started_at;
                            sm.set_running(false);
                            if let Some(t) = started_at {
                                sm.add_system_message(format!(
                                    "worked for {}",
                                    crate::ui::repl::spinner::fmt_elapsed(t)
                                ));
                            }
                        }

                        // Send completion notification
                        completion_tx.send(result).ok();

                        // Post-turn bg drain seam: if any background process
                        // produced output during this turn (or while we were
                        // parked), drain it into a synthetic user turn and
                        // immediately run another iteration. See
                        // `bash-background.md` § "Wiring into the agent loop".
                        Self::run_bg_synthetic_turn_if_any(
                            &state_manager,
                            &agent,
                            &config,
                            &completion_tx,
                        )
                        .await;
                    }

                    QueueMessage::BackgroundOutputReady => {
                        // Wake-up triggered by a reader thread. Same drain
                        // path as the post-turn seam — extracted into a
                        // helper so the contract is identical.
                        Self::run_bg_synthetic_turn_if_any(
                            &state_manager,
                            &agent,
                            &config,
                            &completion_tx,
                        )
                        .await;
                    }

                    QueueMessage::Command(cmd) => {
                        if let Some(ref sm) = state_manager {
                            sm.set_running(true);
                        }
                        // The conversation *before* the command. `/load` swaps it;
                        // an unchanged id after means there is nothing to re-bind.
                        let conversation_before = state_manager
                            .as_ref()
                            .and_then(|sm| sm.get_current_conversation_id());
                        Self::process_command_internal(&cmd, &state_manager, &config).await;

                        // `/load <arg>` may have just swapped the active
                        // conversation — re-bind and rebuild on it.
                        let lcmd = cmd.trim().to_ascii_lowercase();
                        if lcmd.starts_with("/load ") {
                            Self::maybe_rebuild_after_load(
                                conversation_before,
                                &mut agent,
                                &state_manager,
                                &session_hook_cell,
                                &mut event_processor,
                                rebuild_ctx.as_mut(),
                            )
                            .await;
                        } else if lcmd == "/new" {
                            // `/new` reset to a fresh conversation on the
                            // active model; rebuild the agent so config/skill
                            // edits take effect. Keeps the current model —
                            // it does not switch to the agent's configured
                            // model.
                            Self::refresh_agent_after_new(
                                &mut agent,
                                &state_manager,
                                &session_hook_cell,
                                &mut event_processor,
                                rebuild_ctx.as_mut(),
                            )
                            .await;
                        }
                        if let Some(ref sm) = state_manager {
                            sm.set_running(false);
                        }
                        completion_tx.send(CompletionResult::CommandDone).ok();
                    }

                    QueueMessage::StopMarker => {
                        if let Some(message) = pending_stop.lock().unwrap().take() {
                            if let Some(ref sm) = state_manager {
                                sm.set_status(None);
                                sm.add_system_message(message);
                            }
                            completion_tx.send(CompletionResult::Stopped).ok();
                        }
                    }

                    QueueMessage::SwitchModel(alias) => {
                        // `/model` is `/new` + boot the agent against a
                        // different ProviderConfig. The View validated the
                        // alias against its snapshot, but `handle_switch_model`
                        // re-reads config first (picking up newly-added
                        // aliases) and re-resolves, emitting a clean error if
                        // the alias vanished under a reload.
                        if let Some(ref sm) = state_manager {
                            sm.set_running(true);
                        }
                        let outcome = Self::handle_switch_model(
                            &alias,
                            &mut agent,
                            &state_manager,
                            &session_hook_cell,
                            &mut event_processor,
                            rebuild_ctx.as_mut(),
                        )
                        .await;
                        if let Some(ref sm) = state_manager {
                            sm.decrement_pending_input();
                            sm.set_running(false);
                        }
                        match outcome {
                            Ok(()) => {
                                completion_tx.send(CompletionResult::CommandDone).ok();
                            }
                            Err(msg) => {
                                if let Some(ref sm) = state_manager {
                                    sm.add_system_message(format!("❌ /model: {msg}"));
                                }
                                completion_tx.send(CompletionResult::CommandDone).ok();
                            }
                        }
                    }

                    QueueMessage::ChangeCwd(path) => {
                        // Same rebuild seam as SwitchModel, different axis:
                        // chdir + rebuild the system prompt + rebuild the
                        // agent on the *current* model, then reset the
                        // conversation like /new. The View already validated
                        // the path; we re-validate defensively.
                        if let Some(ref sm) = state_manager {
                            sm.set_running(true);
                        }
                        let outcome = Self::handle_change_cwd(
                            &path,
                            &mut agent,
                            &state_manager,
                            &session_hook_cell,
                            &mut event_processor,
                            rebuild_ctx.as_mut(),
                        )
                        .await;
                        if let Some(ref sm) = state_manager {
                            sm.decrement_pending_input();
                            sm.set_running(false);
                        }
                        if let Err(msg) = outcome
                            && let Some(ref sm) = state_manager
                        {
                            sm.add_system_message(format!("❌ /cd: {msg}"));
                        }
                        completion_tx.send(CompletionResult::CommandDone).ok();
                    }

                    QueueMessage::Select(request) => {
                        if let Some(ref sm) = state_manager {
                            sm.set_running(true);
                        }
                        Self::handle_select(
                            request,
                            &mut agent,
                            &state_manager,
                            &session_hook_cell,
                            &mut event_processor,
                            rebuild_ctx.as_mut(),
                        )
                        .await;
                        if let Some(ref sm) = state_manager {
                            sm.decrement_pending_input();
                            sm.set_running(false);
                        }
                        completion_tx.send(CompletionResult::CommandDone).ok();
                    }
                }
            };

            // Panics outside any tool (tools are caught by TimeBudget) abort the turn
            // like Stop does, instead of killing the loop with is_running stuck. Relies
            // on panic = "unwind".
            if let Err(payload) = std::panic::AssertUnwindSafe(dispatch).catch_unwind().await {
                let what = panic_text(payload);
                if let Some(ref sm) = state_manager {
                    sm.close_interrupted_tool_call();
                    sm.set_running(false);
                    sm.add_system_message(format!("⚠ internal error: {what} — turn aborted"));
                }
                completion_tx.send(CompletionResult::Error).ok();
            }

            // A session verb may have reloaded the config into the rebuild
            // context (spec §6.7 step 5); the context is the authority, this
            // loop's copy only follows it.
            if let Some(ctx) = rebuild_ctx.as_ref() {
                config = Arc::clone(&ctx.config);
            }
        }
    }

    /// Rebuild the live `DynAgent` for a new model alias. Runs the
    /// same conversation-reset semantics as `/new`, then constructs a
    /// fresh `DynAgent` against the resolved provider config and
    /// publishes the new ProviderInfo + SessionHook through their
    /// shared cells. Restarts the event-processor task to consume
    /// from the new event channel.
    ///
    /// On error (unknown alias, build failure), returns a string
    /// describing the failure for the caller to surface as a system
    /// message; the previous agent is left intact.
    async fn handle_switch_model(
        alias: &str,
        agent_slot: &mut Arc<DynAgent>,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) -> Result<(), String> {
        let Some(ctx) = rebuild_ctx else {
            return Err("model switching is not available in this session".to_string());
        };
        let sm_for_provider = state_manager
            .clone()
            .ok_or_else(|| "state manager required for /model".to_string())?;

        // No selection ever refuses a model switch (spec §6.5): the model is
        // free with or without subagents.
        //
        // Re-read config + skills first so a newly-added alias resolves and
        // a fresh prompt / registry is in play before we switch. Warnings are
        // surfaced *after* the conversation reset below so
        // `reset_conversation_state()` can't wipe them from the chat.
        let reload_warnings =
            reload_session_config(ctx, &sm_for_provider, &sm_for_provider.session_cwd());

        let Some(resolved) = ctx.config.registry.resolve(alias) else {
            let available = ctx.config.registry.aliases_sorted().join(", ");
            return Err(format!("unknown alias `{alias}`. Available: {available}"));
        };
        let resolved = resolved.clone();

        // Rebuild the agent + publish new provider info — shared with
        // `/load` (which re-activates a saved wire identity).
        Self::rebuild_agent(
            &resolved,
            agent_slot,
            &sm_for_provider,
            session_hook_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await?;

        // Reset conversation-scoped state — same path as /new. Stamps
        // the new wire identity (provider_name + model) on the
        // metadata so /load on the freshly-reset convo (after the next
        // prompt) restores the right model. The new conversation keeps the
        // current `(profile, agent)`. The alias is *also* set on AppState for
        // status-bar display, but is NOT persisted.
        sm_for_provider.reset_conversation_state();
        let convo_name = format!(
            "Conversation {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        sm_for_provider.create_conversation(
            convo_name,
            resolved.provider_name.clone(),
            resolved.model_name.clone(),
            sm_for_provider.session_cwd().to_string_lossy().into_owned(),
        );
        sm_for_provider.set_model(resolved.model_name.clone());
        sm_for_provider.set_provider_name(resolved.provider_name.clone());
        sm_for_provider.set_model_alias(resolved.alias.clone());

        // Announce the swap with a short banner so the user sees it
        // happened. Format mirrors the listing format from
        // process_command_internal::"/model".
        sm_for_provider.add_system_message(format!(
            "🔁 New conversation on {} ({} · {})",
            resolved.alias, resolved.provider_name, resolved.model_name
        ));

        // Surface reload/skill warnings now — after the reset — so they
        // persist in the fresh conversation instead of being cleared.
        for warning in reload_warnings {
            sm_for_provider.add_system_message(warning);
        }

        Ok(())
    }

    /// `/profile`, `/agent` and the picker's `Select` (spec §6.5–§6.6,
    /// plan §4.2). The request is the FULL desired setup: the pair, the
    /// orchestrator model (absent = the agent's configured one) and the
    /// subagent overrides. Refusal order: exact no-op (pair frames only) >
    /// lock > pin > unknown pair > unknown alias > unknown override — a
    /// refusal changes nothing.
    ///
    /// Bare forms render the live lists. Applying swaps `ctx.bound`,
    /// rebuilds the agent on the requested model and only then persists the
    /// selection — a failed rebuild rolls the bound back and
    /// `sm.selection()` was never touched.
    ///
    /// No conversation reset, unlike `/model` and `/cd`: selection is only
    /// possible before the first turn, and minting a new id would break the
    /// web UI's sticky `?convo=` binding; the wire id is re-stamped in place.
    async fn handle_select(
        request: SelectRequest,
        agent_slot: &mut Arc<DynAgent>,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) {
        use crate::selection::{
            SelectChoice, SelectSubmission, override_refusal, render_agent_list,
            render_profile_list, resolve_selection_choice,
        };

        let Some(sm) = state_manager.clone() else {
            return;
        };
        let Some(ctx) = rebuild_ctx else {
            sm.add_system_message(
                "❌ selection is not available in this session (no agent loop attached)."
                    .to_string(),
            );
            return;
        };
        let pin = ctx.profile_pin.clone();
        let has_turns = sm.conversation_has_turns();

        // A typed command always re-applies a name (no model, no overrides);
        // only a pair frame can be the exact current setup.
        let pair_changed = match &request {
            SelectRequest::Pair { selection, .. } => {
                (selection.profile.as_str(), selection.agent.as_str())
                    != (
                        ctx.bound.profile.name.as_str(),
                        ctx.bound.agent().main.name.as_str(),
                    )
            }
            SelectRequest::Command(_) => true,
        };
        // The picker re-sends the current setup on redraws; answering that
        // with a rebuild (or a lock refusal) would be noise.
        if let SelectRequest::Pair { selection, model } = &request
            && !pair_changed
        {
            if selection.subagent_models == ctx.bound.selection().subagent_models
                && model
                    .as_deref()
                    .unwrap_or(ctx.bound.agent().main.model.alias.as_str())
                    == sm.get_model_alias()
            {
                return;
            }
            if has_turns {
                sm.add_system_message(
                    "❌ /agent: models are locked once the conversation has started — /new for a fresh one."
                        .to_string(),
                );
                return;
            }
        }

        // A same-pair request has no steps: it is a models-only change on a
        // fresh `Bound`, which clears the old overrides structurally.
        let (steps, req_model, req_overrides) = match request {
            SelectRequest::Command(SelectSubmission::ListProfiles) => {
                sm.add_system_message(render_profile_list(
                    &ctx.config.catalog,
                    &ctx.bound,
                    pin.as_deref(),
                ));
                return;
            }
            SelectRequest::Command(SelectSubmission::ListAgents) => {
                sm.add_system_message(render_agent_list(&ctx.bound));
                return;
            }
            SelectRequest::Command(step) => (vec![step], None, std::collections::BTreeMap::new()),
            SelectRequest::Pair { selection, model } => {
                let mut steps = Vec::with_capacity(2);
                if pair_changed {
                    if selection.profile != ctx.bound.profile.name {
                        steps.push(SelectSubmission::Profile(selection.profile));
                    }
                    steps.push(SelectSubmission::Agent(selection.agent));
                }
                (steps, model, selection.subagent_models)
            }
        };
        let profile_changed = steps
            .iter()
            .any(|s| matches!(s, SelectSubmission::Profile(_)));
        let (command, label) = match (profile_changed, steps.is_empty()) {
            (true, _) => ("/profile", "Profile"),
            (false, false) => ("/agent", "Agent"),
            (false, true) => ("/agent", "Models"),
        };

        let mut next = if steps.is_empty() {
            Bound::new(Arc::clone(&ctx.bound.profile), ctx.bound.agent_index)
        } else {
            ctx.bound.clone()
        };
        for step in steps {
            let step_command = match step {
                SelectSubmission::Profile(_) => "/profile",
                _ => "/agent",
            };
            match resolve_selection_choice(
                &ctx.config.catalog,
                &next,
                step,
                has_turns,
                pin.as_deref(),
            ) {
                SelectChoice::Apply(bound) => next = bound,
                SelectChoice::Refuse(msg) => {
                    sm.add_system_message(format!("❌ {step_command}: {msg}"));
                    return;
                }
            }
        }

        let model = match req_model {
            Some(alias) => match ctx.config.registry.resolve(&alias) {
                Some(model) => model.clone(),
                None => {
                    sm.add_system_message(format!(
                        "❌ {command}: unknown alias `{alias}`. Available: {}",
                        ctx.config.registry.aliases_sorted().join(", ")
                    ));
                    return;
                }
            },
            None => next.agent().main.model.clone(),
        };

        // Strict on the user frame: the first bad override refuses the
        // whole request (rebinds are the lenient path).
        let (next, rejected) = next.with_subagent_models(&ctx.config.registry, &req_overrides);
        if let Some(rej) = rejected.first() {
            sm.add_system_message(format!(
                "❌ {command}: {}",
                override_refusal(rej, next.agent(), &ctx.config.registry)
            ));
            return;
        }

        let previous = std::mem::replace(&mut ctx.bound, next);
        if let Err(e) = Self::rebuild_agent(
            &model,
            agent_slot,
            &sm,
            session_hook_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await
        {
            ctx.bound = previous;
            sm.add_system_message(format!("❌ {command}: {e}"));
            return;
        }

        publish_bound(&sm, &ctx.bound);
        sm.set_model(model.model_name.clone());
        sm.set_provider_name(model.provider_name.clone());
        sm.set_model_alias(model.alias.clone());
        // Keep the persisted re-activation key truthful: the conversation
        // keeps its id but now runs on the requested model.
        sm.set_conversation_wire_id(model.provider_name.clone(), model.model_name.clone());

        let mut line = format!(
            "{label}: {} › {} — model {}.",
            ctx.bound.profile.name,
            ctx.bound.agent().main.name,
            model.alias
        );
        let overrides = ctx.bound.selection().subagent_models;
        if !overrides.is_empty() {
            let parts: Vec<String> = overrides
                .iter()
                .map(|(subagent, alias)| format!("{subagent} → {alias}"))
                .collect();
            line.push_str(&format!(" Subagent models: {}.", parts.join(", ")));
        }
        sm.add_system_message(line);
    }

    /// Change the working directory and rebuild on a fresh conversation.
    /// The cwd sibling of [`Self::handle_switch_model`] — same reset +
    /// rebuild seam, different axis.
    ///
    /// Sequence (order matters):
    /// 1. `clear_bg()` first, so no `bash_bg` reader thread straddles the
    ///    session-cwd flip with a half-old, half-new view of the tree.
    /// 2. `state_manager.set_session_cwd(target)` — the load-bearing act.
    ///    Everything the prompt derives from the cwd and the
    ///    path-aware tools' `session_cwd` flip here, **without** mutating
    ///    the process-global cwd. Two web sessions in different trees
    ///    stay correct because nothing they share is mutated.
    /// 3. Reload the config for the target tree (spec §6.7) and rebind
    ///    with the `/cd` rule: the target tree's repo profiles win
    ///    (cd-rebind). The rebuild derives the prompt from
    ///    `sm.session_cwd()`, so a later `/model` switch keeps the new cwd
    ///    (the two axes are independent — #124).
    /// 4. Rebuild the agent — on the *current* model when the rebind kept
    ///    the pair, on the new agent's starting model when it switched —
    ///    then reset the conversation like `/new`. Tools snapshot the new
    ///    `session_cwd` at agent-build time, so the rebuild order is
    ///    `set_session_cwd` first, rebuild second.
    ///
    /// A failed model lookup or rebuild rolls the rebind back (bound +
    /// published selection): the fresh conversation never ran on the new
    /// selection, and the old conversation may still hold turns.
    ///
    /// The View pre-validated `path`; we re-validate defensively (config
    /// or the filesystem may have changed under us).
    async fn handle_change_cwd(
        path: &str,
        agent_slot: &mut Arc<DynAgent>,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) -> Result<(), String> {
        let Some(ctx) = rebuild_ctx else {
            return Err("/cd is not available in this session".to_string());
        };
        // Re-validate: the path must still be an existing directory.
        let target = std::path::Path::new(path);
        if !target.is_dir() {
            return Err(format!("not a directory: {path}"));
        }
        let sm = state_manager
            .clone()
            .ok_or_else(|| "state manager required for /cd".to_string())?;

        // Re-read config + skills before rebinding so a config edit lands
        // on this /cd. `session_cwd` is the **target** dir — we pass it
        // explicitly because process cwd never moves (no `set_current_dir`),
        // so the per-repo config and the skill re-scan must read the NEW
        // tree. Warnings are surfaced after the conversation reset below.
        let previous = ctx.bound.clone();
        let reload_warnings = reload_for_cd(ctx, &sm, target);

        // The model the fresh conversation runs on: the session's active
        // model when the rebind kept the pair, the new agent's starting
        // model when it switched. A failure rolls the rebind back — the
        // reload already published the new selection.
        let resolved = match model_after_cd(&previous, ctx, &sm) {
            Ok(model) => model,
            Err(e) => {
                ctx.bound = previous;
                publish_bound(&sm, &ctx.bound);
                return Err(e);
            }
        };

        // Kill bg processes BEFORE the session-cwd flip so no reader
        // thread straddles it — they were rooted in the old tree anyway
        // (ticket #124).
        sm.clear_bg();

        // The load-bearing act. Per-session only — no `set_current_dir`,
        // so two web sessions in different trees stay race-free. Must run
        // BEFORE the agent rebuild below, because `add_builtin_tools`
        // snapshots `sm.session_cwd()` at build time — and the rebuild seam
        // reads it to recompute the system prompt against the new cwd.
        sm.set_session_cwd(target.to_path_buf());

        // Refresh the welcome banner's cwd so the status bar / web banner
        // reflect the new directory (welcome was stamped once at boot).
        sm.update_welcome_cwd(target.to_path_buf());

        if let Err(e) = Self::rebuild_agent(
            &resolved,
            agent_slot,
            &sm,
            session_hook_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await
        {
            // The fresh conversation never ran on the new selection — roll
            // bound + published selection back to the pre-/cd pair.
            ctx.bound = previous;
            publish_bound(&sm, &ctx.bound);
            return Err(e);
        }

        // Reset conversation-scoped state — same path as /new and /model.
        // bg was already killed above (before the chdir); the unified
        // reset's clear_bg is a no-op on the now-empty registry.
        // `create_conversation` persists the new cwd into the metadata
        // so /load re-activates this tree.
        sm.reset_conversation_state();
        let convo_name = format!(
            "Conversation {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        sm.create_conversation(
            convo_name,
            resolved.provider_name.clone(),
            resolved.model_name.clone(),
            target.to_string_lossy().into_owned(),
        );
        sm.set_model(resolved.model_name.clone());
        sm.set_provider_name(resolved.provider_name.clone());
        sm.set_model_alias(resolved.alias.clone());

        sm.add_system_message(format!("📁 New conversation in {path}"));

        // Surface reload/skill warnings after the reset so they persist.
        for warning in reload_warnings {
            sm.add_system_message(warning);
        }

        Ok(())
    }

    /// Rebuild the live `DynAgent` on `model` for the context's `Bound` —
    /// the **shared seam** between `/model <alias>`, `/cd`, `/load`, `/new`
    /// and a selection. Recipe: prompt ([`rebuild_agent_for_resolved`]) →
    /// [`ToolEnv::for_profile`](crate::tools::env::ToolEnv::for_profile) →
    /// the delegate roster iff the agent has subagents → `create_provider`.
    /// Publishes the new `ProviderInfo` + `SessionHook` through their shared
    /// cells, re-inits the context manager, and restarts the event-processor
    /// task on the new receiver.
    ///
    /// `model` may differ from the agent's configured model (a `/model`
    /// switch); the agent's member settings (skills, env, agents.md) still
    /// come from the bound.
    ///
    /// Does NOT reset the conversation (chat/stats/todos) and does NOT
    /// emit a system banner — those are caller-specific:
    /// - `/model` calls `create_conversation(...)` afterwards (fresh
    ///   chat) and emits `🔁 New conversation on ...`.
    /// - `/load` calls `load_conversation(...)` afterwards (restore
    ///   saved chat) and emits `Loaded conversation: '...'`.
    async fn rebuild_agent(
        model: &ResolvedModel,
        agent_slot: &mut Arc<DynAgent>,
        sm_for_provider: &Arc<StateManager>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        state_manager: &Option<Arc<StateManager>>,
        ctx: &RebuildContext,
    ) -> Result<(), String> {
        let general = &ctx.config.general;

        // Validate the compaction model *before* mutating any state, so a
        // failure leaves the previous agent fully intact. See
        // unfuck-compact.md Layer 1.
        let compaction_model = compaction_model_for(model, &general.context).map_err(|e| {
            format!(
                "compaction model unavailable for `{}`: {e} — \
                 set general.context.enabled=false to switch without compaction",
                model.alias
            )
        })?;

        let (new_agent, new_info, new_receiver, new_hook) =
            build_bound_agent(model, ctx, sm_for_provider)?;

        // Thread the resolved reasoning gates into the shared StateManager
        // (single seam for every rebuild verb). The wire gate is
        // provider-name AND preserve_reasoning: a Claude transcript must stop
        // replaying `Reasoning` the moment a foreign provider owns the wire.
        // The display gate mirrors the resolved provider/model
        // `display_reasoning` override.
        //
        // Subagents share this StateManager but never touch these gates
        // (`build_sub_agent` produces no `ProviderInfo`): the gates describe
        // the *main agent's* wire, which is the only history rebuilt from
        // this state — a subagent runs on a fresh, lane-filtered history.
        sm_for_provider
            .set_wire_reasoning(new_info.name == "anthropic" && new_info.preserve_reasoning);
        sm_for_provider.set_display_reasoning(new_info.display_reasoning);
        sm_for_provider.set_supports_vision(new_info.supports_vision);

        // Publish through the shared cell *first* so the event_loop's /stop
        // path immediately sees the new hook.
        {
            let mut hook_guard = session_hook_cell.write().unwrap();
            *hook_guard = new_hook;
        }
        *agent_slot = Arc::new(new_agent);

        // Keep the welcome banner's model-scoped fields in sync so live
        // Views (web banner) show the model just switched to, not the
        // boot model. TUI's banner is a static string — unaffected.
        sm_for_provider.update_welcome_for_model(
            model.provider_name.clone(),
            model.model_name.clone(),
            model.provider_config.max_tokens() as usize,
            crate::config::cost_tracking_enabled(general.cost_tracking, model),
        );

        // Re-init the context manager with the pre-validated compaction
        // model so compaction thresholds match the new context window.
        let cm = ContextManager::new(
            general.context.clone(),
            model.context_size,
            compaction_model.clone(),
        );
        sm_for_provider.init_context_manager(cm);
        // The title model is the same as the compaction model — reuse the Arc
        if let Some(compaction) = compaction_model {
            sm_for_provider.init_title_model(compaction);
        }

        // Restart the event-processor task on the new receiver. The
        // receiver is `Option` because not every provider supports
        // event hooks (Ollama returns `None`); skip the spawn in that
        // case and we'll just have no event-driven UI updates.
        if let Some(handle) = event_processor.take() {
            handle.abort();
        }
        if let Some(rx) = new_receiver {
            let sm_for_processor = state_manager.clone();
            *event_processor = Some(tokio::spawn(async move {
                let mut rx = rx;
                while let Some(event) = rx.recv().await {
                    Self::process_event_for_ui(&sm_for_processor, event);
                }
            }));
        }

        Ok(())
    }

    /// After `/load` swapped the active conversation, re-bind the session to
    /// it and rebuild the agent so the next prompt runs on the loaded
    /// conversation's `(profile, agent)`, model and cwd.
    ///
    /// `/load` is a new binding (spec §6.7 step 6): the saved names go
    /// through `resolve_saved_selection` against a fresh config, read for
    /// the saved cwd — so the agent is always rebuilt when the load landed.
    /// `conversation_before` is the conversation id before the command; an
    /// unchanged id means `/load` failed (or re-loaded the current one) and
    /// the running conversation keeps its bound untouched.
    ///
    /// The error paths don't emit a system message for the no-op cases:
    /// `/load` itself has already reported a rejected load.
    async fn maybe_rebuild_after_load(
        conversation_before: Option<uuid::Uuid>,
        agent_slot: &mut Arc<DynAgent>,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) {
        let Some(sm) = state_manager else { return };
        let Some(ctx) = rebuild_ctx else { return };
        if sm.get_current_conversation_id() == conversation_before {
            return;
        }
        let Some(conv) = sm.get_current_conversation() else {
            return;
        };

        // The reload must read the tree the conversation is restored INTO:
        // `set_session_cwd` runs after it, so `sm.session_cwd()` would read
        // the pre-load tree's repo profiles and skills. An empty (pre-cwd
        // file) or vanished saved cwd falls back to the current tree.
        let saved_cwd = conv.cwd.clone();
        let saved_dir = (!saved_cwd.is_empty()).then(|| std::path::PathBuf::from(&saved_cwd));
        let restore_to = saved_dir.clone().filter(|p| p.is_dir());
        let reload_cwd = restore_to.clone().unwrap_or_else(|| sm.session_cwd());

        // The conversation was already loaded, so warnings land in it.
        let rebind = Rebind::Saved {
            profile: conv.profile.as_deref(),
            agent: conv.agent.as_deref(),
            subagent_models: &conv.subagent_models,
        };
        for warning in reload_config(ctx, sm, &reload_cwd, rebind) {
            sm.add_system_message(warning);
        }

        // Restore the saved cwd best-effort — per-session only, no
        // `set_current_dir`, so concurrent web sessions in other trees stay
        // race-free. A gone path is warned about, not fatal.
        if let Some(target) = restore_to {
            if target != sm.session_cwd() {
                // Kill bg processes rooted in the previous tree.
                sm.clear_bg();
                sm.set_session_cwd(target.clone());
                sm.update_welcome_cwd(target);
            }
        } else if saved_dir.is_some() {
            sm.add_system_message(format!(
                "⚠ /load: saved working directory no longer exists: {saved_cwd}"
            ));
        }

        // The saved wire id is the target; `/load` already refused a
        // conversation whose model is not configured. If it vanished in the
        // reload above, the bound agent's own model keeps the session usable.
        let resolved = ctx
            .config
            .registry
            .find_by_wire_id(&conv.provider_name, &conv.model)
            .unwrap_or(&ctx.bound.agent().main.model)
            .clone();
        if let Err(e) = Self::rebuild_agent(
            &resolved,
            agent_slot,
            sm,
            session_hook_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await
        {
            sm.add_system_message(format!("❌ /load: failed to rebuild agent: {e}"));
            return;
        }
        // Stamp display state to match the freshly-rebuilt agent.
        sm.set_provider_name(resolved.provider_name);
        sm.set_model(resolved.model_name);
        sm.set_model_alias(resolved.alias);
    }

    /// After `/new` has reset to a fresh conversation on the *active*
    /// model, re-read config + skills and rebuild the agent on that same
    /// model, so YAML/skill edits take effect on the new conversation
    /// without a restart. `/new` deliberately keeps your current model
    /// (it does not bounce you to the agent's configured model).
    ///
    /// No-op when the registry/state manager isn't available. If the
    /// active alias was removed from config, the rebuild is skipped and
    /// the session keeps running on the previous agent.
    async fn refresh_agent_after_new(
        agent_slot: &mut Arc<DynAgent>,
        state_manager: &Option<Arc<StateManager>>,
        session_hook_cell: &SharedSessionHook,
        event_processor: &mut Option<tokio::task::JoinHandle<()>>,
        rebuild_ctx: Option<&mut RebuildContext>,
    ) {
        let Some(sm) = state_manager else { return };
        let Some(ctx) = rebuild_ctx else { return };
        let sm_arc = sm.clone();

        // `/new` already reset to a fresh conversation before this runs, so
        // reload/skill warnings are safe to surface immediately. The fresh
        // conversation is zero-turn, so the reload re-resolves the carried
        // `(profile, agent)` in the fresh catalog (spec §6.7 step 6).
        let reload_warnings = reload_session_config(ctx, &sm_arc, &sm.session_cwd());
        for warning in reload_warnings {
            sm.add_system_message(warning);
        }

        // Re-resolve the active model against the fresh registry. Prefer
        // the active alias; fall back to the wire id. A removed alias
        // means the model vanished from config — keep the old agent.
        let Some(resolved) = ctx
            .config
            .registry
            .resolve(&sm.get_model_alias())
            .or_else(|| {
                ctx.config
                    .registry
                    .find_by_wire_id(&sm.get_provider_name(), &sm.get_model())
            })
            .cloned()
        else {
            return;
        };

        if let Err(e) = Self::rebuild_agent(
            &resolved,
            agent_slot,
            &sm_arc,
            session_hook_cell,
            event_processor,
            state_manager,
            ctx,
        )
        .await
        {
            sm.add_system_message(format!("❌ /new: failed to rebuild agent: {e}"));
        }
    }

    /// Process a `SourcedEvent` and update StateManager accordingly.
    ///
    /// This is the Controller's responsibility — it decides how domain events
    /// affect the UI state. The Model (StateManager) is passive and only holds data.
    ///
    /// The event carries the lane that produced it (`source`). Cost/token
    /// roll-up is **lane-agnostic** — a sub-agent's usage counts toward the
    /// parent `/stats` exactly like the orchestrator's (the #1 research fix:
    /// a delegation's cost must never be silent). The lane *is* honoured when
    /// stamping the resulting transcript ChatMessage, so the renderer can
    /// label a sub-agent's turns and `get_agent_history` can filter them out
    /// of the orchestrator's wire context.
    pub(crate) fn process_event_for_ui(
        state_manager: &Option<Arc<StateManager>>,
        sourced: SourcedEvent,
    ) {
        let sm = match state_manager {
            Some(sm) => sm,
            None => return,
        };

        let SourcedEvent { source, event } = sourced;

        match event {
            AgentEvent::CompletionResponse {
                content,
                usage,
                thinking,
                ..
            } => {
                // Roll tokens/cost into session stats, keyed by lane: the
                // parent `/stats` sees sub-agent cost too, and can break it
                // down per role.
                sm.add_request(&source, usage.input_tokens, usage.output_tokens, usage.cost);

                // Surface a **sub-agent's** prose on its own lane. The
                // orchestrator's prose is staged by the hook and committed once
                // per turn by `commit_reply`, so adding it here too would
                // double it — hence the orchestrator-lane guard. A
                // sub-agent's final answer legitimately appears twice in the
                // transcript: once here on its `🧩 role` lane (it *said* it) and
                // once as the orchestrator's `delegate` ToolResult (it *received*
                // it) — distinct lanes, not a duplicate bug.
                if !thinking.is_empty() && !source.is_orchestrator_lane() {
                    // Sub-agent emitted thinking — carry blocks onto its lane
                    // so the rebuilt orchestrator wire never sees them
                    // (is_orchestrator_lane filter on the rebuild side).
                    sm.add_assistant_message_with_thinking(source, content, thinking);
                } else if !source.is_orchestrator_lane() && !content.trim().is_empty() {
                    sm.add_assistant_message_sourced(source, content);
                }
            }
            AgentEvent::ToolCall {
                tool_name,
                arguments,
                call_id,
                response_id,
                ..
            } => {
                // Indicator phase → show the tool name in the working banner
                // (see `workin-baby.md` §6). Cleared on the matching result.
                sm.set_status(Some(tool_name.clone()));
                // Add to chat AND persist, stamped with the producing lane and
                // the response that requested the call.
                sm.add_tool_call(source, response_id, tool_name, arguments, call_id);
            }
            AgentEvent::ToolResult {
                tool_name,
                arguments,
                result,
                call_id,
                ..
            } => {
                // Back to "thinking" — the model is about to reason again.
                sm.set_status(None);
                // Add to chat AND persist, stamped with the producing lane.
                sm.add_tool_result(source, tool_name, arguments, result, call_id);
            }
            AgentEvent::CompletionRequest { .. }
            | AgentEvent::SessionStart { .. }
            | AgentEvent::SessionEnd { .. } => {
                // No UI update needed for these events
            }
        }
    }

    /// Internal process_message — takes the already-built current-turn `Message`
    /// (text or multimodal) and history from `StateManager`, runs the agent,
    /// writes the response back to state. Retries on transient errors.
    ///
    /// **In-loop compaction (`mid-compaction.md`).** When the wired
    /// `SessionHook` detects that the imminent wire payload would breach
    /// the context-window threshold, it terminates rig's agentic loop with
    /// reason `"compact"`. We catch that here, run `force_compact().await`
    /// synchronously, rebuild the `current_turn` from the (now compacted)
    /// `StateManager`, and re-enter `prompt_with_history`. The loop guard
    /// against infinite terminate-restart cycles is in
    /// `apply_compaction` — see that fn's doc and `SessionStats::clear_last_input_tokens`.
    async fn process_message_internal(
        current_turn: rig_core::completion::message::Message,
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
    ) -> CompletionResult {
        let result = Self::run_turn(current_turn, state_manager, agent, config).await;
        // Flushes prose staged this turn on exits that never reach a final
        // reply (Stop, Error); a no-op after a successful commit.
        if let Some(sm) = state_manager.as_ref() {
            sm.commit_reply("");
        }
        result
    }

    /// The retry / compaction / empty-reply-nudge loop behind
    /// [`Self::process_message_internal`].
    async fn run_turn(
        mut current_turn: rig_core::completion::message::Message,
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
    ) -> CompletionResult {
        let mut retry_count = 0;
        let mut nudged = false;
        // One-shot override for the post-compaction iteration. The compact
        // arm fills this with the resumption-shape history returned by
        // `build_resumption_from_tail()`; the loop top consumes it
        // via `.take()` on the very next iteration so the resumption
        // payload reaches the wire intact. Without this override, the
        // top-of-loop `get_agent_history()` re-derives history from
        // StateManager and DUPLICATES the resumption message (which is
        // also `current_turn`), breaking Anthropic / OpenAI conversation
        // invariants. See the data-layer pin
        // `production_resumption_payload_must_not_duplicate_toolresult`.
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;

        // Per-turn cancellation (#183): bind once at function entry. Cancelling
        // this token drops the turn's future — every descendant (wire request,
        // tool call, sub-agent, foreground PTY child via `PtyHandle::drop`) is
        // a child of the awaited `prompt_with_history` below, so the unwind is
        // complete. `biased` ensures the cancel arm wins on the same poll if
        // both it and a ready response race.
        let cancel = state_manager
            .as_ref()
            .map(|sm| sm.turn_cancel_token())
            .unwrap_or_default();

        loop {
            // Compaction is handled at the wire boundary by SessionHook
            // (gate in `on_completion_call`). The handler below catches
            // the resulting Terminate("compact") and rebuilds state.

            // Call the agent with history from StateManager (single source
            // of truth), unless the previous iteration set a one-shot
            // override (the post-compaction case — see `history_override`
            // above).
            let history = derive_history_for_iteration(&mut history_override, state_manager);
            // Stop = drop this future. Everything the turn owns is below it
            // (#183 design §0.1): the wire request, the tool call, the
            // sub-agent, the PTY child that dies through `PtyHandle::drop`.
            // There is deliberately no cancel arm inside the tools — adding
            // one would shadow this one (outer-observes-inner race) and
            // would still be unreachable code in production. The cancel arm
            // below answers the in-flight tool call, if any, as INTERRUPTED.
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    if let Some(sm) = state_manager {
                        sm.close_interrupted_tool_call();
                    }
                    return CompletionResult::Stopped;
                }
                r = agent.as_ref().prompt_with_history(current_turn.clone(), &history) => r,
            };

            match result {
                Ok(response) => {
                    let Some(sm) = state_manager.as_ref() else {
                        return CompletionResult::Success;
                    };
                    // One row per turn (staged prose + final text), persisted by
                    // StateManager.
                    if sm.commit_reply(&response) {
                        sm.set_final_broadcast(true);
                        // Fire-and-forget: upgrade the provisional title
                        // after the first reply
                        sm.maybe_generate_title();
                        return CompletionResult::Success;
                    }
                    if nudged {
                        sm.add_system_message(EMPTY_REPLY_WARNING.to_string());
                        sm.set_final_broadcast(true);
                        return CompletionResult::Success;
                    }
                    // Blank reply with nothing staged: ask once more. The nudge
                    // rides the wire only, so the transcript never shows it.
                    nudged = true;
                    tracing::warn!(target: "peakbot", "empty final reply; nudging the model once");
                    refresh_attempt_from_transcript(sm, &mut current_turn, &mut history_override);
                    let mut history =
                        derive_history_for_iteration(&mut history_override, state_manager);
                    history.push(std::mem::replace(
                        &mut current_turn,
                        rig_core::completion::message::Message::from(EMPTY_REPLY_NUDGE),
                    ));
                    history_override = Some(history);
                    continue;
                }

                Err(PromptError::PromptCancelled { reason, .. }) if reason == "compact" => {
                    // The hook decided the imminent wire payload is over budget.
                    // Run compaction synchronously, rebuild the current turn from
                    // the post-compaction StateManager, and re-enter the loop.
                    let Some(sm) = state_manager.as_ref() else {
                        // No StateManager — we can't compact. This shouldn't
                        // happen in production (the hook's gate only fires
                        // when one is wired), but bail loudly if it does.
                        return CompletionResult::Error;
                    };

                    sm.set_status(Some("Compacting context...".to_string()));
                    let compacted = sm.force_compact().await;
                    sm.set_status(None);

                    let Some(result) = compacted else {
                        // Compaction failed — abort the turn with an honest
                        // error. No retry, no clear_last_input_tokens dance.
                        // The next user turn gets a fresh attempt.
                        // See unfuck-compact.md Layer 2.
                        sm.add_system_message(
                            "❌ Compaction failed — the conversation could not be summarised. \
                             Aborting this turn. Try a new conversation, or check your \
                             compaction model configuration."
                                .to_string(),
                        );
                        sm.set_final_broadcast(true);
                        return CompletionResult::Error;
                    };

                    sm.add_system_message(format!(
                        "Context compacted: {} → {} messages, {} compacted",
                        result.original_count, result.compacted_count, result.num_discarded
                    ));

                    // Build the resumption prompt and history from the post-compaction
                    // StateManager. Unlike the initial dispatch path (which always
                    // uses the last User as the prompt), mid-action resumption
                    // must continue from whatever message the hook terminated
                    // with — which may be a ToolResult or Agent response, not
                    // necessarily a User. `refresh_attempt_from_transcript`
                    // handles this correctly by finding the actual last
                    // non-compacted message as the prompt and everything
                    // before it as history — the same helper the retry arm
                    // uses, so the two can never drift apart again.
                    //
                    // If it returns false (empty state or fresh turn), fall
                    // back to the normal initial-dispatch path for safety.
                    if !refresh_attempt_from_transcript(
                        sm,
                        &mut current_turn,
                        &mut history_override,
                    ) && let Some(turn) = sm.build_current_turn_message()
                    {
                        current_turn = turn;
                    }

                    // Loop. The next prompt_with_history call sees the
                    // compacted history and the correct resumption prompt.
                    // The hook's needs_compaction() now
                    // reads false (apply_compaction cleared last_input_tokens,
                    // OR our explicit clear above did), so it does NOT
                    // re-terminate.
                    continue;
                }

                // Graceful Stop: every tool call already has its real or
                // skipped result, so there is no in-flight call to close.
                Err(PromptError::PromptCancelled { reason, .. })
                    if reason == crate::hooks::session_hook::STOP_REASON =>
                {
                    return CompletionResult::Stopped;
                }

                Err(PromptError::PromptCancelled { .. }) => {
                    // Any other cancellation reason — return error instead of
                    // silently looping (which previously caused an infinite loop).
                    return CompletionResult::Error;
                }

                Err(e) => {
                    // Only transient failures (rate limits, 5xx, transport
                    // drops) are worth retrying; a deterministic 401 / bad
                    // request / MaxTurnsError bails immediately. See #111.
                    if !crate::providers::retry::is_transient_prompt_error(&e) {
                        if let Some(sm) = state_manager {
                            sm.set_status(None);
                            sm.add_system_message(format!(
                                "❌ LLM request failed: {}",
                                crate::ui::app_state::truncate_str(&e.to_string(), 2000)
                            ));
                        }
                        return CompletionResult::Error;
                    }
                    if retry_count >= config.general.retry.max_retries {
                        if let Some(sm) = state_manager {
                            sm.set_status(None);
                            sm.add_system_message(format!(
                                "❌ LLM request failed after {} retries: {}",
                                config.general.retry.max_retries,
                                crate::ui::app_state::truncate_str(&e.to_string(), 2000)
                            ));
                        }
                        return CompletionResult::Error;
                    }
                    let delay =
                        crate::providers::retry::backoff_delay(retry_count, &config.general.retry);
                    tracing::warn!(
                        target: "peakbot",
                        attempt = retry_count + 1,
                        max_retries = config.general.retry.max_retries,
                        backoff_ms = delay.as_millis(),
                        error = %e,
                        "LLM request failed transiently; backing off before retry"
                    );
                    if let Some(sm) = state_manager {
                        sm.set_status(Some(format!(
                            "Retrying (attempt {}/{}) after {:.1}s…",
                            retry_count + 1,
                            config.general.retry.max_retries,
                            delay.as_secs_f64()
                        )));
                    }
                    // The wire call may have failed *mid-turn*, after a tool
                    // round-trip was already executed and persisted. Re-derive
                    // the attempt so the retry continues from the transcript
                    // tail instead of replaying the original user turn on top
                    // of a history that already contains it.
                    if let Some(sm) = state_manager {
                        refresh_attempt_from_transcript(
                            sm,
                            &mut current_turn,
                            &mut history_override,
                        );
                    }
                    tokio::time::sleep(delay).await;
                    retry_count += 1;
                }
            }
        }
    }

    /// Resolve a user-typed conversation reference into a UUID.
    ///
    /// Accepts two forms:
    /// - **1-based ordinal index** (e.g. `1`, `2`, `42`) — looked up in the
    ///   current `list_conversations()` sorted newest-first. This is the
    ///   primary, human-friendly path. Indices are deliberately stateless:
    ///   re-derived on every call so there is no cached mapping to drift out
    ///   of sync with storage.
    /// - **Full UUID** — fallback for scripts, log copy-paste, and the rare
    ///   case where index addressing isn't enough.
    ///
    /// The two formats are syntactically disjoint (digits vs. hyphenated
    /// 36-char string), so the parse-int-first / parse-uuid-second strategy
    /// has no ambiguity. See `conversazione.md` § "The shared resolver" for
    /// the full design rationale.
    fn resolve_conversation_id(arg: &str, sm: &StateManager) -> Result<uuid::Uuid, String> {
        // Try integer first — common case, cheaper to parse, and fails fast
        // (no allocation) when the user typed a UUID.
        if let Ok(n) = arg.parse::<usize>() {
            if n == 0 {
                return Err(
                    "Indices are 1-based. Use /conversations to see available items.".to_string(),
                );
            }
            let mut list = sm
                .list_conversations()
                .ok_or_else(|| "Conversation storage is not configured.".to_string())?;
            list.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
            return list.get(n - 1).map(|c| c.id).ok_or_else(|| {
                format!(
                    "Index {} not found. /conversations shows {} saved conversation(s).",
                    n,
                    list.len()
                )
            });
        }
        // Fallback: full UUID.
        uuid::Uuid::parse_str(arg).map_err(|_| {
            "Invalid argument. Pass an index from /conversations or a full UUID.".to_string()
        })
    }

    /// Drain any pending background-process output and, if non-empty,
    /// inject it as a synthetic user turn and run an agent turn over
    /// it. No-op when buffers are empty OR the capped-tier circuit
    /// breaker is suppressing.
    ///
    /// Called by the agent loop in two places (the "two seams" of
    /// `bash-background.md`):
    /// 1. After a `UserMessage` turn completes (post-turn drain).
    /// 2. On `QueueMessage::BackgroundOutputReady` while the loop is
    ///    parked (idle wake-up).
    ///
    /// Both paths share the same drain + dispatch shape, so the helper
    /// guarantees they behave identically — the only difference is who
    /// triggers them.
    async fn run_bg_synthetic_turn_if_any(
        state_manager: &Option<Arc<StateManager>>,
        agent: &Arc<DynAgent>,
        config: &Config,
        completion_tx: &tokio::sync::broadcast::Sender<CompletionResult>,
    ) {
        let Some(sm) = state_manager.as_ref() else {
            return;
        };
        let Some(synthetic) = sm.drain_bg_output_into_synthetic_turn() else {
            return;
        };
        // Persist the synthetic message with the bg discriminator so
        // /load restores it with the right styling and proc-id
        // provenance.
        sm.add_user_message_from_background(synthetic.text.clone(), synthetic.proc_ids.clone());
        sm.set_running(true);
        let current_turn = sm.build_current_turn_message().unwrap_or_else(|| {
            rig_core::completion::message::Message::from(synthetic.text.as_str())
        });
        let result =
            Self::process_message_internal(current_turn, state_manager, agent, config).await;
        sm.set_running(false);
        // No "worked for …" system row here — bg-driven turns are
        // ambient by design; the user already saw the bg block.
        completion_tx.send(result).ok();
    }

    /// Internal process_command
    async fn process_command_internal(
        cmd: &str,
        state_manager: &Option<Arc<StateManager>>,
        config: &Config,
    ) {
        let cmd_lower = cmd.to_lowercase();

        match cmd_lower.as_str() {
            // /reset was previously in the no-op arm below alongside /stats,
            // /context, etc. with the comment "UI pulls this data". The fossil
            // assumption was that the UI would notice the slash command and
            // handle it itself — but nothing in the REPL does, so the command
            // was silently inert. Per the derived-view-invalidation rule in
            // memory.md (2026-04-24 /new bug), when a reset command is advertised
            // in the popup as "Reset session statistics" (see
            // `ui_trait::builtin_commands`), the handler must actually reset
            // something visible to the user. /reset zeros the counters and keeps
            // the conversation; /new is the one that clears chat history too.
            "/reset" => {
                if let Some(sm) = state_manager {
                    sm.reset_stats();
                    sm.add_system_message("Session statistics reset.".to_string());
                } else {
                    tracing::warn!("State manager not available for /reset command");
                }
            }
            "/stats" => {
                if let Some(sm) = state_manager {
                    let stats = sm.get_stats();
                    let state = sm.get_state();
                    let model_display = if state.stats.model_alias.is_empty() {
                        state.stats.model.clone()
                    } else {
                        state.stats.model_alias.clone()
                    };
                    let msg = format!(
                        "## Session Statistics\n\n\
                         **Model:** {}\n\
                         **API Calls:** {}\n\
                         **Total Cost:** ${:.4}\n\
                         **Input Tokens (last request):** {}\n\
                         **Output Tokens (last request):** {}\n\
                         **Cumulative Input Tokens:** {}",
                        model_display,
                        stats.total_api_calls,
                        stats.total_cost,
                        stats.total_input_tokens,
                        stats.total_output_tokens,
                        stats.cumulative_input_tokens(),
                    );
                    // Per-lane breakdown — only shown once a sub-agent has
                    // run; a single orchestrator lane adds no information
                    // over the flat totals above.
                    let lanes = stats.lanes_sorted();
                    let msg = if lanes.len() > 1 {
                        let rows: String = lanes
                            .iter()
                            .map(|(name, l)| {
                                format!(
                                    "\n| {} | {} | {} | {} | ${:.4} |",
                                    name, l.input_tokens, l.output_tokens, l.api_calls, l.cost
                                )
                            })
                            .collect();
                        format!(
                            "{msg}\n\n### By lane (cumulative)\n\n\
                             | Lane | In | Out | Calls | Cost |\n|---|---|---|---|---|{rows}"
                        )
                    } else {
                        msg
                    };
                    sm.add_system_message(msg);
                } else {
                    tracing::warn!("State manager not available for /stats command");
                }
            }
            "/context" => {
                if let Some(sm) = state_manager {
                    let state = sm.get_state();
                    let needs = sm.needs_compaction();
                    let usage_pct = state.context.usage_percentage();
                    let msg = format!(
                        "## Context Usage\n\n\
                         **Messages:** {}\n\
                         **Context Tokens:** {} / {} ({:.1}%)\n\
                         **Compaction:** {}\n\
                         **Threshold:** {:.0}%{}",
                        state.chat.messages.len(),
                        state.context.current_usage,
                        state.context.window_size,
                        usage_pct,
                        if state.context.compaction_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        state.context.compaction_threshold * 100.0,
                        if needs {
                            " — ⚠️ threshold reached"
                        } else {
                            ""
                        },
                    );
                    sm.add_system_message(msg);
                } else {
                    tracing::warn!("State manager not available for /context command");
                }
            }
            "/compact" => {
                // /compact is an ACTION, not a data-display command.
                // It was mistakenly grouped with /stats and /context above.
                if let Some(sm) = state_manager {
                    // Same status the auto/in-loop path emits, so the working
                    // spinner reads "Compacting context..." instead of falling
                    // back to the default "thinking" label.
                    sm.set_status(Some("Compacting context...".to_string()));
                    let outcome = sm.force_compact().await;
                    sm.set_status(None);
                    match outcome {
                        Some(result) => {
                            sm.add_system_message(format!(
                                "Context compacted: {} → {} messages, {} discarded",
                                result.original_count, result.compacted_count, result.num_discarded
                            ));
                        }
                        None => {
                            sm.add_system_message("Nothing to compact.".to_string());
                        }
                    }
                } else {
                    tracing::warn!("State manager not available for /compact command");
                }
            }
            "/conversations" => {
                // List saved conversations as a system message, addressed by
                // 1-based ordinal index. UUIDs are an implementation detail
                // and deliberately hidden from the user — the index is the
                // human-facing handle that pairs with /load <n>, /delete <n>,
                // and /export <n>. UUIDs are still accepted as a fallback in
                // those commands via `resolve_conversation_id`. See
                // `conversazione.md` for the full design.
                if let Some(sm) = state_manager {
                    match sm.list_conversations() {
                        None => sm.add_system_message(
                            "Conversation storage is not configured.".to_string(),
                        ),
                        Some(list) if list.is_empty() => {
                            sm.add_system_message("No saved conversations.".to_string())
                        }
                        Some(mut list) => {
                            list.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
                            let current_id = sm.get_current_conversation_id();
                            let mut msg = format!("Saved conversations ({}):\n", list.len());
                            for (idx, c) in list.iter().enumerate() {
                                let n = idx + 1;
                                let marker = if Some(c.id) == current_id {
                                    "▶ "
                                } else {
                                    "   "
                                };
                                // Availability check resolves on the
                                // wire identity `(provider_name, model)`
                                // — the stable persistence key.
                                // Aliases are NOT consulted (they're
                                // mutable user handles).
                                let resolved =
                                    config.registry.find_by_wire_id(&c.provider_name, &c.model);
                                let model_tag = match resolved {
                                    Some(r) => {
                                        // Show the current alias as a
                                        // hint next to the wire id —
                                        // helps the user spot which
                                        // entry in their config will
                                        // re-activate.
                                        format!("{} · {}/{}", r.alias, c.provider_name, c.model)
                                    }
                                    None => {
                                        let display_id = if c.provider_name.is_empty() {
                                            // Pre-v5 file with no
                                            // provider_name on disk.
                                            format!("(pre-v5) {}", c.model)
                                        } else {
                                            format!("{}/{}", c.provider_name, c.model)
                                        };
                                        format!("{display_id} ⚠ unavailable")
                                    }
                                };
                                msg.push_str(&format!(
                                    "{}{:>3}  {}  {} msgs  {}  {}\n",
                                    marker,
                                    n,
                                    c.name,
                                    c.message_count,
                                    c.updated_at
                                        .with_timezone(&chrono::Local)
                                        .format("%Y-%m-%d %H:%M"),
                                    model_tag,
                                ));
                            }
                            msg.push_str(
                                "\nUse /load <n> to resume, /delete <n> to remove. UUIDs also accepted.",
                            );
                            sm.add_system_message(msg);
                        }
                    }
                } else {
                    tracing::warn!("State manager not available for /conversations command");
                }
            }
            "/exit" => {
                // No-confirmation quit. Bypasses the Ctrl+C confirmation
                // dialog on purpose — /exit is an explicit request.
                //
                // Cannot flip `ReplUi::running` directly from here (we're
                // in the agent loop), so we set `AppState.exit_requested`
                // via StateManager. The view observes it each tick and
                // breaks its run loop. See `request_exit` docs.
                //
                // No system banner: the terminal is about to be cleared
                // by `ReplUi::shutdown` and any flash of text would be
                // pointless noise. If you *want* a visible "Goodbye",
                // add an `add_system_message` here — but honestly, the
                // cleanest exit is a silent one.
                if let Some(sm) = state_manager {
                    sm.request_exit();
                } else {
                    tracing::warn!("State manager not available for /exit command");
                }
            }
            "/bg" => {
                // Human-facing listing of background processes — mirrors
                // what `bash_bg list` returns to the model. See
                // `bash-background.md` open Q7.
                if let Some(sm) = state_manager {
                    let rows = sm.list_bg();
                    if rows.is_empty() {
                        sm.add_system_message("No background processes.".to_string());
                    } else {
                        let mut msg = format!("Background processes ({}):\n", rows.len());
                        for r in rows {
                            let cooldown = if r.cooldown.is_zero() {
                                "real-time".to_string()
                            } else {
                                format!("{}s cooldown", r.cooldown.as_secs())
                            };
                            let status = match r.status {
                                crate::bg_processes::BgStatus::Running { .. } => {
                                    "running".to_string()
                                }
                                crate::bg_processes::BgStatus::Exited { code, .. } => {
                                    format!("exited({code})")
                                }
                            };
                            let label = r
                                .label
                                .as_ref()
                                .map(|l| format!(" [{l}]"))
                                .unwrap_or_default();
                            msg.push_str(&format!(
                                "  #{} pid={} 🛰 {} · {} · {}/{} lines · {}{}\n",
                                r.id,
                                r.pid,
                                cooldown,
                                status,
                                r.buffer_len,
                                r.capture_cap,
                                r.command,
                                label,
                            ));
                        }
                        sm.add_system_message(msg);
                    }
                } else {
                    tracing::warn!("State manager not available for /bg command");
                }
            }
            "/help" => {
                // Derive the help text from `builtin_commands()` so the popup
                // menu, the dispatcher, and /help stay in lockstep.
                // See `allehailmenu.md` §9.
                if let Some(sm) = state_manager {
                    let mut msg = String::from("Available commands:\n");
                    for cmd in crate::ui::ui_trait::builtin_commands() {
                        let args = if cmd.takes_args { " <args>" } else { "" };
                        msg.push_str(&format!("  /{}{} — {}\n", cmd.name, args, cmd.description));
                    }
                    sm.add_system_message(msg);
                } else {
                    tracing::warn!("State manager not available for /help command");
                }
            }
            "/new" => {
                // Create new conversation via StateManager (single source of truth).
                //
                // `create_conversation` alone only swaps the current-conversation
                // slot; the derived views (chat, stats, todos, bg, bash panel)
                // survive. `reset_conversation_state` clears them all so /new
                // doesn't lie about starting fresh.
                if let Some(sm) = state_manager {
                    sm.reset_conversation_state();
                    let name = format!(
                        "Conversation {}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M")
                    );
                    // Read the active wire identity from AppState — set
                    // at boot and refreshed on every rebuild, so it always
                    // matches the running agent.
                    let provider_name = sm.get_provider_name();
                    let model = sm.get_model();
                    sm.create_conversation(
                        name,
                        provider_name,
                        model,
                        sm.session_cwd().to_string_lossy().into_owned(),
                    );
                    sm.add_system_message("Started a new conversation.".to_string());
                } else {
                    tracing::warn!("State manager not available for /new command");
                }
            }
            "/save" => {
                // Explicit save via StateManager
                if let Some(sm) = state_manager {
                    sm.save_conversation();
                    if let Some(conv) = sm.get_current_conversation() {
                        sm.add_system_message(format!("Conversation saved: {}", conv.name));
                    }
                } else {
                    tracing::warn!("State manager not available for /save command");
                }
            }
            _ if cmd_lower.starts_with("/load ") => {
                if let Some(arg) = cmd.strip_prefix("/load ") {
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /load command");
                        return;
                    };
                    let id = match Self::resolve_conversation_id(arg.trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };

                    // Pre-flight wire-id check: peek at the saved
                    // file's `(provider_name, model)` BEFORE any
                    // teardown so the current conversation survives a
                    // failed load. The wire id is the **stable**
                    // re-activation key — aliases are mutable user
                    // handles in `config.yaml` and are NEVER consulted
                    // here. *(persisted artifacts must carry every
                    // field needed to be re-activated)*
                    let (saved_provider, saved_model) = match sm.peek_conversation_wire_id(id) {
                        Ok(t) => t,
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to read conversation: {e}"));
                            return;
                        }
                    };
                    let resolved = config
                        .registry
                        .find_by_wire_id(&saved_provider, &saved_model)
                        .cloned();
                    // We only consult `resolved` as an availability
                    // check here — the actual stamping of display
                    // state happens inside
                    // `agent_loop::maybe_rebuild_after_load` after a
                    // successful agent rebuild. See the NOTE in the
                    // success branch below.
                    let Some(_resolved) = resolved else {
                        let display_id = if saved_provider.is_empty() {
                            saved_model.clone()
                        } else {
                            format!("{saved_provider}/{saved_model}")
                        };
                        sm.add_system_message(format!("❌ Model '{display_id}' not available."));
                        return;
                    };

                    match sm.load_conversation(id) {
                        Ok(()) => {
                            // NOTE: do NOT pre-stamp display state
                            // (`provider_name` / `model` / `model_alias`)
                            // here. `agent_loop::maybe_rebuild_after_load`
                            // runs immediately after this returns and
                            // uses the still-boot-identity vs the
                            // saved identity as its rebuild guard:
                            //   if sm.get_provider_name() == saved_provider
                            //   && sm.get_model() == saved_model { return; }
                            // Pre-stamping here makes that guard
                            // vacuously true on every load → the
                            // rebuild silently no-ops → the agent
                            // keeps its boot wire id and boot context
                            // window → the user gets the wrong model
                            // and the status bar lies. The display
                            // state is correctly stamped *after* a
                            // successful rebuild, inside
                            // `maybe_rebuild_after_load`. Regression
                            // pin: `tests::load_does_not_prestamp_display_identity`.
                            // See `bugs-opus.md` §"Issue 3+4+5".
                            if let Some(conv) = sm.get_current_conversation() {
                                sm.add_system_message(format!(
                                    "Loaded conversation: '{}'",
                                    conv.name
                                ));
                            }
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to load conversation: {e}"));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/delete ") => {
                if let Some(arg) = cmd.strip_prefix("/delete ") {
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /delete command");
                        return;
                    };
                    let id = match Self::resolve_conversation_id(arg.trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };
                    match sm.delete_conversation(id) {
                        Ok(_) => {
                            sm.add_system_message("Conversation deleted.".to_string());
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Failed to delete: {}", e));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/export ") => {
                if let Some(args) = cmd.strip_prefix("/export ") {
                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                    let Some(sm) = state_manager else {
                        tracing::warn!("State manager not available for /export command");
                        return;
                    };
                    if parts.len() != 2 {
                        sm.add_system_message(
                            "Usage: /export <n|uuid> <json|markdown>".to_string(),
                        );
                        return;
                    }
                    let id = match Self::resolve_conversation_id(parts[0].trim(), sm) {
                        Ok(id) => id,
                        Err(msg) => {
                            sm.add_system_message(format!("❌ {}", msg));
                            return;
                        }
                    };
                    let format = parts[1].to_lowercase();
                    match sm.export_conversation(id, &format) {
                        Ok(output) => {
                            sm.add_system_message(format!("Export:\n{}", output));
                        }
                        Err(e) => {
                            sm.add_system_message(format!("❌ Export failed: {}", e));
                        }
                    }
                }
            }
            _ if cmd_lower.starts_with("/rename ") => {
                if let Some(name) = cmd.strip_prefix("/rename ") {
                    if let Some(sm) = state_manager {
                        match sm.rename_conversation(name.to_string()) {
                            Ok(_) => {
                                sm.add_system_message(format!("Conversation renamed to: {}", name));
                            }
                            Err(e) => {
                                sm.add_system_message(format!("❌ Failed to rename: {}", e));
                            }
                        }
                    } else {
                        tracing::warn!("State manager not available for /rename command");
                    }
                }
            }
            // `/model` — list available models (with the active one
            // marked). `/model <alias>` never reaches here: the TUI
            // intercepts it in the View and `classify_submission` routes it
            // to `SubmitKind::SwitchModel` for web/stdio. This handler runs
            // for the bare `/model` (no arg) case,
            // which includes `/model` followed only by trailing
            // whitespace — the slash-command popup completes `/model`
            // to `/model ` (trailing space, since the command is
            // declared with `takes_args = true`), so a user who hits
            // Enter without typing an alias must still land on the
            // listing path. See `tests::model_with_trailing_whitespace_lists_available_models`.
            _ if cmd_lower.trim_end() == "/model" => {
                let Some(sm) = state_manager else {
                    tracing::warn!("State manager not available for /model command");
                    return;
                };
                let registry = &config.registry;
                let aliases = registry.aliases_sorted();
                if aliases.is_empty() {
                    sm.add_system_message(
                        "No models declared. Add a `providers:` block to config.yaml.".to_string(),
                    );
                    return;
                }
                let current = sm.get_model_alias();
                let mut msg = String::from("Available models:\n");
                for alias in &aliases {
                    let arrow = if &current == alias { "→ " } else { "  " };
                    if let Some(resolved) = registry.resolve(alias) {
                        msg.push_str(&format!(
                            "{arrow}{alias}  ({} · {})\n",
                            resolved.provider_name, resolved.model_name
                        ));
                    }
                }
                msg.push_str("\nUse /model <alias> to switch (starts a new conversation).");
                sm.add_system_message(msg);
            }
            _ if cmd_lower.trim_end() == "/cd" => {
                // Bare /cd — print the session's working directory for
                // orientation. Pure read; works in every View. Use the
                // per-session cwd, not the process-global — the two can
                // differ in web sessions after `/cd`.
                if let Some(sm) = state_manager {
                    sm.add_system_message(sm.session_cwd().to_string_lossy().into_owned());
                }
            }
            _ if cmd_lower.starts_with("/cd ") => {
                // The View should have intercepted `/cd <path>` (validate
                // + confirm + emit ChangeCwd). Reaching here means a
                // frontend with no cwd dialog (stdio) — emit a helpful
                // diagnostic rather than a silent no-op.
                if let Some(sm) = state_manager {
                    sm.add_system_message(
                        "❌ /cd: changing the session directory isn't \
                         supported in this frontend — use the terminal UI \
                         (`peakbot --tui`) to change it."
                            .to_string(),
                    );
                }
            }
            _ => {
                // Unknown command — for now, let the agent handle it
                // The agent will respond via StateManager
            }
        }
    }
}

/// Derive the chat history that the next `prompt_with_history` call will
/// receive, honouring a one-shot override.
///
/// Two callers in the loop:
///
/// 1. **Normal turn**: `override_` is `None` → returns `get_agent_history()`
///    from `StateManager` (the source of truth, single trailing-User strip).
/// 2. **Post-compaction iteration**: `override_` was filled by the compact
///    arm with the resumption-shape history from
///    `StateManager::build_resumption_from_tail()`. We `.take()` it so
///    the next iteration falls back to the normal path.
///
/// **Why this exists as a separate function:** the production bug fixed
/// in the 2026-05-09 cleanup pass was that the compact arm wrote the
/// resumption history to a loop-body-local `history` variable that was
/// shadowed and re-derived on the next iteration via
/// `get_agent_history()`. The override was effectively a no-op and the
/// resumption message ended up duplicated on the wire (once as the
/// prompt, once as the trailing entry of history). Pulling the
/// derivation out into a named function makes the override-vs-derivation
/// contract explicit and unit-testable; without that, the only way to
/// catch the regression would be a full mock-driven runtime test of the
/// agent loop. See the regression pins
/// `derive_history_for_iteration_*` in this module.
fn derive_history_for_iteration(
    override_: &mut Option<Vec<rig_core::completion::message::Message>>,
    state_manager: &Option<Arc<StateManager>>,
) -> Vec<rig_core::completion::message::Message> {
    match override_.take() {
        Some(h) => h,
        None => state_manager
            .as_ref()
            .map(|sm| sm.get_agent_history())
            .unwrap_or_default(),
    }
}

/// Re-derive the next wire attempt — prompt AND history together — from the
/// transcript's live tail. Taking them from one snapshot is the whole point:
/// a prompt captured before a tool round-trip landed, replayed on a history
/// captured after it, duplicates the user turn (Defect 3).
///
/// `false` means there is nothing to resume from (a fresh, single-message
/// turn); the caller keeps the turn it already holds, which is correct there.
fn refresh_attempt_from_transcript(
    sm: &StateManager,
    current_turn: &mut rig_core::completion::message::Message,
    history_override: &mut Option<Vec<rig_core::completion::message::Message>>,
) -> bool {
    match sm.build_resumption_from_tail() {
        Some((prompt, history)) => {
            *current_turn = prompt;
            *history_override = Some(history);
            true
        }
        None => false,
    }
}

/// Handle for a connected MCP server.
///
/// Holds both the tools and the service connection. The service connection
/// must be kept alive for as long as the tools are used, and should be
/// properly closed on drop to avoid the "RunningService dropped without
/// explicit close()" warning.
pub struct McpServerHandle {
    name: String,
    tools: Vec<McpTool>,
    /// The running service connection. Must be closed on drop for clean shutdown.
    /// This uses a wrapper enum since stdio and HTTP transports return different
    /// service types internally.
    service: McpService,
}

/// Wrapper enum for MCP service connections.
///
/// Both stdio and HTTP transports use `RunningService<RoleClient, ()>` when acting
/// as a client, but the inner service type differs (child process vs HTTP client).
/// This enum allows us to store the correct type while providing a uniform interface.
enum McpService {
    Stdio(Option<RunningService<RoleClient, ()>>),
    Http(Option<RunningService<RoleClient, ()>>),
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Take the service out so we don't double-close
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            // Spawn a task to close the service properly
            // This avoids blocking in Drop while ensuring clean shutdown
            tokio::spawn(async move {
                match s.close().await {
                    Ok(reason) => {
                        tracing::debug!("MCP service closed: {:?}", reason);
                    }
                    Err(e) => {
                        tracing::warn!("MCP service close error: {:?}", e);
                    }
                }
            });
        }
    }
}

impl McpServerHandle {
    /// The server's configured name — the key a profile's `mcps:`
    /// gate addresses.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Take ownership of the tools, consuming this handle.
    ///
    /// Note: The MCP service connection will be closed when this handle is dropped.
    /// For explicit control over service shutdown, use `close_and_take_tools()` instead.
    pub fn into_tools(self) -> Vec<Box<dyn ToolDyn>> {
        // Use ManuallyDrop to prevent our Drop impl from running
        // since we're consuming the handle intentionally
        let this = std::mem::ManuallyDrop::new(self);

        // Access fields through the ManuallyDrop'd reference
        this.tools
            .iter()
            .cloned()
            .map(|tool| Box::new(tool) as Box<dyn ToolDyn>)
            .collect()
    }

    /// Close the MCP service connection and get the tools.
    ///
    /// This properly closes the service before extracting tools.
    pub async fn close_and_take_tools(mut self) -> Vec<Box<dyn ToolDyn>> {
        // Close the service first (extract from Option via take on &mut self)
        // We need to do this carefully since we can't move out of self.service
        // Take the service out by matching on &mut self
        let service = match &mut self.service {
            McpService::Stdio(s) => s.take(),
            McpService::Http(s) => s.take(),
        };

        if let Some(mut s) = service {
            s.close().await.ok();
        }

        self.into_tools()
    }
}

/// Flatten per-server MCP tool groups into the flat tool list an agent
/// receives, honouring a profile's `mcps:` gate. Servers keep their
/// input order, tools keep their per-server order; `None` = no gate
/// (every server's tools — today's behaviour), `enabled: false` = none.
/// The servers themselves keep running either way — this only decides
/// which servers' tools reach the agent.
///
/// Takes the groups BY VALUE (not by reference): `Box<dyn ToolDyn>` cannot
/// be cloned (`ToolDyn` carries no `Clone` bound — most tools, including
/// the plain `Tool` blanket impl, have no way to duplicate themselves), so
/// this is a pure ownership filter/flatten rather than a copy. Callers that
/// need the groups again build them twice (cheap: `McpTool: Clone`).
pub fn mcp_tools_for_profile(
    servers: Vec<(String, Vec<Box<dyn ToolDyn>>)>,
    filter: Option<&crate::config::NameFilter>,
) -> Vec<Box<dyn ToolDyn>> {
    servers
        .into_iter()
        .filter(|(name, _)| filter.is_none_or(|f| f.allows(name)))
        .flat_map(|(_, tools)| tools.into_iter())
        .collect()
}

pub async fn connect_mcp_server(config: &McpServerConfig) -> Result<McpServerHandle> {
    // Validate configuration
    if let Err(e) = config.validate() {
        return Err(anyhow::anyhow!("Invalid MCP server config: {}", e));
    }

    match config.transport_type() {
        McpTransportType::Stdio => connect_mcp_stdio(config).await,
        McpTransportType::Sse => {
            // rmcp 0.16 dropped the dedicated SSE client transport. The MCP
            // spec (2025-03-26) replaced SSE with Streamable HTTP, which is
            // wire-compatible with most existing "sse" servers. We route
            // there and warn loudly so users aren't surprised.
            tracing::warn!(
                "MCP server '{}': transport_type 'sse' is deprecated; \
                 routing through streamable-http. Set 'type: streamable-http' \
                 in your config to silence this warning.",
                config.name
            );
            connect_mcp_http(config).await
        }
        McpTransportType::StreamableHttp => connect_mcp_http(config).await,
    }
}

/// Connect to an MCP server using stdio transport (spawns a local process)
async fn connect_mcp_stdio(config: &McpServerConfig) -> Result<McpServerHandle> {
    let command = config
        .command
        .as_ref()
        .ok_or_else(|| anyhow!("stdio transport requires 'command'"))?;

    let mut cmd = tokio::process::Command::new(command);
    if let Some(args) = &config.args {
        cmd.args(args);
    }
    if let Some(env) = &config.env {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }

    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to create child process: {}", e))?;

    let service = ()
        .serve(transport)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?;

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Stdio(Some(service)),
    })
}

/// Connect to an MCP server using HTTP transport (remote server)
async fn connect_mcp_http(config: &McpServerConfig) -> Result<McpServerHandle> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow!("HTTP transport requires 'url'"))?;

    tracing::info!("Connecting to MCP server '{}' at {}", config.name, url);

    // Surface deprecation warnings (e.g. legacy `auth_token`) at connect
    // time. `validate()` already rejected the "both set" footgun, so this
    // is purely informational.
    for warning in config.deprecation_warnings() {
        tracing::warn!("{warning}");
    }

    // Build the streamable-http config with optional bearer token + custom headers.
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.clone());
    // Lazily holds the OAuth-aware HTTP client when the server uses oauth.
    // `None` means the default reqwest client baked into `from_config` will be used.
    let mut auth_client: Option<crate::mcp_auth::AuthorizedClient> = None;

    match config.auth_resolved() {
        Some(crate::config::ResolvedAuth::Bearer { token }) => {
            transport_config = transport_config.auth_header(token);
        }
        Some(crate::config::ResolvedAuth::Oauth {
            client_id,
            client_secret,
            scopes,
        }) => {
            // Slice 2 + Slice 3a: full OAuth 2.1 + PKCE flow.
            // - `client_id` absent → DCR (Linear shape)
            // - `client_id` present → static credentials (Google shape)
            // `authorize` returns an `AuthClient<reqwest::Client>` that
            // implements `StreamableHttpClient`, so we feed it into the
            // transport via `with_client` below.
            let params = crate::mcp_auth::OauthParams {
                client_id,
                client_secret,
                scopes,
            };
            auth_client = Some(
                crate::mcp_auth::authorize(&config.name, url, params)
                    .await
                    .map_err(|e| anyhow!("MCP server '{}': {e}", config.name))?,
            );
        }
        None => {}
    }

    if let Some(headers) = config.headers.as_ref() {
        let mut parsed: std::collections::HashMap<::http::HeaderName, ::http::HeaderValue> =
            std::collections::HashMap::with_capacity(headers.len());
        for (k, v) in headers {
            match (
                ::http::HeaderName::try_from(k.as_str()),
                ::http::HeaderValue::try_from(v.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    parsed.insert(name, value);
                }
                _ => {
                    tracing::warn!(
                        "MCP server '{}': skipping invalid header '{}: {}'",
                        config.name,
                        k,
                        v
                    );
                }
            }
        }
        if !parsed.is_empty() {
            transport_config = transport_config.custom_headers(parsed);
        }
    }

    // `StreamableHttpClientTransport<C>` is generic over the inner HTTP
    // client, so the two branches produce different concrete types and
    // can't be unified by a `let transport = if ...`. Drive `.serve`
    // from inside each arm — the resulting `service` (`RunningService<…>`)
    // has the same type regardless because the transport type-parameter
    // is erased by the worker boundary.
    let service = if let Some(client) = auth_client {
        // OAuth-aware client: signs every request and silently refreshes
        // access tokens.
        ().serve(StreamableHttpClientTransport::with_client(
            client,
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    } else {
        ().serve(StreamableHttpClientTransport::with_client(
            crate::http::client(),
            transport_config,
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to MCP server: {}", e))?
    };

    let server_info = service
        .peer_info()
        .ok_or_else(|| anyhow!("Can't get MCP info"))?;
    tracing::info!(
        "Connected to MCP server '{}': ({}:{})",
        config.name,
        server_info.server_info.name,
        server_info.server_info.version
    );

    let tools = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| McpTool::from_mcp_server(tool, service.peer().clone()))
        .collect::<Vec<_>>();

    tracing::info!("MCP server '{}' has {} tools", config.name, tools.len());

    // Store the service in our wrapper enum, keeping a clone for the tools
    Ok(McpServerHandle {
        name: config.name.clone(),
        tools,
        service: McpService::Http(Some(service)),
    })
}

pub async fn load_mcp_servers(config: &Config) -> Result<Vec<McpServerHandle>> {
    let mut handles = Vec::new();

    let servers = &config.mcps;
    if servers.is_empty() {
        tracing::info!("No MCP servers configured");
        return Ok(Vec::new());
    }

    for server_config in servers {
        if !server_config.enabled {
            continue;
        }

        tracing::info!("Connecting to MCP server: {}", server_config.name);
        match connect_mcp_server(server_config).await {
            Ok(handle) => {
                handles.push(handle);
            }
            Err(e) => {
                tracing::error!(
                    "Failed to connect to MCP server '{}': {}",
                    server_config.name,
                    e
                );
            }
        }
    }

    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::MessageSource;
    use std::collections::HashMap;

    /// One openrouter model + one profile: the replacement for the deleted
    /// `Config::default()`. Compaction is off so `AgentRunner::new` needs no
    /// compaction provider.
    const TEST_GLOBAL_YAML: &str = r#"
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-test
    models:
      - { name: anthropic/claude-3.7-sonnet, alias: sonnet }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: sonnet
"#;

    fn test_config() -> Config {
        use crate::config::resolve::{ParseCtx, Source};
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: TEST_GLOBAL_YAML,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture config must parse")
    }

    /// The wire identity the mock-agent tests boot on — no registry entry
    /// needed, `AgentRunner::new` only reads its context size and provider.
    #[cfg(feature = "mock")]
    fn mock_resolved_model() -> ResolvedModel {
        ResolvedModel {
            alias: "mock".to_string(),
            model_name: "mock-model".to_string(),
            provider_name: "mock".to_string(),
            provider_kind: crate::config::ProviderType::OpenRouter,
            provider_config: crate::config::ProviderConfig::default(),
            context_size: 100_000,
        }
    }

    /// Prompt-shape fixture: memory on/off across profiles, `agents_md`
    /// on/off across agents, one subagent-bearing agent (the old
    /// "orchestrator recipe").
    const PROMPT_GLOBAL_YAML: &str = r#"
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-test
    models:
      - { name: anthropic/claude-3.7-sonnet, alias: sonnet }
profiles:
  main:
    agents:
      solo:
        model: sonnet
      no-agents-md:
        model: sonnet
        agents_md: false
      team:
        model: sonnet
        agents_md: true
        subagents:
          helper:
            prompt: help
      team-no-md:
        model: sonnet
        agents_md: false
        subagents:
          helper:
            prompt: help
  no-memory:
    memory:
      enabled: false
    agents:
      solo:
        model: sonnet
"#;

    /// The `(profile, agent)` of the prompt fixture, for `build_agent_prompt`.
    fn prompt_bound(profile: &str, agent: &str) -> Bound {
        use crate::config::resolve::{ParseCtx, Source};
        let cfg = Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: PROMPT_GLOBAL_YAML,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("prompt fixture must parse");
        cfg.catalog
            .resolve(profile, agent)
            .expect("fixture bound resolves")
    }

    // --- panic_text (shared catch_unwind payload renderer) -------------------

    /// `panic_text` must read the two payload shapes `std::panic::catch_unwind`
    /// actually produces for the macros we use (`panic!("literal")` → `&str`,
    /// `panic!("{}", x)` → `String`) and fall back to a fixed string for
    /// anything else (`std::panic::panic_any` with a non-string payload).
    /// Shared by `agent_loop`'s turn-level backstop and `TimeBudget::call`'s
    /// per-tool panic boundary — this is the one place both learn to read a
    /// caught panic's payload.
    #[test]
    fn panic_text_reads_str_and_string_payloads() {
        let str_payload = std::panic::catch_unwind(|| panic!("lit"))
            .expect_err("the closure panics, so catch_unwind must return Err");
        assert_eq!(panic_text(str_payload), "lit");

        let string_payload = std::panic::catch_unwind(|| panic!("{}", 42))
            .expect_err("the closure panics, so catch_unwind must return Err");
        assert_eq!(panic_text(string_payload), "42");

        let other_payload =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| std::panic::panic_any(7u8)))
                .expect_err("the closure panics, so catch_unwind must return Err");
        assert_eq!(panic_text(other_payload), "unknown panic");
    }

    // --- /help handler tests -------------------------------------------------

    #[tokio::test]
    async fn help_command_emits_system_message_listing_all_builtin_commands() {
        let sm = StateManager::new_arc();
        let config = test_config();

        AgentRunner::process_command_internal("/help", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let system_msgs: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            system_msgs.len(),
            1,
            "/help should produce exactly one system message"
        );
        let body = &system_msgs[0].content;

        // Header
        assert!(body.starts_with("Available commands:"));

        // Every command in the list must appear in the help text
        for cmd in crate::ui::ui_trait::builtin_commands() {
            let needle = format!("/{}", cmd.name);
            assert!(
                body.contains(&needle),
                "/help output missing command: {}",
                needle
            );
            assert!(
                body.contains(&cmd.description),
                "/help output missing description for {}: {}",
                needle,
                cmd.description
            );
        }
    }

    // --- system-prompt shell awareness (#82, PR3) ---------------------------------
    //
    // The shell-specific guidance lives in a rendered `# Shell Tool
    // Usage` section (from `system_prompt_shell.txt` with {shell_tool} /
    // {bg_tool} / {output_dir} placeholders) that only exists when a shell
    // was detected. The core is shell-neutral, and on a PowerShell host the
    // WHOLE prompt must be bash-free (case-insensitive).

    #[test]
    fn system_prompt_powershell_agentless_recipe_names_both_tools_and_hides_bash() {
        // Isolated cwd: the repo's own agents.md mentions bash/PowerShell and
        // would be injected into the prompt under test.
        let cwd = tempfile::tempdir().unwrap();
        let skills = SkillRegistry::new();
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let bound = prompt_bound("main", "solo");
        let prompt =
            build_agent_prompt(bound.agent(), &bound.profile, &skills, Some(ps), cwd.path());
        // The env line + the shell section both name the shell tool…
        assert!(
            prompt.contains("**Shell**: powershell"),
            "PowerShell env line missing from prompt"
        );
        assert!(
            prompt.contains("`powershell`"),
            "prompt must name the `powershell` tool on a PowerShell host"
        );
        // …and the bg tool (PR3: the env line names BOTH tools).
        assert!(
            prompt.contains("`powershell_bg`"),
            "prompt must name the `powershell_bg` tool on a PowerShell host"
        );
        // The shell section is rendered with its new heading and the real
        // output dir (shell_output_dir()), no unrendered placeholders.
        assert!(
            prompt.contains("# Shell Tool Usage"),
            "the `# Shell Tool Usage` section must be rendered for a detected shell"
        );
        assert!(
            prompt.contains(&crate::tools::shell_output_dir().display().to_string()),
            "the shell section must embed shell_output_dir() (no hard-coded /tmp/peakbot)"
        );
        for placeholder in ["{shell_tool}", "{bg_tool}", "{output_dir}"] {
            assert!(
                !prompt.contains(placeholder),
                "unrendered placeholder {placeholder} leaked into the prompt"
            );
        }
        // The old PR2 phrasing is gone…
        assert!(
            !prompt.contains("do not apply"),
            "the PR2 'bash examples elsewhere do not apply' phrasing must be gone"
        );
        // …and the ENTIRE single-agent prompt is bash-free on a PowerShell host.
        assert!(
            !prompt.to_lowercase().contains("bash"),
            "a PowerShell prompt must contain no 'bash' (case-insensitive) anywhere"
        );
    }

    #[test]
    fn system_prompt_powershell_orchestrator_recipe_same_contract() {
        // The subagent-bearing agent must carry the same shell section and
        // bash-free guarantee as the single-agent one.
        let cwd = tempfile::tempdir().unwrap();
        let skills = SkillRegistry::new();
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let bound = prompt_bound("main", "team");
        let prompt =
            build_agent_prompt(bound.agent(), &bound.profile, &skills, Some(ps), cwd.path());
        assert!(
            prompt.contains("`powershell`"),
            "subagent-bearing prompt must name the `powershell` tool"
        );
        assert!(
            prompt.contains("`powershell_bg`"),
            "subagent-bearing prompt must name the `powershell_bg` tool"
        );
        assert!(
            prompt.contains("# Shell Tool Usage"),
            "subagent-bearing prompt must render the `# Shell Tool Usage` section"
        );
        assert!(
            prompt.contains(&crate::tools::shell_output_dir().display().to_string()),
            "subagent-bearing shell section must embed shell_output_dir()"
        );
        for placeholder in ["{shell_tool}", "{bg_tool}", "{output_dir}"] {
            assert!(
                !prompt.contains(placeholder),
                "unrendered placeholder {placeholder} leaked into the prompt"
            );
        }
        assert!(
            !prompt.to_lowercase().contains("bash"),
            "a PowerShell subagent-bearing prompt must contain no 'bash' (case-insensitive)"
        );
    }

    #[test]
    fn system_prompt_bash_recipe_names_bash_tools_and_keeps_no_powershell() {
        let cwd = tempfile::tempdir().unwrap();
        let skills = SkillRegistry::new();
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let bound = prompt_bound("main", "solo");
        let prompt = build_agent_prompt(
            bound.agent(),
            &bound.profile,
            &skills,
            Some(bash),
            cwd.path(),
        );
        assert!(
            prompt.contains("**Shell**: bash"),
            "bash env line missing from prompt"
        );
        assert!(
            prompt.contains("`bash_bg`"),
            "prompt must name the `bash_bg` tool on a Bash host"
        );
        assert!(
            prompt.contains("# Shell Tool Usage"),
            "the `# Shell Tool Usage` section must be rendered for a Bash shell too"
        );
        assert!(
            !prompt.contains("PowerShell"),
            "bash host must not carry PowerShell guidance"
        );
    }

    #[test]
    fn system_prompt_without_shell_has_no_shell_section_and_no_bash() {
        // No shell (Windows with nothing installed): the shell section does
        // not exist at all, the prompt carries no `_bg` tool name, and the
        // core is shell-neutral — so no "bash" reference survives anywhere.
        let cwd = tempfile::tempdir().unwrap();
        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "solo");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, cwd.path());
        assert!(
            !prompt.contains("# Shell Tool Usage"),
            "no shell ⇒ no `# Shell Tool Usage` section"
        );
        assert!(
            !prompt.contains("_bg"),
            "no shell ⇒ no bg tool name in the prompt; got a `_bg` reference"
        );
        assert!(
            !prompt.to_lowercase().contains("bash"),
            "a no-shell prompt must be bash-free (the core is shell-neutral)"
        );
    }

    #[test]
    fn system_prompt_stamps_passed_cwd_and_reads_its_agents_md() {
        // A temp dir that is NOT the process cwd, carrying its own agents.md.
        let dir = std::env::temp_dir().join(format!(
            "peakbot-prompt-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.md"), "SENTINEL-AGENTS-CONTENT").unwrap();

        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "solo");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);

        assert!(
            prompt.contains(&dir.to_string_lossy().to_string()),
            "env block must stamp the passed cwd, not the process cwd"
        );
        assert!(
            prompt.contains("SENTINEL-AGENTS-CONTENT"),
            "agents.md must be read from the passed cwd"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_prompt_includes_memory_section_when_enabled() {
        // The `main` profile has memory on by default; a clean temp dir (no
        // agents.md) keeps the assertion scoped to the memory section.
        let dir = std::env::temp_dir().join(format!(
            "peakbot-mem-on-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "solo");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);
        assert!(
            prompt.contains("# Memory.md"),
            "memory section must be present when memory is enabled"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_prompt_omits_memory_section_when_disabled() {
        let dir = std::env::temp_dir().join(format!(
            "peakbot-mem-off-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let skills = SkillRegistry::new();
        let bound = prompt_bound("no-memory", "solo");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);
        assert!(
            !prompt.contains("# Memory.md"),
            "memory section must be omitted when memory is disabled"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `agents_md: false` omits the agents.md content from the main agent's
    /// system prompt (the agent's `agents_md: false` ceiling).
    #[test]
    fn system_prompt_omits_agents_md_when_disabled() {
        let dir = std::env::temp_dir().join(format!(
            "peakbot-agents-md-off-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.md"), "SENTINEL-AGENTS-OFF").unwrap();

        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "no-agents-md");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);
        assert!(
            !prompt.contains("SENTINEL-AGENTS-OFF"),
            "agents_md: false must omit the agents.md content"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `agents_md: true` (the default) keeps the agents.md content.
    #[test]
    fn system_prompt_includes_agents_md_when_enabled() {
        let dir = std::env::temp_dir().join(format!(
            "peakbot-agents-md-on-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.md"), "SENTINEL-AGENTS-ON").unwrap();

        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "solo");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);
        assert!(
            prompt.contains("SENTINEL-AGENTS-ON"),
            "agents_md: true must inject the agents.md content"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The gate applies to subagent-bearing agents too — one ceiling for the
    /// whole session's main agent.
    #[test]
    fn system_prompt_omits_agents_md_for_subagent_agent_too() {
        let dir = std::env::temp_dir().join(format!(
            "peakbot-agents-md-orch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agents.md"), "SENTINEL-AGENTS-ORCH").unwrap();

        let skills = SkillRegistry::new();
        let bound = prompt_bound("main", "team-no-md");
        let prompt = build_agent_prompt(bound.agent(), &bound.profile, &skills, None, &dir);
        assert!(
            !prompt.contains("SENTINEL-AGENTS-ORCH"),
            "the agents_md gate must hold for a subagent-bearing agent as well"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn help_command_marks_arg_taking_commands_with_args_placeholder() {
        let sm = StateManager::new_arc();
        let config = test_config();

        AgentRunner::process_command_internal("/help", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let body = &state
            .chat
            .messages
            .iter()
            .find(|m| matches!(m.role, crate::ui::MessageRole::System))
            .expect("system message")
            .content;

        // Arg-taking commands should have " <args>" after their name
        assert!(body.contains("/load <args>"));
        assert!(body.contains("/delete <args>"));
        assert!(body.contains("/export <args>"));
        assert!(body.contains("/rename <args>"));

        // No-arg commands must NOT have the placeholder
        assert!(body.contains("/stats —") || body.contains("/stats"));
        assert!(!body.contains("/stats <args>"));
        assert!(!body.contains("/help <args>"));
    }

    // --- /conversations + index-resolver tests -------------------------------
    //
    // The shared `resolve_conversation_id` helper underpins /load, /delete and
    // /export. The /conversations command itself is the index-publishing
    // surface. See `conversazione.md` for the full design.

    /// Minimal helper: build an Arc<StateManager> backed by a fresh
    /// `InMemoryStorage`. Inline `pub use` would be cleaner — but importing
    /// the storage type once at function level keeps the test module's
    /// surface narrow.
    fn sm_with_storage() -> Arc<StateManager> {
        use crate::storage::InMemoryStorage;
        StateManager::new_arc_with_storage(Arc::new(InMemoryStorage::new()))
    }

    /// Build a StateManager pre-seeded with conversations at explicit
    /// `updated_at` offsets (in hours, relative to now). Returns the
    /// `(name, id)` pairs in **insertion order** (NOT display order — display
    /// order is newest-first, derived by the dispatcher at call time).
    ///
    /// Implementation note: we cannot use `StateManager::save_conversation`
    /// here because it stamps `updated_at = now` on every call, defeating the
    /// purpose. Instead we save directly through the storage trait, which is
    /// the source of truth, and then wrap it in the StateManager. No
    /// test-only accessor on StateManager required.
    fn sm_with_seeded(specs: &[(&str, i64)]) -> (Arc<StateManager>, Vec<(String, uuid::Uuid)>) {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use chrono::Utc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let mut out = Vec::new();
        // Seed with the wire identity that matches the fixture registry:
        // provider "openrouter" + the one configured model. This way the
        // `/load` wire-id check accepts these fixtures.
        let provider_name = "openrouter".to_string();
        let model_wire_id = "anthropic/claude-3.7-sonnet".to_string();
        for (name, hours_ago) in specs {
            let mut conv = Conversation::new(
                (*name).to_string(),
                provider_name.clone(),
                model_wire_id.clone(),
                String::new(),
            );
            conv.updated_at = Utc::now() - chrono::Duration::hours(*hours_ago);
            storage.save(&conv).expect("seed save");
            out.push((name.to_string(), conv.id));
        }
        let sm = StateManager::new_arc_with_storage(storage);
        (sm, out)
    }

    fn last_system_msg(sm: &StateManager) -> String {
        sm.get_state()
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::ui::MessageRole::System))
            .expect("at least one system message")
            .content
            .clone()
    }

    #[tokio::test]
    async fn conversations_empty_list_emits_explicit_message() {
        let sm = sm_with_storage();
        let config = test_config();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        assert_eq!(last_system_msg(&sm), "No saved conversations.");
    }

    #[tokio::test]
    async fn conversations_no_storage_emits_distinct_message() {
        let sm = StateManager::new_arc(); // no storage
        let config = test_config();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        assert_eq!(
            last_system_msg(&sm),
            "Conversation storage is not configured."
        );
    }

    #[tokio::test]
    async fn conversations_lists_every_saved_name_with_index() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = test_config();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        for (name, _) in &seeded {
            assert!(
                body.contains(name),
                "missing name {} in body:\n{}",
                name,
                body
            );
        }
        // Indices 1, 2, 3 should appear right-aligned in the row prefix.
        assert!(body.contains("  1  "), "missing index 1 row:\n{}", body);
        assert!(body.contains("  2  "), "missing index 2 row:\n{}", body);
        assert!(body.contains("  3  "), "missing index 3 row:\n{}", body);
    }

    #[tokio::test]
    async fn conversations_sorted_newest_first() {
        // Seeded in jumbled order: 3h ago, 1h ago, 2h ago.
        let (sm, _) = sm_with_seeded(&[("Older", 3), ("Newest", 1), ("Middle", 2)]);
        let config = test_config();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        let pos_newest = body.find("Newest").expect("Newest present");
        let pos_middle = body.find("Middle").expect("Middle present");
        let pos_older = body.find("Older").expect("Older present");
        assert!(
            pos_newest < pos_middle && pos_middle < pos_older,
            "wrong order in body:\n{}",
            body
        );
    }

    #[tokio::test]
    async fn conversations_marks_current() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = test_config();
        // Set Beta as current.
        sm.load_conversation(seeded[1].1).expect("load Beta");
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        // ▶ marker should appear exactly once.
        assert_eq!(
            body.matches("▶ ").count(),
            1,
            "expected exactly one ▶ marker, body:\n{}",
            body
        );
        // It should prefix Beta's row (verify by line containment).
        let beta_line = body
            .lines()
            .find(|l| l.contains("Beta"))
            .expect("Beta line");
        assert!(
            beta_line.contains("▶ "),
            "▶ should mark Beta row:\n{}",
            beta_line
        );
    }

    #[tokio::test]
    async fn conversations_hides_uuids() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = test_config();
        AgentRunner::process_command_internal("/conversations", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        // A UUID is 8-4-4-4-12 hex chars with hyphens. Robust check: no
        // hyphenated 36-char tokens anywhere in the rendered body.
        for token in body.split_whitespace() {
            assert!(
                !(token.len() == 36 && token.matches('-').count() == 4),
                "found UUID-shaped token in /conversations output: {}\nFull body:\n{}",
                token,
                body
            );
        }
    }

    // --- /load + resolver tests ---------------------------------------------

    #[tokio::test]
    async fn load_by_index_resolves_to_correct_uuid() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = test_config();
        // Newest-first: Alpha=1h ago, Beta=2h, Gamma=3h. So index 2 → Beta.
        AgentRunner::process_command_internal("/load 2", &Some(sm.clone()), &config).await;
        let beta_id = seeded[1].1;
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(beta_id),
            "/load 2 should load Beta"
        );
    }

    #[tokio::test]
    async fn load_by_uuid_still_works() {
        let (sm, seeded) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = test_config();
        let alpha_id = seeded[0].1;
        let cmd = format!("/load {}", alpha_id);
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;
        assert_eq!(sm.get_current_conversation_id(), Some(alpha_id));
    }

    #[tokio::test]
    async fn load_index_zero_emits_helpful_error() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1)]);
        let config = test_config();
        AgentRunner::process_command_internal("/load 0", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("1-based"),
            "expected '1-based' guidance, got:\n{}",
            body
        );
        assert!(
            body.contains("/conversations"),
            "expected pointer to /conversations, got:\n{}",
            body
        );
    }

    #[tokio::test]
    async fn load_index_out_of_range_emits_count() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2)]);
        let config = test_config();
        AgentRunner::process_command_internal("/load 99", &Some(sm.clone()), &config).await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("99"),
            "error should cite the bad index:\n{}",
            body
        );
        assert!(body.contains("2"), "error should cite the count:\n{}", body);
    }

    #[tokio::test]
    async fn load_invalid_arg_emits_uniform_error() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1)]);
        let config = test_config();
        AgentRunner::process_command_internal(
            "/load not-a-uuid-or-int",
            &Some(sm.clone()),
            &config,
        )
        .await;
        let body = last_system_msg(&sm);
        assert!(
            body.contains("Invalid argument"),
            "expected resolver's invalid-argument message, got:\n{}",
            body
        );
    }

    /// `/load` of a saved conversation whose wire identity
    /// `(provider_name, model)` is no longer in the registry must
    /// reject the load with the canonical
    /// `Model 'provider/model' not available.` error AND leave the
    /// previously active conversation untouched. v5 contract:
    /// aliases are NOT consulted; only the wire identity is the
    /// stable re-activation key. *(persisted artifacts must carry
    /// every field needed to be re-activated)*
    #[tokio::test]
    async fn load_with_unavailable_wire_id_returns_clean_error_and_does_not_teardown() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Seed one convo whose wire id won't be in the legacy synth
        // registry (provider name "ghost" doesn't match the synth's
        // "openrouter").
        let mut conv = Conversation::new(
            "Stranger".to_string(),
            "ghost-provider".to_string(),
            "ghost-model".to_string(),
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Pre-state: no current conversation loaded.
        assert!(sm.get_current_conversation_id().is_none());

        let config = test_config();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        let body = last_system_msg(&sm);
        assert!(
            body.contains("Model 'ghost-provider/ghost-model' not available."),
            "expected canonical unavailable-model error, got:\n{body}"
        );
        // Critical: the failed load must NOT have torn down whatever
        // was previously current. Since pre-state was None, current
        // must still be None.
        assert!(
            sm.get_current_conversation_id().is_none(),
            "failed /load must leave current conversation untouched"
        );
    }

    /// Pre-v5 conversations on disk default `provider_name` to the
    /// empty string. The wire-id lookup misses cleanly and `/load`
    /// rejects with the canonical error (the user is expected to
    /// retire them or re-save under v5).
    #[tokio::test]
    async fn load_pre_v5_file_treated_as_unavailable() {
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Hand-craft a pre-v5 JSON (no `provider_name` field). Round-
        // trip via serde_json so deserialization populates the
        // `#[serde(default)]` empty string for `provider_name`.
        let id = uuid::Uuid::new_v4();
        let json = format!(
            r#"{{
                "id": "{id}",
                "name": "Old",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "messages": [],
                "model": "anthropic/claude-3.7-sonnet",
                "metadata": {{"message_count": 0}}
            }}"#
        );
        let conv: crate::conversation::Conversation =
            serde_json::from_str(&json).expect("parse pre-v5 fixture");
        assert_eq!(conv.provider_name, "");
        storage.save(&conv).expect("seed save");
        let sm = StateManager::new_arc_with_storage(storage);

        let config = test_config();
        let cmd = format!("/load {id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        let body = last_system_msg(&sm);
        assert!(
            body.contains("not available"),
            "expected unavailable-model error for pre-v5 file, got:\n{body}"
        );
    }

    /// **Regression pin (issue #3+4+5 from `bugs-opus.md`).** `/load`
    /// must NOT pre-stamp the StateManager's display identity
    /// (`provider_name` / `model` / `model_alias`) with the loaded
    /// conversation's saved identity. Stamping is the responsibility
    /// of `agent_loop::maybe_rebuild_after_load`, which runs *after*
    /// `/load` and uses the still-boot-identity as the rebuild guard
    /// (`if sm.get_provider_name() == saved_provider && sm.get_model()
    /// == saved_model { return; }`). When `/load` pre-stamps, that
    /// guard becomes vacuously true on every load, the rebuild is
    /// silently skipped, and the agent keeps its boot wire id and
    /// boot context window — so the user gets the wrong model and
    /// the status bar lies. See `bugs-opus.md` §"Issue 3+4+5".
    #[tokio::test]
    async fn load_does_not_prestamp_display_identity() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        // Seed a conv on the fixture identity that will resolve
        // (`openrouter` + the configured model) so the wire-id check
        // accepts it and `/load` reaches the success branch.
        let model = "anthropic/claude-3.7-sonnet".to_string();
        let mut conv = Conversation::new(
            "Saved".to_string(),
            "openrouter".to_string(),
            model,
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Establish a *different* boot identity on the StateManager.
        // The rebuild guard in `maybe_rebuild_after_load` compares
        // these against the saved identity — they must remain distinct
        // through `/load` so the rebuild fires.
        sm.set_provider_name("boot-prov".to_string());
        sm.set_model("boot-model".to_string());
        sm.set_model_alias("boot-alias".to_string());

        let config = test_config();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        // The conversation swap itself must succeed.
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(saved_id),
            "/load should swap the active conversation"
        );
        // …but the display identity must still be the boot identity,
        // so `maybe_rebuild_after_load`'s guard sees a mismatch and
        // actually rebuilds the agent. Pre-stamping here is the bug.
        assert_eq!(
            sm.get_provider_name(),
            "boot-prov",
            "/load must not pre-stamp provider_name — that's maybe_rebuild_after_load's job"
        );
        assert_eq!(
            sm.get_model(),
            "boot-model",
            "/load must not pre-stamp model — that's maybe_rebuild_after_load's job"
        );
        assert_eq!(
            sm.get_model_alias(),
            "boot-alias",
            "/load must not pre-stamp model_alias — that's maybe_rebuild_after_load's job"
        );
    }

    /// `/load` of a conversation whose wire id resolves under a
    /// renamed alias must succeed: the alias change in `config.yaml`
    /// is invisible to persistence. v5 contract.
    #[tokio::test]
    async fn load_succeeds_when_alias_is_renamed_but_wire_id_is_stable() {
        use crate::conversation::Conversation;
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let model = "anthropic/claude-3.7-sonnet".to_string();
        // Save under the legacy synth identity (provider "openrouter",
        // default model). The alias was "default" at save time —
        // but we don't even care, we never wrote it.
        let mut conv = Conversation::new(
            "Saved".to_string(),
            "openrouter".to_string(),
            model,
            String::new(),
        );
        conv.updated_at = chrono::Utc::now() - chrono::Duration::hours(1);
        storage.save(&conv).expect("seed save");
        let saved_id = conv.id;
        let sm = StateManager::new_arc_with_storage(storage);

        // Use a fresh Config — the alias may have been rebuilt under
        // any name in config.yaml; doesn't matter, the wire-id
        // lookup is alias-blind.
        let config = test_config();
        let cmd = format!("/load {saved_id}");
        AgentRunner::process_command_internal(&cmd, &Some(sm.clone()), &config).await;

        assert_eq!(
            sm.get_current_conversation_id(),
            Some(saved_id),
            "/load should resolve by wire id regardless of alias rename"
        );
    }

    // --- /delete + /export by index -----------------------------------------

    #[tokio::test]
    async fn delete_by_index_works() {
        let (sm, _) = sm_with_seeded(&[("Alpha", 1), ("Beta", 2), ("Gamma", 3)]);
        let config = test_config();
        // /delete 1 → newest (Alpha). After: 2 conversations remain.
        AgentRunner::process_command_internal("/delete 1", &Some(sm.clone()), &config).await;
        let remaining = sm.list_conversations().expect("storage").len();
        assert_eq!(remaining, 2, "/delete 1 should remove one of three");
    }

    #[tokio::test]
    async fn export_by_index_works() {
        let (sm, _) = sm_with_seeded(&[("OnlyOne", 1)]);
        let config = test_config();
        AgentRunner::process_command_internal("/export 1 markdown", &Some(sm.clone()), &config)
            .await;
        let body = last_system_msg(&sm);
        assert!(
            body.starts_with("Export:"),
            "export header missing:\n{}",
            body
        );
        assert!(
            body.contains("OnlyOne"),
            "export should include the conversation name:\n{}",
            body
        );
    }

    // --- /new handler tests --------------------------------------------------
    //
    // Regression guard for the "/new doesn't actually start a new conversation"
    // bug: `sm.create_conversation(...)` only swaps the current-conversation
    // slot; without also clearing `chat.messages` and resetting stats, the
    // agent's next turn still sees the old history (since `get_agent_history()`
    // derives from `chat.messages`) and the token/cost counters keep climbing.

    #[tokio::test]
    async fn new_command_clears_chat_messages() {
        let sm = StateManager::new_arc();
        let config = test_config();

        // Seed a conversation with some user/assistant turns
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi there".to_string());
        assert_eq!(sm.get_state().chat.messages.len(), 2);

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        // After /new, the only remaining message should be the "Started a new
        // conversation." system banner — the prior user/assistant turns must
        // be gone so the next prompt doesn't carry them into the agent.
        let state = sm.get_state();
        let non_system: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| !matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert!(
            non_system.is_empty(),
            "/new must clear user/assistant/tool messages; got {:?}",
            non_system.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn new_command_resets_session_stats() {
        let sm = StateManager::new_arc();
        let config = test_config();

        // Seed the stats as if a request had happened
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 1234;
            stats.total_output_tokens = 567;
            stats.total_api_calls = 3;
            stats.total_cost = 0.42;
        }

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 0, "/new must zero input tokens");
        assert_eq!(stats.total_output_tokens, 0, "/new must zero output tokens");
        assert_eq!(stats.total_api_calls, 0, "/new must zero api calls");
        assert_eq!(stats.total_cost, 0.0, "/new must zero cost");
    }

    #[tokio::test]
    async fn new_command_clears_todo_list() {
        // /new starts a fresh conversation, and the todo list is conceptually
        // scoped to "the work the user is doing right now". Carrying todos
        // from the previous conversation into a new one is the same class of
        // bug as carrying chat history: stale state leaking across a
        // user-initiated reset. See the docs above the /new handler.
        let sm = StateManager::new_arc();
        let config = test_config();

        // Seed the todo list with two tasks
        sm.add_todo("write the docs".to_string());
        sm.add_todo("ship the bug fix".to_string());
        assert_eq!(sm.get_todo_list().list().len(), 2);

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let todos = sm.get_todo_list();
        assert!(
            todos.list().is_empty(),
            "/new must clear the todo list; got {:?}",
            todos.list().iter().map(|t| &t.task).collect::<Vec<_>>()
        );
        // Also verify the UI-facing view was synced (otherwise the panel
        // would keep displaying the dead tasks until the next state update).
        assert!(
            sm.get_state().todo.items.is_empty(),
            "/new must sync the cleared todo list to the UI state"
        );
    }

    #[tokio::test]
    async fn new_command_swaps_in_a_fresh_conversation() {
        let sm = StateManager::new_arc();
        let config = test_config();

        sm.create_conversation(
            "old one".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            String::new(),
        );
        let old_id = sm
            .get_current_conversation_id()
            .expect("seeded conversation");

        AgentRunner::process_command_internal("/new", &Some(sm.clone()), &config).await;

        let new_id = sm
            .get_current_conversation_id()
            .expect("/new must create a current conversation");
        assert_ne!(new_id, old_id, "/new must produce a fresh conversation id");
    }

    // --- /model listing handler tests --------------------------------------
    //
    // Regression guard for "bare `/model` falls through the View interceptor
    // and into the controller's `/model ` (with-args) arm because the slash-
    // command popup completes `/model` to `/model ` (trailing space, since
    // `takes_args = true`). The user submits `/model ` and gets the
    // "not available in this build" diagnostic instead of the listing.
    // See memory.md episodic entry 2026-05-07.

    fn config_with_two_aliases() -> Config {
        use crate::config::resolve::{ParseCtx, Source};
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: r#"
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-test
    models:
      - { name: anthropic/claude-3.7-sonnet, alias: sonnet }
      - { name: openai/gpt-4o, alias: gpt4o }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: sonnet
"#,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("two-alias config must parse")
    }

    fn last_system_message(sm: &Arc<StateManager>) -> String {
        sm.get_state()
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::ui::app_state::MessageRole::System))
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn bare_model_command_lists_available_models() {
        let sm = StateManager::new_arc();
        let config = config_with_two_aliases();

        AgentRunner::process_command_internal("/model", &Some(sm.clone()), &config).await;

        let msg = last_system_message(&sm);
        assert!(
            msg.contains("Available models:"),
            "bare /model must show the listing, got: {msg}"
        );
        assert!(msg.contains("sonnet"), "listing must include 'sonnet'");
        assert!(msg.contains("gpt4o"), "listing must include 'gpt4o'");
    }

    #[tokio::test]
    async fn model_with_trailing_whitespace_lists_available_models() {
        // Repro: slash-command popup completion fills the input with
        // "/model " (trailing space) because `takes_args = true`. If the
        // user hits Enter without typing an alias, the submission is
        // "/model " — which used to fall into the with-args arm and emit
        // the misleading "not available in this build" diagnostic. The
        // controller must treat this as the bare-listing case.
        let sm = StateManager::new_arc();
        let config = config_with_two_aliases();

        AgentRunner::process_command_internal("/model ", &Some(sm.clone()), &config).await;

        let msg = last_system_message(&sm);
        assert!(
            msg.contains("Available models:"),
            "/model with trailing space must show the listing, got: {msg}"
        );
        assert!(
            !msg.contains("not available in this build"),
            "must NOT emit the legacy-build diagnostic for trailing-whitespace bare /model, \
             got: {msg}"
        );
    }

    // --- /model switch: reasoning wire gate ---------------------------------

    /// Two providers (Anthropic: thinking blocks are its wire contract;
    /// Ollama: they are poison there) under one parsed config.
    #[cfg(feature = "mock")]
    fn config_with_anthropic_and_ollama() -> Config {
        use crate::config::resolve::{ParseCtx, Source};
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: r#"
providers:
  - name: anthropic
    type: anthropic
    api_key: sk-ant-test
    models:
      - { name: claude-sonnet-4, alias: claude, context_size: 200000 }
  - name: ollama
    type: ollama
    base_url: http://localhost:11434
    models:
      - { name: llama3, alias: local, context_size: 200000 }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: claude
"#,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("anthropic+ollama config must parse")
    }

    #[cfg(feature = "mock")]
    fn rebuild_ctx_for(config: Config) -> RebuildContext {
        RebuildContext {
            bound: config.catalog.default_bound(),
            config: Arc::new(config),
            mcp_handles: Arc::new(Vec::new()),
            shell_kind: None,
            skills: crate::skills::SkillRegistry::default(),
            todo_tool: None,
            profile_pin: None,
        }
    }

    /// Switching `/model` from an Anthropic model to a non-Anthropic one must
    /// CLOSE the wire-reasoning gate, so a transcript captured under Claude
    /// stops replaying `Reasoning` content the moment Ollama owns the wire
    /// (an Anthropic signature on a foreign wire is a hard 400 at best).
    ///
    /// Entry point is `rebuild_agent` — the shared seam that
    /// `handle_switch_model` (and `/cd`, `/load`, `/new`, a selection)
    /// delegates to, and the site that sets the gate. Driving
    /// `handle_switch_model` itself would first call `reload_session_config`,
    /// which reads the *machine's* real config directory and would swap this
    /// test's registry for whatever the developer has installed — an
    /// environment-dependent test, not a stronger one.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn model_switch_anthropic_to_ollama_closes_wire_reasoning_gate() {
        use crate::reasoning::ThinkingBlock;
        use rig_core::completion::message::{AssistantContent, Message as RigMessage};

        let sm = StateManager::new_arc();

        // A transcript captured under Claude: one assistant turn carrying a
        // signed thinking block, plus a trailing user turn.
        sm.add_user_message("first question".into());
        // Orchestrator lane: the blocks reach the row through the open
        // response, which is also what stamps its `response_id` — a row with
        // no response group never replays reasoning, so opening one is what
        // makes the "gate open" half of this test observable at all.
        sm.begin_response(vec![ThinkingBlock::Thinking {
            text: "deliberation".into(),
            signature: "SIG-ANTHROPIC".into(),
        }]);
        sm.add_assistant_message("answer".into());
        sm.add_user_message("second question".into());

        let has_reasoning = |history: &[RigMessage]| {
            history.iter().any(|m| match m {
                RigMessage::Assistant { content, .. } => content
                    .iter()
                    .any(|c| matches!(c, AssistantContent::Reasoning(_))),
                _ => false,
            })
        };

        let ctx = rebuild_ctx_for(config_with_anthropic_and_ollama());
        let (mock_agent, _mock_info, _rx, mock_hook, _mock_model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        let mut agent_slot: Arc<DynAgent> = Arc::new(mock_agent);
        let hook_cell: SharedSessionHook = Arc::new(std::sync::RwLock::new(mock_hook));
        let mut event_processor: Option<tokio::task::JoinHandle<()>> = None;
        let sm_opt = Some(sm.clone());

        // ── Switch to Anthropic: gate OPEN, blocks reach the wire ──────────
        let claude = ctx
            .config
            .registry
            .resolve("claude")
            .expect("alias claude")
            .clone();
        AgentRunner::rebuild_agent(
            &claude,
            &mut agent_slot,
            &sm,
            &hook_cell,
            &mut event_processor,
            &sm_opt,
            &ctx,
        )
        .await
        .expect("rebuild onto anthropic");

        assert!(
            has_reasoning(&sm.get_agent_history()),
            "on Anthropic with preserve_reasoning on, the rebuilt wire must carry Reasoning",
        );

        // ── Switch to Ollama: gate CLOSED, no block survives the rebuild ───
        let local = ctx
            .config
            .registry
            .resolve("local")
            .expect("alias local")
            .clone();
        AgentRunner::rebuild_agent(
            &local,
            &mut agent_slot,
            &sm,
            &hook_cell,
            &mut event_processor,
            &sm_opt,
            &ctx,
        )
        .await
        .expect("rebuild onto ollama");

        let history = sm.get_agent_history();
        assert!(
            !has_reasoning(&history),
            "after switching to a non-Anthropic provider the wire must carry no Reasoning",
        );
        let json = serde_json::to_string(&history).expect("encode wire");
        assert!(
            !json.contains("SIG-ANTHROPIC"),
            "an Anthropic signature must never reach a foreign provider's wire; got: {json}",
        );
    }

    /// T34 regression: a picker profile switch must refresh the visible tabs
    /// with the selection. Before, `handle_select` only set the selection, so
    /// leaving a profile whose `ui.tabs` hide `settings` kept the gear hidden
    /// (and entering one kept it shown) until the next reload.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn select_profile_republishes_visible_tabs() {
        use crate::config::resolve::{ParseCtx, Selection, Source};

        let config = Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: r#"
providers:
  - name: anthropic
    type: anthropic
    api_key: sk-ant-test
    models:
      - { name: claude-sonnet-4, alias: claude, context_size: 200000 }
general:
  context:
    enabled: false
profiles:
  locked-down:
    ui: { tabs: { disabled: [settings] } }
    agents:
      main: { model: claude }
  open:
    agents:
      main: { model: claude }
"#,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("two-profile config must parse");

        let sm = StateManager::new_arc();
        let mut ctx = rebuild_ctx_for(config);
        publish_bound(&sm, &ctx.bound);
        assert!(
            !sm.get_state()
                .visible_tabs
                .contains(&"settings".to_string()),
            "precondition: the first profile hides settings"
        );

        let (mock_agent, _info, _rx, mock_hook, _model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        let mut agent_slot: Arc<DynAgent> = Arc::new(mock_agent);
        let hook_cell: SharedSessionHook = Arc::new(std::sync::RwLock::new(mock_hook));
        let mut event_processor: Option<tokio::task::JoinHandle<()>> = None;

        AgentRunner::handle_select(
            SelectRequest::Pair {
                selection: Selection {
                    profile: "open".to_string(),
                    agent: "main".to_string(),
                    ..Default::default()
                },
                model: None,
            },
            &mut agent_slot,
            &Some(sm.clone()),
            &hook_cell,
            &mut event_processor,
            Some(&mut ctx),
        )
        .await;

        let state = sm.get_state();
        assert_eq!(state.selection.profile, "open", "the selection applied");
        assert!(
            state.visible_tabs.contains(&"settings".to_string()),
            "switching to a profile with all tabs must re-show settings; got {:?}",
            state.visible_tabs
        );
    }

    /// Regression: `memory.md` must resolve against the *session* cwd, not
    /// the process cwd. When PeakBot runs as a web server started from
    /// $HOME with sessions in project dirs, the old relative
    /// `Path::new("memory.md")` read the wrong file and the compaction
    /// gate silently never fired.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn maybe_compact_memory_resolves_against_session_cwd() {
        use crate::mock::MockResponse;
        use std::io::Write;

        // A session cwd holding an oversized memory.md.
        let dir = tempfile::tempdir().unwrap();
        let memory_path = dir.path().join("memory.md");
        let mut f = std::fs::File::create(&memory_path).unwrap();
        writeln!(f, "MARKER_SESSION_MEMORY").unwrap();
        writeln!(f, "{}", "x".repeat(200)).unwrap();

        // A state manager whose session cwd is that tempdir.
        let sm = StateManager::new_arc();
        sm.set_session_cwd(dir.path().to_path_buf());

        // A mock compaction model that rewrites the file.
        let (model, mock) = crate::providers::create_mock_compaction_model();
        mock.add_response(MockResponse::text("COMPACTED"));
        sm.init_title_model(std::sync::Arc::new(model));

        // Run the gate.
        AgentRunner::maybe_compact_memory(&sm, 100).await;

        // The session-cwd file must have been rewritten in place.
        let rewritten = std::fs::read_to_string(&memory_path).unwrap();
        assert_eq!(
            rewritten, "COMPACTED",
            "session-cwd memory.md must be rewritten in place"
        );

        // Exactly one model call, and the request must carry the session
        // file's content — proof the SESSION file (not a process-cwd file)
        // is what went to the model.
        let requests = mock.get_recorded_requests();
        assert_eq!(
            requests.len(),
            1,
            "compaction must make exactly one model call"
        );
        let last_msg = requests[0].chat_history.last().unwrap();
        let msg_text = match last_msg {
            rig_core::completion::message::Message::User { content } => content
                .iter()
                .map(|c| match c {
                    rig_core::completion::message::UserContent::Text(t) => t.text.clone(),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => panic!("expected user message"),
        };
        assert!(
            msg_text.contains("MARKER_SESSION_MEMORY"),
            "model request must contain the session-cwd memory.md content"
        );

        // A system message must be surfaced on state.
        let state = sm.get_state();
        let system_msgs: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert!(
            system_msgs
                .iter()
                .any(|m| m.content.contains("memory.md compacted")),
            "a 'memory.md compacted' system message must be surfaced on state"
        );
    }

    // --- /reset handler tests -----------------------------------------------
    //
    // Regression guard for the "/reset is silently inert" bug: /reset used to
    // sit in the fossil no-op match arm with a comment saying "UI pulls this
    // data" — but nothing in the REPL handled it, so the counters stayed stuck.
    // See memory.md derived-view-invalidation rule.

    #[tokio::test]
    async fn reset_command_zeros_session_stats() {
        let sm = StateManager::new_arc();
        let config = test_config();

        // Seed the stats as if some requests had happened
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 5000;
            stats.total_output_tokens = 2000;
            stats.total_api_calls = 7;
            stats.total_cost = 1.23;
        }

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 0, "/reset must zero input tokens");
        assert_eq!(
            stats.total_output_tokens, 0,
            "/reset must zero output tokens"
        );
        assert_eq!(stats.total_api_calls, 0, "/reset must zero api calls");
        assert_eq!(stats.total_cost, 0.0, "/reset must zero cost");
    }

    #[tokio::test]
    async fn reset_command_preserves_chat_history() {
        // /reset resets *stats only* — the conversation itself must survive.
        // /new is the one that clears history. Keeping them orthogonal is the
        // whole reason they're two commands.
        let sm = StateManager::new_arc();
        let config = test_config();

        sm.add_user_message("keep me".to_string());
        sm.add_assistant_message("and me".to_string());
        let before = sm.get_state().chat.messages.len();

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let after_msgs = sm.get_state().chat.messages;
        // The two seeded messages must still be there (plus one new system
        // banner confirming the reset).
        let user_and_assistant: Vec<_> = after_msgs
            .iter()
            .filter(|m| !matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            user_and_assistant.len(),
            before,
            "/reset must NOT clear user/assistant messages; /new is the one that does that"
        );
    }

    #[tokio::test]
    async fn reset_command_emits_system_banner() {
        let sm = StateManager::new_arc();
        let config = test_config();

        AgentRunner::process_command_internal("/reset", &Some(sm.clone()), &config).await;

        let state = sm.get_state();
        let system_msgs: Vec<_> = state
            .chat
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::ui::MessageRole::System))
            .collect();
        assert_eq!(
            system_msgs.len(),
            1,
            "/reset should emit exactly one system banner"
        );
        assert!(
            system_msgs[0].content.to_lowercase().contains("reset"),
            "banner should mention the reset; got {:?}",
            system_msgs[0].content
        );
    }

    // --- /exit handler tests ------------------------------------------------
    //
    // /exit must signal the view to quit WITHOUT the Ctrl+C confirmation
    // dialog. Since the dispatcher runs in the agent loop and can't
    // touch ReplUi directly, it sets an AppState flag the view polls.
    // The view-side wiring (ReplUi reads the flag and sets running=false)
    // is covered by repl_impl tests; here we pin the dispatcher contract.

    #[tokio::test]
    async fn exit_command_sets_exit_requested_flag() {
        let sm = StateManager::new_arc();
        let config = test_config();

        assert!(
            !sm.exit_requested(),
            "precondition: no exit request in a fresh state"
        );

        AgentRunner::process_command_internal("/exit", &Some(sm.clone()), &config).await;

        assert!(
            sm.exit_requested(),
            "/exit must flip the exit_requested flag"
        );
    }

    #[tokio::test]
    async fn exit_command_does_not_clear_chat_or_stats() {
        // /exit leaves the world as-is — no "Goodbye" banner, no stats
        // reset, no chat clear. The REPL is about to tear down; extra
        // writes would just flash before the screen is cleared.
        let sm = StateManager::new_arc();
        let config = test_config();

        sm.add_user_message("keep me".to_string());
        sm.add_assistant_message("and me".to_string());
        {
            let stats_arc = sm.stats_arc();
            let mut stats = stats_arc.lock().unwrap();
            stats.total_input_tokens = 42;
            stats.total_cost = 0.01;
        }
        let msgs_before = sm.get_state().chat.messages.len();

        AgentRunner::process_command_internal("/exit", &Some(sm.clone()), &config).await;

        assert_eq!(
            sm.get_state().chat.messages.len(),
            msgs_before,
            "/exit must not touch chat messages"
        );
        let stats_arc = sm.stats_arc();
        let stats = stats_arc.lock().unwrap();
        assert_eq!(stats.total_input_tokens, 42, "/exit must not reset stats");
        assert_eq!(stats.total_cost, 0.01, "/exit must not reset cost");
    }

    // --- Submission routing tests -------------------------------------------
    //
    // Regression guard for the "/new goes straight to the model" bug
    // (see `allehailmenu.md`). The event loop classifies every
    // `UiAction::SendMessage(msg)` through `classify_submission` and routes
    // accordingly. If any arm regresses, slash commands silently become
    // expensive LLM turns again.

    #[test]
    fn classify_plain_text_is_user_message() {
        // T7: `UserMessage` merged with `MultimodalMessage` — plain text is
        // `UserMessage { text, image_tokens: [] }`.
        assert!(matches!(
            classify_submission("hello world"),
            SubmitKind::UserMessage { text, image_tokens }
                if text == "hello world" && image_tokens.is_empty()
        ));
    }

    #[test]
    fn classify_slash_new_is_command() {
        // THE bug: used to classify as UserMessage, got sent to the LLM.
        assert!(matches!(
            classify_submission("/new"),
            SubmitKind::Command(s) if s == "/new"
        ));
    }

    #[test]
    fn classify_slash_help_is_command() {
        assert!(matches!(
            classify_submission("/help"),
            SubmitKind::Command(s) if s == "/help"
        ));
    }

    #[test]
    fn classify_slash_with_args_is_command() {
        // Arg-taking commands (popup closes on space, user finishes typing).
        assert!(matches!(
            classify_submission("/load 123e4567-e89b-12d3-a456-426614174000"),
            SubmitKind::Command(_)
        ));
    }

    #[test]
    fn classify_slash_stop_is_stop_command() {
        // /stop is special: it interrupts the running agent rather than
        // queueing another command behind the current run.
        assert!(matches!(
            classify_submission("/stop"),
            SubmitKind::StopCommand
        ));
    }

    #[test]
    fn classify_slash_pause_and_resume_are_immediate_commands() {
        // /pause and /resume are special like /stop: handled immediately in
        // the event loop (they must work while a turn is running), never
        // queued and never sent to the LLM.
        assert!(matches!(
            classify_submission("/pause"),
            SubmitKind::PauseCommand
        ));
        assert!(matches!(
            classify_submission("/resume"),
            SubmitKind::ResumeCommand
        ));
        // Whitespace-tolerant, like /stop.
        assert!(matches!(
            classify_submission("  /pause  "),
            SubmitKind::PauseCommand
        ));
        assert!(matches!(
            classify_submission("/resume\n"),
            SubmitKind::ResumeCommand
        ));
        // Longer words are NOT pause/resume — they fall through to the
        // generic command arm (e.g. a future /pauser or /resumed).
        assert!(matches!(
            classify_submission("/pauser"),
            SubmitKind::Command(_)
        ));
        assert!(matches!(
            classify_submission("/resumed"),
            SubmitKind::Command(_)
        ));
    }

    #[test]
    fn classify_trims_whitespace_before_deciding() {
        // Trailing newline from the input buffer must not demote a command
        // back to UserMessage.
        assert!(matches!(
            classify_submission("/new\n"),
            SubmitKind::Command(_)
        ));
        assert!(matches!(
            classify_submission("  /stop  "),
            SubmitKind::StopCommand
        ));
    }

    #[test]
    fn classify_mid_sentence_slash_stays_user_message() {
        // A slash that isn't at the start (after trim) is chat content.
        assert!(matches!(
            classify_submission("TODO: /foo or /bar?"),
            SubmitKind::UserMessage { .. }
        ));
    }

    #[test]
    fn classify_empty_is_user_message() {
        // The Enter handler already drops empty buffers; defensive default.
        assert!(matches!(
            classify_submission(""),
            SubmitKind::UserMessage { .. }
        ));
        assert!(matches!(
            classify_submission("   "),
            SubmitKind::UserMessage { .. }
        ));
    }

    // ── T7 — `[img:]` token grammar at classify time (design §3.2/§4.1, A1) ─
    //
    // `classify_submission` is grammar-only now: NO I/O, NO file resolution
    // (that moved to `attachments::collect` at enqueue), NO vision gate
    // (that is deleted). Tokens travel as raw strings in
    // `UserMessage { text, image_tokens }`.
    //
    // RED: `SubmitKind::UserMessage { text, image_tokens }` and
    // `InvalidAttachment(attachments::AttachError)` do not exist yet.

    #[test]
    fn classify_img_path_token_is_extracted_not_resolved() {
        // The file does not need to exist — classify never touches the disk.
        let input = "describe [img:/does/not/matter-7f8a.png]";
        match classify_submission(input) {
            SubmitKind::UserMessage { text, image_tokens } => {
                assert_eq!(text.trim(), "describe");
                assert_eq!(image_tokens, vec!["/does/not/matter-7f8a.png"]);
            }
            other => panic!("expected UserMessage with image_tokens, got {other:?}"),
        }
    }

    #[test]
    fn classify_img_url_token_is_kept_as_normal_token() {
        // Amendment A1: `[img:http(s)://…]` is NOT a grammar error. It stays
        // a normal token; `collect` downloads it into the store at enqueue.
        let input = "[img:https://example.com/a.png] please";
        match classify_submission(input) {
            SubmitKind::UserMessage { text, image_tokens } => {
                assert_eq!(text.trim(), "please");
                assert_eq!(image_tokens, vec!["https://example.com/a.png"]);
            }
            other => panic!("URL tokens must stay UserMessage (A1), got {other:?}"),
        }
    }

    #[test]
    fn classify_img_data_uri_token_is_invalid_attachment() {
        // A1: `[img:data:…]` is a grammar error — the inline data: producer
        // (the old web composer) is gone; the user re-attaches the file.
        let input = "look at [img:data:image/png;base64,iVBORw0K]";
        match classify_submission(input) {
            SubmitKind::InvalidAttachment(e) => {
                assert!(
                    matches!(e, crate::attachments::AttachError::DataUriToken),
                    "data: tokens must map to DataUriToken, got {e:?}"
                );
            }
            other => panic!("expected InvalidAttachment(DataUriToken), got {other:?}"),
        }
    }

    #[test]
    fn classify_multiple_img_tokens_keep_text_order() {
        let input = "[img:/a/one.png] and [img:/b/two.png]";
        match classify_submission(input) {
            SubmitKind::UserMessage { text, image_tokens } => {
                assert_eq!(text.trim(), "and");
                assert_eq!(image_tokens, vec!["/a/one.png", "/b/two.png"]);
            }
            other => panic!("expected UserMessage with both tokens, got {other:?}"),
        }
    }

    #[test]
    fn classify_unterminated_img_token_stays_literal() {
        // No closing bracket → the buffer stays literal (today's behaviour).
        let input = "see [img: /tmp/unfinished";
        match classify_submission(input) {
            SubmitKind::UserMessage { text, image_tokens } => {
                assert!(
                    text.contains("[img:"),
                    "unterminated token must stay literal: {text:?}"
                );
                assert!(image_tokens.is_empty());
            }
            other => panic!("expected literal UserMessage, got {other:?}"),
        }
    }

    // =========================================================================
    // =========================================================================
    // MCP tool filtering at the tool seam — `McpSource` (0.24).
    //
    // The profile's MCP `NameFilter` gates which servers' tools reach the
    // agent. `McpSource::for_test` + `tool_names` exercise the same
    // `NameFilter::allows` gate `tools()` applies over live handles.
    // =========================================================================

    /// The fixture groups: server `alpha` with one tool, server `beta` with
    /// one tool — the shape `McpSource::tools()` derives from the handles.
    fn mcp_fixture_groups() -> Vec<(String, Vec<String>)> {
        vec![
            ("alpha".to_string(), vec!["alpha_tool".to_string()]),
            ("beta".to_string(), vec!["beta_tool".to_string()]),
        ]
    }

    /// No gate (default filter): every server's tools, in server order.
    #[test]
    fn mcp_source_default_filter_keeps_every_server_in_order() {
        let groups = mcp_fixture_groups();
        let source =
            crate::tools::env::McpSource::for_test(&groups, crate::config::NameFilter::default());
        assert_eq!(
            source.tool_names(),
            vec!["alpha_tool", "beta_tool"],
            "an unfiltered source must keep every server in order"
        );
    }

    /// `only:` keeps exactly the named servers (in fixture order).
    #[test]
    fn mcp_source_only_keeps_named_servers() {
        let groups = mcp_fixture_groups();
        let gate = crate::config::NameFilter {
            only: vec!["beta".into()],
            ..Default::default()
        };
        let source = crate::tools::env::McpSource::for_test(&groups, gate);
        assert_eq!(
            source.tool_names(),
            vec!["beta_tool"],
            "only: [beta] must keep exactly beta's tools"
        );
    }

    /// `disabled:` drops the named servers.
    #[test]
    fn mcp_source_disabled_drops_named_servers() {
        let groups = mcp_fixture_groups();
        let gate = crate::config::NameFilter {
            disabled: vec!["alpha".into()],
            ..Default::default()
        };
        let source = crate::tools::env::McpSource::for_test(&groups, gate);
        assert_eq!(
            source.tool_names(),
            vec!["beta_tool"],
            "disabled: [alpha] must drop alpha's tools"
        );
    }

    /// `enabled: false` ⇒ no MCP tools at all (the servers keep running).
    #[test]
    fn mcp_source_enabled_false_keeps_nothing() {
        let groups = mcp_fixture_groups();
        let gate = crate::config::NameFilter {
            enabled: false,
            ..Default::default()
        };
        let source = crate::tools::env::McpSource::for_test(&groups, gate);
        assert!(
            source.tool_names().is_empty(),
            "enabled: false ⇒ no MCP tools, got: {:?}",
            source.tool_names()
        );
    }

    /// Anti-astonishment (the `NameFilter` rule): EMPTY lists are a no-op —
    /// `only: []` never means "none".
    #[test]
    fn mcp_source_empty_lists_allow_all() {
        for gate in [
            crate::config::NameFilter {
                only: vec![],
                ..Default::default()
            },
            crate::config::NameFilter {
                disabled: vec![],
                ..Default::default()
            },
        ] {
            let groups = mcp_fixture_groups();
            let source = crate::tools::env::McpSource::for_test(&groups, gate);
            assert_eq!(
                source.tool_names(),
                vec!["alpha_tool", "beta_tool"],
                "an empty list must be a no-op, got: {:?}",
                source.tool_names()
            );
        }
    }

    /// A name the config layer would have rejected still filters to
    /// nothing at the pure seam (validation is upstream; the filter must
    /// not panic or leak tools on an unknown name).
    #[test]
    fn mcp_source_only_unknown_name_keeps_nothing() {
        let groups = mcp_fixture_groups();
        let gate = crate::config::NameFilter {
            only: vec!["ghost".into()],
            ..Default::default()
        };
        let source = crate::tools::env::McpSource::for_test(&groups, gate);
        assert!(
            source.tool_names().is_empty(),
            "only: [ghost] matches no server ⇒ no tools, got: {:?}",
            source.tool_names()
        );
    }

    /// Edge: a server with ZERO tools contributes nothing and must not
    /// break the filter.
    #[test]
    fn mcp_source_skips_server_with_no_tools() {
        let groups = vec![
            ("alpha".to_string(), Vec::<String>::new()),
            ("beta".to_string(), vec!["beta_tool".to_string()]),
        ];
        let gate = crate::config::NameFilter {
            only: vec!["alpha".into()],
            ..Default::default()
        };
        let source = crate::tools::env::McpSource::for_test(&groups, gate);
        assert!(
            source.tool_names().is_empty(),
            "an allowed server with no tools yields no tools, got: {:?}",
            source.tool_names()
        );
    }

    // --- MCP tests (pre-existing) -------------------------------------------

    #[tokio::test]
    async fn test_connect_mcp_server_invalid_command() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "test_invalid".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("nonexistent_command_xyz123".to_string()),
            args: None,
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        assert!(result.is_err(), "Expected error for invalid command");
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_hello() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server");
        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        println!("Connected to hello-mcp-server with {} tools", tools.len());
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_ENV_VAR".to_string(), "test_value".to_string());

        let config = McpServerConfig {
            name: "hello-mcp-server-with-env".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: Some(env),
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let result = connect_mcp_server(&config).await;
        let handle = result.expect("Failed to connect to hello-mcp-server with env vars");
        let tools = handle.tools();

        assert!(
            !tools.is_empty(),
            "Expected at least one tool with custom env"
        );
    }

    #[tokio::test]
    #[ignore = "needs network + uvx (Python); run locally with --ignored"]
    async fn test_connect_mcp_server_call_tool() {
        let config = McpServerConfig {
            name: "hello-mcp-server".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("uvx".to_string()),
            args: Some(vec![
                "--from".to_string(),
                "git+https://github.com/macsymwang/hello-mcp-server.git".to_string(),
                "hello-mcp-server".to_string(),
            ]),
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };

        let handle = connect_mcp_server(&config)
            .await
            .expect("Failed to connect to hello-mcp-server");

        let tools = handle.tools();
        assert!(!tools.is_empty(), "Expected at least one tool");

        let first_tool = &tools[0];
        println!("Calling tool: {}", first_tool.name());

        let result = first_tool
            .call("{}".to_string())
            .await
            .expect("Failed to call tool");

        println!("Tool call result: {:?}", result);

        assert!(
            !result.is_empty(),
            "Expected non-empty result from tool call"
        );
    }

    #[test]
    fn test_mcp_transport_type_deserialization() {
        // Test stdio (default)
        let yaml = r#"
name: test-stdio
command: npx
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Stdio);
        assert_eq!(config.command.as_ref().unwrap(), "npx");
        assert!(config.url.is_none());

        // Test explicit stdio
        let yaml = r#"
name: test-explicit-stdio
type: stdio
command: npx
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Stdio);

        // Test SSE
        let yaml = r#"
name: test-sse
type: sse
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::Sse);
        assert!(config.command.is_none());
        assert_eq!(config.url.as_ref().unwrap(), "https://example.com/mcp");

        // Test streamable-http (hyphens are removed in lowercase serde)
        let yaml = r#"
name: test-http
type: streamablehttp
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.transport_type, McpTransportType::StreamableHttp);
        assert!(config.command.is_none());
        assert_eq!(config.url.as_ref().unwrap(), "https://example.com/mcp");
    }

    #[test]
    fn test_mcp_http_auth_deserialization() {
        // Defaults: no auth_token, no headers
        let yaml = r#"
name: plain-http
type: streamablehttp
url: https://example.com/mcp
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.auth_token.is_none());
        assert!(config.headers.is_none());

        // Bearer token
        let yaml = r#"
name: with-token
type: streamablehttp
url: https://example.com/mcp
auth_token: "sk-abc123"
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.auth_token.as_deref(), Some("sk-abc123"));

        // Custom headers
        let yaml = r#"
name: with-headers
type: streamablehttp
url: https://example.com/mcp
headers:
  X-Api-Key: my-key
  X-Tenant: acme
"#;
        let config: McpServerConfig = serde_yaml::from_str(yaml).unwrap();
        let headers = config.headers.expect("headers should deserialize");
        assert_eq!(headers.get("X-Api-Key").map(String::as_str), Some("my-key"));
        assert_eq!(headers.get("X-Tenant").map(String::as_str), Some("acme"));
    }

    #[test]
    fn test_mcp_config_validation() {
        // Valid stdio config
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Stdio,
            command: Some("npx".to_string()),
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_ok());

        // Invalid stdio (missing command)
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Stdio,
            command: None,
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_err());

        // Valid HTTP config
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Sse,
            command: None,
            args: None,
            env: None,
            url: Some("https://example.com/mcp".to_string()),
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_ok());

        // Invalid HTTP (missing url)
        let config = McpServerConfig {
            name: "test".to_string(),
            transport_type: McpTransportType::Sse,
            command: None,
            args: None,
            env: None,
            url: None,
            auth_token: None,
            auth: None,
            headers: None,
            enabled: true,
        };
        assert!(config.validate().is_err());
    }

    // ─── Slice 2: OAuth wiring (no in-process pin) ──────────────────────────
    //
    // Slice 1 of the MCP OAuth work parsed the `auth: { type: oauth }`
    // shape but didn't wire the flow — that pin
    // (`oauth_variant_returns_not_yet_implemented`) lived here. Slice 2
    // (this branch) wires the real `mcp_auth::authorize` path. The
    // replacement contract is the pre-merge live Linear smoke test
    // recorded in `autho.md`; there is no in-process pin because the
    // happy path requires DCR + browser + token exchange against a real
    // OAuth server.

    // ─── Mid-action compaction: production wire-payload contract ────────────
    //
    // These tests pin the exact data shape that
    // `AgentRunner::process_message_internal` constructs after the
    // SessionHook fires `terminate("compact")`. The bug we're guarding
    // against is subtle: `build_resumption_from_tail` returns the
    // correct `(prompt, history)` tuple, but if the loop discards the
    // returned history and re-derives it via `get_agent_history()` on
    // the next iteration, the resumption message ends up duplicated on
    // the wire (once as the prompt, once at the tail of history). That
    // breaks Anthropic / OpenAI conversation invariants and the model
    // either re-runs the tool, produces garbage, or refuses.
    //
    // Pinned at the data layer because the runtime race is hard to
    // reproduce deterministically (see procedural rule (c) in
    // `memory.md`: "Pin races at the data layer, not the runtime layer").
    //
    // The fix is a one-shot `history_override: Option<Vec<Message>>`
    // outside the loop body, consumed via `.take()` at the top of the
    // next iteration to override the default `get_agent_history()` call.

    /// Helper: extract every visible text fragment from a rig
    /// `Message` for substring assertions. Walks User text, User
    /// ToolResult text, and Assistant text + tool-call arguments.
    fn message_texts(msg: &rig_core::completion::message::Message) -> Vec<String> {
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, ToolResultContent, UserContent,
        };
        let mut out = Vec::new();
        match msg {
            RigMessage::User { content } => {
                for c in std::iter::once(content.first_ref()).chain(content.rest().iter()) {
                    match c {
                        UserContent::Text(t) => out.push(t.text.clone()),
                        UserContent::ToolResult(tr) => {
                            for rc in std::iter::once(tr.content.first_ref())
                                .chain(tr.content.rest().iter())
                            {
                                if let ToolResultContent::Text(t) = rc {
                                    out.push(t.text.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            RigMessage::Assistant { content, .. } => {
                for c in std::iter::once(content.first_ref()).chain(content.rest().iter()) {
                    match c {
                        AssistantContent::Text(t) => out.push(t.text.clone()),
                        AssistantContent::ToolCall(tc) => {
                            // Include tool-call arguments so substring
                            // checks can verify ToolCall preservation.
                            out.push(tc.function.arguments.to_string());
                        }
                        _ => {}
                    }
                }
            }
            RigMessage::System { .. } => {}
        }
        out
    }

    /// Count occurrences of `needle` across a slice of rig Messages.
    fn count_occurrences(msgs: &[rig_core::completion::message::Message], needle: &str) -> usize {
        msgs.iter()
            .flat_map(message_texts)
            .filter(|t| t.contains(needle))
            .count()
    }

    /// **The bug.** After a tool round-trip the chat ends in a
    /// `ToolResult`. When mid-action compaction fires, the production
    /// loop must use the resumption tuple's history (returned alongside
    /// the prompt by `build_resumption_from_tail`) — NOT a fresh
    /// `get_agent_history()` call, which still includes the trailing
    /// ToolResult and would duplicate it on the wire.
    ///
    /// This test exercises [`derive_history_for_iteration`] — the same
    /// function the production loop calls — with the post-compact-arm
    /// state: `history_override = Some(resumption_history)` and a
    /// `current_turn = ToolResult`. Asserts the resumption marker
    /// appears exactly once across `(prompt + derived_history)`.
    ///
    /// **Without `derive_history_for_iteration` honouring the override**
    /// (i.e. the regression introduced in `a1d705c`), this test fails:
    /// the trailing ToolResult ends up duplicated.
    ///
    /// Pinned by [`memory.md` §"context fukup #2 (2026-05-09)"].
    #[test]
    fn derive_history_for_iteration_does_not_duplicate_toolresult_on_resume() {
        let sm = Arc::new(StateManager::new());
        // Simulate a tool round-trip: User → Agent → ToolCall → ToolResult.
        // Compaction fires after the ToolResult lands.
        sm.add_user_message("list files".to_string());
        sm.add_assistant_message("I'll run ls for you".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            None,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "UNIQUE_TOOLRESULT_MARKER_12345".to_string(),
            Some("call_1".to_string()),
        );

        // The compact arm sets current_turn = prompt and stashes
        // resumption_history into the one-shot override.
        let (prompt, resumption_history) = sm
            .build_resumption_from_tail()
            .expect("non-empty conversation must produce a resumption");
        let mut history_override = Some(resumption_history);

        // The loop top calls derive_history_for_iteration to get the
        // history that goes on the wire. With the override Some, this
        // MUST return the resumption history verbatim — NOT re-derive
        // from StateManager (which would re-include the trailing
        // ToolResult and duplicate it).
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());
        let derived_history = derive_history_for_iteration(&mut history_override, &sm_opt);

        // Assertion: the resumption marker must appear exactly ONCE
        // across (prompt + derived_history).
        let prompt_count = count_occurrences(
            std::slice::from_ref(&prompt),
            "UNIQUE_TOOLRESULT_MARKER_12345",
        );
        let history_count = count_occurrences(&derived_history, "UNIQUE_TOOLRESULT_MARKER_12345");
        let total = prompt_count + history_count;

        assert_eq!(
            total,
            1,
            "Resumption ToolResult must appear exactly once across (prompt + history); \
             got {prompt_count} in prompt + {history_count} in history = {total}. \
             prompt={prompt:?}, history_len={}",
            derived_history.len()
        );

        // The override must have been consumed (one-shot semantics).
        // The next iteration would fall through to get_agent_history().
        assert!(
            history_override.is_none(),
            "history_override must be cleared after .take() in derive_history_for_iteration"
        );
    }

    /// No-regression: when there is no override, `derive_history_for_iteration`
    /// must fall through to `StateManager::get_agent_history()`.
    #[test]
    fn derive_history_for_iteration_falls_through_to_state_manager_when_no_override() {
        let sm = Arc::new(StateManager::new());
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi there".to_string());

        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());
        let derived = derive_history_for_iteration(&mut history_override, &sm_opt);

        // Should match get_agent_history() exactly.
        let expected = sm.get_agent_history();
        assert_eq!(
            derived.len(),
            expected.len(),
            "without override, must return get_agent_history() verbatim"
        );
    }

    /// No-regression: empty `(None, None)` returns an empty Vec.
    #[test]
    fn derive_history_for_iteration_handles_no_state_manager() {
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;
        let sm_opt: Option<Arc<StateManager>> = None;
        let derived = derive_history_for_iteration(&mut history_override, &sm_opt);
        assert!(derived.is_empty());
    }

    /// **The bug.** This is the *original* data-shape pin, kept as a
    /// no-regression for the underlying mismatch between
    /// `build_resumption_from_tail` (returns the full resumption
    /// shape) and `get_agent_history` (only strips trailing User).
    /// If anyone "fixes" `get_agent_history` to also strip trailing
    /// ToolResult — masking the lib.rs bug instead of fixing the
    /// wiring — this test stays meaningful: it documents the
    /// asymmetry and the per-method contracts. The actual fix lives
    /// at the wiring layer and is pinned by
    /// `derive_history_for_iteration_does_not_duplicate_toolresult_on_resume`.
    #[test]
    fn naive_get_agent_history_after_resumption_demonstrates_the_bug() {
        let sm = StateManager::new();
        sm.add_user_message("list files".to_string());
        sm.add_assistant_message("I'll run ls for you".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            None,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "UNIQUE_TOOLRESULT_MARKER_12345".to_string(),
            Some("call_1".to_string()),
        );

        let (prompt, _) = sm
            .build_resumption_from_tail()
            .expect("non-empty conversation must produce a resumption");
        // This is the BROKEN pattern (re-derive history from state).
        // get_agent_history() doesn't strip trailing non-User messages,
        // so the ToolResult stays in history AND is the prompt.
        let history_naive = sm.get_agent_history();

        let prompt_count = count_occurrences(
            std::slice::from_ref(&prompt),
            "UNIQUE_TOOLRESULT_MARKER_12345",
        );
        let history_count = count_occurrences(&history_naive, "UNIQUE_TOOLRESULT_MARKER_12345");

        // This documents the bug: the broken pattern produces a
        // duplicate. The fix (use the resumption_history from the
        // tuple, not re-derive) is pinned by the test above.
        assert_eq!(prompt_count, 1, "prompt has the marker once");
        assert_eq!(
            history_count, 1,
            "naive get_agent_history() ALSO has the marker (the source of the duplication)"
        );
    }

    // ── Defect 3: retry loop resends a stale turn ───────────────────────
    //
    // The retry arm rebuilt `history` fresh every iteration via
    // `derive_history_for_iteration` but left `current_turn` at the original
    // user prompt. When the failing wire call was a continuation *inside* an
    // already in-flight turn (tool call + result persisted), the retry sent
    // the user turn twice: once as the tail of history, once as the prompt.
    // Observed twice in production (wire captures: requests 3-5 each carried
    // the original "read /tmp/screencap.png" prompt on top of a history that
    // already contained the completed tool exchange). See
    // `tickets/pr-b-retry-correctness.md`, Defect 3.

    /// *The arm-level pin.* Drives the real `process_message_internal` with
    /// a transient 429 injected into the mock model, so the assertion lands
    /// on the SECOND wire payload the **retry arm itself** built — not on
    /// the helper's return value, which the three `refresh_attempt_from_*`
    /// tests below already cover.
    ///
    /// Removing or short-circuiting the `refresh_attempt_from_transcript`
    /// call in the transient-retry arm (`process_message_internal`, the
    /// block immediately before `tokio::time::sleep(delay)`) makes this
    /// fail with `2 != 1` — the duplicated user turn from the production
    /// trace.
    ///
    /// The completed tool round-trip is written straight to the transcript:
    /// in production those rows arrive on the async event-processing task,
    /// and awaiting that here would only add a race. What the arm reads is
    /// the transcript tail, and the tail is identical either way.
    #[cfg(feature = "mock")]
    #[tokio::test]
    async fn retry_arm_resumes_from_the_tail_after_a_tool_roundtrip() {
        let sm = StateManager::new_arc();
        sm.add_user_message("read /tmp/screencap.png".to_string());
        sm.add_assistant_message("I'll look at it".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            None,
            "view_image".to_string(),
            r#"{"path":"/tmp/screencap.png"}"#.to_string(),
            Some("c1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "view_image".to_string(),
            r#"{"path":"/tmp/screencap.png"}"#.to_string(),
            "ok".to_string(),
            Some("c1".to_string()),
        );

        let (agent, _info, _events, _hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        // A 429 is transient (`is_transient_prompt_error`), so the arm under
        // test runs; the second attempt succeeds and ends the turn.
        model.add_response(crate::mock::MockResponse::error(
            "429 rate limited by provider",
        ));
        model.add_response(crate::mock::MockResponse::text("done"));

        let mut config = test_config();
        config.general.retry.max_retries = 1;
        config.general.retry.initial_delay_ms = 1;

        let current_turn = sm
            .build_current_turn_message()
            .expect("the original user turn was dispatched");
        let agent: Arc<DynAgent> = Arc::new(agent);
        let sm_opt = Some(sm.clone());

        let result =
            AgentRunner::process_message_internal(current_turn, &sm_opt, &agent, &config).await;
        assert!(
            matches!(result, CompletionResult::Success),
            "the retried attempt must succeed and end the turn"
        );

        let requests = model.get_recorded_requests();
        assert_eq!(
            requests.len(),
            2,
            "one failed attempt plus exactly one retry"
        );
        // `chat_history` is the whole wire payload — history ++ [prompt].
        let retry_payload = &requests[1].chat_history;
        assert_eq!(
            count_occurrences(retry_payload, "read /tmp/screencap.png"),
            1,
            "the retry must resume from the transcript tail: the user turn \
             belongs on the wire exactly once, but the retry arm resent it \
             as the prompt on top of a history that already contained it. \
             Payload: {retry_payload:?}"
        );
    }

    /// After a tool round-trip has landed, `refresh_attempt_from_transcript`
    /// must promote the ToolResult tail to `current_turn` and stash
    /// everything before it — including the matching ToolCall — into
    /// `history_override`. The pair must survive the split intact.
    #[test]
    fn refresh_attempt_from_transcript_after_tool_roundtrip_promotes_the_toolresult() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};

        let sm = StateManager::new();
        sm.add_user_message("read /tmp/screencap.png".to_string());
        sm.add_assistant_message("I'll look at it".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            None,
            "view_image".to_string(),
            r#"{"path":"/tmp/screencap.png"}"#.to_string(),
            Some("c1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "view_image".to_string(),
            r#"{"path":"/tmp/screencap.png"}"#.to_string(),
            "ok".to_string(),
            Some("c1".to_string()),
        );

        let mut current_turn = sm
            .build_current_turn_message()
            .expect("the original user turn was dispatched");
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;

        let refreshed =
            refresh_attempt_from_transcript(&sm, &mut current_turn, &mut history_override);
        assert!(
            refreshed,
            "a tool round-trip landed; there is a tail to resume from"
        );

        match &current_turn {
            RigMessage::User { content } => match content.first_ref() {
                UserContent::ToolResult(tr) => assert_eq!(tr.id, "c1"),
                other => panic!("expected ToolResult content, got {other:?}"),
            },
            other => panic!("expected User message, got {other:?}"),
        }

        let history = history_override
            .as_ref()
            .expect("history_override must be Some after a successful refresh");
        let last = history.last().expect("history must not be empty");
        let tc_id = match last {
            RigMessage::Assistant { content, .. } => content.iter().find_map(|c| match c {
                AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
                _ => None,
            }),
            _ => None,
        };
        assert_eq!(
            tc_id.as_deref(),
            Some("c1"),
            "the matching ToolCall must be the last entry of history; got {history:?}"
        );
    }

    /// No-tool-call retry path (e.g. a 429 on the very first wire call): the
    /// live tail is still the User row, so there is nothing to resume from.
    /// The helper must leave `current_turn` and `history_override` alone.
    #[test]
    fn refresh_attempt_from_transcript_on_a_fresh_turn_leaves_the_turn_alone() {
        let sm = StateManager::new();
        sm.add_user_message("just a fresh question".to_string());

        let original_turn = sm
            .build_current_turn_message()
            .expect("the user turn was dispatched");
        let mut current_turn = original_turn.clone();
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;

        let refreshed =
            refresh_attempt_from_transcript(&sm, &mut current_turn, &mut history_override);

        assert!(
            !refreshed,
            "a fresh single-message turn has nothing to resume from"
        );
        assert_eq!(
            count_occurrences(std::slice::from_ref(&current_turn), "just a fresh question"),
            count_occurrences(
                std::slice::from_ref(&original_turn),
                "just a fresh question"
            ),
            "current_turn must be unchanged when there is nothing to resume from"
        );
        assert!(
            history_override.is_none(),
            "history_override must stay None when there is nothing to resume from"
        );
    }

    /// A multimodal user turn (image attachment) retried before any tool
    /// call must keep its image content after refresh — guards against a
    /// `last_msg_to_rig` regression that would silently drop vision on
    /// retry.
    #[tokio::test]
    async fn refresh_attempt_from_transcript_preserves_user_image_attachments() {
        use base64::Engine;
        use rig_core::completion::message::{Message as RigMessage, UserContent};

        let tmp = tempfile::tempdir().unwrap();
        let store = crate::attachments::UploadStore::new(
            tmp.path().to_path_buf(),
            crate::config::UploadsConfig::default(),
        );
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
            .expect("valid base64 fixture");
        let att = store
            .store(uuid::Uuid::new_v4(), "screencap.png", &png[..])
            .await
            .expect("store the PNG");

        let sm = StateManager::new().with_uploads(store);
        sm.set_supports_vision(true);
        sm.add_user_message_with_attachments("what is in this image?".to_string(), vec![att]);
        // A second user-facing row so the retry has a tail to consider —
        // an assistant turn precedes the retried attempt in production;
        // here we retry the very first (and only) turn, mirroring a 429
        // on the initial wire call with an image attached.
        let mut current_turn = sm
            .build_current_turn_message()
            .expect("the multimodal user turn was dispatched");
        let mut history_override: Option<Vec<rig_core::completion::message::Message>> = None;

        let _ = refresh_attempt_from_transcript(&sm, &mut current_turn, &mut history_override);

        match &current_turn {
            RigMessage::User { content } => {
                assert!(
                    content.iter().any(|c| matches!(c, UserContent::Image(_))),
                    "refreshed prompt must still carry the image attachment; content={content:?}"
                );
            }
            other => panic!("expected User message, got {other:?}"),
        }
    }

    // ── Phase 3: event lane tag + cost roll-up ──────────────────────────
    //
    // A sub-agent's events reach the shared event channel wrapped in a
    // `SourcedEvent` carrying its lane (`MessageSource::SubAgent { role }`).
    // `process_event_for_ui` must (a) roll token/cost into the parent stats
    // *regardless* of lane — the #1 research fix, a delegation's 15× cost
    // can't be silent in `/stats` — and (b) stamp that lane onto the tool
    // ChatMessages it produces so the transcript can label them.

    /// A `CompletionResponse` from a sub-agent lane rolls its tokens and
    /// cost into the parent session stats exactly as an orchestrator turn
    /// would. Cost accounting is lane-agnostic.
    #[test]
    fn sub_agent_completion_response_rolls_cost_into_stats() {
        use crate::hooks::events::{AgentEvent, SourcedEvent, TokenUsage};

        let sm = Arc::new(StateManager::new());
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());

        AgentRunner::process_event_for_ui(
            &sm_opt,
            SourcedEvent {
                source: MessageSource::SubAgent {
                    role: "researcher".to_string(),
                },
                event: AgentEvent::CompletionResponse {
                    content: "done".to_string(),
                    reasoning: None,
                    thinking: vec![],

                    usage: TokenUsage {
                        input_tokens: 100,
                        output_tokens: 50,
                        total_tokens: 150,
                        cost: 0.02,
                    },
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let stats = sm.get_stats();
        assert_eq!(stats.total_api_calls, 1, "sub-agent call must count");
        assert_eq!(stats.total_input_tokens, 100);
        assert_eq!(stats.total_output_tokens, 50);
        assert!(
            (stats.total_cost - 0.02).abs() < f64::EPSILON,
            "sub-agent cost must roll into /stats, got {}",
            stats.total_cost
        );
    }

    /// A `ToolCall` from a sub-agent lane produces a transcript ChatMessage
    /// stamped `MessageSource::SubAgent { role }` — the renderer keys on it.
    #[test]
    fn sub_agent_tool_call_is_stamped_with_sub_agent_source() {
        use crate::hooks::events::{AgentEvent, SourcedEvent};
        use crate::ui::app_state::MessageRole;

        let sm = Arc::new(StateManager::new());
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());

        AgentRunner::process_event_for_ui(
            &sm_opt,
            SourcedEvent {
                source: MessageSource::SubAgent {
                    role: "researcher".to_string(),
                },
                event: AgentEvent::ToolCall {
                    tool_name: "bash".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                    call_id: Some("c1".to_string()),
                    response_id: None,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let msgs = sm.get_state().chat.messages;
        let last = msgs.last().expect("a tool-call message was added");
        assert_eq!(last.role, MessageRole::ToolCall);
        assert_eq!(
            last.source,
            MessageSource::SubAgent {
                role: "researcher".to_string()
            },
            "sub-agent tool call must carry its lane"
        );
    }

    /// The orchestrator's own tool calls stay on the `Human` lane — the
    /// default when no sub-agent source is set. Guards against a regression
    /// that would leak the default into a non-Human lane.
    #[test]
    fn orchestrator_tool_call_stays_human_lane() {
        use crate::hooks::events::{AgentEvent, SourcedEvent};
        use crate::ui::app_state::MessageRole;

        let sm = Arc::new(StateManager::new());
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());

        AgentRunner::process_event_for_ui(
            &sm_opt,
            SourcedEvent {
                source: MessageSource::Human,
                event: AgentEvent::ToolCall {
                    tool_name: "bash".to_string(),
                    arguments: "{}".to_string(),
                    call_id: None,
                    response_id: None,
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let msgs = sm.get_state().chat.messages;
        let last = msgs.last().expect("a tool-call message was added");
        assert_eq!(last.role, MessageRole::ToolCall);
        assert_eq!(last.source, MessageSource::Human);
    }

    // ── sub-agent prose surfaces on its lane ──────────────────────────────

    /// A sub-agent `CompletionResponse` with non-empty text produces an
    /// assistant transcript message tagged `SubAgent { role }` — so a
    /// prose-heavy role (reviewer) is visible on its own lane, not silent.
    #[test]
    fn sub_agent_completion_prose_lands_on_its_lane() {
        use crate::hooks::events::{AgentEvent, SourcedEvent, TokenUsage};
        use crate::ui::app_state::MessageRole;

        let sm = Arc::new(StateManager::new());
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());

        AgentRunner::process_event_for_ui(
            &sm_opt,
            SourcedEvent {
                source: MessageSource::SubAgent {
                    role: "reviewer".to_string(),
                },
                event: AgentEvent::CompletionResponse {
                    content: "The diff looks solid; one nit on naming.".to_string(),
                    reasoning: None,
                    thinking: vec![],

                    usage: TokenUsage {
                        input_tokens: 10,
                        output_tokens: 8,
                        total_tokens: 18,
                        cost: 0.0,
                    },
                    timestamp: chrono::Utc::now(),
                },
            },
        );

        let msgs = sm.get_state().chat.messages;
        let last = msgs.last().expect("prose message added");
        assert_eq!(last.role, MessageRole::Agent);
        assert_eq!(
            last.source,
            MessageSource::SubAgent {
                role: "reviewer".to_string()
            },
            "sub-agent prose must carry its lane"
        );
        assert!(last.content.contains("one nit on naming"));
    }

    /// The orchestrator's own `CompletionResponse` text must NOT be added by
    /// the event path — it already enters via `prompt_with_history`'s return
    /// value (`add_assistant_message`). Adding it here too would double it.
    /// Only stats move for an orchestrator-lane completion.
    #[test]
    fn orchestrator_completion_prose_is_not_double_added() {
        use crate::hooks::events::{AgentEvent, SourcedEvent, TokenUsage};

        let sm = Arc::new(StateManager::new());
        let sm_opt: Option<Arc<StateManager>> = Some(sm.clone());

        let before = sm.get_state().chat.messages.len();
        AgentRunner::process_event_for_ui(
            &sm_opt,
            SourcedEvent {
                source: MessageSource::Human,
                event: AgentEvent::CompletionResponse {
                    content: "I'll handle that.".to_string(),
                    reasoning: None,
                    thinking: vec![],

                    usage: TokenUsage {
                        input_tokens: 5,
                        output_tokens: 3,
                        total_tokens: 8,
                        cost: 0.0,
                    },
                    timestamp: chrono::Utc::now(),
                },
            },
        );
        let after = sm.get_state().chat.messages.len();
        assert_eq!(
            before, after,
            "orchestrator completion must not add a transcript message (the return-value path owns that)"
        );
        // ...but stats still moved.
        assert_eq!(sm.get_stats().total_api_calls, 1);
    }

    /// T4 — stop message rendering. Pure-function test, no fixtures.
    #[test]
    fn stop_message_renders_tally() {
        use crate::state::StopTally;

        // (a) A foreground shell was running: the with-clause sentence.
        assert_eq!(
            super::stop_message(StopTally { shell: true }),
            "Agent stopped by user (killed 1 shell process)",
            "shell ⇒ with-clause ('killed 1 shell process')"
        );

        // (b) Nothing was running: the bare sentence, identical to today.
        assert_eq!(
            super::stop_message(StopTally::default()),
            "Agent stopped by user",
            "empty tally ⇒ bare sentence (back-compat with pre-#183 wording)"
        );
    }

    // =========================================================================
    // ─── interrupted tool results (interrupted-tool-results design §5 b3/c) ───

    /// b3 — Stop mid-tool: the cancel arm must answer the in-flight
    /// orchestrator-lane ToolCall with a persisted INTERRUPTED result that
    /// quotes the sub-agent's last message. Modelled on
    /// `retry_arm_resumes_from_the_tail_after_a_tool_roundtrip`: a real
    /// hanging `bash sleep 30` is the in-flight tool, and the sub-agent's
    /// persisted work is emulated with a sourced assistant row.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn stop_mid_tool_persists_interrupted_result() {
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::MessageRole;
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");
        sm.add_user_message("go".to_string());

        let (agent, _info, mut events, _hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        // A real in-flight tool: the bash PTY child hangs for 30s, far past
        // this test's own ≤10s budget.
        model.add_response(crate::mock::MockResponse::tool_call(
            "bash",
            serde_json::json!({ "command": "sleep 30" }),
        ));

        // Event pump: tool rows land through the async event-processor path.
        let sm_opt_pump = Some(sm.clone());
        let pump = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                AgentRunner::process_event_for_ui(&sm_opt_pump, ev);
            }
        });

        // Mint the per-turn token exactly as production does (the agent_loop
        // UserMessage arm) so the turn binds the token Stop will cancel.
        sm.set_running(true);

        let current_turn = sm
            .build_current_turn_message()
            .expect("the user turn was dispatched");
        let agent = Arc::new(agent);
        let config = test_config();
        let sm_opt = Some(sm.clone());
        let turn = tokio::spawn(async move {
            AgentRunner::process_message_internal(current_turn, &sm_opt, &agent, &config).await
        });

        // Wait (≤5s) until the ToolCall row has landed and the tool is in flight.
        let call_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if let Some(tail) = msgs.last()
                    && tail.role == MessageRole::ToolCall
                    && tail.tool_name.as_deref() == Some("bash")
                {
                    return tail.call_id.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the bash ToolCall row must land within 5s");

        // Emulate the sub-agent's persisted work (the forensics quote source).
        sm.add_assistant_message_sourced(
            MessageSource::SubAgent {
                role: "junior".into(),
            },
            "halfway done".to_string(),
        );

        // Stop: cancels the per-turn token; the turn's future is dropped.
        sm.stop_turn_processes();

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn)
            .await
            .expect("the cancelled turn must finish within 5s")
            .expect("the turn task must not panic");
        assert!(
            matches!(result, CompletionResult::Stopped),
            "a cancelled turn must report Stopped"
        );

        // The orchestrator lane's last row is the synthesized answer.
        let msgs = sm.get_state().chat.messages;
        let last_orch = msgs
            .iter()
            .rev()
            .find(|m| m.source.is_orchestrator_lane())
            .expect("the orchestrator lane is non-empty");
        assert_eq!(
            last_orch.role,
            MessageRole::ToolResult,
            "the in-flight call must be answered, not left dangling; transcript={msgs:?}"
        );
        assert_eq!(
            last_orch.call_id, call_id,
            "the answer must carry the bash call's id"
        );
        assert_eq!(last_orch.tool_name.as_deref(), Some("bash"));
        let text = last_orch.tool_result.as_deref().unwrap_or_default();
        assert!(
            text.starts_with("INTERRUPTED: the turn was stopped before `bash` returned."),
            "got: {text}"
        );
        assert!(
            text.contains("halfway done"),
            "the sub-agent's last message must be quoted; got: {text}"
        );

        // Persisted — the notice survives reload.
        let id = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");
        let conv = storage.load(id).expect("conversation persists");
        assert!(
            conv.messages.iter().any(|m| {
                matches!(
                    m,
                    crate::conversation::Message::ToolResult { call_id: cid, .. }
                        if cid == &call_id
                )
            }),
            "the INTERRUPTED result must be on disk"
        );

        pump.abort();
    }

    // =========================================================================
    // ─── orchestrator reply: staged prose + empty-reply nudge (ticket #294) ───
    //
    // Written FIRST against the locked design (TDD — production code absent):
    //   1. `AssistantParts` gains `has_tool_call`; the orchestrator hook
    //      stages `parts.text` via `StateManager::stage_prose` for every
    //      response that carried a tool call (sub-agent hooks never stage).
    //   2. `StateManager::commit_reply(final_text) -> bool` merges the staged
    //      chunks + the final text into ONE assistant row — blank-dropped,
    //      deduped by trimmed text (first occurrence wins), joined with
    //      `"\n\n"`; a single chunk stays byte-identical. False when nothing
    //      remains (no row, open-response thinking cleared).
    //   3. `process_message_internal` calls `commit_reply(&response)` in the
    //      Ok arm; a false commit nudges ONCE with the NON-persisted
    //      `crate::EMPTY_REPLY_NUDGE` user message, and a second false commit
    //      warns with `crate::EMPTY_REPLY_WARNING`. The wrapper flushes staged
    //      prose on every exit (`commit_reply("")`), so Stop/Error still keep
    //      the prose.
    //
    // RED state: `stage_prose` / `commit_reply` / `EMPTY_REPLY_NUDGE` /
    // `EMPTY_REPLY_WARNING` do not exist yet, so the suite fails to COMPILE on
    // exactly those names — the intended RED. (T4 is a pure no-regression
    // guard: once the suite compiles it is green on old AND new behaviour.)
    //
    // Pattern: `stop_mid_tool_persists_interrupted_result` — create_mock_agent,
    // event pump, `process_message_internal`. `fu(x)` below is
    // `MockResponse::tool_call_with_follow_up("todo", …, x)`: one response
    // carrying a tool call AND the prose `x` (the dropped-prose shape of
    // ticket #294).

    /// One `todo` tool call plus the prose `follow_up` in the SAME response —
    /// the exact wire shape that used to lose `follow_up` on the orchestrator
    /// lane.
    #[cfg(feature = "mock")]
    fn fu(follow_up: &str) -> crate::mock::MockResponse {
        crate::mock::MockResponse::tool_call_with_follow_up(
            "todo",
            serde_json::json!({"action": "add", "tasks": ["t"]}),
            follow_up,
        )
    }

    /// Boot conversation + one user turn + mock agent + event pump + the
    /// turn task. `responses` are queued in order. The pump writes the
    /// ToolCall/ToolResult rows (the assistant row itself is written
    /// synchronously by the commit seam, so no assertion here races it).
    #[cfg(feature = "mock")]
    fn staged_reply_fixture(
        responses: Vec<crate::mock::MockResponse>,
    ) -> (
        Arc<StateManager>,
        crate::mock::MockCompletionModel,
        tokio::task::JoinHandle<CompletionResult>,
    ) {
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");
        sm.add_user_message("go".to_string());

        let (agent, _info, mut events, _hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        model.add_responses(responses);

        let sm_opt_pump = Some(sm.clone());
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                AgentRunner::process_event_for_ui(&sm_opt_pump, ev);
            }
        });

        sm.set_running(true);
        let current_turn = sm
            .build_current_turn_message()
            .expect("the user turn was dispatched");
        let agent = Arc::new(agent);
        let config = test_config();
        let sm_opt = Some(sm.clone());
        let turn = tokio::spawn(async move {
            AgentRunner::process_message_internal(current_turn, &sm_opt, &agent, &config).await
        });
        (sm, model, turn)
    }

    /// Orchestrator Agent rows after the (only) user message.
    #[cfg(feature = "mock")]
    fn agent_rows_after_user(
        msgs: &[crate::ui::app_state::ChatMessage],
    ) -> Vec<&crate::ui::app_state::ChatMessage> {
        use crate::ui::app_state::MessageRole;
        let user_idx = msgs
            .iter()
            .position(|m| m.role == MessageRole::User)
            .expect("the user message is in the transcript");
        msgs[user_idx + 1..]
            .iter()
            .filter(|m| m.role == MessageRole::Agent && m.source.is_orchestrator_lane())
            .collect()
    }

    /// T1 — prose in a tool-call response is NOT dropped: it is staged and
    /// committed into the final reply, which lands as exactly ONE orchestrator
    /// agent row, `"\n\n"`-joined with the closing text.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t1_tool_call_prose_is_staged_into_the_final_reply() {
        let (sm, model, turn) = staged_reply_fixture(vec![
            fu("ANSWER"),
            crate::mock::MockResponse::text("🙏 amen"),
        ]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(
            rows.len(),
            1,
            "exactly one orchestrator agent row after the user msg; transcript={msgs:?}"
        );
        assert_eq!(
            rows[0].content, "ANSWER\n\n🙏 amen",
            "staged tool-call prose + closing text, joined with a blank line"
        );
        // The wire context carries "ANSWER" exactly once (inside the merged row).
        assert_eq!(
            count_occurrences(&sm.get_agent_history(), "ANSWER"),
            1,
            "ANSWER must occur once in get_agent_history()"
        );
        assert_eq!(model.request_count(), 2, "no nudge on a non-empty final");
    }

    /// T2 — a final reply duplicating the staged prose collapses to ONE row.
    /// Dedup is by trimmed text, first occurrence wins.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t2_duplicate_final_reply_collapses_to_one_row() {
        let (sm, _model, turn) =
            staged_reply_fixture(vec![fu("X"), crate::mock::MockResponse::text("X")]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "X", "dedup keeps the first occurrence");
    }

    /// T3 — multiple staged chunks join in order with blank lines; a
    /// duplicated chunk collapses to its first occurrence.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t3_multiple_staged_chunks_join_with_blank_lines() {
        let (sm, _model, turn) =
            staged_reply_fixture(vec![fu("A"), fu("B"), crate::mock::MockResponse::text("C")]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "A\n\nB\n\nC");
    }

    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t3_duplicate_staged_chunk_collapses() {
        let (sm, _model, turn) =
            staged_reply_fixture(vec![fu("A"), fu("A"), crate::mock::MockResponse::text("C")]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "A\n\nC", "the duplicate chunk is dropped");
    }

    /// T4 — the common path must not move: a plain text reply is persisted
    /// BYTE-IDENTICAL (no trim/rejoin) and makes exactly ONE request (no
    /// nudge machinery on a non-empty reply).
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t4_plain_text_reply_is_byte_identical_and_makes_one_request() {
        let (sm, model, turn) = staged_reply_fixture(vec![crate::mock::MockResponse::text("hi")]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "hi", "byte-identical — no trim/rejoin");
        assert_eq!(
            model.request_count(),
            1,
            "a non-empty final reply must not trigger a nudge"
        );
    }

    /// T5 — an empty final reply after staged prose commits the staged prose
    /// (the reply is NOT empty once staged), so NO nudge fires: exactly two
    /// requests, one row with the staged text.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t5_empty_final_after_staged_prose_commits_without_nudge() {
        let (sm, model, turn) =
            staged_reply_fixture(vec![fu("ANSWER"), crate::mock::MockResponse::text("")]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "ANSWER");
        assert_eq!(
            model.request_count(),
            2,
            "the staged prose made the reply non-empty — no nudge"
        );
    }

    /// T6 — a truly empty final reply (Anthropic empty end_turn → "") gets ONE
    /// nudge: the second request's payload ends with a User message carrying
    /// `EMPTY_REPLY_NUDGE`, preceded by the original user message. The nudge
    /// is NOT persisted (no user row for it), and no blank row is pushed.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t6_empty_reply_nudges_with_nonpersisted_user_message() {
        let (sm, model, turn) = staged_reply_fixture(vec![
            crate::mock::MockResponse::text(""),
            crate::mock::MockResponse::text("real"),
        ]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "real");
        assert!(
            !msgs.iter().any(|m| m.content.trim().is_empty()
                && m.role == crate::ui::app_state::MessageRole::Agent),
            "no blank agent row may be persisted; transcript={msgs:?}"
        );
        assert_eq!(model.request_count(), 2, "exactly one nudge");

        // The nudge is a wire-only user message: NOT in the transcript…
        assert!(
            !msgs.iter().any(|m| m.content == crate::EMPTY_REPLY_NUDGE),
            "the nudge must not be persisted; transcript={msgs:?}"
        );
        // …but it IS the prompt of the second request, right after the
        // original user message.
        let requests = model.get_recorded_requests();
        assert_eq!(requests.len(), 2);
        let payload = &requests[1].chat_history;
        let user_texts: Vec<String> = payload
            .iter()
            .filter_map(|m| match m {
                rig_core::completion::message::Message::User { content } => {
                    content.iter().find_map(|c| match c {
                        rig_core::completion::message::UserContent::Text(t) => Some(t.to_string()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            user_texts,
            vec!["go".to_string(), crate::EMPTY_REPLY_NUDGE.to_string()],
            "2nd request ends with the nudge User message preceded by the \
             original user message; payload={payload:?}"
        );
    }

    /// T7 — the nudge after a completed tool round-trip resumes from the
    /// transcript tail: the THIRD request's history tail is the ToolResult,
    /// the prompt is the nudge, and the tool call/result pair is intact.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t7_nudge_after_tool_roundtrip_carries_intact_pairs() {
        let (sm, model, turn) = staged_reply_fixture(vec![
            crate::mock::MockResponse::tool_call(
                "todo",
                serde_json::json!({"action": "add", "tasks": ["t"]}),
            ),
            crate::mock::MockResponse::text(""),
            crate::mock::MockResponse::text("ok"),
        ]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(rows[0].content, "ok");

        let requests = model.get_recorded_requests();
        assert_eq!(requests.len(), 3, "tool call, empty reply, nudged reply");
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};
        let payload = &requests[2].chat_history;

        // Prompt is the nudge; the message before it is the ToolResult.
        let last = payload.last().expect("payload non-empty");
        match last {
            RigMessage::User { content } => {
                let text = content.iter().find_map(|c| match c {
                    UserContent::Text(t) => Some(t.to_string()),
                    _ => None,
                });
                assert_eq!(
                    text.as_deref(),
                    Some(crate::EMPTY_REPLY_NUDGE),
                    "the 3rd request's prompt must be the nudge"
                );
            }
            other => panic!("expected a User prompt, got {other:?}"),
        }
        let tail_before_prompt = payload
            .get(payload.len() - 2)
            .expect("payload has a message before the prompt");
        assert!(
            matches!(
                tail_before_prompt,
                RigMessage::User { content }
                    if content
                        .iter()
                        .any(|c| matches!(c, UserContent::ToolResult(_)))
            ),
            "the history tail must be the ToolResult; got {tail_before_prompt:?}"
        );
        // The tool pair is intact: a ToolCall whose id matches a ToolResult.
        let call_ids: Vec<String> = payload
            .iter()
            .flat_map(|m| match m {
                RigMessage::Assistant { content, .. } => content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                _ => vec![],
            })
            .collect();
        assert!(
            !call_ids.is_empty(),
            "the 3rd request must carry the tool call; payload={payload:?}"
        );
        assert!(
            payload.iter().any(|m| matches!(
                m,
                RigMessage::User { content }
                    if content.iter().any(|c| matches!(
                        c,
                        UserContent::ToolResult(tr) if call_ids.contains(&tr.id)
                    ))
            )),
            "the tool result must pair with its call id; payload={payload:?}"
        );
    }

    /// T8 — empty, then whitespace-only: no agent row at all, exactly two
    /// requests (one nudge, then give up), and ONE system message starting
    /// with the ⚠ warning.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t8_double_empty_reply_yields_warning_not_blank_row() {
        let (sm, model, turn) = staged_reply_fixture(vec![
            crate::mock::MockResponse::text(""),
            crate::mock::MockResponse::text("  "),
        ]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert!(
            rows.is_empty(),
            "no agent row may be persisted for an empty-then-blank turn; transcript={msgs:?}"
        );
        assert_eq!(
            model.request_count(),
            2,
            "one nudge, then the warning — no third request"
        );
        let warnings: Vec<&crate::ui::app_state::ChatMessage> = msgs
            .iter()
            .filter(|m| {
                m.role == crate::ui::app_state::MessageRole::System && m.content.starts_with('⚠')
            })
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "exactly one ⚠ system message; transcript={msgs:?}"
        );
        assert_eq!(
            warnings[0].content,
            crate::EMPTY_REPLY_WARNING,
            "the warning carries the exact locked wording"
        );
    }

    /// T9 — Stop mid-tool: the staged prose of the interrupted response is
    /// flushed by the wrapper's `commit_reply("")` on the Stopped exit — the
    /// agent row "PARTIAL" lands after the INTERRUPTED tool result, and the
    /// staging buffer is empty afterwards.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t9_stop_mid_tool_flushes_staged_prose_as_final_row() {
        use crate::ui::app_state::MessageRole;

        let storage: Arc<dyn crate::storage::ConversationStorage> =
            Arc::new(crate::storage::InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage);
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");
        sm.add_user_message("go".to_string());

        let (agent, _info, mut events, _hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        // A real in-flight tool (30s sleep) carrying the staged prose.
        model.add_response(crate::mock::MockResponse::tool_call_with_follow_up(
            "bash",
            serde_json::json!({ "command": "sleep 30" }),
            "PARTIAL",
        ));

        let sm_opt_pump = Some(sm.clone());
        let pump = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                AgentRunner::process_event_for_ui(&sm_opt_pump, ev);
            }
        });

        sm.set_running(true);
        let current_turn = sm
            .build_current_turn_message()
            .expect("the user turn was dispatched");
        let agent = Arc::new(agent);
        let config = test_config();
        let sm_opt = Some(sm.clone());
        let turn = tokio::spawn(async move {
            AgentRunner::process_message_internal(current_turn, &sm_opt, &agent, &config).await
        });

        // Wait (≤5s) until the ToolCall row has landed and the tool is in flight.
        let call_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if let Some(tail) = msgs.last()
                    && tail.role == MessageRole::ToolCall
                    && tail.tool_name.as_deref() == Some("bash")
                {
                    return tail.call_id.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the bash ToolCall row must land within 5s");

        // Stop: cancels the per-turn token; the turn's future is dropped.
        sm.stop_turn_processes();

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn)
            .await
            .expect("the cancelled turn must finish within 5s")
            .expect("the turn task must not panic");
        assert!(
            matches!(result, CompletionResult::Stopped),
            "a cancelled turn must report Stopped"
        );

        let msgs = sm.get_state().chat.messages;
        // The INTERRUPTED result is answered for the in-flight call…
        let interrupted_idx = msgs
            .iter()
            .rposition(|m| m.role == MessageRole::ToolResult && m.call_id == call_id)
            .expect("the INTERRUPTED result row exists");
        let interrupted = &msgs[interrupted_idx];
        assert!(
            interrupted
                .tool_result
                .as_deref()
                .unwrap_or_default()
                .starts_with("INTERRUPTED")
        );
        // …and the staged prose lands as the final agent row, after it.
        let rows: Vec<&crate::ui::app_state::ChatMessage> = msgs
            .iter()
            .skip(interrupted_idx + 1)
            .filter(|m| m.role == MessageRole::Agent && m.source.is_orchestrator_lane())
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "exactly one agent row after the INTERRUPTED result; transcript={msgs:?}"
        );
        assert_eq!(
            rows[0].content, "PARTIAL",
            "the staged prose is the flushed final row"
        );
        // Staging is empty afterwards: another flush commits nothing.
        let before = msgs.len();
        assert!(
            !sm.commit_reply(""),
            "staging must be empty after the flush"
        );
        assert_eq!(
            sm.get_state().chat.messages.len(),
            before,
            "no further row may appear"
        );

        pump.abort();
    }

    /// T10 — a non-transient LLM error after staged prose: the turn reports
    /// Error with its ❌ system message, and the staged prose is still
    /// persisted as the agent row (the wrapper's flush-on-exit).
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t10_hard_error_flushes_staged_prose_and_keeps_error_row() {
        let (sm, _model, turn) = staged_reply_fixture(vec![
            fu("A"),
            crate::mock::MockResponse::error("authentication failed"),
        ]);

        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(
            matches!(result, CompletionResult::Error),
            "a non-transient error must report Error"
        );

        let msgs = sm.get_state().chat.messages;
        let rows = agent_rows_after_user(&msgs);
        assert_eq!(rows.len(), 1, "one row; transcript={msgs:?}");
        assert_eq!(
            rows[0].content, "A",
            "the staged prose survives the error exit"
        );
        assert!(
            msgs.iter().any(|m| {
                m.role == crate::ui::app_state::MessageRole::System && m.content.starts_with('❌')
            }),
            "the ❌ failure message must be present; transcript={msgs:?}"
        );
    }

    /// T11 — persistence: after the T1 shape (staged prose + closing text),
    /// reloading the conversation from storage yields "ANSWER" exactly ONCE in
    /// the messages and exactly ONCE in the rebuilt wire history — the merged
    /// row round-trips, and nothing duplicates.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn t11_merged_reply_survives_reload_without_duplicating() {
        use crate::storage::{ConversationStorage, InMemoryStorage};

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");
        sm.add_user_message("go".to_string());

        let (agent, _info, mut events, _hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        model.add_responses(vec![
            fu("ANSWER"),
            crate::mock::MockResponse::text("🙏 amen"),
        ]);
        let sm_opt_pump = Some(sm.clone());
        let pump = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                AgentRunner::process_event_for_ui(&sm_opt_pump, ev);
            }
        });
        sm.set_running(true);
        let current_turn = sm
            .build_current_turn_message()
            .expect("the user turn was dispatched");
        let agent = Arc::new(agent);
        let config = test_config();
        let sm_opt = Some(sm.clone());
        let turn = tokio::spawn(async move {
            AgentRunner::process_message_internal(current_turn, &sm_opt, &agent, &config).await
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), turn)
            .await
            .expect("the turn finishes")
            .expect("no panic");
        assert!(matches!(result, CompletionResult::Success));

        // Reload from storage into the SAME StateManager.
        let id = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");
        sm.load_conversation(id).expect("conversation reloads");

        let msgs = sm.get_state().chat.messages;
        let containing: Vec<&crate::ui::app_state::ChatMessage> = msgs
            .iter()
            .filter(|m| m.content.contains("ANSWER"))
            .collect();
        assert_eq!(
            containing.len(),
            1,
            "ANSWER must live in exactly one reloaded message; messages={msgs:?}"
        );
        assert_eq!(
            containing[0].content, "ANSWER\n\n🙏 amen",
            "the merged row round-trips byte-identical"
        );
        assert_eq!(
            count_occurrences(&sm.get_agent_history(), "ANSWER"),
            1,
            "ANSWER must occur once in the wire history after reload"
        );

        pump.abort();
    }

    // T12 — no-regression pin (pre-existing, must stay UNCHANGED):
    // `orchestrator_completion_prose_is_not_double_added` (above) — an
    // orchestrator `CompletionResponse` event adds NO transcript row; the
    // commit path owns that row.

    /// c — teardown mid-tool: when the action channel closes with a turn in
    /// flight, `run_loop` must abort the agent task and answer the in-flight
    /// orchestrator-lane ToolCall with a persisted INTERRUPTED result. Not
    /// covered by b3: teardown never reaches the cancel arm — it takes the
    /// abort branch, a different hook.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn teardown_mid_tool_persists_interrupted_result() {
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::MessageRole;
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");

        let (agent, info, events, hook, model) =
            crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                .expect("mock agent builds");
        model.add_response(crate::mock::MockResponse::tool_call(
            "bash",
            serde_json::json!({ "command": "sleep 30" }),
        ));

        // Compaction off: AgentRunner::new must not need a compaction provider.
        let config = Arc::new(test_config());
        let model = mock_resolved_model();

        let mut runner = AgentRunner::new(
            agent,
            config,
            info,
            SkillRegistry::default(),
            Some(events),
            Some(sm.clone()),
            hook,
            &model,
        )
        .expect("runner builds");

        let (tx, rx) = mpsc::unbounded_channel::<UiAction>();
        let h = tokio::spawn(async move { runner.run_loop(rx).await });

        tx.send(UiAction::SendMessage {
            text: "go".into(),
            attachments: Vec::new(),
        })
        .expect("the action channel is open");

        // Wait (≤10s) until the ToolCall row has landed and the tool is in
        // flight, then close the action channel (session teardown).
        let call_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if let Some(tail) = msgs.last()
                    && tail.role == MessageRole::ToolCall
                    && tail.tool_name.as_deref() == Some("bash")
                {
                    return tail.call_id.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the bash ToolCall row must land within 10s");

        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");

        // The same INTERRUPTED row as b3 — without the sub-agent quote —
        // persisted.
        let msgs = sm.get_state().chat.messages;
        let last_orch = msgs
            .iter()
            .rev()
            .find(|m| m.source.is_orchestrator_lane())
            .expect("the orchestrator lane is non-empty");
        assert_eq!(
            last_orch.role,
            MessageRole::ToolResult,
            "the in-flight call must be answered at teardown; transcript={msgs:?}"
        );
        assert_eq!(
            last_orch.call_id, call_id,
            "the answer must carry the bash call's id"
        );
        assert_eq!(last_orch.tool_name.as_deref(), Some("bash"));
        let text = last_orch.tool_result.as_deref().unwrap_or_default();
        assert!(
            text.starts_with("INTERRUPTED: the turn was stopped before `bash` returned."),
            "got: {text}"
        );
        assert!(
            !text.contains("Last message from sub-agent"),
            "no sub-agent rows exist — the quote must not appear; got: {text}"
        );

        let id = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");
        let conv = storage.load(id).expect("conversation persists");
        assert!(
            conv.messages.iter().any(|m| {
                matches!(
                    m,
                    crate::conversation::Message::ToolResult { call_id: cid, .. }
                        if cid == &call_id
                )
            }),
            "the INTERRUPTED result must be on disk"
        );
    }

    /// Shared e2e harness for the `mod tests` panic suite: builds a mock
    /// agent around `tool`, wires it through `AgentRunner::new` with
    /// compaction off, and spawns `run_loop` on the given actions channel.
    /// Returns the `MockCompletionModel` handle (for `add_response` /
    /// `request_count` / `get_recorded_requests`), the `UiAction` sender,
    /// and the `run_loop` task's `JoinHandle` — callers queue responses
    /// on the model BEFORE sending `UiAction::SendMessage`, drive the turn
    /// via `tx`, and `drop(tx)` + await the handle for teardown.
    ///
    /// Extracted from the panic-wedge test (formerly inlined) so the new
    /// panic-becomes-tool-result e2e test doesn't duplicate the ~40 lines of
    /// agent/runner wiring.
    #[cfg(feature = "mock")]
    fn spawn_mock_runner(
        sm: &Arc<StateManager>,
        tool: Box<dyn ToolDyn>,
        responses: Vec<crate::mock::MockResponse>,
        supports_vision: bool,
    ) -> (
        crate::mock::MockCompletionModel,
        mpsc::UnboundedSender<UiAction>,
        tokio::task::JoinHandle<()>,
    ) {
        let model = crate::mock::MockCompletionModel::new();
        let model_clone = model.clone();
        let (ev_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let hook = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
            .with_state_manager(sm)
            .with_wire_label("mock".to_string(), "mock-model".to_string());

        let agent = rig_core::agent::AgentBuilder::new(model)
            .preamble("test prompt")
            .max_tokens(1024)
            .default_max_turns(4)
            .hook(hook.clone())
            .tools(vec![tool])
            .build();

        let info = ProviderInfo {
            name: "mock".to_string(),
            model: "mock-model".to_string(),
            supports_vision,
            preserve_reasoning: true,
            display_reasoning: false,
        };

        for response in responses {
            model_clone.add_response(response);
        }

        // Compaction off: AgentRunner::new must not need a compaction provider.
        let config = Arc::new(test_config());
        let model = mock_resolved_model();

        let mut runner = AgentRunner::new(
            DynAgent::Mock(agent),
            config,
            info,
            SkillRegistry::default(),
            Some(events),
            Some(sm.clone()),
            Arc::new(hook),
            &model,
        )
        .expect("runner builds");

        let (tx, rx) = mpsc::unbounded_channel::<UiAction>();
        let h = tokio::spawn(async move { runner.run_loop(rx).await });
        (model_clone, tx, h)
    }

    // ── T7 — controller attachment ingest (file-uploads design §4.1) ───────
    //
    // **Status: compile-fail until T7 lands.** Written against the locked
    // design interface: `UiAction::SendMessage { text, attachments }`
    // (§3.2), `StateManager::with_uploads` (§5.5), and the `collect`-based
    // ingest that replaces the deleted vision gate. The `PanicTool` is
    // registered but never called (the mock answers with plain text).

    /// Shared fixture: a boot conversation plus a tempdir upload store wired
    /// into the SM. `tmp` must outlive the test (the store's root lives there).
    #[cfg(feature = "mock")]
    fn attachment_ingest_fixture() -> (
        Arc<StateManager>,
        crate::attachments::UploadStore,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::attachments::UploadStore::new(
            tmp.path().to_path_buf(),
            crate::config::UploadsConfig {
                max_file_mb: 50,
                max_files: 10,
            },
        );
        let sm = Arc::new(StateManager::new().with_uploads(store.clone()));
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");
        (sm, store, tmp)
    }

    /// Unknown attachment id → system "❌ …", and NOTHING is queued: no user
    /// row, no LLM call (design §4.1 `collect` → `find` → None).
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn controller_unknown_attachment_id_refuses_and_queues_nothing() {
        use crate::ui::app_state::MessageRole;

        let (sm, _store, _tmp) = attachment_ingest_fixture();
        let (model, tx, h) = spawn_mock_runner(&sm, Box::new(PanicTool(sm.clone())), vec![], true);

        tx.send(UiAction::SendMessage {
            text: "hi".to_string(),
            attachments: vec![uuid::Uuid::new_v4()],
        })
        .expect("the action channel is open");

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if msgs
                    .iter()
                    .any(|m| m.role == MessageRole::System && m.content.starts_with("❌"))
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("an unknown attachment id must surface a ❌ system message within 10s");

        let msgs = sm.get_state().chat.messages;
        assert!(
            !msgs.iter().any(|m| m.role == MessageRole::User),
            "a refused send must not queue a user message: {msgs:?}"
        );
        assert_eq!(
            model.request_count(),
            0,
            "a refused send must not call the model"
        );

        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }

    /// `/new` carrying ids → refusal, and `/new` is NOT run (design §4.1:
    /// Command + non-empty attachments → system "❌ Attachments can't be sent
    /// with a /command.", not queued).
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn controller_command_with_attachments_is_refused() {
        use crate::ui::app_state::MessageRole;

        let (sm, _store, _tmp) = attachment_ingest_fixture();
        let convo_before = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");

        let (model, tx, h) = spawn_mock_runner(&sm, Box::new(PanicTool(sm.clone())), vec![], true);

        tx.send(UiAction::SendMessage {
            text: "/new".to_string(),
            attachments: vec![uuid::Uuid::new_v4()],
        })
        .expect("the action channel is open");

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if msgs.iter().any(|m| {
                    m.role == MessageRole::System
                        && m.content
                            .contains("Attachments can't be sent with a /command")
                }) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("/new with attachments must surface the refusal system message within 10s");

        assert_eq!(
            sm.get_current_conversation_id(),
            Some(convo_before),
            "the refused /new must not switch conversations"
        );
        assert_eq!(
            model.request_count(),
            0,
            "a refused command must not call the model"
        );

        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }

    /// Regression of the OLD vision refusal (deleted in T7): a non-vision
    /// model + an image attachment is QUEUED — the image degrades to a file
    /// reference at turn-build time, it is never refused at ingest.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn controller_non_vision_model_with_image_attachment_is_queued() {
        use crate::ui::app_state::MessageRole;
        use base64::Engine;

        let (sm, store, _tmp) = attachment_ingest_fixture();
        let convo_id = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
            .expect("valid base64 fixture");
        let att = store
            .store(convo_id, "cat.png", &png[..])
            .await
            .expect("store the PNG");

        // supports_vision = false: the old gate would have refused this send.
        let (model, tx, h) = spawn_mock_runner(
            &sm,
            Box::new(PanicTool(sm.clone())),
            vec![crate::mock::MockResponse::text("ok")],
            false,
        );

        tx.send(UiAction::SendMessage {
            text: "what's in this?".to_string(),
            attachments: vec![att.id],
        })
        .expect("the action channel is open");

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = sm.get_state();
                let landed = state
                    .chat
                    .messages
                    .iter()
                    .any(|m| m.role == MessageRole::User && m.content == "what's in this?");
                if landed && !sm.is_running() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "non-vision model + image attachment must be QUEUED (the old vision \
             refusal is gone); the user row must land within 10s",
        );

        assert_eq!(
            model.request_count(),
            1,
            "the queued turn must run exactly one LLM request"
        );

        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }

    /// 11 ids with `max_files: 10` → TooMany refusal (the count check runs
    /// before any lookup/copy, so the ids need not exist), nothing queued.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn controller_eleven_attachments_with_max_ten_is_too_many() {
        use crate::ui::app_state::MessageRole;

        let (sm, _store, _tmp) = attachment_ingest_fixture();
        let (model, tx, h) = spawn_mock_runner(&sm, Box::new(PanicTool(sm.clone())), vec![], true);

        let ids: Vec<uuid::Uuid> = (0..11).map(|_| uuid::Uuid::new_v4()).collect();
        tx.send(UiAction::SendMessage {
            text: "x".to_string(),
            attachments: ids,
        })
        .expect("the action channel is open");

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let msgs = sm.get_state().chat.messages;
                if msgs.iter().any(|m| {
                    m.role == MessageRole::System
                        && m.content.starts_with("❌")
                        && m.content.contains("11")
                        && m.content.contains("10")
                }) {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("11 ids with max_files 10 must surface a TooMany ❌ message within 10s");

        let msgs = sm.get_state().chat.messages;
        assert!(
            !msgs.iter().any(|m| m.role == MessageRole::User),
            "a TooMany refusal must not queue a user message: {msgs:?}"
        );
        assert_eq!(
            model.request_count(),
            0,
            "a TooMany refusal must not call the model"
        );

        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }

    /// A rig tool that panics once its own ToolCall row has landed in the
    /// transcript — the production wedge in miniature: a panic inside a tool
    /// call (dead-pty EIO → the subscriber's `eprintln!` → panic on dead
    /// stderr) must not take the agent loop down with it.
    struct PanicTool(Arc<StateManager>);

    impl rig_core::tool::Tool for PanicTool {
        const NAME: &'static str = "panic_tool";
        type Error = std::convert::Infallible;
        type Args = serde_json::Value;
        type Output = String;

        async fn definition(&self, _p: String) -> rig_core::completion::ToolDefinition {
            rig_core::completion::ToolDefinition {
                name: Self::NAME.to_string(),
                description: "panics once its ToolCall row has landed".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn call(&self, _args: serde_json::Value) -> Result<String, Self::Error> {
            // Wait (≤5s) until the hook's ToolCall event has been processed
            // and the row is the transcript's tail — the panic must land
            // *after* the call is in the transcript, or the fix's
            // `close_interrupted_tool_call` has nothing to answer.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let msgs = self.0.get_state().chat.messages;
                if let Some(tail) = msgs.last()
                    && tail.role == crate::ui::app_state::MessageRole::ToolCall
                    && tail.tool_name.as_deref() == Some(Self::NAME)
                {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("boom from panic_tool (ToolCall row never landed)");
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("boom from panic_tool");
        }
    }

    /// NEW (post `TimeBudget` panic boundary): a tool registered the way
    /// production registers every tool — through `TimeBudget::wrap`, same as
    /// `budget_all` in `src/providers/mod.rs` — must have its panic caught
    /// AT THE TOOL BOUNDARY and turned into an `Ok(panic_message(..))` tool
    /// result. The turn must NOT abort: the model sees the panic as an
    /// ordinary (if alarming) tool result and gets to react to it in the
    /// same turn, exactly like a tool that returns `Err` today.
    ///
    /// RED on master: `TimeBudget::call` has no panic boundary yet, so the
    /// panic propagates all the way out to the `agent_loop` backstop
    /// (`panic_outside_tool_boundary_aborts_turn_and_loop_keeps_serving`'s
    /// path) — the turn aborts, "recovered" never arrives, and this test's
    /// 10s wait times out.
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn tool_panic_becomes_tool_result_and_turn_continues() {
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::MessageRole;
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");

        // Wrapped through the PRODUCTION constructor, exactly as `budget_all`
        // wraps every tool the model can call — this is the seam the fix
        // owns, not a hand-rolled test double of it.
        let tool: Box<dyn ToolDyn> = Box::new(crate::tools::TimeBudget::wrap(
            Box::new(PanicTool(sm.clone())),
            &crate::config::TimeoutsConfig::default(),
        ));

        let (model, tx, h) = spawn_mock_runner(
            &sm,
            tool,
            vec![
                crate::mock::MockResponse::tool_call("panic_tool", serde_json::json!({})),
                crate::mock::MockResponse::text("recovered"),
            ],
            true,
        );

        tx.send(UiAction::SendMessage {
            text: "go".into(),
            attachments: Vec::new(),
        })
        .expect("the action channel is open");

        // Wait (≤10s) for the SAME turn to recover: the model's follow-up
        // text ("recovered") must land and the loop must finish the turn
        // normally — no second SendMessage, no turn abort in between.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = sm.get_state();
                let recovered = state
                    .chat
                    .messages
                    .iter()
                    .any(|m| m.role == MessageRole::Agent && m.content.contains("recovered"));
                if recovered && !sm.is_running() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "a panicking tool must not abort the turn: \"recovered\" must land within 10s \
             on master the panic propagates past TimeBudget untouched and the turn aborts \
             instead, so this never arrives",
        );

        let msgs = sm.get_state().chat.messages;
        let call = msgs
            .iter()
            .find(|m| {
                m.source.is_orchestrator_lane()
                    && m.role == MessageRole::ToolCall
                    && m.tool_name.as_deref() == Some("panic_tool")
            })
            .expect("the panic_tool ToolCall row must be in the transcript");
        let call_id = call.call_id.clone();
        let answer = msgs
            .iter()
            .find(|m| {
                m.source.is_orchestrator_lane()
                    && m.role == MessageRole::ToolResult
                    && m.call_id == call_id
            })
            .expect("the panicking call must be answered with a tool result, not left open");
        let text = answer.tool_result.as_deref().unwrap_or_default();
        assert!(
            text.starts_with("💥 PANIC: tool `panic_tool` crashed: boom from panic_tool"),
            "got: {text}"
        );

        assert!(
            !msgs.iter().any(|m| m.role == MessageRole::System
                && (m.content.contains("turn aborted") || m.content.contains("internal error"))),
            "a tool panic must not trip the turn-level backstop; transcript={msgs:?}"
        );

        assert_eq!(
            model.request_count(),
            2,
            "exactly two LLM calls: the tool_call turn and the follow-up that saw the panic result"
        );
        let recorded = model.get_recorded_requests();
        let second_request_history = format!("{:?}", recorded[1].chat_history);
        assert!(
            second_request_history.contains("PANIC: tool `panic_tool` crashed"),
            "the model's second request must include the panic result in its history; got: {second_request_history}"
        );

        // Teardown: closing the action channel ends run_loop promptly.
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }

    /// Regression test for the agent-loop panic wedge (PR #349's backstop):
    /// a panic OUTSIDE the `TimeBudget` tool boundary — e.g. in the agent
    /// loop's own dispatch plumbing — must still abort the *turn*, not the
    /// whole agent loop, and the loop must keep serving. Uses the
    /// UNWRAPPED `PanicTool` (production never registers an unwrapped tool;
    /// this stands in for a panic the per-tool boundary can't reach, so the
    /// turn-level `catch_unwind` in `agent_loop` is what must catch it).
    ///
    /// Passes on master (the backstop already exists) and must keep passing
    /// after the `TimeBudget` panic boundary lands — the two are
    /// independent safety nets, inner (per-tool) and outer (per-turn).
    #[cfg(feature = "mock")]
    #[tokio::test(flavor = "multi_thread")]
    async fn panic_outside_tool_boundary_aborts_turn_and_loop_keeps_serving() {
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::MessageRole;
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::new());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        sm.ensure_boot_conversation(std::path::Path::new("."), "mock-model");

        let (_model, tx, h) = spawn_mock_runner(
            &sm,
            Box::new(PanicTool(sm.clone())),
            vec![
                crate::mock::MockResponse::tool_call("panic_tool", serde_json::json!({})),
                crate::mock::MockResponse::text("still alive"),
            ],
            true,
        );

        tx.send(UiAction::SendMessage {
            text: "go".into(),
            attachments: Vec::new(),
        })
        .expect("the action channel is open");

        // Wait (≤10s) for the turn to abort: `is_running` must clear and a
        // system message must carry the panic payload.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = sm.get_state();
                let aborted = state.chat.messages.iter().any(|m| {
                    m.role == MessageRole::System
                        && m.content.contains("internal error: boom from panic_tool")
                });
                if aborted && !sm.is_running() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "the panicked turn must abort within 10s (system message carrying the \
             panic payload + is_running cleared)",
        );

        // The in-flight call must be answered with the persisted INTERRUPTED
        // result (the row `close_interrupted_tool_call` writes). Located by
        // call_id: the fix appends the "internal error" system row right
        // after the answer, so the answer is not necessarily the lane's tail.
        let msgs = sm.get_state().chat.messages;
        let call = msgs
            .iter()
            .find(|m| {
                m.source.is_orchestrator_lane()
                    && m.role == MessageRole::ToolCall
                    && m.tool_name.as_deref() == Some("panic_tool")
            })
            .expect("the panic_tool ToolCall row must be in the transcript");
        let call_id = call.call_id.clone();
        let answer = msgs
            .iter()
            .find(|m| {
                m.source.is_orchestrator_lane()
                    && m.role == MessageRole::ToolResult
                    && m.call_id == call_id
            })
            .expect("the in-flight panic_tool call must be answered; transcript={msgs:?}");
        assert_eq!(
            answer.tool_name.as_deref(),
            Some("panic_tool"),
            "the answer must name the tool"
        );
        let text = answer.tool_result.as_deref().unwrap_or_default();
        assert!(
            text.starts_with("INTERRUPTED: the turn was stopped before `panic_tool` returned."),
            "got: {text}"
        );

        // The answer must be persisted, not just in memory.
        let id = sm
            .get_current_conversation_id()
            .expect("boot conversation exists");
        let conv = storage.load(id).expect("conversation persists");
        assert!(
            conv.messages.iter().any(|m| {
                matches!(
                    m,
                    crate::conversation::Message::ToolResult { call_id: cid, .. }
                        if cid == &call_id
                )
            }),
            "the INTERRUPTED result must be on disk"
        );

        // No sub-agent was running — the slot must be empty.
        assert!(
            sm.get_state().sub_agent.is_none(),
            "no sub-agent exists in this test; the slot must be None"
        );

        // The loop must keep serving: the next turn runs to completion.
        tx.send(UiAction::SendMessage {
            text: "again".into(),
            attachments: Vec::new(),
        })
        .expect("the action channel is open");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = sm.get_state();
                let replied = state
                    .chat
                    .messages
                    .iter()
                    .any(|m| m.role == MessageRole::Agent && m.content.contains("still alive"));
                if replied && !sm.is_running() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("after the aborted turn the loop must serve the next message within 10s");

        // Teardown: closing the action channel ends run_loop promptly.
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(10), h)
            .await
            .expect("run_loop must finish within 10s of the action channel closing")
            .expect("the run_loop task must not panic");
    }
    #[cfg(feature = "mock")]
    mod two_stage_stop {
        use super::*;
        use crate::ui::app_state::MessageRole;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::Notify;

        const GRACEFUL_MSG: &str = "Agent stopped by user (after the current tool call)";
        const IMMEDIATE_MSG: &str = "Agent stopped by user";
        const REAL: &str = "REAL_RESULT";

        /// A tool that blocks until the test releases it, then returns
        /// [`REAL`]. Makes "the tool is in flight" deterministic: `started`
        /// fires on entry, `release` lets it finish.
        struct GateTool {
            started: Arc<Notify>,
            release: Arc<Notify>,
            ran: Arc<AtomicUsize>,
        }

        impl rig_core::tool::Tool for GateTool {
            const NAME: &'static str = "gate_tool";
            type Error = std::convert::Infallible;
            type Args = serde_json::Value;
            type Output = String;

            async fn definition(&self, _p: String) -> rig_core::completion::ToolDefinition {
                rig_core::completion::ToolDefinition {
                    name: Self::NAME.to_string(),
                    description: "blocks until the test releases it".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            }

            async fn call(&self, _args: serde_json::Value) -> Result<String, Self::Error> {
                self.ran.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                self.release.notified().await;
                Ok(REAL.to_string())
            }
        }

        struct Rig {
            sm: Arc<StateManager>,
            storage: Arc<dyn crate::storage::ConversationStorage>,
            model: crate::mock::MockCompletionModel,
            tx: mpsc::UnboundedSender<UiAction>,
            h: tokio::task::JoinHandle<()>,
            started: Arc<Notify>,
            release: Arc<Notify>,
            ran: Arc<AtomicUsize>,
        }

        impl Rig {
            fn send(&self, text: &str) {
                self.tx
                    .send(UiAction::SendMessage {
                        text: text.to_string(),
                        attachments: Vec::new(),
                    })
                    .expect("the action channel is open");
            }

            fn press(&self) {
                self.tx
                    .send(UiAction::RequestStop)
                    .expect("the action channel is open");
            }

            async fn wait_tool_started(&self) {
                tokio::time::timeout(std::time::Duration::from_secs(10), self.started.notified())
                    .await
                    .expect("the gate tool must start within 10s");
            }

            /// Wait (≤10s) for `cond` over the chat transcript.
            async fn wait_for(
                &self,
                what: &str,
                cond: impl Fn(&[crate::ui::app_state::ChatMessage]) -> bool,
            ) {
                let sm = self.sm.clone();
                let ok = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
                    loop {
                        if cond(&sm.get_state().chat.messages) {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                })
                .await;
                assert!(
                    ok.is_ok(),
                    "timed out waiting for: {what}; transcript={:?}",
                    self.sm.get_state().chat.messages
                );
            }

            async fn wait_stop_pending(&self) {
                let sm = self.sm.clone();
                let ok = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
                    while !sm.get_state().stop_pending {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await;
                assert!(
                    ok.is_ok(),
                    "the first press must arm AppState.stop_pending (graceful stage)"
                );
            }

            async fn wait_stop_message(&self) {
                self.wait_for("a 'Agent stopped by user…' system message", |m| {
                    m.iter().any(is_stop_msg)
                })
                .await;
                // Settle: a buggy second message would land shortly after.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }

            fn stop_messages(&self) -> Vec<String> {
                self.sm
                    .get_state()
                    .chat
                    .messages
                    .iter()
                    .filter(|m| is_stop_msg(m))
                    .map(|m| m.content.clone())
                    .collect()
            }

            async fn shutdown(self) {
                self.h.abort();
                let _ = self.h.await;
            }
        }

        fn is_stop_msg(m: &crate::ui::app_state::ChatMessage) -> bool {
            m.role == MessageRole::System && m.content.starts_with("Agent stopped by user")
        }

        /// Build a mock-backed runner around [`GateTool`].
        fn spawn_rig(responses: Vec<crate::mock::MockResponse>) -> Rig {
            let storage: Arc<dyn crate::storage::ConversationStorage> =
                Arc::new(crate::storage::InMemoryStorage::new());
            let sm = StateManager::new_arc_with_storage(storage.clone());

            let model = crate::mock::MockCompletionModel::new();
            for r in responses {
                model.add_response(r);
            }
            let (ev_tx, events) = tokio::sync::mpsc::unbounded_channel();
            let inner = SessionHook::with_context_tracking(Some(ev_tx), sm.stats_arc())
                .with_state_manager(&sm)
                .with_wire_label("mock".to_string(), "mock-model".to_string());

            let (tx, rx) = mpsc::unbounded_channel::<UiAction>();

            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let ran = Arc::new(AtomicUsize::new(0));
            let gate = GateTool {
                started: started.clone(),
                release: release.clone(),
                ran: ran.clone(),
            };
            let agent = rig_core::agent::AgentBuilder::new(model.clone())
                .preamble("test prompt")
                .max_tokens(1024)
                .default_max_turns(4)
                .hook(inner.clone())
                .tools(vec![Box::new(gate) as Box<dyn ToolDyn>])
                .build();

            let info = ProviderInfo {
                name: "mock".to_string(),
                model: "mock-model".to_string(),
                supports_vision: false,
                preserve_reasoning: true,
                display_reasoning: false,
            };
            // Compaction off: AgentRunner::new must not need a compaction provider.
            let config = Arc::new(test_config());
            let resolved = mock_resolved_model();
            let mut runner = AgentRunner::new(
                DynAgent::Mock(agent),
                config,
                info,
                SkillRegistry::default(),
                Some(events),
                Some(sm.clone()),
                Arc::new(inner),
                &resolved,
            )
            .expect("runner builds");
            let h = tokio::spawn(async move { runner.run_loop(rx).await });

            Rig {
                sm,
                storage,
                model,
                tx,
                h,
                started,
                release,
                ran,
            }
        }

        fn gate_call() -> crate::mock::MockResponse {
            crate::mock::MockResponse::tool_call("gate_tool", serde_json::json!({}))
        }

        fn rows(
            sm: &Arc<StateManager>,
            role: MessageRole,
        ) -> Vec<crate::ui::app_state::ChatMessage> {
            sm.get_state()
                .chat
                .messages
                .into_iter()
                .filter(|m| m.role == role)
                .collect()
        }

        fn mentions_interrupted(sm: &Arc<StateManager>) -> bool {
            sm.get_state().chat.messages.iter().any(|m| {
                m.content.contains("INTERRUPTED")
                    || m.tool_result
                        .as_deref()
                        .is_some_and(|t| t.contains("INTERRUPTED"))
            })
        }

        // ── graceful ────────────────────────────────────────────────────────

        #[tokio::test(flavor = "multi_thread")]
        async fn graceful_stop_records_real_tool_result_and_no_interrupted() {
            let rig = spawn_rig(vec![
                gate_call(),
                crate::mock::MockResponse::text("SHOULD NOT BE SENT"),
            ]);
            rig.send("go");
            rig.wait_tool_started().await;

            rig.press();
            rig.wait_stop_pending().await;
            assert!(rig.sm.is_running(), "graceful leaves the turn running");
            assert!(
                rows(&rig.sm, MessageRole::ToolResult).is_empty(),
                "the tool is still in flight — nothing may be recorded for it yet"
            );

            rig.release.notify_one(); // the in-flight tool now finishes
            rig.wait_stop_message().await;

            let results = rows(&rig.sm, MessageRole::ToolResult);
            assert_eq!(
                rig.ran.load(Ordering::SeqCst),
                1,
                "the tool ran exactly once"
            );
            assert_eq!(results.len(), 1, "exactly one tool result");
            assert_eq!(results[0].tool_name.as_deref(), Some("gate_tool"));
            assert_eq!(
                results[0].tool_result.as_deref(),
                Some(REAL),
                "the in-flight call records its REAL result, not INTERRUPTED"
            );
            assert!(!mentions_interrupted(&rig.sm));
            assert_eq!(
                rig.model.request_count(),
                1,
                "the turn must end BEFORE the next LLM call"
            );
            assert_eq!(
                rig.model.remaining(),
                1,
                "the 2nd canned reply was never fetched"
            );
            assert!(
                rows(&rig.sm, MessageRole::Agent)
                    .iter()
                    .all(|m| !m.content.contains("SHOULD NOT BE SENT")),
                "no assistant answer after the graceful stop"
            );
            assert_eq!(rig.stop_messages(), vec![GRACEFUL_MSG.to_string()]);
            assert!(!rig.sm.is_running(), "the turn ended");
            assert!(
                !rig.sm.get_state().stop_pending,
                "the flag is reset at turn end"
            );

            // The real result is persisted (survives reload).
            let id = rig
                .sm
                .get_current_conversation_id()
                .expect("boot conversation");
            let conv = rig.storage.load(id).expect("conversation persists");
            assert!(
                conv.messages.iter().any(|m| matches!(
                    m,
                    crate::conversation::Message::ToolResult { result, .. } if result == REAL
                )),
                "the REAL result must be on disk"
            );
            rig.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn graceful_stop_then_next_turn_runs_normally() {
            let rig = spawn_rig(vec![
                gate_call(),
                crate::mock::MockResponse::text("second answer"),
            ]);
            rig.send("go");
            rig.wait_tool_started().await;
            rig.press();
            rig.wait_stop_pending().await;
            rig.release.notify_one();
            rig.wait_stop_message().await;
            assert_eq!(rig.model.request_count(), 1);

            // A stale drain flag would swallow this message; a stale
            // stop_pending would stop it before its first LLM call.
            rig.send("next");
            rig.wait_for("the second turn's answer", |m| {
                m.iter()
                    .any(|x| x.role == MessageRole::Agent && x.content == "second answer")
            })
            .await;
            assert_eq!(rig.model.request_count(), 2);
            assert_eq!(
                rig.stop_messages(),
                vec![GRACEFUL_MSG.to_string()],
                "no second stop notice for the second turn"
            );
            assert!(!rig.sm.get_state().stop_pending);
            rig.shutdown().await;
        }

        // ── escalation ──────────────────────────────────────────────────────

        #[tokio::test(flavor = "multi_thread")]
        async fn double_press_is_immediate_and_persists_interrupted() {
            let rig = spawn_rig(vec![
                gate_call(),
                crate::mock::MockResponse::text("SHOULD NOT BE SENT"),
            ]);
            rig.send("go");
            rig.wait_tool_started().await;

            rig.press();
            rig.wait_stop_pending().await;
            rig.press(); // escalate; the gate is deliberately never released
            rig.wait_stop_message().await;

            let results = rows(&rig.sm, MessageRole::ToolResult);
            assert_eq!(results.len(), 1);
            let text = results[0].tool_result.as_deref().unwrap_or_default();
            assert!(
                text.starts_with("INTERRUPTED: the turn was stopped before `gate_tool` returned."),
                "the 2nd press is today's abort — got: {text}"
            );
            assert_ne!(text, REAL);
            assert_eq!(rig.model.request_count(), 1);
            assert_eq!(
                rig.stop_messages(),
                vec![IMMEDIATE_MSG.to_string()],
                "an escalated stop posts the IMMEDIATE message once, not the graceful one"
            );
            assert!(!rig.sm.is_running());
            assert!(!rig.sm.get_state().stop_pending);

            let id = rig
                .sm
                .get_current_conversation_id()
                .expect("boot conversation");
            let conv = rig.storage.load(id).expect("conversation persists");
            assert!(
                conv.messages.iter().any(|m| matches!(
                    m,
                    crate::conversation::Message::ToolResult { result, .. }
                        if result.starts_with("INTERRUPTED")
                )),
                "the INTERRUPTED result must be on disk"
            );
            rig.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn drain_posts_single_message_after_escalation() {
            let rig = spawn_rig(vec![
                gate_call(),
                crate::mock::MockResponse::text("SHOULD NOT BE SENT"),
            ]);
            rig.send("go");
            rig.wait_tool_started().await;
            rig.send("queued follow-up"); // queued behind the running turn

            rig.press();
            rig.wait_stop_pending().await;
            rig.press(); // escalate
            rig.press(); // third press: no-op
            rig.wait_stop_message().await;

            assert_eq!(
                rig.stop_messages(),
                vec![IMMEDIATE_MSG.to_string()],
                "press, press, press ⇒ exactly ONE system message == stop_message(tally)"
            );
            assert!(
                rows(&rig.sm, MessageRole::User)
                    .iter()
                    .all(|m| !m.content.contains("queued follow-up")),
                "the queued message must be discarded, never dequeued"
            );
            assert_eq!(
                rig.model.request_count(),
                1,
                "the queued message never reached the model"
            );
            assert_eq!(rig.sm.get_state().pending_input_count, 0);
            rig.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn drain_posts_graceful_message_and_discards_queue() {
            let rig = spawn_rig(vec![
                gate_call(),
                crate::mock::MockResponse::text("SHOULD NOT BE SENT"),
            ]);
            rig.send("go");
            rig.wait_tool_started().await;
            rig.send("queued follow-up");

            rig.press();
            rig.wait_stop_pending().await;
            rig.release.notify_one();
            rig.wait_stop_message().await;

            assert_eq!(rig.stop_messages(), vec![GRACEFUL_MSG.to_string()]);
            assert!(
                rows(&rig.sm, MessageRole::User)
                    .iter()
                    .all(|m| !m.content.contains("queued follow-up")),
                "queued messages are discarded on a graceful stop too"
            );
            assert_eq!(rig.model.request_count(), 1);
            assert_eq!(rig.sm.get_state().pending_input_count, 0);
            assert!(!rig.sm.is_running());
            rig.shutdown().await;
        }

        // ── no-ops ──────────────────────────────────────────────────────────

        #[tokio::test(flavor = "multi_thread")]
        async fn idle_press_is_a_noop_and_leaves_no_residue() {
            let rig = spawn_rig(vec![crate::mock::MockResponse::text("hello")]);
            rig.press();
            rig.press();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;

            assert!(
                rig.stop_messages().is_empty(),
                "an idle press posts nothing"
            );
            assert!(!rig.sm.get_state().stop_pending);
            assert!(!rig.sm.is_running());

            // …and the next message is neither swallowed by a stale drain
            // flag nor stopped by a stale stop_pending.
            rig.send("hi");
            rig.wait_for("the reply", |m| {
                m.iter()
                    .any(|x| x.role == MessageRole::Agent && x.content == "hello")
            })
            .await;
            rig.shutdown().await;
        }

        // ── process_message_internal: the new `stop` arm ────────────────────

        /// A graceful stop already pending when the turn begins: the hook
        /// terminates with reason `stop` before any LLM request, and
        /// `process_message_internal` maps that to `Stopped` — not `Error`,
        /// and without fabricating an INTERRUPTED result.
        #[tokio::test(flavor = "multi_thread")]
        async fn stop_reason_maps_to_stopped_without_closing_a_tool_call() {
            let sm = StateManager::new_arc();
            sm.add_user_message("go".to_string());
            let (agent, _info, _events, _hook, model) =
                crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                    .expect("mock agent builds");
            model.add_response(crate::mock::MockResponse::text("SHOULD NOT BE SENT"));

            sm.set_running(true);
            assert_eq!(sm.request_stop(), crate::state::StopStep::Graceful);

            let current_turn = sm.build_current_turn_message().expect("the user turn");
            let agent = Arc::new(agent);
            let sm_opt = Some(sm.clone());
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                AgentRunner::process_message_internal(
                    current_turn,
                    &sm_opt,
                    &agent,
                    &test_config(),
                ),
            )
            .await
            .expect("the stopped turn must finish promptly");

            assert!(
                matches!(result, CompletionResult::Stopped),
                "PromptCancelled(\"stop\") must map to Stopped (was Error before two-stage stop)"
            );
            assert_eq!(model.request_count(), 0, "no LLM request after the press");
            assert!(
                sm.get_state()
                    .chat
                    .messages
                    .iter()
                    .all(|m| m.role != MessageRole::ToolResult),
                "the graceful arm must not synthesise INTERRUPTED results"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RED tests (selection-models plan §9.1, items 28–38): `handle_select` applies
// the full desired setup `{profile, agent, model?, subagent_models}`, and
// `classify_submission` routes `/model <alias>`.
//
// Harness = `select_profile_republishes_visible_tabs`: a mock agent, a
// `RebuildContext` over the plan's shared fixture, and `handle_select` called
// directly. Before every test the baseline is seeded the way a booted session
// has it: `get_model_alias()` = the bound agent's model + a current
// conversation stamped with that model's wire id, so the no-op guard and the
// wire-id restamp have something real to compare against.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(all(test, feature = "mock"))]
mod select_models_tests {
    use super::*;
    use crate::config::resolve::{ParseCtx, Selection, Source};
    use crate::selection::SelectSubmission;
    use crate::ui::app_state::MessageRole;
    use std::collections::BTreeMap;

    /// The plan's shared Rust fixture (§9).
    const TEAM_MODELS_YAML: &str = r#"
providers:
  - name: anthropic
    type: anthropic
    api_key: sk-ant-test
    models:
      - { name: claude-opus-5,   alias: opus,   context_size: 200000 }
      - { name: claude-sonnet-5, alias: sonnet, context_size: 200000 }
      - { name: claude-haiku-5,  alias: haiku,  context_size: 200000 }
general: { context: { enabled: false } }
profiles:
  dev:
    agents:
      team:
        model: sonnet
        subagents:
          reviewer: { prompt: "Review." }
          tester:   { prompt: "Test.",  model: haiku }
          twin:     { prompt: "Twin.",  model: sonnet }
      solo: { model: opus }
  research:
    agents:
      main: { model: haiku }
"#;

    const LOCKED_MODELS: &str =
        "❌ /agent: models are locked once the conversation has started — /new for a fresh one.";
    const LOCKED_AGENT: &str =
        "❌ /agent: the agent is locked once the conversation has started — /new for a fresh one.";

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn pair(
        profile: &str,
        agent: &str,
        models: &[(&str, &str)],
        model: Option<&str>,
    ) -> SelectRequest {
        SelectRequest::Pair {
            selection: Selection {
                profile: profile.to_string(),
                agent: agent.to_string(),
                subagent_models: map(models),
            },
            model: model.map(str::to_string),
        }
    }

    /// A booted-session stand-in over the shared fixture: `dev › team` bound,
    /// alias `sonnet`, one current conversation stamped with sonnet's wire id.
    struct Harness {
        sm: Arc<StateManager>,
        ctx: RebuildContext,
        agent_slot: Arc<DynAgent>,
        hook_cell: SharedSessionHook,
        event_processor: Option<tokio::task::JoinHandle<()>>,
    }

    impl Harness {
        fn new() -> Self {
            let config = Config::parse(
                Source {
                    path: std::path::Path::new("/global/config.yaml"),
                    text: TEAM_MODELS_YAML,
                },
                None,
                &ParseCtx { known_skills: &[] },
            )
            .expect("the shared team-models fixture parses");

            let sm = StateManager::new_arc();
            let ctx = RebuildContext {
                bound: config.catalog.default_bound(),
                config: Arc::new(config),
                mcp_handles: Arc::new(Vec::new()),
                shell_kind: None,
                skills: crate::skills::SkillRegistry::default(),
                todo_tool: None,
                profile_pin: None,
            };
            // Seed the baseline: the model the bound agent is configured on.
            sm.set_model("claude-sonnet-5".to_string());
            sm.set_provider_name("anthropic".to_string());
            sm.set_model_alias("sonnet".to_string());
            sm.create_conversation(
                "seed".to_string(),
                "anthropic".to_string(),
                "claude-sonnet-5".to_string(),
                String::new(),
            );
            publish_bound(&sm, &ctx.bound);

            let (mock_agent, _info, _rx, mock_hook, _model) =
                crate::providers::create_mock_agent("test prompt", 4, sm.clone())
                    .expect("mock agent builds");
            Self {
                sm,
                ctx,
                agent_slot: Arc::new(mock_agent),
                hook_cell: Arc::new(std::sync::RwLock::new(mock_hook)),
                event_processor: None,
            }
        }

        async fn select(&mut self, request: SelectRequest) {
            AgentRunner::handle_select(
                request,
                &mut self.agent_slot,
                &Some(self.sm.clone()),
                &self.hook_cell,
                &mut self.event_processor,
                Some(&mut self.ctx),
            )
            .await;
        }

        fn messages(&self) -> Vec<String> {
            self.sm
                .get_state()
                .chat
                .messages
                .iter()
                .filter(|m| matches!(m.role, MessageRole::System))
                .map(|m| m.content.clone())
                .collect()
        }

        fn last_message(&self) -> Option<String> {
            self.messages().pop()
        }

        fn conv(&self) -> crate::Conversation {
            self.sm
                .get_current_conversation()
                .expect("the seeded conversation")
        }
    }

    /// §9.1/28: a pair with overrides lands in the bound, the live mirror AND
    /// the conversation, and the confirmation names the overrides.
    #[tokio::test]
    async fn select_pair_with_subagent_models_records_and_publishes() {
        let mut h = Harness::new();
        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;

        let want = map(&[("tester", "opus")]);
        assert_eq!(h.ctx.bound.selection().subagent_models, want, "the bound");
        assert_eq!(h.sm.selection().subagent_models, want, "the live mirror");
        assert_eq!(h.conv().subagent_models, want, "the conversation");
        assert_eq!(
            h.last_message().as_deref(),
            Some("Models: dev › team — model sonnet. Subagent models: tester → opus.")
        );
    }

    /// §9.1/29: an orchestrator model in the frame swaps the live model in
    /// place — new alias, re-stamped wire id, SAME conversation id.
    #[tokio::test]
    async fn select_pair_with_orchestrator_model_rebuilds_in_place() {
        let mut h = Harness::new();
        let id_before = h.conv().id;
        h.select(pair("dev", "team", &[], Some("opus"))).await;

        assert_eq!(h.sm.get_model_alias(), "opus", "the active alias");
        let conv = h.conv();
        assert_eq!(
            (conv.provider_name.as_str(), conv.model.as_str()),
            ("anthropic", "claude-opus-5"),
            "the conversation's wire id follows the model"
        );
        assert_eq!(conv.id, id_before, "no new conversation (sticky ?convo=)");
    }

    /// §9.1/30: the bound is already `dev/team`, but a request that only adds
    /// overrides is a CHANGE — the no-op guard must not swallow it.
    #[tokio::test]
    async fn select_models_only_change_is_not_swallowed_by_noop_guard() {
        let mut h = Harness::new();
        let before = h.messages().len();
        h.select(pair("dev", "team", &[("reviewer", "haiku")], None))
            .await;

        assert_eq!(
            h.messages().len(),
            before + 1,
            "a confirmation line must appear"
        );
        assert_eq!(
            h.last_message().as_deref(),
            Some("Models: dev › team — model sonnet. Subagent models: reviewer → haiku.")
        );
    }

    /// §9.1/31: the exact same setup again is silent — no message, no change.
    #[tokio::test]
    async fn select_exact_noop_is_silent() {
        let mut h = Harness::new();

        // Baseline: same pair, no overrides, no model.
        let before = h.messages().len();
        h.select(pair("dev", "team", &[], None)).await;
        assert_eq!(h.messages().len(), before, "plain no-op is silent");

        // An explicit model equal to the configured one is also a no-op.
        h.select(pair("dev", "team", &[], Some("sonnet"))).await;
        assert_eq!(
            h.messages().len(),
            before,
            "configured-model no-op is silent"
        );

        // Once overrides are applied, re-sending them is a no-op too.
        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;
        assert_eq!(
            h.ctx.bound.selection().subagent_models,
            map(&[("tester", "opus")]),
            "precondition: the overrides applied"
        );
        let after_apply = h.messages().len();
        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;
        assert_eq!(
            h.messages().len(),
            after_apply,
            "repeat of the same setup is silent"
        );
        assert_eq!(
            h.ctx.bound.selection().agent,
            "team",
            "the bound is unchanged"
        );
    }

    /// §9.1/32: an override for a subagent the agent doesn't have is refused,
    /// and NOTHING changes.
    #[tokio::test]
    async fn select_unknown_subagent_refused_nothing_changes() {
        let mut h = Harness::new();
        let sel_before = h.sm.selection();
        let conv_before = h.conv();
        h.select(pair("dev", "team", &[("ghost", "opus")], None))
            .await;

        assert_eq!(
            h.last_message().as_deref(),
            Some(
                "❌ /agent: agent 'team' has no subagent 'ghost'. Subagents: reviewer, tester, twin"
            )
        );
        assert_eq!(h.sm.selection(), sel_before, "selection untouched");
        assert_eq!(h.sm.get_model_alias(), "sonnet", "alias untouched");
        let conv = h.conv();
        assert_eq!(
            conv.subagent_models, conv_before.subagent_models,
            "conversation untouched"
        );
        assert_eq!(conv.model, conv_before.model, "wire id untouched");
    }

    /// §9.1/33: an unknown alias — for the orchestrator or for a subagent —
    /// is refused with the available list.
    #[tokio::test]
    async fn select_unknown_alias_refused() {
        let mut h = Harness::new();
        h.select(pair("dev", "team", &[], Some("nope"))).await;
        assert_eq!(
            h.last_message().as_deref(),
            Some("❌ /agent: unknown alias `nope`. Available: haiku, opus, sonnet"),
            "orchestrator alias"
        );
        assert_eq!(h.sm.get_model_alias(), "sonnet", "nothing changed");

        h.select(pair("dev", "team", &[("tester", "nope")], None))
            .await;
        assert_eq!(
            h.last_message().as_deref(),
            Some(
                "❌ /agent: unknown alias `nope` for subagent 'tester'. Available: haiku, opus, sonnet"
            ),
            "subagent alias"
        );
        assert!(
            h.ctx.bound.selection().subagent_models.is_empty(),
            "nothing changed"
        );
    }

    /// §9.1/34: after the first turn the models are locked; a pair change gets
    /// the EXISTING agent-locked copy.
    #[tokio::test]
    async fn select_models_refused_after_first_turn() {
        let mut h = Harness::new();
        h.sm.add_user_message("hi".to_string());

        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;
        assert_eq!(h.last_message().as_deref(), Some(LOCKED_MODELS));
        assert!(
            h.ctx.bound.selection().subagent_models.is_empty(),
            "a refusal never changes the setup"
        );

        h.select(pair("dev", "solo", &[], None)).await;
        assert_eq!(
            h.last_message().as_deref(),
            Some(LOCKED_AGENT),
            "a pair change keeps the existing agent-lock string"
        );
        assert_eq!(h.ctx.bound.agent().main.name, "team", "still on team");
    }

    /// §9.1/35: picking another agent starts clean — overrides gone, model back
    /// to the new agent's configured model.
    #[tokio::test]
    async fn select_agent_switch_clears_overrides_and_resets_model() {
        let mut h = Harness::new();
        h.select(pair("dev", "team", &[("tester", "opus")], Some("opus")))
            .await;
        assert_eq!(
            h.ctx.bound.selection().subagent_models,
            map(&[("tester", "opus")]),
            "precondition: the overrides applied"
        );
        assert_eq!(
            h.sm.get_model_alias(),
            "opus",
            "precondition: model applied"
        );

        h.select(pair("dev", "solo", &[], None)).await;
        assert!(
            h.ctx.bound.selection().subagent_models.is_empty(),
            "bound cleared"
        );
        assert!(
            h.sm.selection().subagent_models.is_empty(),
            "mirror cleared"
        );
        assert!(h.conv().subagent_models.is_empty(), "conversation cleared");
        assert_eq!(h.sm.get_model_alias(), "opus", "solo's configured model");
        assert!(
            h.last_message()
                .is_some_and(|m| m.starts_with("Agent: dev › solo — model opus.")),
            "got: {:?}",
            h.last_message()
        );
    }

    /// §9.1/36: a profile change and a model in one frame apply together.
    #[tokio::test]
    async fn select_profile_change_with_models_applies_both() {
        let mut h = Harness::new();
        h.select(pair("research", "main", &[], Some("sonnet")))
            .await;

        assert_eq!(h.ctx.bound.profile.name, "research");
        assert_eq!(h.ctx.bound.agent().main.name, "main");
        assert_eq!(
            h.sm.get_model_alias(),
            "sonnet",
            "the requested model, not haiku"
        );
        assert!(
            h.last_message()
                .is_some_and(|m| m.starts_with("Profile: research › main — model sonnet.")),
            "got: {:?}",
            h.last_message()
        );
    }

    /// §9.1/37: a typed `/agent` or `/profile` re-applies the agent fresh —
    /// overrides cleared.
    #[tokio::test]
    async fn typed_profile_and_agent_commands_clear_overrides() {
        let mut h = Harness::new();
        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;
        assert_eq!(
            h.ctx.bound.selection().subagent_models,
            map(&[("tester", "opus")]),
            "precondition: the overrides applied"
        );

        h.select(SelectRequest::Command(SelectSubmission::Agent(
            "team".to_string(),
        )))
        .await;
        assert!(
            h.ctx.bound.selection().subagent_models.is_empty(),
            "/agent cleared the bound"
        );
        assert!(
            h.sm.selection().subagent_models.is_empty(),
            "/agent cleared the mirror"
        );
        assert!(
            h.conv().subagent_models.is_empty(),
            "/agent cleared the conversation"
        );

        // Same for `/profile`.
        h.select(pair("dev", "team", &[("tester", "opus")], None))
            .await;
        assert_eq!(
            h.ctx.bound.selection().subagent_models,
            map(&[("tester", "opus")]),
            "precondition: re-applied"
        );
        h.select(SelectRequest::Command(SelectSubmission::Profile(
            "dev".to_string(),
        )))
        .await;
        assert!(
            h.ctx.bound.selection().subagent_models.is_empty(),
            "/profile cleared the bound"
        );
    }

    /// §9.1/38: `/model <alias>` is its own submission kind (web parity with
    /// the TUI); bare `/model` and `/models` stay commands.
    #[test]
    fn classify_model_with_alias_is_switch_model() {
        assert!(
            matches!(classify_submission("/model opus"), SubmitKind::SwitchModel(a) if a == "opus"),
            "plain form"
        );
        assert!(
            matches!(
                classify_submission("  /model   opus  "),
                SubmitKind::SwitchModel(a) if a == "opus"
            ),
            "surrounding whitespace is trimmed"
        );
        assert!(
            matches!(classify_submission("/model"), SubmitKind::Command(_)),
            "bare /model lists"
        );
        assert!(
            matches!(classify_submission("/model   "), SubmitKind::Command(_)),
            "blank alias is the bare form"
        );
        assert!(
            matches!(classify_submission("/models"), SubmitKind::Command(_)),
            "a longer keyword is not /model"
        );
    }

    // ─── cd-rebind: the model a `/cd` rebuilds on ──────────────────────────
    //
    // `model_after_cd` (private, the `/cd` seam): the SAME pair keeps the
    // session's ACTIVE model (the `handle_change_cwd` lookup: active alias,
    // else wire id); a CHANGED pair starts on the new agent's configured
    // model.

    /// cd-rebind: the SAME pair after a reload keeps the session's ACTIVE
    /// model — a `/model` switch (alias `opus`) survives the `/cd`.
    #[test]
    fn cd_model_same_pair_keeps_active_model() {
        let h = Harness::new();
        h.sm.set_model_alias("opus".to_string());
        let previous = h.ctx.bound.clone();

        let model = model_after_cd(&previous, &h.ctx, h.sm.as_ref())
            .expect("the active alias resolves in the registry");

        assert_eq!(
            model.alias, "opus",
            "the active alias wins for the same pair"
        );
        assert_eq!(model.model_name, "claude-opus-5");
    }

    /// cd-rebind: a CHANGED pair after a reload starts on the NEW agent's
    /// configured model — the old active alias is discarded.
    #[test]
    fn cd_model_switched_pair_uses_new_agents_model() {
        let mut h = Harness::new();
        let previous = h.ctx.bound.clone(); // dev/team
        h.ctx.bound = h
            .ctx
            .config
            .catalog
            .resolve("research", "main")
            .expect("research/main");

        let model = model_after_cd(&previous, &h.ctx, h.sm.as_ref())
            .expect("the new agent's configured model");

        // `ResolvedModel` is not `PartialEq`: compare its identity fields.
        let want = &h.ctx.bound.agent().main.model;
        assert_eq!(
            (&model.alias, &model.provider_name, &model.model_name),
            (&want.alias, &want.provider_name, &want.model_name),
            "the new agent's configured model is the rebuild model"
        );
        assert_eq!(model.alias, "haiku");
    }

    // ─── Two-stage Stop (TDD: written before the implementation) ────────────
    //
    // Contract under test (public behaviour only — no reliance on the
    // internals of the controller's `pending_stop` cell):
    //   * 1st `UiAction::RequestStop` while a turn runs → graceful: the
    //     in-flight tool finishes and records its REAL result; the turn then
    //     ends before the next LLM call; no INTERRUPTED row.
    //   * Tool calls requested after the press are SKIPPED (recorded, not run).
    //   * A text-only answer arriving after the press completes normally.
    //   * 2nd press → today's immediate abort (INTERRUPTED persisted).
    //   * Further / idle presses are no-ops.
    //   * Queued messages are discarded on both stops; exactly ONE system
    //     message is posted per stop; the next turn runs normally.
}
