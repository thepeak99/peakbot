//! RED tests: device cards and the device chain (spec §4).
//!
//! Covers C1–C6 (each violation → its specific error), fork detection,
//! genesis pinning, last-device self-revoke, revoked re-add, and a
//! proptest: random valid op sequences always verify, and any
//! single-byte mutation fails.
mod common;

use cloud::Error;
use cloud::card::DeviceCard;
use cloud::chain::{self, ChainOp, VerifiedChain};
use common::*;
use proptest::prelude::*;

fn add_by_a(seq: u64, prev: [u8; 32], c: &DeviceCard) -> Vec<u8> {
    let a = card_a();
    entry(1, seq, prev, ChainOp::Add, Some(c), None, &a)
}

fn revoke_by_a(seq: u64, prev: [u8; 32], target: [u8; 32]) -> Vec<u8> {
    let a = card_a();
    entry(1, seq, prev, ChainOp::Revoke, None, Some(target), &a)
}

fn verify(entries: &[Vec<u8>], pinned: Option<[u8; 32]>) -> Result<VerifiedChain, Error> {
    chain::extend(None, entries, pinned)
}

// -- happy paths -------------------------------------------------------------

#[test]
fn genesis_entry_verifies() {
    let e0 = genesis_a();
    let s = verify(&[e0], None).expect("genesis verifies");
    assert_eq!(s.head.seq, 0);
    assert_eq!(s.hashes.len(), 1);
    assert_eq!(s.active.len(), 1);
    assert!(s.revoked.is_empty());
    assert_eq!(s.genesis, s.head.hash);
}

#[test]
fn add_b_by_a_advances_head() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let s = verify(&[e0, e1], None).expect("genesis+add verifies");
    assert_eq!(s.head.seq, 1);
    assert_eq!(s.active.len(), 2);
}

// -- C1: genesis -------------------------------------------------------------

#[test]
fn c1_add_op_at_seq0_is_bad_genesis() {
    let a = card_a();
    let e0 = entry(1, 0, [0; 32], ChainOp::Add, Some(&a), None, &a);
    assert_eq!(verify(&[e0], None), Err(Error::BadGenesis));
}

#[test]
fn c1_genesis_with_nonzero_prev_is_bad_genesis() {
    let a = card_a();
    let e0 = entry(1, 0, [0x7; 32], ChainOp::Genesis, Some(&a), None, &a);
    assert_eq!(verify(&[e0], None), Err(Error::BadGenesis));
}

#[test]
fn c1_genesis_op_at_seq1_is_bad_genesis() {
    let a = card_a();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // A second Genesis at seq 1, linked to head.
    let e1 = entry(1, 1, h0, ChainOp::Genesis, Some(&a), None, &a);
    assert_eq!(verify(&[e0, e1], None), Err(Error::BadGenesis));
}

// -- C2: prev link / fork F1 --------------------------------------------------

#[test]
fn c2_wrong_prev_is_bad_prev() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    // Wrong prev: not the genesis hash.
    let e1 = entry(1, 1, [0x9; 32], ChainOp::Add, Some(&b), None, &a);
    assert_eq!(verify(&[e0, e1], None), Err(Error::BadPrev));
}

#[test]
fn fork_entry_at_newer_seq_is_bad_prev() {
    // F1: the relay's next entry does not link to our head.
    let a = card_a();
    let b = card_b();
    let c = card_c();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let state = verify(&[e0, e1], None).expect("base chain");
    // Forked entry: seq 2 but prev is NOT our head hash h1.
    let forked = entry(1, 2, [0x5; 32], ChainOp::Add, Some(&c), None, &a);
    assert_eq!(chain::sync(&state, &[forked]), Err(Error::BadPrev));
}

#[test]
fn same_seq_conflict_is_gap() {
    // Two values claim seq 0 with different hashes: the second entry's
    // seq (0) is not expected (1) → Gap.
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let e0_fork = entry(2, 0, [0; 32], ChainOp::Genesis, Some(&b), None, &b);
    assert_eq!(verify(&[e0, e0_fork], None), Err(Error::Gap));
}

// -- C3: signer active ---------------------------------------------------------

#[test]
fn c3_inactive_signer_is_signer_not_active() {
    let a = card_a();
    let b = card_b();
    let c = card_c();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // C signs an Add, but C is not in the chain.
    let e1 = entry(3, 1, h0, ChainOp::Add, Some(&b), None, &c);
    assert_eq!(verify(&[e0, e1], None), Err(Error::SignerNotActive));
}

// -- C4: re-add ----------------------------------------------------------------

#[test]
fn c4_readd_active_is_readd() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    // Add B again while active.
    let e2 = entry(1, 2, h1, ChainOp::Add, Some(&b), None, &a);
    assert_eq!(verify(&[e0, e1, e2], None), Err(Error::ReAdd));
}

#[test]
fn revoked_id_can_never_be_readded() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    let e2 = revoke_by_a(2, h1, b.device_id());
    let h2 = chain::entry_hash(&e2);
    // Re-add B after revocation.
    let e3 = entry(1, 3, h2, ChainOp::Add, Some(&b), None, &a);
    assert_eq!(verify(&[e0, e1, e2, e3], None), Err(Error::ReAdd));
}

