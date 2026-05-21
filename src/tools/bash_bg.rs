//! `bash_bg` — manage long-running background processes.
//!
//! Sibling to the synchronous `bash` tool. Where `bash` blocks the turn
//! until the command exits and returns its captured output, `bash_bg`
//! spawns a PTY-backed process, returns a numeric id immediately, and
//! streams the process's output into a ring buffer. Output reaches the
//! LLM via the agent loop's drain seams (between turns and on idle wake-
//! up) as synthetic user turns framed with a `[bg output]` header.
//!
//! See `bash-background.md` for the full design.

use crate::bg_processes::{BgError, BgStatus, DEFAULT_CAPTURE_LINES, StartParams};
use crate::state::StateManager;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum BashBgError {
    #[error("state manager not initialised; bash_bg cannot operate")]
    NoStateManager,
    #[error("invalid action `{0}` — expected one of: start, stop, list, send_line")]
    InvalidAction(String),
    #[error("missing field `{0}` for action `{1}`")]
    MissingField(&'static str, &'static str),
    #[error(transparent)]
    Bg(#[from] BgError),
}

/// The `bash_bg` tool. Stateless controller — all process state lives in
/// `StateManager::bg`.
#[derive(Default, Clone)]
pub struct BashBgTool {
    state_manager: Option<Arc<StateManager>>,
    /// Optional environment variables to set for spawned processes,
    /// inherited from the `bash:` config section.
    env: Option<HashMap<String, String>>,
}

impl BashBgTool {
    /// Create with configured environment variables (same source as `bash`).
    pub fn new_with_env(
        state_manager: Arc<StateManager>,
        env: Option<HashMap<String, String>>,
    ) -> Self {
        Self {
            state_manager: Some(state_manager),
            env,
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct BashBgArgs {
    /// Brief thought — match `todo`/`bash`/`think` convention.
    #[allow(dead_code)]
    #[serde(default)]
    pub thought: String,

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
    /// Declares this process as an **external input source** (telegram
    /// bridge, webhook receiver, IRC bot, etc.). Its output bypasses
    /// the capped-tier circuit breaker and resets the consecutive-auto-
    /// turns counter — exactly like a real human typing would. Use for
    /// processes whose output represents a person, an inbox, or a
    /// queue of external requests. Leave `false` (the default) for
    /// logs, metrics, build watchers, or other observation streams.
    /// If an `treat_as_user_input` process appears to be in a feedback
    /// loop (the same line repeating, pathological output), stop it.
    #[serde(default)]
    pub treat_as_user_input: Option<bool>,

    // ── stop / send_line ───────────────────────────────────────────
    /// Numeric process id returned by `start`.
    #[serde(default)]
    pub id: Option<u32>,

    // ── send_line ──────────────────────────────────────────────────
    /// Line to write to the process's stdin. A trailing newline is
    /// added automatically if absent.
    #[serde(default)]
    pub line: Option<String>,
}

impl Tool for BashBgTool {
    const NAME: &'static str = "bash_bg";
    type Error = BashBgError;
    type Args = BashBgArgs;
    type Output = Value;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: r#"Manage long-running background processes via a PTY.

Distinct from the synchronous `bash` tool: `bash_bg start` returns
immediately with a numeric id; the process keeps running until you call
`bash_bg stop <id>`, it exits on its own, or the conversation ends.

## How notifications work (READ THIS)

You are **event-driven** with respect to background processes. The
runtime will deliver new turns to you automatically whenever:
  - a bg process emits output (debounced ~500ms), OR
  - a bg process exits (always, even with `capture_output_lines: 0`,
    even when the circuit breaker is suppressing chatter).

The new turn arrives as a synthetic user message framed with
`[bg output]`. Each contributing process gets one block with a header
like `─── #3 `tail -f log` (12 new lines) ───` or
`─── #3 `tail -f log` (exited, code 0, 2 final lines) ───`.

**DO NOT** poll. **DO NOT** `sleep N && bash_bg list` to wait for a
process to finish. **DO NOT** spin on `bash_bg list` between turns.
That wastes turns and tokens for no reason — the framework already
guarantees you'll be re-woken on output or exit.

The correct pattern after `bash_bg start`:
  1. If you have other work to do this turn, do it.
  2. Otherwise, finish your turn with a brief text reply (e.g.
     "Started build #3 in background; I'll review the output when
     it lands."). The next synthetic `[bg output]` turn is your
     cue to act.

Use `bash_bg list` only for an on-demand status check the user asked
for, never as a wait loop.

## Tiers

Blocks marked `(unlimited, …)` come from processes started with
`treat_as_user_input: true` and represent external input (a telegram
bridge, webhook receiver, IRC bot). Treat them like a real user
message arriving in the conversation. Plain blocks are observation
feeds (logs, build watchers, metrics) — read, act if needed, but
don't feel obliged to reply to every line.

## Actions

  • `start`  — spawn a new process. Required: `command`. Optional:
               `capture_output_lines` (default 200; `0` discards
               output but you still get the exit notification),
               `cwd`, `label`, `treat_as_user_input` (set true for
               telegram/webhook/IRC bridges — anything that brings
               external input into the conversation).
  • `stop`   — kill a process. Required: `id`. Returns final buffer
               tail and exit code in one last pass.
  • `list`   — snapshot all current processes. For on-demand status
               only, NOT for waiting.
  • `send_line` — write a line to a running process's stdin.
               Required: `id`, `line`.

## Notes

  • `/new`, `/model`, and `/load` kill all background processes.
  • Stopped/exited processes are removed from the registry on the
    next drain (after their final tail is delivered to you).
  • If a `treat_as_user_input` process appears to be in a feedback
    loop (same line repeating, pathological output), stop it — there
    is no structural rate limit on the unlimited tier."#
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain why you're starting/stopping/listing/writing."
                    },
                    "action": {
                        "type": "string",
                        "enum": ["start", "stop", "list", "send_line"],
                        "description": "Which verb to invoke."
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command to spawn (required for `start`). Executed via `sh -c`."
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
                    "treat_as_user_input": {
                        "type": "boolean",
                        "description": "Set true when the process represents an external input source (telegram bridge, webhook receiver, IRC bot). Such processes bypass the consecutive-bg-turns circuit breaker. Default false."
                    },
                    "id": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Process id returned by `start` (required for `stop` and `send_line`)."
                    },
                    "line": {
                        "type": "string",
                        "description": "Text to write to the process's stdin (required for `send_line`). Newline appended if absent."
                    }
                },
                "required": ["thought", "action"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let sm = self
            .state_manager
            .as_ref()
            .ok_or(BashBgError::NoStateManager)?;

        match args.action.as_str() {
            "start" => {
                let command = args
                    .command
                    .ok_or(BashBgError::MissingField("command", "start"))?;
                let capture_cap = args.capture_output_lines.unwrap_or(DEFAULT_CAPTURE_LINES);
                let treat_as_user_input = args.treat_as_user_input.unwrap_or(false);
                let entry = sm.start_bg(StartParams {
                    command: command.clone(),
                    capture_cap,
                    cwd: args.cwd,
                    label: args.label,
                    treat_as_user_input,
                    env: self.env.clone(),
                })?;
                Ok(json!({
                    "id": entry.id,
                    "pid": entry.pid,
                    "command": command,
                    "capture_output_lines": capture_cap,
                    "treat_as_user_input": treat_as_user_input,
                    "message": format!(
                        "Started bg #{} (pid {}). Output will appear between turns as `[bg output]`.",
                        entry.id, entry.pid
                    ),
                }))
            }
            "stop" => {
                let id = args.id.ok_or(BashBgError::MissingField("id", "stop"))?;
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
                            "treat_as_user_input": r.treat_as_user_input,
                        })
                    })
                    .collect();
                Ok(json!({ "processes": entries }))
            }
            "send_line" => {
                let id = args
                    .id
                    .ok_or(BashBgError::MissingField("id", "send_line"))?;
                let line = args
                    .line
                    .ok_or(BashBgError::MissingField("line", "send_line"))?;
                let bytes_written = sm.send_bg_line(id, line)?;
                Ok(json!({
                    "id": id,
                    "bytes_written": bytes_written,
                }))
            }
            other => Err(BashBgError::InvalidAction(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bash_bg_without_state_manager_errors_cleanly() {
        let tool = BashBgTool::default();
        let err = tool
            .call(BashBgArgs {
                thought: String::new(),
                action: "list".into(),
                command: None,
                capture_output_lines: None,
                cwd: None,
                label: None,
                treat_as_user_input: None,
                id: None,
                line: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, BashBgError::NoStateManager));
    }

    #[tokio::test]
    async fn bash_bg_unknown_action_returns_coach_message() {
        let sm = Arc::new(StateManager::new());
        let tool = BashBgTool::new_with_env(sm, None);
        let err = tool
            .call(BashBgArgs {
                thought: String::new(),
                action: "bogus".into(),
                command: None,
                capture_output_lines: None,
                cwd: None,
                label: None,
                treat_as_user_input: None,
                id: None,
                line: None,
            })
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("start"));
        assert!(msg.contains("stop"));
        assert!(msg.contains("list"));
        assert!(msg.contains("send_line"));
    }

    #[tokio::test]
    async fn bash_bg_start_without_command_returns_coach_message() {
        let sm = Arc::new(StateManager::new());
        let tool = BashBgTool::new_with_env(sm, None);
        let err = tool
            .call(BashBgArgs {
                thought: String::new(),
                action: "start".into(),
                command: None,
                capture_output_lines: None,
                cwd: None,
                label: None,
                treat_as_user_input: None,
                id: None,
                line: None,
            })
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("command"));
        assert!(msg.contains("start"));
    }

    #[tokio::test]
    async fn bash_bg_list_on_empty_registry_returns_empty_array() {
        let sm = Arc::new(StateManager::new());
        let tool = BashBgTool::new_with_env(sm, None);
        let out = tool
            .call(BashBgArgs {
                thought: String::new(),
                action: "list".into(),
                command: None,
                capture_output_lines: None,
                cwd: None,
                label: None,
                treat_as_user_input: None,
                id: None,
                line: None,
            })
            .await
            .unwrap();
        assert_eq!(out["processes"].as_array().unwrap().len(), 0);
    }
}
