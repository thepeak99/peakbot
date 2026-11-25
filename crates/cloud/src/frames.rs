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

pub use crate::framing::{CreditLedger, FragmentAssembler, ReassemblyBudget};

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

/// True once a pending handshake created at `created` is past
/// `HANDSHAKE_TIMEOUT` (30 s, spec §8.6), with the boundary inclusive as in
/// [`session_expired`]. Saturating subtraction keeps clock skew
/// (`now < created`) a non-expired, non-panicking case.
pub fn handshake_expired(created: u64, now: u64) -> bool {
    now.saturating_sub(created) >= constants::HANDSHAKE_TIMEOUT
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
    /// Open a stream (id > 0, strictly increasing within the opener's
    /// namespace; either role may open).
    Open(Op),
    /// Existing JSON wire frame (Attach streams only). A message up to
    /// `FRAME_ONESHOT_MAX` rides here; larger ones are fragmented.
    Frame(String),
    /// File chunk, len ≤ MAX_CHUNK. On Attach, a fragment piece.
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
    /// Start one fragmented Attach message: the exact UTF-8 byte length the
    /// following `Chunk`s will carry.
    FrameBegin { len: u32 },
    /// Complete the fragmented Attach message currently in flight.
    FrameEnd,
    /// Receiver → sender window update: up to `bytes` more may be sent.
    Credit { bytes: u32 },
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
        /// Declared body length (the browser's Content-Length), when known.
        /// A value over the host cap fast-fails before any body is read; the
        /// actual byte count is still verified against it at End.
        length: Option<u64>,
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

/// First stream id minted by `role` (pipe v3, spec §9.5): the initiator
/// opens from 1 upward, the responder from 2^31 upward. The two ranges are
/// disjoint, so each side owns a namespace and both may open streams on
/// the same session.
pub const fn first_stream_id(role: Role) -> u32 {
    match role {
        Role::Initiator => 1,
        Role::Responder => 1 << 31,
    }
}

/// The namespace an id belongs to: ids below the responder base are the
/// initiator's, the rest are the responder's.
fn stream_namespace(id: u32) -> Role {
    if id >= first_stream_id(Role::Responder) {
        Role::Responder
    } else {
        Role::Initiator
    }
}

fn namespace_index(role: Role) -> usize {
    match role {
        Role::Initiator => 0,
        Role::Responder => 1,
    }
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
///
/// `End` is a HALF-close (S3/S4): each direction ends on its own and the
/// stream closes when both have. The mux is fed messages of both
/// directions without a direction tag, so direction is inferred: on a
/// non-Attach stream everything before `Head` belongs to the opening
/// side, hence a first `End` before `Head` is the request's End. This
/// works identically for streams opened by either role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Live {
        attach: bool,
        /// Head seen on this (non-Attach) stream; at most one (S4).
        headed: bool,
        /// The request direction (the opening side) has ended; known only
        /// when the End arrived before `Head`.
        req_ended: bool,
        /// Number of `End`s seen so far (0 or 1; the second closes).
        ends: u8,
    },
    Closed,
    Reset,
}

/// The stream table for one session side (spec §9.5, S1–S5).
///
/// Pipe v3 is duplex: both roles open streams, each in its own namespace
/// (`first_stream_id`). Because one mux sees its own locally fed messages
/// and the peer's inbound ones, Opens in either namespace are accepted and
/// ordering is enforced per namespace.
#[derive(Debug, Default)]
pub struct Mux {
    streams: BTreeMap<u32, StreamState>,
    /// Highest stream id ever opened in each namespace (S1: strictly
    /// increasing within a namespace; the namespaces are independent).
    max_opened: [u32; 2],
    /// Streams not yet closed/reset across both namespaces (S2 cap).
    open_count: usize,
}

impl Mux {
    /// New mux for a session side.
    ///
    /// The `role` parameter is retained for source compatibility only: the
    /// mux does not classify Opens by a stored role. The actor mints ids
    /// from `first_stream_id(role)`, and the namespace is derived from each
    /// Open's id, so both roles are accepted in both namespaces.
    pub fn new(_role: Role) -> Self {
        Self::default()
    }

