//! One peer session (p3-design §D.5): `Handshaking(Initiator|Responder)`
//! → `Open { tx: FrameSender, rx: FrameReceiver, mux: Mux, established_at,
//! streams }`. The actor owns this; callers only see
//! [`super::stream::CloudStream`].

use std::collections::HashMap;
use std::sync::Arc;

use cloud::{
    DeviceId, SessionId, constants,
    frames::{
        CreditLedger, FragmentAssembler, FrameReceiver, FrameSender, Mux, Op, ReassemblyBudget,
        Role, StreamMsg,
    },
    handshake::{Initiator, Responder},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

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
    /// Aggregate in-flight fragment bytes across all Attach streams.
    pub reassembly: ReassemblyBudget,
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
    pub sent_end: bool,
    /// The peer sent `End` on its direction. `End` is a HALF-close (S3/S4):
    /// when the caller then drops, we still owe our own `End`, or the mux
    /// pair never closes and its slot leaks until `MAX_STREAMS`.
    pub peer_ended: bool,
    /// Transport v2 framing + credit state; `None` for non-Attach ops.
    pub attach: Option<AttachState>,
    /// Non-Attach bulk send window: one permit per byte the peer allows.
    /// The stream's forwarder acquires `wire_cost` permits before handing a
    /// record to the actor and permanently consumes them (never returns
    /// them), so `available_permits()` is exactly the credit ledger. `None`
    /// for Attach, whose credit lives in [`AttachState`].
    pub credit: Option<Arc<Semaphore>>,
}

/// One Attach direction's v2 state: the send window, the inbound
/// reassembler and the ordered outbound records waiting for credit.
#[derive(Debug)]
pub struct AttachState {
    pub send_credit: CreditLedger,
    /// Inbound data bytes consumed since the last `Credit` was returned.
    pub recv_since_grant: u32,
    pub fragments: FragmentAssembler,
    pub outbox: std::collections::VecDeque<Outgoing>,
    /// Logical bytes queued but not yet on the wire (bounded).
    pub queued_bytes: usize,
    /// The caller dropped; finish the outbox, then end or reset.
    pub caller_gone: bool,
    /// Monotonic time of the last inbound fragment activity. The run loop
    /// reaps an assembly that makes no progress for `RECV_STALL_SECS`; there
    /// is no per-stream timer task. Only real fragment progress touches it —
    /// idle time between complete messages is not a stall.
    pub last_progress: tokio::time::Instant,
}

/// One queued outbound item. A large text is serialized as
/// `FrameBegin`/`Chunk*`/`FrameEnd` without copying it into chunks up
/// front; a small record passes through whole.
pub enum Outgoing {
    Record {
        msg: StreamMsg,
        permit: Option<OwnedSemaphorePermit>,
    },
    Fragmented {
        text: String,
        offset: usize,
        begun: bool,
        permit: Option<OwnedSemaphorePermit>,
    },
}

/// The record the front of the outbox will produce next.
enum Next {
    Record,
    Begin,
    End,
    Chunk,
}

impl AttachState {
    pub fn new(budget: ReassemblyBudget) -> Self {
        Self {
            send_credit: CreditLedger::new(constants::CREDIT_INITIAL, constants::CREDIT_MAX),
            recv_since_grant: 0,
            fragments: FragmentAssembler::with_budget(budget),
            outbox: std::collections::VecDeque::new(),
            queued_bytes: 0,
            caller_gone: false,
            last_progress: tokio::time::Instant::now(),
        }
    }

