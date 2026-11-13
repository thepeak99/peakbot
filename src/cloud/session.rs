//! One peer session (p3-design §D.5): `Handshaking(Initiator|Responder)`
//! → `Open { tx: FrameSender, rx: FrameReceiver, mux: Mux, established_at,
//! streams }`. The actor owns this; callers only see [`super::stream::CloudStream`].

use cloud::handshake::{Initiator, Responder};

/// A local profile as reported by the `Profiles` op (§E.3: local profiles
/// only — `remote:` profiles never cross).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    pub color: String,
}

/// The private per-peer session inside the actor's `Active` phase.
pub struct Session {
    pub phase: Phase,
}

pub enum Phase {
    Handshaking(Handshaking),
    Open { established_at: u64 },
}

pub enum Handshaking {
    Initiator(Initiator),
    Responder(Responder),
}
