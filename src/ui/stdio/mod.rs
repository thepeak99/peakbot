//! StdioUi — a PeakBot `Ui` implementation that speaks NDJSON over stdio.
//!
//! Selected with `peakbot --stdio`. One stdin line = one inbound message
//! from the client (e.g. an IDE plugin); one stdout line = one outbound
//! message. Everything below the View (agent, providers, tools, skills,
//! MCP, persistence, cost tracking) is the same machinery the TUI drives.
//!
//! ## Stdout discipline
//!
//! Stdout is the protocol channel — *only* NDJSON lines go there. `main`
//! routes `tracing` to stderr under `--stdio` so logs can't corrupt it.
//!
//! ## Wire protocol
//!
//! ### Inbound (client → agent)
//!
//! ```json
//! {"type":"send_message","text":"hello"}
//! {"type":"stop"}
//! {"type":"pause"}
//! {"type":"resume"}
//! {"type":"switch_model","alias":"sonnet"}
//! {"type":"request_conversations"}
//! {"type":"request_recent_dirs"}
//! {"type":"shutdown"}
//! ```
//!
//! Slash commands (`/new`, `/save`, `/load <id>`, `/stats`, `/model`,
//! `/help`, etc.) are sent as plain `send_message` payloads whose
//! `text` starts with `/`. PeakBot classifies them internally — see
//! `AgentRunner::classify_submission`.
//!
//! ### Outbound (agent → client)
//!
//! ```json
//! {"type":"ready"}
//! {"type":"models_available","active":"sonnet","models":[{"alias":"sonnet","provider_name":"openrouter","model_name":"anthropic/claude-sonnet-4.6","context_size":200000}]}
//! {"type":"state","state":{...AppState...}}
//! {"type":"conversations_list","items":[{"id":"<uuid>","name":"...","updated_at":"<iso8601>","message_count":42,"model":"..."}]}
//! {"type":"recent_dirs","dirs":["/path/one","/path/two"]}
//! {"type":"error","message":"..."}
//! ```
//!
//! `models_available` is emitted **once** at boot, right after `ready`.
//! The registry is immutable for the life of the process — clients can
//! cache it.
//!
//! `conversations_list` is **pull-only**: the client sends
//! `request_conversations` for a fresh snapshot. Answered directly in the
//! stdin task (not a `UiAction`) and never cached — `/save`, `/delete`,
//! `/rename`, and auto-saves mutate the list, so a cache would need
//! invalidation we don't want.
//!
//! ## Concurrency
//!
//! Three tasks share one stdout: the state-broadcast loop, the stdin
//! reader, and the writer task. Producers push `OutboundMessage` values
//! onto an MPSC; a single writer task drains it and serialises to
//! stdout. NDJSON line atomicity is preserved by construction.

use crate::ui::outbound::{OutboundTx, outbound_channel};
use crate::ui::wire::{
    InboundMessage, ModelInfo, OutboundMessage, build_conversations_snapshot, build_dir_listing,
    build_recent_dirs,
};
use crate::{StateManager, Ui, UiAction};
use anyhow::Result;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

/// `Ui` implementation that pumps `AppState` broadcasts to stdout as
/// NDJSON and reads `UiAction` requests from stdin as NDJSON.
pub struct StdioUi {
    state_manager: Arc<StateManager>,
    action_sender: UnboundedSender<UiAction>,
    /// Empty in the legacy single-provider path — no picker to populate, so
    /// `models_available` is skipped.
    models: Vec<ModelInfo>,
    active_alias: String,
}

impl StdioUi {
    pub fn new(
        state_manager: Arc<StateManager>,
        action_sender: UnboundedSender<UiAction>,
        models: Vec<ModelInfo>,
        active_alias: String,
    ) -> Self {
        Self {
            state_manager,
            action_sender,
            models,
            active_alias,
        }
    }
}

