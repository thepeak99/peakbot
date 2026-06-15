//! StdioUi — a PeakBot `Ui` implementation that speaks NDJSON over stdio.
//!
//! Selected at launch with `peakbot --stdio`. Each line on stdin is one
//! inbound message from the client (e.g. an IDE plugin); each line on
//! stdout is one outbound message from the agent. The agent, providers,
//! tools, skills, MCP servers, conversation persistence, and cost
//! tracking are all the same machinery the TUI drives — only the View
//! differs.
//!
//! ## Stdout discipline
//!
//! Stdout is the protocol channel — *only* NDJSON lines emitted here go
//! there. `main` routes `tracing` to stderr under `--stdio` so the
//! client never has to parse around log noise.
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
//! `request_conversations` whenever it wants a fresh snapshot. This is a
//! read-only side channel answered directly in the stdin task — it never
//! reaches the agent loop, so it deliberately is not a `UiAction`. The
//! host keeps no cache; `/save`, `/delete`, `/rename` and per-turn
//! auto-saves mutate the list, so any server-side cache would need
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
    /// Send a user message (or slash command starting with `/`).
    SendMessage { text: String },
    /// Request the agent to stop the current turn.
    Stop,
    /// Switch the active model by alias.
    SwitchModel { alias: String },
    /// Ask for a fresh snapshot of saved conversations (pull model).
    RequestConversations,
    /// Cleanly tear down the agent loop.
    Shutdown,
}

/// One entry in the `models_available` outbound message. Mirrors the
/// public fields of `ResolvedModel` that the UI needs to render a model
/// picker. Built once at boot from the [`ModelRegistry`].
#[derive(Debug, Serialize, Clone)]
pub struct ModelInfo {
    /// Canonical user handle — what `/model <alias>` accepts.
    pub alias: String,
    /// Informational provider name (`"openrouter"`, `"patchnotes"`, ...).
    pub provider_name: String,
    /// Wire id (model name as the provider knows it).
    pub model_name: String,
    /// Resolved context size in tokens.
    pub context_size: usize,
}

/// Build the static `Vec<ModelInfo>` handed to [`StdioUi`] for the
/// one-shot `models_available` emission. Empty when the registry is
/// empty (legacy single-provider path) — `run()` skips the emission in
/// that case.
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

/// One entry in the `conversations_list` outbound message. Trimmed
/// subset of `ConversationSummary` — only what a dropdown picker needs.
/// Backend already sorts newest first.
#[derive(Debug, Serialize)]
struct ConversationSummaryWire {
    /// Wire id (UUID stringified) — fed back to peakbot as `/load <id>`.
    id: String,
    /// Human-readable name as the user named it (or default).
    name: String,
    /// ISO 8601 UTC of the most recent update.
    updated_at: String,
    /// Total messages in the conversation.
    message_count: usize,
    /// Wire id of the model the conversation last used.
    model: String,
}

/// Outbound message envelopes to the client. Owning (no lifetime) so
/// any task can push to the shared writer channel without dancing
/// around borrows.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutboundMessage {
    /// Initial handshake — sent once after subscribe so the client can
    /// render the welcome banner before any user input is sent.
    Ready,
    /// Static snapshot of the model registry. Emitted once after
    /// `ready`. `active` is the boot alias (may be empty in the legacy
    /// single-provider path).
    ModelsAvailable {
        active: String,
        models: Vec<ModelInfo>,
    },
    /// A full `AppState` snapshot. Mirrors the broadcast the TUI sees.
    /// Boxed because `AppState` is large — keeps the enum slim so the
    /// channel's per-message overhead stays cheap (clippy's
    /// `large_enum_variant` lint).
    State { state: Box<AppState> },
    /// Reply to `request_conversations`. May be empty if no storage is
    /// configured or there are no saved conversations.
    ConversationsList { items: Vec<ConversationSummaryWire> },
    /// A non-fatal protocol or parse error.
    Error { message: String },
}

/// `Ui` implementation that pumps `AppState` broadcasts to stdout as
/// NDJSON and reads `UiAction` requests from stdin as NDJSON.
pub struct StdioUi {
    state_manager: Arc<StateManager>,
    action_sender: UnboundedSender<UiAction>,
    /// Snapshot of the model registry built at boot. Empty in the
    /// legacy single-provider path — in that case we skip the
    /// `models_available` emission (no picker to populate).
    models: Vec<ModelInfo>,
    /// Boot alias, mirrored from the registry. May be empty in the
    /// legacy path; the picker simply shows no preselection then.
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
        // No-op: stdout writes are funneled through `run()`'s writer
        // task. The `ready` + `models_available` emissions happen as
        // the first messages on that channel.
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<OutboundMessage>();

        // Writer task: SOLE owner of stdout. Drains the MPSC and writes
        // one NDJSON line per message. Two-writer races are impossible
        // by construction — exactly one task ever touches stdout.
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

        // Boot emissions: ready + (optional) models_available. Ordering
        // matters — the client expects `ready` first.
        let _ = out_tx.send(OutboundMessage::Ready);
        if !self.models.is_empty() {
            let _ = out_tx.send(OutboundMessage::ModelsAvailable {
                active: self.active_alias.clone(),
                models: self.models.clone(),
            });
        }

        // Stdin reader: forwards UiActions to the controller, replies
        // directly to pull-style requests (currently only
        // `request_conversations`).
        let action_sender = self.action_sender.clone();
        let state_manager = self.state_manager.clone();
        let stdin_tx = out_tx.clone();
        let stdin_task = tokio::spawn(async move {
            if let Err(e) = run_stdin_loop(action_sender, stdin_tx, state_manager).await {
                tracing::warn!("stdin loop ended with error: {e:?}");
            }
        });

        // State broadcast loop. Owns `out_tx` until exit, then drops it
        // so the writer task drains and exits.
        let mut state_rx = self.state_manager.subscribe();
        while let Some(state) = state_rx.recv().await {
            let exit = state.exit_requested;
            if out_tx
                .send(OutboundMessage::State {
                    state: Box::new(state),
                })
                .is_err()
            {
                // Writer dropped its receiver — nothing left to listen.
                break;
            }
            if exit {
                break;
            }
        }

        // Tear down: stdin task may still be parked on a read; abort it.
        // Drop our local sender, then await the writer to drain.
        stdin_task.abort();
        drop(out_tx);
        let _ = writer_task.await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        // Best-effort flush of stdout.
        let mut stdout = tokio::io::stdout();
        let _ = stdout.flush().await;
        Ok(())
    }
}

/// Read NDJSON from stdin and dispatch [`UiAction`]s. Returns when stdin
/// closes or a shutdown message is received.
///
/// Owns a clone of the outbound channel so pull-style replies
/// (`conversations_list`, `error`) can be sent without touching stdout
/// directly.
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
                // Submit /exit as a slash command — AgentRunner sets
                // `exit_requested` on AppState, which kicks the state
                // loop out of `run` and lets `main` tear everything
                // down cleanly.
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

/// Build the wire-shaped snapshot of saved conversations. Returns an
/// empty vec when no storage is configured — the client treats that as
/// "hide the picker".
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

/// Write a single NDJSON line to stdout with newline + flush. Only ever
/// called from the writer task — no other code path may touch stdout,
/// to keep line atomicity.
async fn write_line(line: &str) -> Result<()> {
    let mut stdout = tokio::io::stdout();
    stdout.write_all(line.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}