    /// Queue one caller message in order. Small Frames stay one record;
    /// a Frame over the one-shot threshold is fragmented as it is sent.
    /// Returns the reset code on an application-level size rejection.
    pub fn enqueue(
        &mut self,
        msg: StreamMsg,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<(), u16> {
        let bytes = match &msg {
            StreamMsg::Frame(text) => text.len(),
            StreamMsg::Chunk(chunk) => chunk.len(),
            _ => 0,
        };
        if let StreamMsg::Frame(text) = &msg
            && text.len() > constants::MAX_LOGICAL_MESSAGE
        {
            return Err(constants::reset::TOO_LARGE);
        }
        if let StreamMsg::Chunk(chunk) = &msg
            && chunk.len() > constants::MAX_CHUNK
        {
            return Err(constants::reset::PROTOCOL);
        }
        if self.queued_bytes.saturating_add(bytes) > constants::MAX_LOGICAL_MESSAGE {
            return Err(constants::reset::TOO_LARGE);
        }
        self.queued_bytes += bytes;
        match msg {
            StreamMsg::Frame(text) if text.len() > constants::FRAME_ONESHOT_MAX => {
                self.outbox.push_back(Outgoing::Fragmented {
                    text,
                    offset: 0,
                    begun: false,
                    permit,
                });
            }
            other => {
                self.outbox
                    .push_back(Outgoing::Record { msg: other, permit });
            }
        }
        Ok(())
    }

    /// The next wire record, consuming its credit atomically. `None` means
    /// the outbox is empty or the window is exhausted; a later `Credit`
    /// re-runs this. Credit is checked BEFORE any offset advances, so a
    /// blocked chunk never skips bytes.
    pub fn next_record(&mut self) -> Option<(StreamMsg, bool)> {
        let (pending, cost) = {
            let front = self.outbox.front()?;
            match front {
                Outgoing::Record { msg, .. } => (Next::Record, wire_cost(msg)),
                Outgoing::Fragmented {
                    text,
                    offset,
                    begun,
                    ..
                } => {
                    if !*begun {
                        (Next::Begin, 0)
                    } else {
                        let remaining = text.len() - *offset;
                        if remaining == 0 {
                            (Next::End, 0)
                        } else {
                            (Next::Chunk, remaining.min(constants::MAX_CHUNK))
                        }
                    }
                }
            }
        };
        let cost = u32::try_from(cost).ok()?;
        if self.send_credit.available() < cost {
            return None;
        }
        self.send_credit.consume(cost).ok()?;
        let front = self.outbox.front_mut()?;
        match (pending, front) {
            (Next::Record, Outgoing::Record { msg, .. }) => Some((msg.clone(), true)),
            (Next::Begin, Outgoing::Fragmented { text, begun, .. }) => {
                *begun = true;
                Some((
                    StreamMsg::FrameBegin {
                        len: text.len() as u32,
                    },
                    false,
                ))
            }
            (Next::End, Outgoing::Fragmented { .. }) => Some((StreamMsg::FrameEnd, true)),
            (Next::Chunk, Outgoing::Fragmented { text, offset, .. }) => {
                let n = (text.len() - *offset).min(constants::MAX_CHUNK);
                let chunk = text.as_bytes()[*offset..*offset + n].to_vec();
                *offset += n;
                Some((StreamMsg::Chunk(chunk), false))
            }
            _ => None,
        }
    }

    /// Drop the record just sent, keeping `queued_bytes` honest.
    pub fn pop_sent(&mut self) {
        if let Some(item) = self.outbox.pop_front() {
            self.queued_bytes = self.queued_bytes.saturating_sub(item.queued_len());
        }
    }
}

impl Outgoing {
    fn queued_len(&self) -> usize {
        match self {
            Outgoing::Record { msg, .. } => wire_cost(msg),
            Outgoing::Fragmented { text, .. } => text.len(),
        }
    }
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outgoing::Record { msg, .. } => f.debug_tuple("Record").field(msg).finish(),
            Outgoing::Fragmented {
                text,
                offset,
                begun,
                ..
            } => f
                .debug_struct("Fragmented")
                .field("len", &text.len())
                .field("offset", offset)
                .field("begun", begun)
                .finish(),
        }
    }
}

/// Credit bills data bytes only; control records are free.
pub fn wire_cost(msg: &StreamMsg) -> usize {
    match msg {
        StreamMsg::Frame(text) => text.len(),
        StreamMsg::Chunk(chunk) => chunk.len(),
        _ => 0,
    }
}
