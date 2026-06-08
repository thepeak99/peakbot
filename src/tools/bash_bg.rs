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

use crate::bg_processes::{
    BgError, BgStatus, DEFAULT_CAPTURE_LINES, DEFAULT_COOLDOWN_SECS, StartParams,
};
use crate::state::StateManager;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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
  - a bg process's output is flushed (see Cooldown below), OR
  - a bg process exits (always, even with `capture_output_lines: 0`,
    even mid-cooldown).

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

  • `/new`, `/model`, and `/load` kill all background processes.
  • Stopped/exited processes are removed from the registry on the
    next drain (after their final tail is delivered to you).
  • A real user message flushes all buffered bg output immediately,
    regardless of cooldown."#
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
                        "description": "Shell command to spawn (required for `start`). Executed via the detected shell (bash/sh on Unix, PowerShell on Windows)."
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
                let cooldown_secs = args.cooldown_secs.unwrap_or(DEFAULT_COOLDOWN_SECS);
                let entry = sm.start_bg(StartParams {
                    command: command.clone(),
                    capture_cap,
                    cwd: args.cwd,
                    label: args.label,
                    cooldown: Duration::from_secs(cooldown_secs),
                    env: self.env.clone(),
                    shell: String::new(),
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
                            "cooldown_secs": r.cooldown.as_secs(),
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
                cooldown_secs: None,
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
                cooldown_secs: None,
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
                cooldown_secs: None,
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
                cooldown_secs: None,
                id: None,
                line: None,
            })
            .await
            .unwrap();
        assert_eq!(out["processes"].as_array().unwrap().len(), 0);
    }
}
