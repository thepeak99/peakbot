//! Remote profiles (T6) — the WS relay (design §5.2, §6.3).
//!
//! A browser socket attached to remote profile L is piped to `/ws` on L's
//! server, re-attached there as profile P. Inbound frames are classified by
//! the §6.3 table, outbound `state` is localized; nothing local is touched.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message as TMsg;

use super::{Directory, WsStream};
use crate::config::resolve::ProfileInfo;
use crate::ui::app_state::AppState;
use crate::ui::outbound::{Disconnected, OutboundTx};
use crate::ui::web::conversations::local_rows;
use crate::ui::web::images::rewrite_markdown_images;
use crate::ui::web::refuse;
use crate::ui::web::registry::SessionRegistry;
use crate::ui::wire::{InboundMessage, OutboundMessage, PROTOCOL_VERSION};

/// §5.2 step 5: `attach` sent → `ready` received.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// §5.2 step 6: the remote pings every 30 s, so this much silence is a dead leg.
const READ_IDLE: Duration = Duration::from_secs(75);
/// Bounds the clean close of the remote leg once the browser is gone.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

const CHIP_REFUSAL: &str = "switch profiles from the chip";
const E_SHUTDOWN: &str = "shutdown isn't available in a remote profile.";

/// Process-wide because `WsState` is built in many places (tests included);
/// it re-reads the global config on every call anyway (§5.11).
static DIRECTORY: LazyLock<Directory> = LazyLock::new(Directory::new);

/// What the local side knows about the tab: L, its remote target P (and
/// agent pin), and the local catalogue the SPA's chip shows.
pub struct LocalView {
    pub name: String,
    pub target_profile: String,
    pub target_agent: Option<String>,
    pub catalog: Vec<ProfileInfo>,
}

/// The §6.3 inbound table.
#[derive(Debug)]
pub enum InboundDisposition {
    Forward,
    Ignore,
    ForwardRewritten(String),
    Refuse(String),
    InterceptLocal(LocalVerb),
    ShutdownBlocked,
    Invalid,
}

/// Frames answered by the local server, never forwarded (§5.6).
#[derive(Debug)]
pub enum LocalVerb {
    RequestConversations,
    KillSession {
        convo: String,
        profile: Option<String>,
    },
}

pub fn classify_inbound(text: &str, view: &LocalView) -> InboundDisposition {
    use InboundDisposition as D;
    let Ok(msg) = serde_json::from_str::<InboundMessage>(text) else {
        return D::Invalid;
    };
    match msg {
        InboundMessage::SendMessage { .. }
        | InboundMessage::Stop
        | InboundMessage::Pause
        | InboundMessage::Resume
        | InboundMessage::SwitchModel { .. }
        | InboundMessage::SwitchCwd { .. }
        | InboundMessage::ListDir { .. }
        | InboundMessage::RequestRecentDirs => D::Forward,
        InboundMessage::Attach { .. } => D::Ignore,
        InboundMessage::Select { profile, agent, .. } => {
            if profile != view.name {
                return D::Refuse(CHIP_REFUSAL.to_string());
            }
            if let Some(pin) = &view.target_agent
                && agent != *pin
            {
                return D::Refuse(format!(
                    "Remote profile '{}' is pinned to agent '{pin}'.",
                    view.name
                ));
            }
            // Rewrite via `Value` so every other field rides along unchanged.
            let mut frame: serde_json::Value =
                serde_json::from_str(text).expect("it parsed as InboundMessage above");
            frame["profile"] = view.target_profile.clone().into();
            D::ForwardRewritten(frame.to_string())
        }
        InboundMessage::RequestConversations => D::InterceptLocal(LocalVerb::RequestConversations),
        InboundMessage::KillSession { convo, profile } => {
            D::InterceptLocal(LocalVerb::KillSession { convo, profile })
        }
        InboundMessage::Shutdown => D::ShutdownBlocked,
    }
}

