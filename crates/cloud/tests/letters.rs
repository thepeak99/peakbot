//! RED tests: enrollment letters (spec §7) — TTL ± skew boundaries,
//! to≠self, replay set, 8 KiB cap, unknown version, sender state per
//! kind.
mod common;

use cloud::Error;
use cloud::Head;
use cloud::card::DeviceCard;
use cloud::chain::{self, ChainOp, VerifiedChain};
use cloud::letter::{
    self, Dispatch, LetterBody, LetterIn, LetterKind, LetterKindBody, RecipientState, ReplaySet,
};
use common::*;

const HEAD_N: Head = Head {
    seq: 1,
    hash: [0; 32],
};

fn card_for(n: u8) -> DeviceCard {
    match n {
        1 => card_a(),
        2 => card_b(),
        _ => card_c(),
    }
}

/// Build a sealed EnrollRequest from sender `sender_n` to `to_n`.
/// Returns (sealed, letter_id, from).
fn enroll_request(
    sender_n: u8,
    to_n: u8,
    letter_id: [u8; 16],
    created_at: u64,
    has_mac: Option<[u8; 32]>,
) -> (Vec<u8>, [u8; 16], [u8; 32]) {
    let c = card_for(sender_n);
    let to = card_for(to_n);
    let body = LetterBody {
        letter_id,
        from: c.device_id(),
        to: to.device_id(),
        created_at,
        kind: LetterKind::EnrollRequest,
        body: LetterKindBody::EnrollRequest {
            card: c.encode(),
            head_n: HEAD_N,
            has_mac: has_mac.is_some(),
            mac: has_mac,
        },
    };
    let signed =
        letter::signed_letter(&body, keypair(sender_n).private(), &mut rng(0xC3)).expect("sign");
    let aad = letter::letter_aad(letter_id, c.device_id(), to.device_id());
    let sealed = letter::seal_letter(&to.public_key().expect("pk"), &signed, &aad, &mut rng(0xC3))
        .expect("seal");
    (sealed, letter_id, c.device_id())
}

/// Build a sealed EnrollReject from active sender `sender_n` to `to_n`.
fn enroll_reject(
    sender_n: u8,
    to_n: u8,
    letter_id: [u8; 16],
    created_at: u64,
) -> (Vec<u8>, [u8; 16], [u8; 32]) {
    let c = card_for(sender_n);
    let to = card_for(to_n);
    let body = LetterBody {
        letter_id,
        from: c.device_id(),
        to: to.device_id(),
        created_at,
        kind: LetterKind::EnrollReject,
        body: LetterKindBody::EnrollReject,
    };
    let signed =
        letter::signed_letter(&body, keypair(sender_n).private(), &mut rng(0xA1)).expect("sign");
    let aad = letter::letter_aad(letter_id, c.device_id(), to.device_id());
    let sealed = letter::seal_letter(&to.public_key().expect("pk"), &signed, &aad, &mut rng(0xA1))
        .expect("seal");
    (sealed, letter_id, c.device_id())
}

/// Run the recipient checks (spec §7.5).
#[allow(clippy::too_many_arguments)] // mirrors LetterIn's fields
fn check(
    sealed: Vec<u8>,
    relay_from: [u8; 32],
    relay_id: [u8; 16],
    self_n: u8,
    self_state: RecipientState,
    now: u64,
    chain: &VerifiedChain,
    seen: &mut ReplaySet,
) -> Result<letter::AcceptedLetter, Error> {
    let kp = keypair(self_n);
    let mut in_ = LetterIn {
        sealed,
        relay_from,
        relay_letter_id: relay_id,
        self_id: card_for(self_n).device_id(),
        self_sk: kp.private(),
        self_state,
        now,
        chain,
        seen,
    };
    letter::check_letter(&mut in_)
}