impl Ui for StdioUi {
    async fn init(&mut self) -> Result<()> {
        // All stdout writes funnel through `run()`'s writer task.
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let (out_tx, mut out_rx) = outbound_channel();

        // Sole owner of stdout — keeps NDJSON lines atomic since no other
        // task ever writes there. No write timeout (NDJSON has no half-open
        // pipe; a slow consumer is legitimate backpressure, bounded by the
        // coalescing slot — see `src/ui/outbound.rs` §2.6).
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.next().await {
                let line = match serde_json::to_string(&msg) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("failed to serialise outbound message: {e:?}");
                        continue;
                    }
                };
                if let Err(e) = write_line(&line).await {
                    tracing::warn!("failed to write to stdout: {e:?}");
                    break;
                }
            }
        });

        // `ready` must precede `models_available`.
        let _ = out_tx.send(OutboundMessage::Ready);
        if !self.models.is_empty() {
            let _ = out_tx.send(OutboundMessage::ModelsAvailable {
                active: self.active_alias.clone(),
                models: self.models.clone(),
            });
        }

        // Forwards UiActions to the controller; answers pull-style requests
        // (`request_conversations`) directly.
        let action_sender = self.action_sender.clone();
        let state_manager = self.state_manager.clone();
        let stdin_tx = out_tx.clone();
        let stdin_task = tokio::spawn(async move {
            if let Err(e) = run_stdin_loop(action_sender, stdin_tx, state_manager).await {
                tracing::warn!("stdin loop ended with error: {e:?}");
            }
        });

        // Holds `out_tx` until exit, then drops it so the writer drains.
        let mut state_rx = self.state_manager.subscribe();
        while let Some(state) = state_rx.recv().await {
            let exit = state.exit_requested;
            if out_tx.publish_state(Arc::new(state)).is_err() {
                // Writer dropped its receiver.
                break;
            }
            if exit {
                break;
            }
        }

        // stdin task may be parked on a read; abort it, then drain the writer.
        stdin_task.abort();
        drop(out_tx);
        let _ = writer_task.await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        let mut stdout = tokio::io::stdout();
        let _ = stdout.flush().await;
        Ok(())
    }
}

/// Read NDJSON from stdin and dispatch [`UiAction`]s. Returns when stdin
/// closes or a shutdown message arrives. Owns an outbound-channel clone so
/// pull-style replies bypass stdout.
async fn run_stdin_loop(
    action_sender: UnboundedSender<UiAction>,
    out_tx: OutboundTx,
    state_manager: Arc<StateManager>,
) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !dispatch_stdin_line(trimmed, &action_sender, &out_tx, &state_manager) {
            break;
        }
    }
    Ok(())
}

/// Dispatch one inbound line. Returns `false` when the loop should stop
/// (channel closed or `shutdown` received). Split out of
/// [`run_stdin_loop`] so the InboundMessage→UiAction mapping is testable
/// without a real stdin.
fn dispatch_stdin_line(
    trimmed: &str,
    action_sender: &UnboundedSender<UiAction>,
    out_tx: &OutboundTx,
    state_manager: &StateManager,
) -> bool {
    match serde_json::from_str::<InboundMessage>(trimmed) {
        Ok(InboundMessage::SendMessage { text }) => {
            action_sender.send(UiAction::SendMessage(text)).is_ok()
        }
        Ok(InboundMessage::Stop) => action_sender.send(UiAction::RequestStop).is_ok(),
        // Pause/resume the running sub-agent — same immediate path as Stop
        // (the controller handles them without queueing behind the turn).
        Ok(InboundMessage::Pause) => action_sender.send(UiAction::PauseSubAgent).is_ok(),
        Ok(InboundMessage::Resume) => action_sender.send(UiAction::ResumeSubAgent).is_ok(),
        Ok(InboundMessage::SwitchModel { alias }) => {
            action_sender.send(UiAction::SwitchModel(alias)).is_ok()
        }
        Ok(InboundMessage::SwitchCwd { path }) => {
            action_sender.send(UiAction::ChangeCwd(path)).is_ok()
        }
        Ok(InboundMessage::SelectPipeline { name }) => {
            action_sender.send(UiAction::SelectPipeline(name)).is_ok()
        }
        Ok(InboundMessage::ListDir { path }) => out_tx.send(build_dir_listing(&path)).is_ok(),
        Ok(InboundMessage::RequestConversations) => {
            // stdio is single-session with no registry — no conversation
            // is "active" in the sticky-session sense.
            let items = build_conversations_snapshot(state_manager, &Default::default());
            out_tx
                .send(OutboundMessage::ConversationsList { items })
                .is_ok()
        }
        Ok(InboundMessage::RequestRecentDirs) => {
            let dirs = build_recent_dirs(state_manager);
            out_tx.send(OutboundMessage::RecentDirs { dirs }).is_ok()
        }
        // Sticky-session frames are web-only (no registry over stdio) —
        // accept and ignore so the shared enum stays exhaustive.
        Ok(InboundMessage::Attach { .. }) | Ok(InboundMessage::KillSession { .. }) => true,
        Ok(InboundMessage::Shutdown) => {
            // `/exit` sets `exit_requested`, which unwinds the state loop
            // and lets `main` tear down cleanly.
            let _ = action_sender.send(UiAction::SendMessage("/exit".to_string()));
            false
        }
        Err(e) => out_tx
            .send(OutboundMessage::Error {
                message: format!("invalid inbound JSON: {e}"),
            })
            .is_ok(),
    }
}

