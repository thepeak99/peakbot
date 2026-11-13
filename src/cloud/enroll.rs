//! Enrollment logic (p3-design §G.2–G.6): genesis, `EnrollRequest` with
//! FROZEN head (§5.5), approver, reject, revoke, logout+wipe. Called only
//! from the CloudLink actor; the web API (H.1) is the user surface.
//!
//! The SAS rule (spec §5.5, G.3 step 1): the head is captured BEFORE
//! anything is shown — `frozen = chain.head()` — and the words are
//! `sas::words(own_card, &frozen)`; the letter ships `frozen`, and the
//! approver recomputes from `letter.head`. A letter whose head is not in
//! our verified history is dropped (G.4 step 2).

use cloud::{
    Head,
    card::DeviceCard,
    chain::{ChainEntry, VerifiedChain},
    letter::{AcceptedLetter, LetterBody},
    sas,
};
use perma_crypto::v1;
use rand_core::CryptoRng;

use super::link::EnrollRequestInfo;

/// The genesis entry (seq 0, prev `[0;32]`, `ChainOp::Genesis`, own card)
/// for an empty account (G.2 step 3).
pub fn genesis_entry<R: CryptoRng>(
    card: &DeviceCard,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> cloud::Result<ChainEntry> {
    todo!("p3 PR-B: genesis_entry")
}

/// Turn an accepted `EnrollRequest` letter into approver state (G.4 step 2):
/// recompute the SAS **from the letter's head**; a head that is not in our
/// verified history (fork/stale/foreign) → `None` (drop, logged at debug).
pub fn request_info(
    chain: &VerifiedChain,
    accepted: &AcceptedLetter,
    now: u64,
) -> Option<EnrollRequestInfo> {
    todo!("p3 PR-B: request_info (frozen-head check + SAS recompute)")
}

/// The `Add` entry at `head+1` for the letter's card (G.4 step 3).
pub fn approve_entry<R: CryptoRng>(
    chain: &VerifiedChain,
    card: &[u8],
    signer: DeviceCard,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> cloud::Result<ChainEntry> {
    todo!("p3 PR-B: approve_entry")
}

/// The `EnrollReject` letter body (G.4 step 4).
pub fn reject_letter(
    now: u64,
    from: cloud::DeviceId,
    to: cloud::DeviceId,
) -> LetterBody {
    todo!("p3 PR-B: reject_letter")
}

/// The `Revoke` entry at `head+1` for `target` (G.5 step 1).
pub fn revoke_entry<R: CryptoRng>(
    chain: &VerifiedChain,
    target: cloud::DeviceId,
    signer: DeviceCard,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> cloud::Result<ChainEntry> {
    todo!("p3 PR-B: revoke_entry")
}

/// The SAS words for a pending device's card at the frozen head (G.3 step 1).
pub fn sas_for(card: &[u8], frozen: &Head) -> sas::SasWords {
    todo!("p3 PR-B: sas_for")
}