/// A chain where C was added and then revoked (A, B active; C revoked).
fn chain_with_revoked_c() -> VerifiedChain {
    let a = card_a();
    let b = card_b();
    let c = card_c();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    let e2 = entry(1, 2, h1, ChainOp::Add, Some(&c), None, &a);
    let h2 = chain::entry_hash(&e2);
    let e3 = entry(1, 3, h2, ChainOp::Revoke, None, Some(c.device_id()), &a);
    chain::extend(None, &[e0, e1, e2, e3], None).expect("chain verifies")
}

// -- happy paths ---------------------------------------------------------------

#[test]
fn enroll_request_happy_path_dispatches_sas() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    let mut seen = ReplaySet::new();
    let acc = check(
        sealed,
        from,
        lid,
        1,
        RecipientState::Active,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("valid request accepted");
    assert_eq!(acc.dispatch, Dispatch::Sas);
    assert!(
        seen.contains(lid, NOW),
        "letter id must enter the replay set"
    );
}

#[test]
fn enroll_request_with_mac_dispatches_qr() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, Some([0x77; 32]));
    let mut seen = ReplaySet::new();
    let acc = check(
        sealed,
        from,
        lid,
        1,
        RecipientState::Active,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("valid request accepted");
    assert_eq!(acc.dispatch, Dispatch::Qr);
}

#[test]
fn enroll_reject_happy_path_dispatches_reject() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_reject(1, 3, [0xDD; 16], NOW);
    let mut seen = ReplaySet::new();
    let acc = check(
        sealed,
        from,
        lid,
        3,
        RecipientState::Pending,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("valid reject accepted");
    assert_eq!(acc.dispatch, Dispatch::Reject);
}

// -- TTL ± skew boundaries (spec §7.5 step 6: TTL 900 + SKEW 300) ----------------

#[test]
fn ttl_boundary_exactly_1200s_old_accepted() {
    let chain = chain_ab();
    // now - created_at = TTL + SKEW exactly: `now ≤ created_at + 1200`
    // is inclusive.
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW - 1200, None);
    let mut seen = ReplaySet::new();
    check(
        sealed,
        from,
        lid,
        1,
        RecipientState::Active,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("boundary age accepted");
}

#[test]
fn ttl_1201s_old_rejected_expired() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW - 1201, None);
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::Expired)
    );
}

#[test]
fn future_skew_300s_accepted() {
    let chain = chain_ab();
    // created_at = now + SKEW exactly: `created_at ≤ now + 300` inclusive.
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW + 300, None);
    let mut seen = ReplaySet::new();
    check(
        sealed,
        from,
        lid,
        1,
        RecipientState::Active,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("boundary future accepted");
}

#[test]
fn future_skew_301s_rejected() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW + 301, None);
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::Future)
    );
}

// -- routing fields (spec §7.5 step 4) -------------------------------------------

#[test]
fn to_mismatch_rejected() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    // Deliver to B, but the letter says to A. The seal AAD binds `to`
    // (spec §7.3), so the open in step 2 fails before the step-4 id
    // check (spec wins over the original IdMismatch expectation).
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            2,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::DecryptionError)
    );
}

#[test]
fn letter_id_mismatch_rejected() {
    let chain = chain_ab();
    let (sealed, _lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    // The AAD binds the relay letter_id (spec §7.3): a mismatched relay
    // field fails at the step-2 open, before the step-4 id check.
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            [0xFF; 16],
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::DecryptionError)
    );
}

#[test]
fn from_mismatch_rejected() {
    let chain = chain_ab();
    let (sealed, lid, _from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    // The AAD binds the relay from (spec §7.3): a mismatched relay field
    // fails at the step-2 open, before the step-4 id check.
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            [0x99; 32],
            lid,
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::DecryptionError)
    );
}

// -- replay set (spec §7.5 step 5) -------------------------------------------------

#[test]
fn replay_same_id_rejected() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    let mut seen = ReplaySet::new();
    check(
        sealed.clone(),
        from,
        lid,
        1,
        RecipientState::Active,
        NOW,
        &chain,
        &mut seen,
    )
    .expect("first delivery accepted");
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::Replayed)
    );
}

