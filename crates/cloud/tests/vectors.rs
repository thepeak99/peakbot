//! RED tests: spec §15 test vectors.
//!
//! LABEL (v1 KAT honesty rule): these are SELF-GENERATED REGRESSION
//! vectors, not independent KATs. The constants below are
//! `TBD_FILL_AT_IMPLEMENTATION`: they are generated once from the first
//! implementation (deterministic seeds + seeded RNGs, spec §15.1),
//! committed (in `tests/vectors/pbc1.json` and here), and then these
//! tests freeze them against drift. An independent re-implementation by
//! the tester MUST reproduce them. Until they are filled, every test
//! below fails on the equality assert — the expected RED.
//!
//! Determinism (spec §15.1): seeds A = 0x01×64, B = 0x02×64, C = 0x03×64,
//! D = 0x04×64; role RNGs `ChaCha20Rng::from_seed` with [0xA1;32] (I/A),
//! [0xB2;32] (R/B), [0xC3;32] (N/C), [0xD4;32] (attacker); fixed
//! `now = 1 800 000 000`.
mod common;

use cloud::chain::{self, ChainOp};
use cloud::handshake::{Initiator, Responder};
use common::*;

/// Filled at implementation time (see module label).
const TBD: &str = "TBD_FILL_AT_IMPLEMENTATION";

// -- id_{A,B} ------------------------------------------------------------------------

const ID_A: &str = TBD;
const ID_B: &str = TBD;

#[test]
fn vector_id_a_and_b() {
    let id_a = hex32(keypair(1).public().identity());
    let id_b = hex32(keypair(2).public().identity());
    assert_ne!(id_a, id_b, "distinct seeds give distinct identities");
    assert_eq!(id_a, ID_A, "id_A regression");
    assert_eq!(id_b, ID_B, "id_B regression");
}

// -- card_{A,B} ------------------------------------------------------------------------

const CARD_A: &str = TBD;
const CARD_B: &str = TBD;

#[test]
fn vector_card_a_and_b_ce() {
    let a = card_a();
    let b = card_b();
    let ca = a.encode();
    let cb = b.encode();
    // Card CE invariants (spec §4.1): version byte 1, max length 3280.
    assert_eq!(ca[0], 1, "card starts with version byte 1");
    assert!(ca.len() <= 3280);
    assert!(cb.len() <= 3280);
    assert_eq!(hex(&ca), CARD_A, "card_A regression");
    assert_eq!(hex(&cb), CARD_B, "card_B regression");
}

// -- chain_genesis_A ---------------------------------------------------------------------

const GENESIS_A_ENTRY: &str = TBD;
const GENESIS_A_HASH: &str = TBD;

#[test]
fn vector_chain_genesis_a() {
    let a = card_a();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    // Genesis invariants: prev is 32 zero bytes, op is Genesis.
    let parsed = cloud::chain::ChainEntry::parse(&e0).expect("parse");
    assert!(parsed.prev.iter().all(|x| *x == 0));
    assert_eq!(parsed.op, ChainOp::Genesis);
    assert_eq!(hex(&e0), GENESIS_A_ENTRY, "genesis entry regression");
    assert_eq!(hex32(h0), GENESIS_A_HASH, "genesis hash regression");
}

// -- chain_add_B_by_A -----------------------------------------------------------------------

const ADD_B_ENTRY: &str = TBD;
const ADD_B_HASH: &str = TBD;

#[test]
fn vector_chain_add_b_by_a() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    let head = chain::extend(None, &[e0, e1.clone()], None)
        .expect("chain")
        .head;
    assert_eq!(head.seq, 1);
    assert_eq!(head.hash, h1);
    assert_eq!(hex(&e1), ADD_B_ENTRY, "add_B entry regression");
    assert_eq!(hex32(h1), ADD_B_HASH, "add_B hash regression");
}

// -- chain_revoke_B_by_A ----------------------------------------------------------------------

const REVOKE_B_HASH: &str = TBD;

