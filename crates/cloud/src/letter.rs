//! Enrollment letters (spec §7): kinds, SignedLetter CE, seal/open,
//! recipient checks (silent-drop order), replay set.
//!
//! CE layout (spec §7.2):
//! ```text
//! letter_body   = u8(1) ‖ fix[16](letter_id) ‖ fix[32](from) ‖ fix[32](to)
//!              ‖ u64(created_at) ‖ u8(kind) ‖ kind_body
//! kind_body     = bytes(card) ‖ head(head_N) ‖ u8(has_mac) ‖ [fix[32](mac)]
//! signed_letter = letter_body ‖ bytes(sig)   (len == 3309)
//! ```

use perma_crypto::v1;

use crate::Head;
use crate::chain::VerifiedChain;
use crate::{DeviceId, LetterId, Result};

/// Letter kinds (spec §7.1). CE tags: 1 = EnrollRequest, 2 = EnrollReject.
/// There are no other kinds (design D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetterKind {
    /// Sender pending (card not in chain) → recipient active.
    EnrollRequest,
    /// Sender active → recipient pending.
    EnrollReject,
}

/// The kind-specific body (spec §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetterKindBody {
    EnrollRequest {
        /// CE card bytes of the new device (≤ 3280).
        card: Vec<u8>,
        /// N's head (the frozen one, spec §5.5).
        head_n: Head,
        has_mac: bool,
        /// The QR MAC iff `has_mac` (spec §6.3).
        mac: Option<[u8; 32]>,
    },
    /// Empty body (spec §7.1).
    EnrollReject,
}

/// A parsed letter body (spec §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetterBody {
    pub letter_id: LetterId,
    pub from: DeviceId,
    pub to: DeviceId,
    /// Sender's `now`, Unix seconds.
    pub created_at: u64,
    pub kind: LetterKind,
    pub body: LetterKindBody,
}

impl LetterBody {
    /// Parse CE letter-body bytes (version must be 1; unknown kind tags are
    /// `Malformed`, P3).
    pub fn parse(raw: &[u8]) -> Result<Self> {
        todo!()
    }

    /// Re-encode to CE bytes.
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }
}

/// `signed_letter = letter_body ‖ bytes(sig)` where
/// `sig = Sign_from("pbc1 letter sig"; letter_body)` (spec §7.2).
/// Uses `sign_with_rng` (A5) for reproducible vectors.
pub fn signed_letter<R: rand_core::CryptoRng>(
    body: &LetterBody,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> Result<Vec<u8>> {
    todo!()
}

/// Parse a signed letter, returning the body and the raw signature.
pub fn parse_signed_letter(raw: &[u8]) -> Result<(LetterBody, Vec<u8>)> {
    todo!()
}

/// The seal AAD (spec §7.3):
/// `str("pbc1 letter aad") ‖ fix[16](letter_id) ‖ fix[32](from) ‖ fix[32](to)`.
/// Binds the relay-visible routing fields so the relay cannot re-route a
/// sealed letter.
pub fn letter_aad(letter_id: LetterId, from: DeviceId, to: DeviceId) -> Vec<u8> {
    todo!()
}

/// One-shot seal to the recipient's LONG-TERM card KEM key:
/// `v1::seal_with_rng(to_pk, signed_letter, aad, rng)` (spec §7.3, A6).
pub fn seal_letter<R: rand_core::CryptoRng>(
    to_pk: &v1::PublicKey,
    signed: &[u8],
    aad: &[u8],
    rng: &mut R,
) -> Result<Vec<u8>> {
    todo!()
}

/// Open a sealed letter: `v1::open(self_sk, aad, sealed)`. An unknown seal
/// version is `Error::UnknownVersion`; any other failure is
/// `Error::DecryptionError` (spec §7.5 step 2).
pub fn open_letter(sk: &v1::PrivateKey, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    todo!()
}

/// The recipient's state (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientState {
    Active,
    Pending,
}

/// In-memory replay set (spec §7.5 step 5). Entries expire after
/// `SEEN_RETENTION` (20 min = TTL + skew).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaySet {
    /// (letter_id, inserted_at)
    entries: Vec<(LetterId, u64)>,
}

impl ReplaySet {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `id` is in the set and not expired at `now`.
    pub fn contains(&self, id: LetterId, now: u64) -> bool {
        todo!()
    }

    /// Record `id` as seen at `now`.
    pub fn insert(&mut self, id: LetterId, now: u64) {
        todo!()
    }

    /// Drop entries older than `SEEN_RETENTION` at `now` (checked
    /// arithmetic).
    pub fn prune(&mut self, now: u64) {
        todo!()
    }
}

/// Everything the recipient check needs (spec §7.5). Pure: no I/O, `now`
/// and the chain view are parameters.
pub struct LetterIn<'a> {
    /// The sealed blob from the relay's `Letter` message.
    pub sealed: Vec<u8>,
    /// Relay-visible `from`.
    pub relay_from: DeviceId,
    /// Relay-visible `letter_id`.
    pub relay_letter_id: LetterId,
    /// The recipient's own DeviceId.
    pub self_id: DeviceId,
    /// The recipient's long-term private key (for `v1::open`).
    pub self_sk: &'a v1::PrivateKey,
    /// The recipient's state (active or pending).
    pub self_state: RecipientState,
    /// Current Unix time in seconds (spec §1.5).
    pub now: u64,
    /// The recipient's verified chain view (frozen-attempt for pending).
    pub chain: &'a VerifiedChain,
    /// The recipient's replay set. Inserted into on success (step 8).
    pub seen: &'a mut ReplaySet,
}

/// What to do with an accepted letter (spec §7.5 step 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// EnrollRequest with has_mac = 0 → SAS flow (§5.4).
    Sas,
    /// EnrollRequest with has_mac = 1 → QR flow (§6.4).
    Qr,
    /// EnrollReject → ends N's attempt.
    Reject,
}

/// An accepted letter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedLetter {
    pub body: LetterBody,
    pub dispatch: Dispatch,
}

/// The recipient checks, in the spec's exact order (spec §7.5):
/// 1. `len(sealed) ≤ MAX_LETTER` → `LetterTooLarge`.
/// 2. `v1::open(self_sk, aad(relay_letter_id, relay_from, self), sealed)`
///    → `UnknownVersion` / `DecryptionError`.
/// 3. Parse; `version == 1`; kind allowed for the recipient state →
///    `KindNotAllowed`.
/// 4. `letter_id`/`from`/`to` agree with the relay fields and self →
///    `IdMismatch`.
/// 5. Replay set → `Replayed`.
/// 6. Time, checked arithmetic: `created_at ≤ now + SKEW` → `Future`;
///    `now ≤ created_at + TTL_LETTER + SKEW` → `Expired`.
/// 7. Sender per kind:
///    - EnrollRequest: `from == DeviceId(card)`, card parses,
///      `from ∉ active ∪ revoked` → `SenderNotPending`; then
///      `Verify_card("pbc1 letter sig"; body, sig)` → `BadSignature`.
///    - EnrollReject: `from ∈ active` → `SenderNotActive`; then verify
///      under the active card's key → `BadSignature`.
/// 8. Insert `letter_id` into `seen` (the caller then sends `Ack`).
///
/// Returns the accepted letter and its dispatch. Any failure is a silent
/// drop plus a local log of the named reason (spec §7.5).
pub fn check_letter(in_: &mut LetterIn) -> Result<AcceptedLetter> {
    todo!()
}
