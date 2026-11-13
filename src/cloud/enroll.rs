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
    let entry = cloud::chain::build_entry(
        0,
        [0; 32],
        cloud::chain::ChainOp::Genesis,
        Some(card.encode()),
        None,
        card.device_id(),
        sk,
        rng,
    )?;
    Ok(entry)
}

/// Turn an accepted `EnrollRequest` letter into approver state (G.4 step 2):
/// recompute the SAS **from the letter's head**; a head that is not in our
/// verified history (fork/stale/foreign) → `None` (drop, logged at debug).
pub fn request_info(
    chain: &VerifiedChain,
    accepted: &AcceptedLetter,
    now: u64,
) -> Option<EnrollRequestInfo> {
    let cloud::letter::LetterKindBody::EnrollRequest { card, head_n, .. } = &accepted.body.body
    else {
        return None;
    };
    // The letter's head must be in our verified history (G.4 step 2).
    if chain.hashes.get(head_n.seq as usize) != Some(&head_n.hash) {
        return None;
    }
    let parsed = DeviceCard::parse(card).ok()?;
    let expires_at = accepted
        .body
        .created_at
        .saturating_add(cloud::constants::TTL_LETTER);
    // An already-expired request is not worth showing (G.4 step 5).
    if expires_at <= now {
        return None;
    }
    Some(EnrollRequestInfo {
        letter_id: accepted.body.letter_id,
        device: parsed.device_id(),
        name: parsed.name.clone(),
        platform: parsed.platform.to_string(),
        card: card.clone(),
        sas: sas::words(card, head_n),
        expires_at,
    })
}

/// The `Add` entry at `head+1` for the letter's card (G.4 step 3).
pub fn approve_entry<R: CryptoRng>(
    chain: &VerifiedChain,
    card: &[u8],
    signer: DeviceCard,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> cloud::Result<ChainEntry> {
    cloud::chain::build_entry(
        chain.head.seq.saturating_add(1),
        chain.head.hash,
        cloud::chain::ChainOp::Add,
        Some(card.to_vec()),
        None,
        signer.device_id(),
        sk,
        rng,
    )
}

/// The `EnrollReject` letter body (G.4 step 4): the fresh `letter_id` is
/// minted by the caller (it is the dedupe + `Ack` key).
pub fn reject_letter(
    letter_id: cloud::LetterId,
    now: u64,
    from: cloud::DeviceId,
    to: cloud::DeviceId,
) -> LetterBody {
    LetterBody {
        letter_id,
        from,
        to,
        created_at: now,
        kind: cloud::letter::LetterKind::EnrollReject,
        body: cloud::letter::LetterKindBody::EnrollReject,
    }
}

/// The `Revoke` entry at `head+1` for `target` (G.5 step 1).
pub fn revoke_entry<R: CryptoRng>(
    chain: &VerifiedChain,
    target: cloud::DeviceId,
    signer: DeviceCard,
    sk: &v1::PrivateKey,
    rng: &mut R,
) -> cloud::Result<ChainEntry> {
    cloud::chain::build_entry(
        chain.head.seq.saturating_add(1),
        chain.head.hash,
        cloud::chain::ChainOp::Revoke,
        None,
        Some(target),
        signer.device_id(),
        sk,
        rng,
    )
}

/// The SAS words for a pending device's card at the frozen head (G.3 step 1).
pub fn sas_for(card: &[u8], frozen: &Head) -> sas::SasWords {
    sas::words(card, frozen)
}

/// Why an enrollment step was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollError {
    /// The chain head moved after the card was produced: revealing the SAS
    /// against the new head would show words the approver cannot match
    /// (spec §5.5). The caller must re-freeze and repost.
    HeadMoved,
}

/// One pending device's enrollment attempt: the head is captured BEFORE
/// anything is shown (spec §5.5, G.3 step 1) and the SAS is a pure
/// function of `(card, frozen)`.
#[derive(Debug, Clone)]
pub struct EnrollAttempt {
    frozen: Head,
    card: Vec<u8>,
    sas: sas::SasWords,
}

impl EnrollAttempt {
    /// Freeze `chain.head` and compute the words for `card`.
    pub fn freeze(chain: &VerifiedChain, card: &DeviceCard) -> Self {
        let frozen = chain.head;
        let sas = sas::words(&card.encode(), &frozen);
        Self {
            frozen,
            card: card.encode(),
            sas,
        }
    }

    /// The frozen head (the letter ships exactly this).
    pub fn frozen(&self) -> Head {
        self.frozen
    }

    /// The CE card bytes the letter ships.
    pub fn card(&self) -> &[u8] {
        &self.card
    }

    /// The frozen SAS words (published as `Pending.sas`).
    pub fn sas(&self) -> sas::SasWords {
        self.sas
    }

    /// Reveal the words, but only while `current` is still the frozen
    /// head; a moved head is an error, not a silent re-derivation.
    pub fn reveal(&self, current: &Head) -> Result<sas::SasWords, EnrollError> {
        if *current == self.frozen {
            Ok(self.sas)
        } else {
            Err(EnrollError::HeadMoved)
        }
    }
}