#[test]
fn vector_chain_revoke_b_by_a() {
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    let e2 = entry(1, 2, h1, ChainOp::Revoke, None, Some(b.device_id()), &a);
    let h2 = chain::entry_hash(&e2);
    let s = chain::extend(None, &[e0, e1, e2], None).expect("chain");
    // Final active/revoked sets (spec §15).
    assert!(!s.active.contains_key(&b.device_id()));
    assert!(s.revoked.contains(&b.device_id()));
    assert!(s.active.contains_key(&a.device_id()));
    assert_eq!(s.head.seq, 2);
    assert_eq!(hex32(h2), REVOKE_B_HASH, "revoke_B head hash regression");
}

// -- sas_C_at_head1 ------------------------------------------------------------------------------

const SAS_C_DIGEST: &str = TBD;
const SAS_C_WORDS: &str = TBD;

#[test]
fn vector_sas_c_at_head1() {
    let c = card_c();
    // head1 = the chain head after A adds B (seq 1).
    let a = card_a();
    let b = card_b();
    let e0 = entry(1, 0, [0; 32], ChainOp::Genesis, Some(&a), None, &a);
    let h0 = chain::entry_hash(&e0);
    let e1 = entry(1, 1, h0, ChainOp::Add, Some(&b), None, &a);
    let h1 = chain::entry_hash(&e1);
    let head = cloud::Head { seq: 1, hash: h1 };
    let d = cloud::sas::digest(&c.encode(), &head);
    let w = cloud::sas::words(&c.encode(), &head);
    assert_eq!(hex32(d), SAS_C_DIGEST, "sas_C digest regression");
    assert_eq!(
        format!("{:?}", w.as_indices()),
        SAS_C_WORDS,
        "sas_C words regression"
    );
}

// -- relay_hello_A ---------------------------------------------------------------------------------

const HELLO_A_TBS: &str = TBD;
const HELLO_A_SIG: &str = TBD;

#[test]
fn vector_relay_hello_a() {
    let a = keypair(1);
    let tbs = cloud::codec::hello_tbs(
        "https://relay.example",
        [0x5A; 32],
        a.public().identity(),
        0,
    );
    let sig = cloud::codec::sign_hello(a.private(), &tbs, &mut rng(0xA1)).expect("sign");
    cloud::codec::verify_hello(a.public(), &tbs, &sig).expect("verify");
    assert_eq!(hex(&tbs), HELLO_A_TBS, "hello_A tbs regression");
    assert_eq!(hex(&sig), HELLO_A_SIG, "hello_A sig regression");
}

// -- hs_A_to_B ----------------------------------------------------------------------------------------

const HS_K_IR: &str = TBD;
const HS_K_RI: &str = TBD;

#[test]
fn vector_hs_a_to_b_transcript() {
    let c = chain_ab();
    let a = card_a();
    let b = card_b();
    let no_fetch = |_from: u64| Ok(Vec::new() as Vec<Vec<u8>>);
    let (mut i, hs1) =
        Initiator::start(&c, a.device_id(), b.device_id(), &mut rng(0xA1)).expect("start");
    let (mut r, hs2) = Responder::on_hs1(
        &c,
        b.device_id(),
        a.device_id(),
        &hs1,
        &no_fetch,
        &mut rng(0xB2),
    )
    .expect("on hs1");
    let hs3 = i.on_hs2(b.device_id(), &hs2, &no_fetch).expect("on hs2");
    let ki = i.keys().expect("keys");
    let kr = r.on_hs3(&c, a.device_id(), &hs3).expect("on hs3");
    assert_eq!(ki, kr, "handshake keys must agree");
    assert_eq!(hex(&ki.k_ir), HS_K_IR, "k_IR regression");
    assert_eq!(hex(&ki.k_ri), HS_K_RI, "k_RI regression");
}

// -- data_A_to_B (inner bincode of the first frame: Open(Attach)) ---------------------------------------

const DATA_FRAME0_INNER: &str = TBD;

#[test]
fn vector_data_a_to_b_open_attach_frame() {
    let inner = cloud::frames::Inner {
        stream: 1,
        msg: cloud::frames::StreamMsg::Open(cloud::frames::Op::Attach),
    };
    let bytes = cloud::frames::encode_inner(&inner);
    assert_eq!(cloud::frames::decode_inner(&bytes).expect("decode"), inner);
    assert_eq!(hex(&bytes), DATA_FRAME0_INNER, "frame0 inner regression");
}
