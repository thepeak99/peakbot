//! RED tests: handshake Hs1/Hs2/Hs3 (spec §8) — a sans-I/O state machine
//! with the key schedule. Covers the happy path, every field tamper,
//! wrong r, inactive i, revoked peer, newer-head sync, stale chain, and
//! replayed Hs1.
mod common;

use cloud::Error;
use cloud::Head;
use cloud::Result;
use cloud::chain::VerifiedChain;
use cloud::handshake::{Hs1, Initiator, Responder, SessionKeys};
use common::*;

type Fetch = dyn Fn(u64) -> Result<Vec<Vec<u8>>>;

fn no_fetch() -> impl Fn(u64) -> Result<Vec<Vec<u8>>> {
    |_from_seq: u64| Ok(Vec::new())
}

/// The two-device chain: A (genesis) + B (added).
fn chain() -> VerifiedChain {
    chain_ab()
}

/// A full happy-path handshake I=A → R=B with the given fetch (used by
/// both sides for §8.5 sync). Returns (I keys, R keys).
fn full_handshake(fetch: &Fetch) -> (SessionKeys, SessionKeys) {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1))
        .expect("initiator starts");
    let (mut r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &fetch,
        keypair(2).private(), &mut rng(0xB2),
    )
    .expect("responder on hs1");
    let hs3 = i
        .on_hs2(b.device_id(), &hs2, fetch)
        .expect("initiator on hs2");
    let ki = i.keys().expect("initiator keys");
    let kr = r.on_hs3(&c, a.device_id(), &hs3).expect("responder on hs3");
    (ki, kr)
}

// -- happy path -------------------------------------------------------------------

#[test]
fn happy_path_equal_keys_both_directions() {
    let f = no_fetch();
    let (ki, kr) = full_handshake(&f);
    assert_eq!(ki.k_ir, kr.k_ir, "I→R key must match");
    assert_eq!(ki.k_ri, kr.k_ri, "R→I key must match");
    assert_ne!(ki.k_ir, ki.k_ri, "directions must have distinct keys");
}

// -- Hs1 field tampering -------------------------------------------------------------

#[test]
fn hs1_tamper_r_field_aborts_misrouted() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut t = hs1.clone();
    t.r[0] ^= 1; // r ≠ self
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &t,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::Misrouted));
}

#[test]
fn hs1_tamper_i_field_aborts_misrouted() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut t = hs1.clone();
    t.i[0] ^= 1; // i ≠ relay-supplied from
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &t,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::Misrouted));
}

#[test]
fn hs1_i_equals_self_rejected() {
    let c = chain();
    let b = card_b();
    let hs1 = Hs1 {
        sid: [0; 16],
        i: b.device_id(), // i == self
        r: b.device_id(),
        eph_pk: vec![0; 1216],
        head: c.head,
    };
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        b.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::Misrouted));
}

#[test]
fn hs1_tamper_head_aborts_trust() {
    // Same seq, different hash → fork F2.
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut t = hs1.clone();
    t.head = Head {
        seq: t.head.seq,
        hash: [0xAA; 32],
    };
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &t,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::Trust));
}

