//! One error type for the whole crate: every named abort in the spec is a
//! variant. "Abort" (spec preamble) means: stop processing, discard state,
//! zeroize secrets, report the named error.

/// All pbc1 aborts (spec §§4, 6, 7, 8, 9, 10, 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    // -- parsing / versioning (§1.2, §11) --
    /// A CE structure failed to parse: P1–P4, per-field limits, sizes.
    #[error("malformed structure")]
    Malformed,
    /// A version byte that is not 1 (spec V1: exact match, no negotiation).
    #[error("unknown version")]
    UnknownVersion,
    /// An algorithm id that is not the v1 literal (spec §2, V4).
    #[error("unsupported algorithm")]
    UnsupportedAlg,
    /// A key that fails validation (spec §4.1, §8.6 step 4).
    #[error("malformed key")]
    MalformedKey,

    // -- device chain (§4.4) --
    /// Entry seq is not `expected_seq`.
    #[error("sequence gap")]
    Gap,
    /// C1: genesis violations (non-Genesis op, non-zero prev, non-self-signed).
    #[error("bad genesis")]
    BadGenesis,
    /// C2: `prev` does not link to the current head (fork F1).
    #[error("bad prev hash")]
    BadPrev,
    /// Entry signature does not verify under the required key.
    #[error("bad signature")]
    BadSignature,
    /// C3: signer was not active at seq-1.
    #[error("signer not active")]
    SignerNotActive,
    /// C4: Add of an id already active or revoked.
    #[error("re-add of known device")]
    ReAdd,
    /// C5: Revoke of an id that is not active.
    #[error("bad revoke")]
    BadRevoke,
    /// C6: the last active device cannot revoke itself.
    #[error("last device")]
    LastDevice,
    /// §4.5: genesis hash differs from the pinned value (never auto-adopt).
    #[error("genesis mismatch")]
    GenesisMismatch,
    /// §4.6: more than MAX_CHAIN_ENTRIES.
    #[error("chain too long")]
    TooLong,

    // -- trust / heads (§4.6, §8.5) --
    /// Fork or rollback detected (F2/F3/F4): the account is untrusted.
    #[error("trust: fork or rollback")]
    Trust,
    /// The relay cannot serve up to the peer head (F3, retry later).
    #[error("stale chain")]
    StaleChain,
    /// Peer or self is not active in the verified chain (§8.5 step 3).
    #[error("not active")]
    NotActive,
    /// Pipe routing field mismatch (§8.6 step 2).
    #[error("misrouted")]
    Misrouted,

    // -- QR/link enrollment (§6) --
    /// Link grammar violation (§6.1).
    #[error("bad link")]
    BadLink,
    /// MAC over the enrollment data does not verify (§6.3/§6.4).
    #[error("bad mac")]
    BadMac,
    /// No outstanding, unexpired, unused QR secret (§6.4).
    #[error("mac unexpected")]
    MacUnexpected,

    // -- letters (§7) --
    /// Sealed blob larger than MAX_LETTER (§7.5 step 1).
    #[error("letter too large")]
    LetterTooLarge,
    /// `v1::open` failed on a seal of the right version (§7.5 step 2).
    #[error("decryption error")]
    DecryptionError,
    /// Letter kind not allowed for the recipient's state (§7.1/§7.5 step 3).
    #[error("letter kind not allowed for state")]
    KindNotAllowed,
    /// letter_id / from / to disagree between the relay fields and the body
    /// (§7.5 step 4).
    #[error("letter routing mismatch")]
    IdMismatch,
    /// letter_id already in the replay set (§7.5 step 5).
    #[error("replayed letter")]
    Replayed,
    /// Letter older than TTL + skew (§7.5 step 6).
    #[error("letter expired")]
    Expired,
    /// Letter from the future beyond skew (§7.5 step 6).
    #[error("letter from the future")]
    Future,
    /// EnrollRequest sender is already active or revoked (§7.5 step 7).
    #[error("enroll request sender not pending")]
    SenderNotPending,
    /// EnrollReject sender is not active (§7.5 step 7).
    #[error("reject sender not active")]
    SenderNotActive,

    // -- data frames / sessions (§9) --
    /// Counter gap or duplicate (§9.2 rule 2); the session closes.
    #[error("sequence error")]
    Sequence,
    /// Stream-rule violation (S1–S5) or inner decode failure.
    #[error("protocol violation")]
    Protocol,
    /// A limit was reached (MAX_RECORDS, session lifetime, envelope size).
    #[error("limit reached")]
    Limit,
    /// Chain change made peer or self not active (§9.6).
    #[error("revoked")]
    Revoked,
    /// The sender tried to write more bytes than its credit window allows.
    #[error("credit exhausted")]
    Credit,
    /// Handshake timer expired (§8.6).
    #[error("timeout")]
    Timeout,
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