/// Write one NDJSON line (+ newline + flush). Writer-task only, to keep
/// line atomicity.
async fn write_line(line: &str) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Inbound dispatch tests: `InboundMessage` → `UiAction` mapping.
    //! Mirrors the web `dispatch_inbound` pause/resume tests — the stdio
    //! surface must produce the same controller actions.

    use super::*;

    fn fixture() -> (
        UnboundedSender<UiAction>,
        tokio::sync::mpsc::UnboundedReceiver<UiAction>,
        OutboundTx,
        Arc<StateManager>,
    ) {
        let (action_tx, action_rx) = tokio::sync::mpsc::unbounded_channel();
        let (out_tx, _out_rx) = outbound_channel();
        let sm = StateManager::new_arc();
        (action_tx, action_rx, out_tx, sm)
    }

    #[test]
    fn stdin_pause_maps_to_pause_sub_agent_action() {
        let (tx, mut rx, out_tx, sm) = fixture();
        let kept = dispatch_stdin_line(r#"{"type":"pause"}"#, &tx, &out_tx, &sm);
        assert!(kept, "pause line must keep the stdin loop alive");
        assert!(
            matches!(rx.try_recv().unwrap(), UiAction::PauseSubAgent),
            "pause line must surface as UiAction::PauseSubAgent"
        );
    }

    #[test]
    fn stdin_resume_maps_to_resume_sub_agent_action() {
        let (tx, mut rx, out_tx, sm) = fixture();
        let kept = dispatch_stdin_line(r#"{"type":"resume"}"#, &tx, &out_tx, &sm);
        assert!(kept, "resume line must keep the stdin loop alive");
        assert!(
            matches!(rx.try_recv().unwrap(), UiAction::ResumeSubAgent),
            "resume line must surface as UiAction::ResumeSubAgent"
        );
    }

    #[test]
    fn stdin_stop_still_maps_to_request_stop() {
        // Regression guard for the arm next door.
        let (tx, mut rx, out_tx, sm) = fixture();
        let kept = dispatch_stdin_line(r#"{"type":"stop"}"#, &tx, &out_tx, &sm);
        assert!(kept);
        assert!(matches!(rx.try_recv().unwrap(), UiAction::RequestStop));
    }

    #[tokio::test]
    async fn stdin_invalid_json_reports_error_and_keeps_loop() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = outbound_channel();
        let sm = StateManager::new_arc();
        let kept = dispatch_stdin_line("not json", &tx, &out_tx, &sm);
        assert!(kept, "a bad line must not tear down the loop");
        // The error envelope goes out the outbound channel, not to the
        // controller.
        let msg = out_rx.next().await.expect("error envelope emitted");
        assert!(
            matches!(msg, OutboundMessage::Error { .. }),
            "bad line must surface as an Error envelope, got {msg:?}"
        );
        assert!(!tx.is_closed(), "action channel must stay open");
    }
}
