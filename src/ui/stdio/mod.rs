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
//! {"type":"switch_model","alias":"sonnet"}
//! {"type":"request_conversations"}
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

use crate::config::ModelRegistry;
use crate::ui::app_state::AppState;
use crate::{StateManager, Ui, UiAction};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{self, UnboundedSender};

/// Inbound message types from the client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InboundMessage {
    /// `text` may be a slash command (`/new`, `/load <id>`, …) — peakbot
    /// classifies it internally.
    SendMessage {
        text: String,
    },
    Stop,
    SwitchModel {
        alias: String,
    },
    RequestConversations,
    Shutdown,
}

/// One `models_available` entry — the subset of `ResolvedModel` a model
/// picker needs.
#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    /// What `/model <alias>` accepts.
    pub alias: String,
    pub provider_name: String,
    /// Model name as the provider knows it.
    pub model_name: String,
    pub context_size: usize,
}

/// Snapshot for [`StdioUi`]'s one-shot `models_available` emission. Empty
/// for the legacy single-provider path, where `run()` skips the emission.
pub fn build_models_snapshot(registry: &ModelRegistry) -> Vec<ModelInfo> {
    registry
        .iter_sorted()
        .into_iter()
        .map(|(_, rm)| ModelInfo {
            alias: rm.alias.clone(),
            provider_name: rm.provider_name.clone(),
            model_name: rm.model_name.clone(),
            context_size: rm.context_size,
        })
        .collect()
}

/// Trimmed subset of `ConversationSummary` for a dropdown picker
/// (backend already sorts newest first).
#[derive(Debug, Serialize)]
struct ConversationSummaryWire {
    /// Fed back as `/load <id>`.
    id: String,
    name: String,
    /// ISO 8601 UTC.
    updated_at: String,
    message_count: usize,
    model: String,
}

/// Outbound message envelopes to the client. Owning (no lifetime) so any
/// task can push to the shared writer channel.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutboundMessage {
    /// Handshake, sent once before any user input so the client can render
    /// the welcome banner.
    Ready,
    /// One-shot registry snapshot, emitted right after `ready`. `active`
    /// may be empty in the legacy single-provider path.
    ModelsAvailable {
        active: String,
        models: Vec<ModelInfo>,
    },
    /// A full `AppState` snapshot — the same broadcast the TUI sees. Boxed
    /// to keep the enum slim (clippy `large_enum_variant`).
    State { state: Box<AppState> },
    /// Reply to `request_conversations`; empty when no storage is configured.
    ConversationsList { items: Vec<ConversationSummaryWire> },
    /// A non-fatal protocol or parse error.
    Error { message: String },
}

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
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutboundMessage>();

        // Sole owner of stdout — keeps NDJSON lines atomic since no other
        // task ever writes there.
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
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
            if out_tx
                .send(OutboundMessage::State {
                    state: Box::new(state),
                })
                .is_err()
            {
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
    out_tx: mpsc::UnboundedSender<OutboundMessage>,
    state_manager: Arc<StateManager>,
) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<InboundMessage>(trimmed) {
            Ok(InboundMessage::SendMessage { text }) => {
                if action_sender.send(UiAction::SendMessage(text)).is_err() {
                    break;
                }
            }
            Ok(InboundMessage::Stop) => {
                if action_sender.send(UiAction::RequestStop).is_err() {
                    break;
                }
            }
            Ok(InboundMessage::SwitchModel { alias }) => {
                if action_sender.send(UiAction::SwitchModel(alias)).is_err() {
                    break;
                }
            }
            Ok(InboundMessage::RequestConversations) => {
                let items = build_conversations_snapshot(&state_manager);
                if out_tx
                    .send(OutboundMessage::ConversationsList { items })
                    .is_err()
                {
                    break;
                }
            }
            Ok(InboundMessage::Shutdown) => {
                // `/exit` sets `exit_requested`, which unwinds the state loop
                // and lets `main` tear down cleanly.
                let _ = action_sender.send(UiAction::SendMessage("/exit".to_string()));
                break;
            }
            Err(e) => {
                let envelope = OutboundMessage::Error {
                    message: format!("invalid inbound JSON: {e}"),
                };
                if out_tx.send(envelope).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Wire-shaped snapshot of saved conversations. Empty when no storage is
/// configured — the client treats that as "hide the picker".
fn build_conversations_snapshot(sm: &StateManager) -> Vec<ConversationSummaryWire> {
    sm.list_conversations()
        .unwrap_or_default()
        .into_iter()
        .map(|s| ConversationSummaryWire {
            id: s.id.to_string(),
            name: s.name,
            updated_at: s.updated_at.to_rfc3339(),
            message_count: s.message_count,
            model: s.model,
        })
        .collect()
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