/// The remote's snapshot as the local SPA must see it (§6.3): exactly four
/// changes, everything else untouched.
pub fn localize_state(mut state: AppState, view: &LocalView) -> AppState {
    // Re-root under `/r/L/` so a remote can't make the browser load a local
    // same-origin endpoint (§7); `//host` is not root-relative.
    let root = format!("/r/{}", path_segment(&view.name));
    for m in &mut state.chat.messages {
        let rewritten = rewrite_markdown_images(&m.content, |dest| {
            (dest.starts_with('/') && !dest.starts_with("//")).then(|| format!("{root}{dest}"))
        });
        if let std::borrow::Cow::Owned(s) = rewritten {
            m.content = s;
        }
    }

    let agents = state
        .catalog
        .iter()
        .find(|p| p.name == view.target_profile)
        .map(|p| {
            p.agents
                .iter()
                .filter(|a| view.target_agent.as_ref().is_none_or(|pin| a.name == *pin))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    state.catalog = view.catalog.clone();
    if let Some(l) = state.catalog.iter_mut().find(|p| p.name == view.name) {
        l.agents = agents;
    }

    if state.selection.profile == view.target_profile {
        state.selection.profile = view.name.clone();
    }
    state.profile_pinned = false;
    state
}

/// Percent-encode a profile name for a URL path (the name rule allows spaces).
fn path_segment(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Which leg ended the pump.
enum End {
    Local,
    Remote,
}

/// Run one relayed browser socket to completion (§5.2). `convo`/`agent` are
/// the browser's attach fields. `registry` only answers the local list.
pub async fn run(
    mut local: SplitStream<WebSocket>,
    out_tx: OutboundTx,
    mut writer_task: JoinHandle<()>,
    registry: SessionRegistry,
    view: LocalView,
    convo: Option<String>,
    agent: Option<String>,
) {
    let dialed = match DIRECTORY.client(&view.name).await {
        Ok(client) => client.dial().await.map(|ws| (client, ws)),
        Err(e) => Err(e),
    };
    let (client, ws) = match dialed {
        Ok(pair) => pair,
        Err(e) => return refuse(out_tx, writer_task, e.to_string()).await,
    };
    let (mut remote_tx, mut remote_rx) = ws.split();

    let mut attach = serde_json::json!({
        "type": "attach",
        "profile": view.target_profile,
        "relayed": true,
    });
    if let Some(c) = convo {
        attach["convo"] = c.into();
    }
    if let Some(a) = view.target_agent.clone().or(agent) {
        attach["agent"] = a.into();
    }
    if remote_tx
        .send(TMsg::Text(attach.to_string().into()))
        .await
        .is_err()
    {
        return refuse(out_tx, writer_task, e_lost(&view)).await;
    }

    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut remote_rx, &view)).await;
    let frames = match handshake {
        Ok(Ok(frames)) => frames,
        Ok(Err(message)) => return refuse(out_tx, writer_task, message).await,
        Err(_) => return refuse(out_tx, writer_task, client.peer.timeout().to_string()).await,
    };
    for frame in frames {
        let _ = out_tx.send(frame);
    }

    let mut writer_done = false;
    let idle = tokio::time::sleep(READ_IDLE);
    tokio::pin!(idle);
    let end = loop {
        tokio::select! {
            frame = local.next() => match frame {
                Some(Ok(Message::Text(t))) => {
                    if let Some(end) = inbound(t.as_str(), &view, &out_tx, &mut remote_tx, &registry).await {
                        break end;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break End::Local,
                Some(Ok(_)) => {}
            },
            frame = remote_rx.next() => {
                idle.as_mut().reset(Instant::now() + READ_IDLE);
                match frame {
                    Some(Ok(TMsg::Text(t))) => {
                        if outbound(t.as_str(), &view, &out_tx).is_err() {
                            break End::Local;
                        }
                    }
                    Some(Ok(TMsg::Close(_)) | Err(_)) | None => break End::Remote,
                    Some(Ok(_)) => {}
                }
            },
            () = &mut idle => break End::Remote,
            _ = &mut writer_task => {
                writer_done = true;
                break End::Local;
            }
        }
    };

    match end {
        // A clean close only detaches; the remote session lives on (§5.7).
        End::Local => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, remote_tx.close()).await;
        }
        End::Remote => {
            let _ = out_tx.send(OutboundMessage::Error {
                message: e_lost(&view),
            });
        }
    }
    drop(out_tx);
    if !writer_done {
        let _ = writer_task.await;
    }
}

/// Read until `ready` (§5.2 step 5). `Ok` is what to forward; `Err` the one
/// error frame to send before closing. `attached` is held back until the
/// version passes, so a refused remote forwards nothing but the error.
async fn handshake(
    remote: &mut SplitStream<WsStream>,
    view: &LocalView,
) -> Result<Vec<OutboundMessage>, String> {
    let mut frames = Vec::new();
    while let Some(Ok(frame)) = remote.next().await {
        let TMsg::Text(text) = frame else {
            if matches!(frame, TMsg::Close(_)) {
                break;
            }
            continue;
        };
        match serde_json::from_str::<OutboundMessage>(text.as_str()) {
            Ok(OutboundMessage::Ready { v }) if v == PROTOCOL_VERSION => {
                frames.push(OutboundMessage::Ready { v });
                return Ok(frames);
            }
            Ok(OutboundMessage::Ready { v }) => {
                return Err(format!(
                    "Remote profile '{}': remote PeakBot speaks protocol v{v}, \
                     this PeakBot speaks v{PROTOCOL_VERSION} — upgrade one side so they match.",
                    view.name
                ));
            }
            Ok(OutboundMessage::Error { message }) => return Err(message),
            Ok(attached @ OutboundMessage::Attached { .. }) => frames.push(attached),
            // Nothing else is expected before `ready`.
            _ => {}
        }
    }
    Err(e_lost(view))
}

/// One browser frame through the §6.3 inbound table. `Some` ends the pump.
async fn inbound(
    text: &str,
    view: &LocalView,
    out_tx: &OutboundTx,
    remote: &mut SplitSink<WsStream, TMsg>,
    registry: &SessionRegistry,
) -> Option<End> {
    if text.trim().is_empty() {
        return None;
    }
    let reply = match classify_inbound(text, view) {
        // The raw text, not a re-encoding: forwards are byte-identical (§5.3).
        InboundDisposition::Forward => return forward(remote, text.to_string()).await,
        InboundDisposition::ForwardRewritten(rewritten) => {
            return forward(remote, rewritten).await;
        }
        InboundDisposition::Ignore => return None,
        InboundDisposition::Refuse(message) => OutboundMessage::Error { message },
        InboundDisposition::ShutdownBlocked => OutboundMessage::Error {
            message: E_SHUTDOWN.to_string(),
        },
        InboundDisposition::Invalid => {
            let e = serde_json::from_str::<InboundMessage>(text)
                .err()
                .map_or_else(String::new, |e| e.to_string());
            OutboundMessage::Error {
                message: format!("invalid inbound JSON: {e}"),
            }
        }
        // T7 replaces this with the merged local + remote list (§5.6).
        InboundDisposition::InterceptLocal(LocalVerb::RequestConversations) => {
            OutboundMessage::ConversationsList {
                items: local_rows(registry),
                error: None,
            }
        }
        // T7 routes the kill by `profile` (§5.6); until then it is dropped.
        InboundDisposition::InterceptLocal(LocalVerb::KillSession { convo, profile }) => {
            tracing::debug!(%convo, ?profile, "relay: kill_session awaits T7 routing");
            return None;
        }
    };
    out_tx.send(reply).err().map(|Disconnected| End::Local)
}

async fn forward(remote: &mut SplitSink<WsStream, TMsg>, text: String) -> Option<End> {
    remote
        .send(TMsg::Text(text.into()))
        .await
        .err()
        .map(|_| End::Remote)
}

/// One remote frame through the §6.3 outbound table.
fn outbound(text: &str, view: &LocalView, out_tx: &OutboundTx) -> Result<(), Disconnected> {
    let msg = match serde_json::from_str::<OutboundMessage>(text) {
        Ok(msg) => msg,
        Err(e) => {
            tracing::debug!("relay: dropping an unknown or unparseable remote frame: {e}");
            return Ok(());
        }
    };
    match msg {
        OutboundMessage::State { state } => {
            let state = localize_state(Arc::unwrap_or_clone(state), view);
            out_tx.publish_state(Arc::new(state))
        }
        // Never solicited (§6.3 rows 11/12), and `ready` is the handshake's.
        OutboundMessage::ConversationsList { .. } | OutboundMessage::Ready { .. } => Ok(()),
        other => out_tx.send(other),
    }
}

fn e_lost(view: &LocalView) -> String {
    format!(
        "Lost the connection to remote profile '{}'. Reconnecting…",
        view.name
    )
}