#[test]
fn replay_after_retention_renewed() {
    // seen entries expire after SEEN_RETENTION = 1200 s (TTL + skew).
    let mut seen = ReplaySet::new();
    seen.insert([0xEE; 16], NOW);
    assert!(seen.contains([0xEE; 16], NOW));
    seen.prune(NOW + 1200);
    assert!(
        !seen.contains([0xEE; 16], NOW + 1200),
        "entry must expire after 20 min"
    );
}

// -- caps and versions (spec §7.4, §7.5 step 1-2) -----------------------------------

#[test]
fn oversize_sealed_rejected() {
    let chain = chain_ab();
    let sealed = vec![0x01u8; cloud::constants::MAX_LETTER + 1]; // 8193
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            [0x99; 32],
            [0xEE; 16],
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::LetterTooLarge)
    );
}

#[test]
fn unknown_seal_version_rejected() {
    let chain = chain_ab();
    // A seal whose first byte is not v1::VERSION = 1.
    let sealed = vec![0x02u8; 100];
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            [0x99; 32],
            [0xEE; 16],
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::UnknownVersion)
    );
}

#[test]
fn aad_mismatch_reroute_rejected() {
    // The relay re-routes: sealed for A, but the relay claims a
    // different letter_id → the AAD built from the relay fields does not
    // match → open fails.
    let chain = chain_ab();
    let (sealed, _lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            [0x01; 16],
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::DecryptionError)
    );
}

// -- sender state per kind (spec §7.5 step 7) ----------------------------------------

#[test]
fn enroll_request_from_active_sender_rejected() {
    let chain = chain_ab();
    // A is already active in the chain.
    let (sealed, lid, from) = enroll_request(1, 2, [0xEE; 16], NOW, None);
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            2,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::SenderNotPending)
    );
}

#[test]
fn enroll_request_from_revoked_sender_rejected() {
    let chain = chain_with_revoked_c();
    // C is in the revoked set.
    let (sealed, lid, from) = enroll_request(3, 1, [0xEE; 16], NOW, None);
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            1,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::SenderNotPending)
    );
}

#[test]
fn enroll_reject_from_inactive_sender_rejected() {
    let chain = chain_ab();
    // C is not active; a reject must come from an active device.
    let (sealed, lid, from) = enroll_reject(3, 1, [0xDD; 16], NOW);
    let mut seen = ReplaySet::new();
    // Recipient must be Pending for a Reject (§7.1; step 3 precedes step 7).
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            1,
            RecipientState::Pending,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::SenderNotActive)
    );
}

// -- kind vs recipient state (spec §7.1) ---------------------------------------------

#[test]
fn request_to_pending_recipient_kind_not_allowed() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_request(3, 2, [0xEE; 16], NOW, None);
    // Recipient B is active in the chain, but claims to be pending:
    // EnrollRequest requires an active recipient.
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            2,
            RecipientState::Pending,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::KindNotAllowed)
    );
}

#[test]
fn reject_to_active_recipient_kind_not_allowed() {
    let chain = chain_ab();
    let (sealed, lid, from) = enroll_reject(1, 2, [0xDD; 16], NOW);
    // Recipient B is active: EnrollReject requires a pending recipient.
    let mut seen = ReplaySet::new();
    assert_eq!(
        check(
            sealed,
            from,
            lid,
            2,
            RecipientState::Active,
            NOW,
            &chain,
            &mut seen
        ),
        Err(Error::KindNotAllowed)
    );
}

// -- parse (spec §1.2 P3) ---------------------------------------------------------------

#[test]
fn unknown_kind_byte_rejected() {
    // Hand-rolled letter_body bytes: version 1, ids, created_at, kind 3
    // (not in the registry) → Malformed.
    let mut raw = vec![0x01u8];
    raw.extend_from_slice(&[0xEE; 16]); // letter_id
    raw.extend_from_slice(&[0x11; 32]); // from
    raw.extend_from_slice(&[0x22; 32]); // to
    raw.extend_from_slice(&1_800_000_000u64.to_be_bytes());
    raw.push(0x03); // kind: unknown
    assert_eq!(LetterBody::parse(&raw), Err(Error::Malformed));
}
