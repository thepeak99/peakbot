//! Data frames: record protection (spec §9.1–§9.3), inner stream
//! messages (spec §9.4), stream multiplexing rules (spec §9.5), session
//! termination limits (spec §9.6).
//!
//! ```text
//! nonce = 0^16 ‖ u64(ctr)                     (24 bytes, XChaCha20)
//! aad   = u8(1) ‖ u8(4) ‖ fix[16](sid) ‖ u64(ctr)   (26 bytes)
//! ct    = aead_seal(k_send, nonce, aad, inner)      (A3)
//! ```

use std::collections::BTreeMap;

use perma_crypto::v1;

use crate::constants;
use crate::{Error, Result, SessionId};

// ---------------------------------------------------------------------------
// Record protection (spec §9.1)
// ---------------------------------------------------------------------------

/// The 24-byte XChaCha20 nonce: `0^16 ‖ u64(ctr)` (spec §9.1).
pub fn record_nonce(ctr: u64) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[16..].copy_from_slice(&ctr.to_be_bytes());
    nonce
}

/// The 26-byte AAD: `u8(1) ‖ u8(4) ‖ fix[16](sid) ‖ u64(ctr)` — the Data
/// pipe_msg prefix before `bytes(ct)` (spec §9.1).
pub fn record_aad(sid: &SessionId, ctr: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(26);
    out.push(constants::PIPE_VERSION);
    out.push(4);
    out.extend_from_slice(sid);
    out.extend_from_slice(&ctr.to_be_bytes());
    out
}

/// True while `ctr` may still be sent: `ctr < MAX_RECORDS` (2^32)
/// (spec §9.6). The sender MUST NOT send `ctr = 2^32`.
pub fn ctr_allowed(ctr: u64) -> bool {
    ctr < constants::MAX_RECORDS
}

/// True once the session is past `SESSION_LIFETIME` (24 h) from
/// `established_at`, with checked arithmetic (spec §1.5, §9.6): an
/// underflow (now < established_at) is out of range, never a panic.
pub fn session_expired(established_at: u64, now: u64) -> bool {
    established_at.saturating_add(constants::SESSION_LIFETIME) <= now
}

// ---------------------------------------------------------------------------
// Sender / receiver (spec §9.2)
// ---------------------------------------------------------------------------

/// A data-frame sender (one direction of an established session).
/// `ctr` starts at 0 and increases by exactly 1 per frame.
#[derive(Debug)]
pub struct FrameSender {
    sid: SessionId,
    k_send: [u8; 32],
    ctr: u64,
}

impl FrameSender {
    /// New sender for `sid` with the direction's send key.
    pub fn new(sid: SessionId, k_send: [u8; 32]) -> Self {
        Self {
            sid,
            k_send,
            ctr: 0,
        }
    }

    /// Seal `inner` (bincode plaintext, spec §9.4) into a Data pipe_msg
    /// with the next counter. At the `MAX_RECORDS` limit the sender must
    /// not send: `Error::Limit` (the caller closes with reason `Limit`).
    pub fn seal(&mut self, inner: &[u8]) -> Result<crate::handshake::Data> {
        if !ctr_allowed(self.ctr) {
            return Err(Error::Limit);
        }
        let aad = record_aad(&self.sid, self.ctr);
        let ct = v1::aead_seal(&self.k_send, &record_nonce(self.ctr), &aad, inner)
            .map_err(|_| Error::DecryptionError)?;
        let d = crate::handshake::Data {
            sid: self.sid,
            ctr: self.ctr,
            ct,
        };
        self.ctr += 1;
        Ok(d)
    }
}

/// What `Receiver::open` did (spec §9.2 rule 1: a stale sid is dropped
/// silently, not an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvOutcome {
    /// The frame's sid did not match: dropped silently, session intact.
    Dropped,
    /// The frame authenticated and decoded.
    Inner(Inner),
}

/// A data-frame receiver (spec §9.2). STRICT: any failure closes the
/// session (`closed()` becomes true) and the named error is returned.
#[derive(Debug)]
pub struct FrameReceiver {
    sid: SessionId,
    k_recv: [u8; 32],
    expected: u64,
    /// The abort that closed the session, if any.
    closed: Option<Error>,
}

impl FrameReceiver {
    /// New receiver for `sid` with the direction's receive key.
    pub fn new(sid: SessionId, k_recv: [u8; 32]) -> Self {
        Self {
            sid,
            k_recv,
            expected: 0,
            closed: None,
        }
    }