#[test]
fn hs1_tamper_eph_pk_diverges_keys() {
    // A tampered ephemeral KEM pk decodes to a (different) key: the
    // encapsulated shared secret diverges, so the two sides derive
    // different session keys and the session is unusable (ct lies under
    // both signatures; the first Data frame would fail AEAD).
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut t = hs1.clone();
    t.eph_pk[100] ^= 1;
    let (mut r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &t,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .expect("responder accepts decodable pk");
    let hs3 = i
        .on_hs2(b.device_id(), &hs2, &no_fetch())
        .expect("initiator on hs2");
    let ki = i.keys().expect("keys");
    let kr = r.on_hs3(&c, a.device_id(), &hs3).expect("responder on hs3");
    assert_ne!(ki.k_ir, kr.k_ir, "tampered eph_pk must diverge the keys");
}

// -- Hs2 field tampering ---------------------------------------------------------------

#[test]
fn hs2_tamper_sig_r_aborts_bad_signature() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let (_r, mut hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .unwrap();
    hs2.sig_r[50] ^= 1;
    let res = i.on_hs2(b.device_id(), &hs2, &no_fetch());
    assert_eq!(res.err(), Some(Error::BadSignature));
}

#[test]
fn hs2_tamper_head_aborts_trust() {
    // R presents a head with the same seq but a different hash → F2.
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let (_r, mut hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .unwrap();
    hs2.head = Head {
        seq: hs2.head.seq,
        hash: [0xBB; 32],
    };
    let res = i.on_hs2(b.device_id(), &hs2, &no_fetch());
    assert_eq!(res.err(), Some(Error::Trust));
}

#[test]
fn hs2_tamper_ct_diverges_keys() {
    // ct is under sig_R (verified by I) and under sig_I (verified by R);
    // a tampered ct that R signed is accepted by both signatures, but the
    // decapsulated shared secret diverges → different keys.
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let (mut r, mut hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .unwrap();
    hs2.ct[10] ^= 1;
    let hs3 = i
        .on_hs2(b.device_id(), &hs2, &no_fetch())
        .expect("sig_R covers the tampered ct");
    let ki = i.keys().expect("keys");
    let kr = r.on_hs3(&c, a.device_id(), &hs3).expect("responder on hs3");
    assert_ne!(ki.k_ir, kr.k_ir, "tampered ct must diverge the keys");
}

// -- Hs3 tampering -----------------------------------------------------------------------

#[test]
fn hs3_tamper_sig_i_aborts_bad_signature() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let (mut r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .unwrap();
    let mut hs3 = i.on_hs2(b.device_id(), &hs2, &no_fetch()).expect("hs3");
    hs3.sig_i[7] ^= 1;
    let res = r.on_hs3(&c, a.device_id(), &hs3);
    assert_eq!(res.err(), Some(Error::BadSignature));
}

// -- identity / state ---------------------------------------------------------------------

#[test]
fn inactive_i_aborts_not_active() {
    // C was never added to the chain.
    let c = chain();
    let b = card_b();
    let cdev = card_c();
    let hs1 = Hs1 {
        sid: [0; 16],
        i: cdev.device_id(),
        r: b.device_id(),
        eph_pk: vec![0; 1216],
        head: c.head,
    };
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        cdev.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::NotActive));
}

#[test]
fn revoked_peer_aborts_not_active() {
    // A chain where B was revoked; R = B is no longer active.
    let c = {
        let a = card_a();
        let b = card_b();
        let e0 = entry(
            1,
            0,
            [0; 32],
            cloud::chain::ChainOp::Genesis,
            Some(&a),
            None,
            &a,
        );
        let h0 = cloud::chain::entry_hash(&e0);
        let e1 = entry(1, 1, h0, cloud::chain::ChainOp::Add, Some(&b), None, &a);
        let h1 = cloud::chain::entry_hash(&e1);
        let e2 = entry(
            1,
            2,
            h1,
            cloud::chain::ChainOp::Revoke,
            None,
            Some(b.device_id()),
            &a,
        );
        cloud::chain::extend(None, &[e0, e1, e2], None).expect("chain")
    };
    let a = card_a();
    let b = card_b();
    let hs1 = Hs1 {
        sid: [0; 16],
        i: a.device_id(),
        r: b.device_id(),
        eph_pk: vec![0; 1216],
        head: c.head,
    };
    let res = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    );
    assert_eq!(res.err(), Some(Error::NotActive));
}

// -- head sync (spec §8.5) -------------------------------------------------------------------

