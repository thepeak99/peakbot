//! Protocol constants (spec §12). All compile-time, no configuration.

/// Protocol version 1 (spec §11): exact match, no negotiation.
pub const PROTOCOL_VERSION: u8 = 1;
/// Pipe protocol version byte (spec §8.2): Transport v2 (fragmentation +
/// credit). Exact match, no v1 fallback.
pub const PIPE_VERSION: u8 = 2;

/// Letter TTL (spec §7.5 step 6, §12).
pub const TTL_LETTER: u64 = 900;
/// Clock skew allowance (spec §7.5 step 6, §12).
pub const SKEW: u64 = 300;
/// Replay-set retention: TTL + skew (spec §7.5 step 5, §12).
pub const SEEN_RETENTION: u64 = 1200;
/// Max sealed letter blob (spec §7.4, product-owner approved 8 KiB).
pub const MAX_LETTER: usize = 8192;
/// Max pending letters per recipient (spec §7.4).
pub const MAX_PENDING_LETTERS: usize = 8;
/// Max pending devices per account (spec §5.5 R4).
pub const MAX_PENDING_DEVICES: usize = 4;
/// QR secret lifetime, single use (spec §6.2, §12).
pub const QR_SECRET_TTL: u64 = 900;
/// Hello window after Challenge (spec §10.1).
pub const HELLO_WINDOW: u64 = 30;
/// Handshake timer (spec §8.6).
pub const HANDSHAKE_TIMEOUT: u64 = 30;
/// Chain cap (spec §4.6).
pub const MAX_CHAIN_ENTRIES: usize = 65_536;
/// Entries per `Chain` relay message (spec §10.1, §12).
pub const MAX_CHAIN_BATCH: usize = 256;
/// bincode limit for the inner stream plaintext (spec §9.4, §12).
pub const MAX_INNER: usize = 1_048_576;
/// Max stream chunk (spec §9.4, §12).
pub const MAX_CHUNK: usize = 65_536;
/// Largest logical Attach message, in UTF-8 bytes. Also the per-session
/// aggregate reassembly budget.
pub const MAX_LOGICAL_MESSAGE: usize = 64 * 1024 * 1024;
/// A Frame up to this size rides in one `StreamMsg::Frame`; larger messages
/// use `FrameBegin`/`Chunk*`/`FrameEnd`. Must fit one credit window and
/// `MAX_INNER` with room for bincode framing.
pub const FRAME_ONESHOT_MAX: usize = 256 * 1024;
/// Per-direction Attach receive window, in bytes (initial = max).
pub const CREDIT_INITIAL: u32 = 512 * 1024;
pub const CREDIT_MAX: u32 = 512 * 1024;
/// The receiver returns credit every time this many bytes are consumed.
pub const CREDIT_GRANT: u32 = 256 * 1024;
/// Sender stall budget: how long a bulk sender may wait for the peer's
/// credit before the stream is abandoned. The clock restarts for every
/// message, so a steadily progressing transfer is never capped.
pub const CREDIT_STALL_SECS: u64 = 60;
/// Receiver stall budget: how long a bulk receiver may wait for the next
/// data record before the stream is abandoned. Idle time between records,
/// not total transfer duration.
pub const RECV_STALL_SECS: u64 = 120;
/// Max concurrently open streams (spec §9.5 S2).
pub const MAX_STREAMS: u32 = 64;
/// Per-direction record cap (spec §9.6): sender must not send ctr = 2^32.
pub const MAX_RECORDS: u64 = 0x1_0000_0000; // 2^32 (spec §9.6)
/// Session lifetime in seconds (spec §9.6).
pub const SESSION_LIFETIME: u64 = 86_400;
/// bincode decode limit for a Pipe-carrying envelope (spec §12).
pub const MAX_ENVELOPE_PIPE: usize = MAX_INNER + 4096;
/// bincode decode limit for a Chain-carrying envelope (spec §12).
pub const MAX_ENVELOPE_CHAIN: usize = 6671 * MAX_CHAIN_BATCH + 4096;

/// Close reason codes (spec §9.3). Informational only, never used for
/// decisions.
pub mod close_reason {
    pub const NORMAL: u8 = 0;
    pub const PROTOCOL: u8 = 1;
    pub const SEQUENCE: u8 = 2;
    pub const DECRYPTION_ERROR: u8 = 3;
    pub const LIMIT: u8 = 4;
    pub const REVOKED: u8 = 5;
    pub const TRUST: u8 = 6;
    pub const UNKNOWN_VERSION: u8 = 7;
    pub const REPLACED: u8 = 8;
    pub const TIMEOUT: u8 = 9;
    pub const MISROUTED: u8 = 10;
}

/// Stream reset codes (spec §9.4). Unknown codes are treated as Internal.
pub mod reset {
    pub const UNSUPPORTED: u16 = 1;
    pub const NOT_FOUND: u16 = 2;
    pub const TOO_LARGE: u16 = 3;
    pub const INTERNAL: u16 = 4;
    pub const CANCELLED: u16 = 5;
    pub const PROTOCOL: u16 = 6;
}

/// Relay error codes (spec §10.1).
pub mod relay_error {
    pub const UNSUPPORTED_VERSION: u16 = 1;
    pub const UNAUTHORIZED: u16 = 2;
    pub const BAD_HELLO: u16 = 3;
    pub const NOT_PERMITTED: u16 = 4;
    pub const CONFLICT: u16 = 5;
    pub const INVALID_ENTRY: u16 = 6;
    pub const MAILBOX_FULL: u16 = 7;
    pub const TOO_MANY_PENDING: u16 = 8;
    pub const TOO_LARGE: u16 = 9;
    pub const RATE_LIMITED: u16 = 10;
}