    /// Feed one inner message; returns the events it caused. Enforces:
    /// - S1: id 0 reserved; either role may `Open`, but each Open must use
    ///   an id strictly greater than every previously opened id in ITS
    ///   namespace (initiator ids from [`first_stream_id`]`(Initiator)`,
    ///   responder ids from [`first_stream_id`]`(Responder)`). The two
    ///   namespaces are ordered independently.
    /// - S2: at most `MAX_STREAMS` open at once across both namespaces; a
    ///   further Open answers `Reset(TooLarge)` (event, not abort).
    /// - S3: Attach: `Frame`, the v2 `FrameBegin`/`Chunk*`/`FrameEnd`
    ///   fragment grammar, `Credit`, and `End` per direction; closed when
    ///   both ended. `Head` → `Protocol`. The mux is direction-agnostic, so
    ///   the endpoint assemblers own per-direction fragment ordering.
    /// - S4: other ops: opener `Open, Chunk*, End`; peer `Head, Chunk*,
    ///   End`; `End` is a HALF-close (the stream closes on the second End);
    ///   out of sequence, `Frame`, or a v2 Attach-only record → `Protocol`.
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
            // S1: an id must be strictly increasing within its own
            // namespace. Both roles open, so no role check applies here;
            // the locally fed and inbound messages share this table.
            let ns = namespace_index(stream_namespace(id));
            if id <= self.max_opened[ns] {
                return Err(Error::Protocol);
            }
            if self.open_count >= constants::MAX_STREAMS as usize {
                events.push(MuxEvent::AnswerReset(id, constants::reset::TOO_LARGE));
                return Ok(events);
            }
            self.streams.insert(
                id,
                StreamState::Live {
                    attach: matches!(op, Op::Attach),
                    headed: false,
                    req_ended: false,
                    ends: 0,
                },
            );
            self.max_opened[ns] = id;
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
            StreamState::Live {
                attach,
                headed,
                req_ended,
                ends,
            } => {
                let live = |headed, req_ended, ends| StreamState::Live {
                    attach,
                    headed,
                    req_ended,
                    ends,
                };
                match &inner.msg {
                    StreamMsg::Frame(_) => {
                        if !attach {
                            return Err(Error::Protocol);
                        }
                        Ok(events)
                    }
                    StreamMsg::Chunk(c) => {
                        if c.len() > constants::MAX_CHUNK {
                            return Err(Error::Protocol);
                        }
                        if attach {
                            // v2 Attach fragment piece; ordering lives in the
                            // endpoint assembler (the mux sees both directions).
                            return Ok(events);
                        }
                        // Before Head only the request can send; after its
                        // End a Chunk is out of sequence (S4).
                        if !headed && req_ended {
                            return Err(Error::Protocol);
                        }
                        Ok(events)
                    }
                    StreamMsg::FrameBegin { .. } | StreamMsg::FrameEnd => {
                        if !attach {
                            return Err(Error::Protocol);
                        }
                        Ok(events)
                    }
                    // Credit is legal on every live stream: live bulk ops
                    // (Upload/Get bodies, Head/Chunk replies) share the one
                    // per-direction window with Attach.
                    StreamMsg::Credit { .. } => Ok(events),
                    StreamMsg::Head { .. } => {
                        if attach || headed {
                            return Err(Error::Protocol);
                        }
                        self.streams.insert(id, live(true, req_ended, ends));
                        Ok(events)
                    }
                    StreamMsg::End => {
                        if !attach && !headed {
                            // Only the request can End before Head; a
                            // second End without a Head is out of sequence.
                            if req_ended {
                                return Err(Error::Protocol);
                            }
                            self.streams.insert(id, live(false, true, 1));
                            return Ok(events);
                        }
                        if ends >= 1 {
                            // Both directions ended (S3/S4).
                            self.streams.insert(id, StreamState::Closed);
                            self.open_count -= 1;
                            events.push(MuxEvent::Closed(id));
                        } else {
                            self.streams.insert(id, live(headed, req_ended, 1));
                        }
                        Ok(events)
                    }
                    StreamMsg::Reset(code) => {
                        self.streams.insert(id, StreamState::Reset);
                        self.open_count -= 1;
                        events.push(MuxEvent::Reset(id, normalize_reset(*code)));
                        Ok(events)
                    }
                    StreamMsg::Open(_) => Err(Error::Protocol),
                }
            }
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