    /// Process one Data pipe_msg (spec §9.2, in order):
    /// 1. sid mismatch → `Ok(Dropped)` (silent, session intact).
    /// 2. `ctr ≥ 2^32` → `Limit` (range check precedes the equality
    ///    check).
    /// 3. `ctr != expected` → `Sequence` (gap or duplicate).
    /// 4. `aead_open` → `DecryptionError`.
    /// 5. bincode decode with limit `MAX_INNER` → `Protocol`.
    ///
    /// Any Err closes the session.
    pub fn open(&mut self, d: &crate::handshake::Data) -> Result<RecvOutcome> {
        if let Some(e) = self.closed {
            return Err(e);
        }
        if d.sid != self.sid {
            return Ok(RecvOutcome::Dropped);
        }
        if d.ctr >= constants::MAX_RECORDS {
            self.close(Error::Limit);
            return Err(Error::Limit);
        }
        if d.ctr != self.expected {
            self.close(Error::Sequence);
            return Err(Error::Sequence);
        }
        let aad = record_aad(&d.sid, d.ctr);
        let pt = match v1::aead_open(&self.k_recv, &record_nonce(d.ctr), &aad, &d.ct) {
            Ok(pt) => pt,
            Err(_) => {
                self.close(Error::DecryptionError);
                return Err(Error::DecryptionError);
            }
        };
        let inner = decode_inner(&pt).inspect_err(|&e| self.close(e))?;
        self.expected += 1;
        Ok(RecvOutcome::Inner(inner))
    }

    /// True once any §9.2 failure closed the session.
    pub fn closed(&self) -> bool {
        self.closed.is_some()
    }

    fn close(&mut self, e: Error) {
        self.closed = Some(e);
    }
}

// ---------------------------------------------------------------------------
// Inner plaintext (spec §9.4)
// ---------------------------------------------------------------------------

/// Inner stream message (spec §9.4): bincode 2, `config::standard()`,
/// limit `MAX_INNER`. Variant index = declaration order.
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub struct Inner {
    pub stream: u32,
    pub msg: StreamMsg,
}

/// Stream messages (spec §9.4).
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum StreamMsg {
    /// Open a stream (id > 0, strictly increasing, initiator only).
    Open(Op),
    /// Existing JSON wire frame (Attach streams only).
    Frame(String),
    /// File chunk, len ≤ MAX_CHUNK.
    Chunk(Vec<u8>),
    /// Response head for non-Attach ops.
    Head {
        status: u16,
        content_type: String,
        length: Option<u64>,
    },
    /// End this direction.
    End,
    /// Reset the stream with an error code.
    Reset(u16),
}

/// The six allowed operations (spec §9.4); nothing else is expressible.
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum Op {
    Attach,
    Profiles,
    List,
    /// Kill a run: UUID, RFC 4122 byte order.
    Kill {
        id: [u8; 16],
    },
    Get(GetTarget),
    Upload {
        convo: [u8; 16],
        name: String,
        content_type: String,
    },
}

/// Get targets (spec §9.4).
#[derive(Debug, Clone, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum GetTarget {
    Image { id: String },
    Upload { convo: [u8; 16], id: String },
}

/// Encode an `Inner` with bincode standard config (spec §9.4).
pub fn encode_inner(inner: &Inner) -> Vec<u8> {
    // In-memory encoding only fails on allocation failure; the empty frame
    // then fails authentication on the receiver.
    bincode::encode_to_vec(inner, bincode::config::standard()).unwrap_or_default()
}

/// Decode an `Inner` with the `MAX_INNER` limit; a decoding error is a
/// `Protocol` abort (spec §9.2 rule 4).
pub fn decode_inner(bytes: &[u8]) -> Result<Inner> {
    let (inner, _) = bincode::decode_from_slice(
        bytes,
        bincode::config::standard().with_limit::<{ constants::MAX_INNER }>(),
    )
    .map_err(|_| Error::Protocol)?;
    Ok(inner)
}

// ---------------------------------------------------------------------------
// Stream multiplexing (spec §9.5)
// ---------------------------------------------------------------------------

/// Which side of the session the mux belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Role {
    #[default]
    Initiator,
    Responder,
}

/// What a fed message caused the peer to observe / answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxEvent {
    /// A stream fully closed (both directions ended, spec S3).
    Closed(u32),
    /// A Reset was received for the stream.
    Reset(u32, u16),
    /// S2: the open-stream cap was hit; answer this Open with
    /// `Reset(TooLarge)`.
    AnswerReset(u32, u16),
}

/// Per-stream state as seen by one side (spec §9.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Open {
        attach: bool,
        /// Head sent on this (non-Attach) stream; at most one (S4).
        headed: bool,
    },
    /// One direction ended (S3/S4): the second End closes the stream.
    Ended {
        attach: bool,
    },
    Closed,
    Reset,
}

/// The stream table for one session side (spec §9.5, S1–S5).
#[derive(Debug, Default)]
pub struct Mux {
    role: Role,
    streams: BTreeMap<u32, StreamState>,
    /// Highest stream id ever opened (S1: strictly increasing).
    max_opened: u32,
    /// Streams not yet closed/reset (S2 cap).
    open_count: usize,
}

impl Mux {
    /// New mux for a session side.
    pub fn new(role: Role) -> Self {
        Self {
            role,
            ..Self::default()
        }
    }

