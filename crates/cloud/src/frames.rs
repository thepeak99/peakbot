//! Data frames: record protection (spec §9.1–§9.3), inner stream
//! messages (spec §9.4), stream multiplexing rules (spec §9.5), session
//! termination limits (spec §9.6).
//!
//! ```text
//! nonce = 0^16 ‖ u64(ctr)                     (24 bytes, XChaCha20)
//! aad   = u8(1) ‖ u8(4) ‖ fix[16](sid) ‖ u64(ctr)   (26 bytes)
//! ct    = aead_seal(k_send, nonce, aad, inner)      (A3)
//! ```

use crate::{Result, SessionId};

// ---------------------------------------------------------------------------
// Record protection (spec §9.1)
// ---------------------------------------------------------------------------

/// The 24-byte XChaCha20 nonce: `0^16 ‖ u64(ctr)` (spec §9.1).
pub fn record_nonce(ctr: u64) -> [u8; 24] {
    todo!()
}

/// The 26-byte AAD: `u8(1) ‖ u8(4) ‖ fix[16](sid) ‖ u64(ctr)` — the Data
/// pipe_msg prefix before `bytes(ct)` (spec §9.1).
pub fn record_aad(sid: &SessionId, ctr: u64) -> Vec<u8> {
    todo!()
}

/// True while `ctr` may still be sent: `ctr < MAX_RECORDS` (2^32)
/// (spec §9.6). The sender MUST NOT send `ctr = 2^32`.
pub fn ctr_allowed(ctr: u64) -> bool {
    todo!()
}

/// True once the session is past `SESSION_LIFETIME` (24 h) from
/// `established_at`, with checked arithmetic (spec §1.5, §9.6): an
/// underflow (now < established_at) is out of range, never a panic.
pub fn session_expired(established_at: u64, now: u64) -> bool {
    todo!()
}

// ---------------------------------------------------------------------------
// Sender / receiver (spec §9.2)
// ---------------------------------------------------------------------------

/// A data-frame sender (one direction of an established session).
/// `ctr` starts at 0 and increases by exactly 1 per frame.
#[derive(Debug)]
pub struct FrameSender {
    _private: (),
}

impl FrameSender {
    /// New sender for `sid` with the direction's send key.
    pub fn new(sid: SessionId, k_send: [u8; 32]) -> Self {
        todo!()
    }

    /// Seal `inner` (bincode plaintext, spec §9.4) into a Data pipe_msg
    /// with the next counter. At the `MAX_RECORDS` limit the sender must
    /// not send: `Error::Limit` (the caller closes with reason `Limit`).
    pub fn seal(&mut self, inner: &[u8]) -> Result<crate::handshake::Data> {
        todo!()
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
    _private: (),
}

impl FrameReceiver {
    /// New receiver for `sid` with the direction's receive key.
    pub fn new(sid: SessionId, k_recv: [u8; 32]) -> Self {
        todo!()
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
        todo!()
    }

    /// True once any §9.2 failure closed the session.
    pub fn closed(&self) -> bool {
        todo!()
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
    todo!()
}

/// Decode an `Inner` with the `MAX_INNER` limit; a decoding error is a
/// `Protocol` abort (spec §9.2 rule 4).
pub fn decode_inner(bytes: &[u8]) -> Result<Inner> {
    todo!()
}

// ---------------------------------------------------------------------------
// Stream multiplexing (spec §9.5)
// ---------------------------------------------------------------------------

/// Which side of the session the mux belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
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

/// The stream table for one session side (spec §9.5, S1–S5).
#[derive(Debug, Default)]
pub struct Mux {
    _private: (),
}

impl Mux {
    /// New mux for a session side.
    pub fn new(role: Role) -> Self {
        todo!()
    }

    /// Feed one inner message; returns the events it caused. Enforces:
    /// - S1: id 0 reserved; only the initiator sends `Open`; each Open
    ///   uses an id strictly greater than every previously opened id.
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
        todo!()
    }
}

/// Map an unknown reset code to `Internal` (spec §9.4: unknown codes are
/// treated as Internal).
pub fn normalize_reset(code: u16) -> u16 {
    todo!()
}
