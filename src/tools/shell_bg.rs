//! `shell_bg` — manage long-running background processes.
//!
//! Sibling to the synchronous shell tool (`bash` / `powershell`). Where the
//! shell tool blocks the turn until the command exits and returns its
//! captured output, this tool spawns a PTY-backed process, returns a
//! numeric id immediately, and streams the process's output into a ring
//! buffer. Output reaches the LLM via the agent loop's drain seams
//! (between turns and on idle wake-up) as synthetic user turns framed with
//! a `[bg output]` header.
//!
//! The wire name is per-shell (`bash_bg` / `powershell_bg`, see
//! `ShellKind::bg_tool_name`), and the model-facing description is a
//! template filled from the owned `ShellKind`.

use crate::bg_processes::{
    BgError, BgStatus, DEFAULT_CAPTURE_LINES, DEFAULT_COOLDOWN_SECS, StartParams,
};
use crate::state::StateManager;
use crate::tools::ShellKind;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum ShellBgError {
    #[error("state manager not initialised; the background-process tool cannot operate")]
    NoStateManager,
    #[error("invalid action `{0}` — expected one of: start, stop, list, send_line")]
    InvalidAction(String),
    #[error("missing field `{0}` for action `{1}`")]
    MissingField(&'static str, &'static str),
    #[error(transparent)]
    Bg(#[from] BgError),
}

/// The background-process tool. Stateless controller — all process state
/// lives in `StateManager::bg`; the `ShellKind` is fixed at construction
/// (the tool is built inside the shell arm of `add_builtin_tools`).
#[derive(Clone)]
pub struct ShellBgTool {
    /// Shell every `start` spawns with; also names the tool on the wire.
    shell: ShellKind,
    state_manager: Option<Arc<StateManager>>,
    /// Optional environment variables to set for spawned processes,
    /// inherited from the `bash:` config section.
    env: Option<HashMap<String, String>>,
}

impl ShellBgTool {
    pub fn new(
        shell: ShellKind,
        state_manager: Option<Arc<StateManager>>,
        env: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            shell,
            state_manager,
            env,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct ShellBgArgs {
    /// One of: `start`, `stop`, `list`, `send_line`.
    pub action: String,

    // ── start ──────────────────────────────────────────────────────
    #[serde(default)]
    pub command: Option<String>,
    /// Lines retained in the per-process ring buffer. `0` disables
    /// capture entirely (output still drained from the PTY so the
    /// child doesn't block, but no lines reach the LLM). Defaults to
    /// 200 when omitted.
    #[serde(default)]
    pub capture_output_lines: Option<usize>,
    /// Optional working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Optional human-readable tag, shown in `list` and the `/bg` slash
    /// command.
    #[serde(default)]
    pub label: Option<String>,
    /// Output-coalescing window in seconds. After this process injects a
    /// `[bg output]` turn, further output accumulates and is flushed in a
    /// single batch once `cooldown_secs` elapses. `0` = real-time (inject
    /// every batch). Omitted ⇒ 60. Set `0` for external-input sources
    /// (telegram/webhook/IRC bridges) where you need to react to each
    /// line immediately; leave the default for logs, metrics, and build
    /// watchers so you aren't woken on every line.
    #[serde(default)]
    pub cooldown_secs: Option<u64>,

    // ── stop / send_line ───────────────────────────────────────────
    /// Numeric process id returned by `start`.
    #[serde(default)]
    pub id: Option<u32>,

    // ── send_line ──────────────────────────────────────────────────
    /// Line to write to the process's stdin. Terminated with `\r` (Enter)
    /// unless it already ends with one.
    #[serde(default)]
    pub line: Option<String>,
}

/// Model-facing description with `{shell_tool}` / `{bg_tool}` placeholders,
/// filled per shell in `definition()`. Kept as a template (not `format!`)
/// so the prose can freely contain literal braces and backticks.
const DESCRIPTION_TEMPLATE: &str = r#"Manage long-running background processes via a PTY.

Distinct from the synchronous `{shell_tool}` tool: `{bg_tool} start` returns
immediately with a numeric id; the process keeps running until you call
`{bg_tool} stop <id>` or it exits on its own. The registry is session-wide
and shared by every agent in this session — ids are one namespace, and a
process outlives the turn that started it. If you are a sub-agent, it
also outlives your delegation: hand it back or stop it.

## How notifications work (READ THIS)

You are **event-driven** with respect to background processes. The
runtime will deliver new turns to you automatically whenever:
  - a bg process's output is flushed (see Cooldown below), OR
  - a bg process exits (always, even with `capture_output_lines: 0`,
    even mid-cooldown).

The new turn arrives as a synthetic user message framed with
`[bg output]`. Each contributing process gets one block with a header
like `─── #3 `npm run dev` (12 new lines) ───` or
`─── #3 `npm run dev` (exited, code 0, 2 final lines) ───`.

**DO NOT** poll. **DO NOT** `sleep N && {bg_tool} list` to wait for a
process to finish. **DO NOT** spin on `{bg_tool} list` between turns.
That wastes turns and tokens for no reason — the framework already
guarantees you'll be re-woken on output or exit.

The correct pattern after `{bg_tool} start`:
  1. If you have other work to do this turn, do it.
  2. Otherwise, finish your turn with a brief text reply (e.g.
     "Started build #3 in background; I'll review the output when
     it lands."). The next synthetic `[bg output]` turn is your
     cue to act.

Use `{bg_tool} list` only for an on-demand status check the user asked
for, never as a wait loop.

## Cooldown (output pacing)

Each process has a per-process `cooldown_secs` (default 60). After a
process injects a `[bg output]` turn, its further output is **coalesced**
— buffered and flushed in one batch once the cooldown elapses — so a
chatty log doesn't wake you on every line. `cooldown_secs: 0` disables
coalescing (real-time: every batch injects immediately).

Pick the cooldown by intent:
  - **Logs / metrics / build watchers** → leave the 60s default (or set
    a larger value). You'll get periodic digests, not a firehose.
  - **External input** (a telegram bridge, webhook receiver, IRC bot —
    anything that brings a person, inbox, or request queue into the
    conversation) → set `cooldown_secs: 0` so you react to each line
    immediately, as if the human typed it. Respond to those blocks like
    real user messages. If such a process appears to be in a feedback
    loop (the same line repeating, pathological output), stop it.

## Actions

  • `start`  — spawn a new process. Required: `command`. Optional:
               `capture_output_lines` (default 200; `0` discards
               output but you still get the exit notification),
               `cwd`, `label`, `cooldown_secs` (default 60; `0` for
               real-time external-input bridges).
  • `stop`   — kill a process. Required: `id`. Returns final buffer
               tail and exit code in one last pass.
  • `list`   — snapshot all current processes. For on-demand status
               only, NOT for waiting.
  • `send_line` — write a line to a running process's stdin.
               Required: `id`, `line`.

## Notes

  • The registry is shared session state. `{bg_tool} list` shows every
    process in the session, including ones another agent started.
    Do not stop a process you did not start unless you were asked to.
  • `/new`, `/model`, and `/load` kill all background processes.
    Nothing else reaps them — ending a turn or a delegation does not.
  • Stopped/exited processes are removed from the registry on the
    next drain (after their final tail is delivered to you).
  • A real user message flushes all buffered bg output immediately,
    regardless of cooldown."#;

impl Tool for ShellBgTool {
    // rig requires a const; the wire name is per-shell — see `name()`.
    const NAME: &'static str = "bash_bg";
    type Error = ShellBgError;
    type Args = ShellBgArgs;
    type Output = Value;

    fn name(&self) -> String {
        self.shell.bg_tool_name().into()
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let shell_tool = self.shell.name();
        let description = DESCRIPTION_TEMPLATE
            .replace("{shell_tool}", shell_tool)
            .replace("{bg_tool}", self.shell.bg_tool_name());
        ToolDefinition {
            name: self.name(),
            description,
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "stop", "list", "send_line"],
                        "description": "Which verb to invoke."
                    },
                    "command": {
                        "type": "string",
                        "description": format!(
                            "Shell command to spawn (required for `start`). Executed via the detected shell (`{shell_tool}` syntax)."
                        )
                    },
                    "capture_output_lines": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Lines retained in the per-process ring buffer. 0 disables output capture (but the exit notification still fires). Defaults to 200."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the spawned process."
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional human-friendly tag shown in `list` and the /bg slash command."
                    },
                    "cooldown_secs": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Seconds to coalesce this process's output before injecting a `[bg output]` turn. 0 = real-time (inject every batch). Default 60. Use 0 for external-input bridges (telegram/webhook/IRC) so you react to each line immediately; leave the default for logs/metrics/watchers."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Process id returned by `start` (required for `stop` and `send_line`)."
                    },
                    "line": {
                        "type": "string",
                        "description": "Text to write to the process's stdin (required for `send_line`). Enter (`\\r`) is appended if absent."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let sm = self
            .state_manager
            .as_ref()
            .ok_or(ShellBgError::NoStateManager)?;

        match args.action.as_str() {
            "start" => {
                let command = args
                    .command
                    .ok_or(ShellBgError::MissingField("command", "start"))?;
                let capture_cap = args.capture_output_lines.unwrap_or(DEFAULT_CAPTURE_LINES);
                let cooldown_secs = args.cooldown_secs.unwrap_or(DEFAULT_COOLDOWN_SECS);
                let entry = sm.start_bg(StartParams {
                    command: command.clone(),
                    capture_cap,
                    cwd: args.cwd,
                    label: args.label,
                    cooldown: Duration::from_secs(cooldown_secs),
                    env: self.env.clone(),
                    shell: self.shell.clone(),
                })?;
                Ok(json!({
                    "id": entry.id,
                    "pid": entry.pid,
                    "command": command,
                    "capture_output_lines": capture_cap,
                    "cooldown_secs": cooldown_secs,
                    "message": format!(
                        "Started bg #{} (pid {}). Output will appear between turns as `[bg output]`.",
                        entry.id, entry.pid
                    ),
                }))
            }
            "stop" => {
                let id = args.id.ok_or(ShellBgError::MissingField("id", "stop"))?;
                let (exit_code, final_lines) = sm.stop_bg(id)?;
                Ok(json!({
                    "id": id,
                    "exit_code": exit_code,
                    "final_output_lines": final_lines,
                }))
            }
            "list" => {
                let rows = sm.list_bg();
                let entries: Vec<Value> = rows
                    .into_iter()
                    .map(|r| {
                        let (status, exit_code) = match r.status {
                            BgStatus::Running { .. } => ("running", None::<i32>),
                            BgStatus::Exited { code, .. } => ("exited", Some(code)),
                        };
                        json!({
                            "id": r.id,
                            "pid": r.pid,
                            "command": r.command,
                            "label": r.label,
                            "status": status,
                            "exit_code": exit_code,
                            "buffer_len": r.buffer_len,
                            "capture_cap": r.capture_cap,
                            "cooldown_secs": r.cooldown.as_secs(),
                        })
                    })
                    .collect();
                Ok(json!({ "processes": entries }))
            }
            "send_line" => {
                let id = args
                    .id
                    .ok_or(ShellBgError::MissingField("id", "send_line"))?;
                let line = args
                    .line
                    .ok_or(ShellBgError::MissingField("line", "send_line"))?;
                let bytes_written = sm.send_bg_line(id, line)?;
                Ok(json!({
                    "id": id,
                    "bytes_written": bytes_written,
                }))
            }
            other => Err(ShellBgError::InvalidAction(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ShellKind;

    fn bash_shell() -> ShellKind {
        ShellKind::Bash {
            path: "/bin/sh".to_string(),
        }
    }

    fn ps_shell() -> ShellKind {
        ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        }
    }

    fn args(action: &str) -> ShellBgArgs {
        ShellBgArgs {
            action: action.into(),
            command: None,
            capture_output_lines: None,
            cwd: None,
            label: None,
            cooldown_secs: None,
            id: None,
            line: None,
        }
    }

    #[tokio::test]
    async fn bg_without_state_manager_errors_cleanly() {
        let tool = ShellBgTool::new(bash_shell(), None, None);
        let err = tool.call(args("list")).await.unwrap_err();
        assert!(matches!(err, ShellBgError::NoStateManager));
    }

    #[tokio::test]
    async fn bg_unknown_action_returns_coach_message() {
        let sm = Arc::new(StateManager::new());
        let tool = ShellBgTool::new(bash_shell(), Some(sm), None);
        let err = tool.call(args("bogus")).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("start"));
        assert!(msg.contains("stop"));
        assert!(msg.contains("list"));
        assert!(msg.contains("send_line"));
    }

    #[tokio::test]
    async fn bg_start_without_command_returns_coach_message() {
        let sm = Arc::new(StateManager::new());
        let tool = ShellBgTool::new(bash_shell(), Some(sm), None);
        let err = tool.call(args("start")).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("command"));
        assert!(msg.contains("start"));
    }

    #[tokio::test]
    async fn bg_list_on_empty_registry_returns_empty_array() {
        let sm = Arc::new(StateManager::new());
        let tool = ShellBgTool::new(bash_shell(), Some(sm), None);
        let out = tool.call(args("list")).await.unwrap();
        assert_eq!(out["processes"].as_array().unwrap().len(), 0);
    }

    /// Error text reaches a PowerShell model verbatim — it must stay
    /// shell-neutral.
    #[test]
    fn errors_are_shell_neutral() {
        for err in [
            ShellBgError::NoStateManager,
            ShellBgError::InvalidAction("x".into()),
            ShellBgError::MissingField("command", "start"),
        ] {
            assert!(
                !err.to_string().to_lowercase().contains("bash"),
                "error text must not name bash: {err}"
            );
        }
    }

    /// The rig `const NAME` stays `"bash_bg"` — a compile-time constant
    /// cannot depend on the detected shell. The dynamic `name()` (next
    /// test) is what carries the per-shell identity.
    #[test]
    fn const_name_stays_bash_bg() {
        assert_eq!(ShellBgTool::NAME, "bash_bg");
    }

    /// `name()` returns the shell-derived wire name — never the const —
    /// and stays in lockstep with `ShellKind::bg_tool_name()`, the single
    /// source of truth.
    #[test]
    fn name_follows_shell_bg_tool_name() {
        let ps = ShellBgTool::new(ps_shell(), None, None);
        assert_eq!(ps.name(), "powershell_bg");
        assert_eq!(ps.name(), ps_shell().bg_tool_name());

        let bash = ShellBgTool::new(bash_shell(), None, None);
        assert_eq!(bash.name(), "bash_bg");
        assert_eq!(bash.name(), bash_shell().bg_tool_name());
    }

    /// `definition().name` must equal `name()` — the wire name the model
    /// sees is the shell-derived one, for both shells.
    #[tokio::test]
    async fn definition_name_matches_name_for_both_shells() {
        for tool in [
            ShellBgTool::new(bash_shell(), None, None),
            ShellBgTool::new(ps_shell(), None, None),
        ] {
            let def = tool.definition(String::new()).await;
            assert_eq!(
                def.name,
                tool.name(),
                "definition name must match name() for shell {}",
                tool.name()
            );
        }
    }

    /// A PowerShell host's bg tool must not mention bash ANYWHERE in its
    /// description, and no `{placeholder}` may leak to the model.
    #[tokio::test]
    async fn powershell_bg_description_is_bash_free_and_placeholder_free() {
        let tool = ShellBgTool::new(ps_shell(), None, None);
        let def = tool.definition(String::new()).await;
        let lower = def.description.to_lowercase();
        assert!(
            !lower.contains("bash"),
            "powershell_bg description must not mention bash; found in: {}",
            def.description
        );
        assert!(
            !def.description.contains('{'),
            "unrendered placeholder leaked into powershell_bg description: {}",
            def.description
        );
    }

    /// The example command in the description is shell-neutral
    /// (`npm run dev`), not the bash-ism `tail -f`.
    #[tokio::test]
    async fn powershell_bg_description_uses_neutral_example_command() {
        let tool = ShellBgTool::new(ps_shell(), None, None);
        let def = tool.definition(String::new()).await;
        assert!(
            def.description.contains("npm run dev"),
            "description example should be the neutral `npm run dev`; got: {}",
            def.description
        );
        assert!(
            !def.description.contains("tail -f"),
            "description must not keep the bash-ism `tail -f` example; got: {}",
            def.description
        );
    }

    /// On a Bash host the description keeps naming `bash_bg` and `bash`,
    /// and still renders placeholder-free.
    #[tokio::test]
    async fn bash_bg_description_mentions_bash_tools_and_renders() {
        let tool = ShellBgTool::new(bash_shell(), None, None);
        let def = tool.definition(String::new()).await;
        assert!(
            def.description.contains("bash_bg"),
            "bash bg description must name `bash_bg`; got: {}",
            def.description
        );
        assert!(
            def.description.contains("bash"),
            "bash bg description must name the `bash` tool; got: {}",
            def.description
        );
        assert!(
            !def.description.contains('{'),
            "unrendered placeholder leaked into bash_bg description: {}",
            def.description
        );
    }
}