    /// Feed one inner message; returns the events it caused. Enforces:
    /// - S1: id 0 reserved; only the initiator sends `Open` (an Open on
    ///   the responder side is a `Protocol` abort); each Open uses an id
    ///   strictly greater than every previously opened id.
    /// - S2: at most `MAX_STREAMS` open at once; a further Open answers
    ///   `Reset(TooLarge)` (event, not abort).
    /// - S3: Attach: `Frame` both ways, `End` per direction, closed when
    ///   both ended; `Chunk`/`Head` on Attach → `Protocol`.
    /// - S4: other ops: initiator `Open, Chunk*, End`; responder
    ///   `Head, Chunk*, End`; out of sequence or `Frame` → `Protocol`.
    /// - S5: `Reset` ends a stream both ways; messages for a reset/closed
    ///   id are dropped silently (no event); a message for an id that was
    ///   never opened → `Protocol`.
    /// - `Chunk` len > `MAX_CHUNK` → `Protocol`.
    pub fn feed(&mut self, inner: &Inner) -> Result<Vec<MuxEvent>> {
        let id = inner.stream;
        let mut events = Vec::new();
        if id == 0 {
            return Err(Error::Protocol);
        }
        if let StreamMsg::Open(op) = &inner.msg {
            if self.streams.contains_key(&id) {
                return Err(Error::Protocol);
            }
            // S1: only the initiator sends Open.
            if self.role == Role::Responder {
                return Err(Error::Protocol);
            }
            if id <= self.max_opened {
                return Err(Error::Protocol);
            }
            if self.open_count >= constants::MAX_STREAMS as usize {
                events.push(MuxEvent::AnswerReset(id, constants::reset::TOO_LARGE));
                return Ok(events);
            }
            self.streams.insert(
                id,
                StreamState::Open {
                    attach: matches!(op, Op::Attach),
                    headed: false,
                },
            );
            self.max_opened = id;
            self.open_count += 1;
            return Ok(events);
        }
        // Any other message needs an opened stream (S5).
        let state = *self.streams.get(&id).ok_or(Error::Protocol)?;
        match state {
            StreamState::Closed | StreamState::Reset => {
                // S5: in-flight messages are dropped silently.
                Ok(events)
            }
            StreamState::Open { attach, headed } => match &inner.msg {
                StreamMsg::Frame(_) => {
                    if !attach {
                        return Err(Error::Protocol);
                    }
                    Ok(events)
                }
                StreamMsg::Chunk(c) => {
                    if attach || c.len() > constants::MAX_CHUNK {
                        return Err(Error::Protocol);
                    }
                    Ok(events)
                }
                StreamMsg::Head { .. } => {
                    if attach || headed {
                        return Err(Error::Protocol);
                    }
                    self.streams.insert(
                        id,
                        StreamState::Open {
                            attach,
                            headed: true,
                        },
                    );
                    Ok(events)
                }
                StreamMsg::End => {
                    self.streams.insert(id, StreamState::Ended { attach });
                    Ok(events)
                }
                StreamMsg::Reset(code) => {
                    self.streams.insert(id, StreamState::Reset);
                    self.open_count -= 1;
                    events.push(MuxEvent::Reset(id, normalize_reset(*code)));
                    Ok(events)
                }
                StreamMsg::Open(_) => Err(Error::Protocol),
            },
            StreamState::Ended { attach } => match &inner.msg {
                StreamMsg::Frame(_) => {
                    if !attach {
                        return Err(Error::Protocol);
                    }
                    Ok(events)
                }
                StreamMsg::Chunk(c) => {
                    if attach || c.len() > constants::MAX_CHUNK {
                        return Err(Error::Protocol);
                    }
                    Ok(events)
                }
                StreamMsg::End => {
                    // Both directions ended (S3/S4).
                    self.streams.insert(id, StreamState::Closed);
                    self.open_count -= 1;
                    events.push(MuxEvent::Closed(id));
                    Ok(events)
                }
                StreamMsg::Reset(code) => {
                    self.streams.insert(id, StreamState::Reset);
                    self.open_count -= 1;
                    events.push(MuxEvent::Reset(id, normalize_reset(*code)));
                    Ok(events)
                }
                StreamMsg::Head { .. } => Err(Error::Protocol),
                StreamMsg::Open(_) => Err(Error::Protocol),
            },
        }
    }
}

/// Map an unknown reset code to `Internal` (spec §9.4: unknown codes are
/// treated as Internal).
pub fn normalize_reset(code: u16) -> u16 {
    match code {
        constants::reset::UNSUPPORTED
        | constants::reset::NOT_FOUND
        | constants::reset::TOO_LARGE
        | constants::reset::INTERNAL
        | constants::reset::CANCELLED
        | constants::reset::PROTOCOL => code,
        _ => constants::reset::INTERNAL,
    }
}
