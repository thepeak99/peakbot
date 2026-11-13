//! One peer session (p3-design §D.5): `Handshaking(Initiator|Responder)`
//! → `Open { tx: FrameSender, rx: FrameReceiver, mux: Mux, established_at,
//! streams }`. The actor owns this; callers only see
//! [`super::stream::CloudStream`].

use std::collections::HashMap;

use cloud::{
    DeviceId, SessionId,
    frames::{FrameReceiver, FrameSender, Mux, Op, Role, StreamMsg},
    handshake::{Initiator, Responder},
};
use tokio::sync::{mpsc, oneshot};

use super::link::CloudError;
use super::stream::CloudStream;

/// The private per-peer session inside the actor's live phase.
#[derive(Debug)]
pub struct Session {
    pub peer: DeviceId,
    pub phase: Phase,
    /// `open()` calls queued while this peer's handshake is in flight;
    /// flushed (or failed) when the phase resolves.
    pub queued: Vec<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>,
    /// Tie-break / responder-side `open()`: a responder cannot send
    /// `Open` itself (mux S1), so this open binds to the first stream the
    /// winning initiator opens.
    pub claim: Option<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>,
    pub established_at: u64,
}

/// A local profile as reported by the `Profiles` op (§E.3: local profiles
/// only — `remote:` profiles never cross).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub color: String,
}

#[derive(Debug)]
pub enum Phase {
    Handshaking(Handshaking),
    Open(Open),
}

// Boxing the 400-byte `Initiator` would only move the allocation onto the
// per-handshake path; there is at most one per peer.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Handshaking {
    Initiator(Initiator),
    Responder(Responder),
}

#[derive(Debug)]
pub struct Open {
    pub sid: SessionId,
    pub tx: FrameSender,
    pub rx: FrameReceiver,
    pub mux: Mux,
    pub role: Role,
    /// Live stream id → the actor→caller side (bounded 32; a full
    /// receiver resets the stream, §D.5 backpressure).
    pub streams: HashMap<u32, StreamSide>,
    /// Next stream id for THIS side's opens (strictly increasing, S1).
    pub next_stream: u32,
}

#[derive(Debug)]
pub struct StreamSide {
    /// Actor → caller. Dropped when the stream closes (the caller sees
    /// `None`).
    pub to_caller: mpsc::Sender<StreamMsg>,
    /// Cancels the outbound forwarder task (session/stream close).
    pub forward_stop: oneshot::Sender<()>,
    /// The caller sent `End` on this direction: a later drop must not
    /// also send `Reset(CANCELLED)`.
    pub ended: bool,
}