// -- C5: revoke ----------------------------------------------------------------

#[test]
fn c5_revoke_unknown_target_is_bad_revoke() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // Revoke B, but B was never added.
    let e1 = revoke_by_a(1, h0, b.device_id());
    assert_eq!(verify(&[e0, e1], None), Err(Error::BadRevoke));
}

// -- C6: last device -----------------------------------------------------------

#[test]
fn c6_last_device_cannot_self_revoke() {
    let a = card_a();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // Only A is active; A tries to revoke itself.
    let e1 = revoke_by_a(1, h0, a.device_id());
    assert_eq!(verify(&[e0, e1], None), Err(Error::LastDevice));
}

// -- signature -----------------------------------------------------------------

#[test]
fn bad_signature_is_bad_signature() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // The entry claims signer A (active) but is signed with B's key:
    // signer field = A, actual key = B.
    let e1_bad = entry(2, 1, h0, ChainOp::Add, Some(&b), None, &a);
    assert_eq!(verify(&[e0, e1_bad], None), Err(Error::BadSignature));
}

// -- sequence gap ----------------------------------------------------------------

#[test]
fn seq_gap_is_gap() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    // Skip seq 1; go straight to seq 2.
    let h0 = chain::entry_hash(&e0);
    let e2 = entry(1, 2, h0, ChainOp::Add, Some(&b), None, &a);
    assert_eq!(verify(&[e0, e2], None), Err(Error::Gap));
}

// -- genesis pinning ---------------------------------------------------------------

#[test]
fn genesis_mismatch_rejected_when_pinned() {
    let e0 = genesis_a();
    // Pin a genesis that is NOT the real one.
    assert_eq!(verify(&[e0], Some([0x0; 32])), Err(Error::GenesisMismatch));
}

#[test]
fn pin_match_accepted() {
    let e0 = genesis_a();
    let real = chain::entry_hash(&e0);
    let s = verify(&[e0], Some(real)).expect("matching pin accepted");
    assert_eq!(s.genesis, real);
}

// -- malformed ---------------------------------------------------------------------

#[test]
fn malformed_trailing_byte_is_malformed() {
    let mut e0 = genesis_a();
    e0.push(0x00);
    assert_eq!(verify(&[e0], None), Err(Error::Malformed));
}

#[test]
fn malformed_oversize_card_length_is_malformed() {
    let e0 = genesis_a();
    // The card length field is at offset 1+8+32+1 = 42 (u32 BE). Bump it
    // by 1 so the declared length exceeds the actual card bytes.
    let mut bad = e0.clone();
    let off = 42;
    bad[off + 3] += 1;
    assert_eq!(verify(&[bad], None), Err(Error::Malformed));
}

// -- proptest: random valid op sequences always verify ----------------------------

proptest! {
    #[test]
    fn random_valid_op_sequences_verify(n in 1usize..=8) {
        let a = card_a();
        let mut active: Vec<[u8; 32]> = vec![a.device_id()];
        let mut entries: Vec<Vec<u8>> = vec![entry(1, 0, [0;32], ChainOp::Genesis, Some(&a), None, &a)];
        let mut prev = chain::entry_hash(&entries[0]);
        let mut next_dev = 2u8;
        for i in 0..n {
            let want_revoke = i % 2 == 1 && active.len() >= 2;
            if want_revoke {
                let t = active.pop().unwrap();
                let e = revoke_by_a((i + 1) as u64, prev, t);
                prev = chain::entry_hash(&e);
                entries.push(e);
            } else {
                let dc = card(next_dev, &format!("dev{next_dev}"), 1);
                next_dev += 1;
                let e = add_by_a((i + 1) as u64, prev, &dc);
                active.push(dc.device_id());
                prev = chain::entry_hash(&e);
                entries.push(e);
            }
        }
        let s = chain::extend(None, &entries, None)
            .unwrap_or_else(|e| panic!("valid op sequence must verify, got {e:?}"));
        prop_assert_eq!(s.head.seq, entries.len() as u64 - 1);
        prop_assert_eq!(s.active.len(), active.len());
    }
}

// -- proptest: any single-byte mutation fails --------------------------------------

proptest! {
    #[test]
    fn single_byte_mutation_always_fails(
        entry_idx in 0usize..3,
        byte_idx in 0usize..6000,
    ) {
        let a = card_a();
        let b = card_b();
        let c = card_c();
        let e0 = entry(1, 0, [0;32], ChainOp::Genesis, Some(&a), None, &a);
        let h0 = chain::entry_hash(&e0);
        let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
        let h1 = chain::entry_hash(&e1);
        let e2 = entry(1, 2, h1, ChainOp::Add, Some(&c), None, &a);
        let mut all = vec![e0, e1, e2];
        all[entry_idx][byte_idx] ^= 0xFF; // guaranteed change
        let res = chain::extend(None, &all, None);
        prop_assert!(res.is_err(), "single-byte mutation must fail, got {:?}", res.map(|s| s.head));
    }
}