#[test]
fn newer_peer_head_forces_sync() {
    // I's chain is stale (genesis only); R is at seq 1. I must sync via
    // the fetch closure to accept R's newer head.
    let c = chain();
    let stale = cloud::chain::extend(
        None,
        &[entry(
            1,
            0,
            [0; 32],
            cloud::chain::ChainOp::Genesis,
            Some(&card_a()),
            None,
            &card_a(),
        )],
        None,
    )
    .expect("stale chain");
    let a = card_a();
    let b = card_b();
    // Fetch returns entry 1 (the Add of B) when asked from seq 1.
    let fetch = move |from_seq: u64| -> cloud::Result<Vec<Vec<u8>>> {
        assert_eq!(from_seq, 1);
        let e0 = entry(
            1,
            0,
            [0; 32],
            cloud::chain::ChainOp::Genesis,
            Some(&card_a()),
            None,
            &card_a(),
        );
        let h0 = cloud::chain::entry_hash(&e0);
        let e1 = entry(
            1,
            1,
            h0,
            cloud::chain::ChainOp::Add,
            Some(&card_b()),
            None,
            &card_a(),
        );
        Ok(vec![e1])
    };
    // Start the handshake from the stale chain; R presents the newer head.
    let (mut i, hs1) = Initiator::start(&stale, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1))
        .expect("start from stale");
    let (mut r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &fetch,
        keypair(2).private(), &mut rng(0xB2),
    )
    .expect("responder on hs1");
    let hs3 = i
        .on_hs2(b.device_id(), &hs2, &fetch)
        .expect("initiator syncs");
    let _ = r.on_hs3(&c, a.device_id(), &hs3).expect("responder on hs3");
}

#[test]
fn stale_head_without_relay_data_aborts_stale_chain() {
    // I is stale; the relay cannot serve the missing entry → StaleChain.
    let c = chain();
    let stale = cloud::chain::extend(
        None,
        &[entry(
            1,
            0,
            [0; 32],
            cloud::chain::ChainOp::Genesis,
            Some(&card_a()),
            None,
            &card_a(),
        )],
        None,
    )
    .expect("stale chain");
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&stale, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1))
        .expect("start from stale");
    let (_r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .expect("responder on hs1");
    let res = i.on_hs2(b.device_id(), &hs2, &no_fetch());
    assert_eq!(res.err(), Some(Error::StaleChain));
}

// -- replay -----------------------------------------------------------------------------------

#[test]
fn replayed_hs1_cannot_complete_new_handshake() {
    // A live session is established (sid S1). The attacker replays the
    // original Hs1: R replaces its pending state with a FRESH ct, so the
    // old Hs3 (signed over the old th3) no longer verifies. The attacker
    // cannot complete a new handshake, and the live session is untouched
    // (an established session is not touched until a new one completes).
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (mut i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let (mut r, hs2_live) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB2),
    )
    .unwrap();
    let hs3 = i
        .on_hs2(b.device_id(), &hs2_live, &no_fetch())
        .expect("live session hs3");
    let _live_keys = r
        .on_hs3(&c, a.device_id(), &hs3)
        .expect("live session established");

    // Replay the same Hs1. A fresh attempt uses fresh KEM randomness
    // (spec §8.6) — a distinct seed, since the same seed would
    // deterministically reproduce the same ct (spec §15.1).
    let (mut r2, _hs2b) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch(),
        keypair(2).private(), &mut rng(0xB3),
    )
    .expect("R re-pends with fresh ct");
    // The old Hs3 must NOT verify against the new pending state.
    let res = r2.on_hs3(&c, a.device_id(), &hs3);
    assert_eq!(res.err(), Some(Error::BadSignature));
}

// -- pipe message wire format --------------------------------------------------------------------

#[test]
fn pipe_msg_roundtrip_and_version() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let m = cloud::handshake::PipeMsg::Hs1(hs1.clone());
    let bytes = m.encode();
    let parsed = cloud::handshake::PipeMsg::parse(&bytes).expect("roundtrip");
    assert_eq!(parsed, m);
}

#[test]
fn pipe_msg_unknown_version_rejected() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut bytes = cloud::handshake::PipeMsg::Hs1(hs1).encode();
    bytes[0] = 2; // version 2
    assert_eq!(
        cloud::handshake::PipeMsg::parse(&bytes),
        Err(Error::UnknownVersion)
    );
}

#[test]
fn pipe_msg_trailing_byte_rejected() {
    let c = chain();
    let a = card_a();
    let b = card_b();
    let (_i, hs1) = Initiator::start(&c, a.device_id(), b.device_id(), keypair(1).private(), &mut rng(0xA1)).unwrap();
    let mut bytes = cloud::handshake::PipeMsg::Hs1(hs1).encode();
    bytes.push(0x00);
    assert_eq!(
        cloud::handshake::PipeMsg::parse(&bytes),
        Err(Error::Malformed)
    );
}
