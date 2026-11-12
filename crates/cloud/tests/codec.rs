//! RED tests: relay message codecs (spec §10) — roundtrip of every relay
//! message, unknown version → Err, Hello tbs layout and signature.
mod common;

use cloud::Error;
use cloud::codec::{self, FromRelay, ToRelay};
use common::*;

fn all_to_relay() -> Vec<ToRelay> {
    vec![
        ToRelay::Hello {
            device: [0x01; 32],
            sig: vec![0xAA; 3309],
        },
        ToRelay::HelloPending {
            card: vec![0xBB; 100],
            sig: vec![0xCC; 3309],
        },
        ToRelay::ChainFetch { from_seq: 0 },
        ToRelay::ChainFetch { from_seq: u64::MAX },
        ToRelay::ChainAppend {
            entry: vec![0xDD; 500],
        },
        ToRelay::EnrollGenesis {
            entry: vec![0xEE; 500],
        },
        ToRelay::Pipe {
            to: [0x02; 32],
            bytes: vec![0x01, 0x01, 0x00],
        },
        ToRelay::Post {
            to: [0x02; 32],
            letter_id: [0x03; 16],
            sealed: vec![0x04; 1000],
        },
        ToRelay::Ack {
            letter_id: [0x05; 16],
        },
    ]
}

fn all_from_relay() -> Vec<FromRelay> {
    vec![
        FromRelay::Challenge { nonce: [0x06; 32] },
        FromRelay::Welcome {
            head_seq: None,
            pending: false,
        },
        FromRelay::Welcome {
            head_seq: Some(42),
            pending: true,
        },
        FromRelay::Chain {
            entries: vec![vec![0x07; 100], vec![0x08; 200]],
        },
        FromRelay::ChainChanged { head_seq: 7 },
        FromRelay::Pipe {
            from: [0x09; 32],
            bytes: vec![0x01, 0x02, 0x00],
        },
        FromRelay::Letter {
            from: [0x0A; 32],
            letter_id: [0x0B; 16],
            sealed: vec![0x0C; 900],
        },
        FromRelay::Unreachable { to: [0x0D; 32] },
        FromRelay::Error { code: 5 },
    ]
}

#[test]
fn to_relay_roundtrip_all_variants() {
    for msg in all_to_relay() {
        let bytes = codec::encode_to_relay(&msg);
        let parsed = codec::decode_to_relay(&bytes).expect("roundtrip");
        assert_eq!(parsed, msg, "ToRelay roundtrip failed");
    }
}

#[test]
fn from_relay_roundtrip_all_variants() {
    for msg in all_from_relay() {
        let bytes = codec::encode_from_relay(&msg);
        let parsed = codec::decode_from_relay(&bytes).expect("roundtrip");
        assert_eq!(parsed, msg, "FromRelay roundtrip failed");
    }
}

#[test]
fn envelope_starts_with_version_byte_one() {
    let bytes = codec::encode_to_relay(&ToRelay::ChainFetch { from_seq: 0 });
    assert_eq!(bytes[0], 1);
}

#[test]
fn unknown_version_byte_rejected() {
    let mut bytes = codec::encode_to_relay(&ToRelay::ChainFetch { from_seq: 0 });
    bytes[0] = 2;
    assert_eq!(codec::decode_to_relay(&bytes), Err(Error::UnknownVersion));
    assert_eq!(codec::decode_from_relay(&bytes), Err(Error::UnknownVersion));
}

#[test]
fn zero_version_byte_rejected() {
    let mut bytes = codec::encode_to_relay(&ToRelay::ChainFetch { from_seq: 0 });
    bytes[0] = 0;
    assert_eq!(codec::decode_to_relay(&bytes), Err(Error::UnknownVersion));
}

#[test]
fn envelope_over_limit_rejected() {
    // A Pipe carrying more than MAX_INNER + 4096 bytes.
    let big = ToRelay::Pipe {
        to: [0x02; 32],
        bytes: vec![0u8; cloud::constants::MAX_ENVELOPE_PIPE + 1],
    };
    let bytes = codec::encode_to_relay(&big);
    assert_eq!(codec::decode_to_relay(&bytes), Err(Error::Malformed));
}

// -- Hello signature (spec §10.2) -------------------------------------------------

#[test]
fn hello_tbs_layout_is_canonical() {
    // Independently computed expected bytes:
    // str(origin) = u32(len) ‖ origin; then nonce, device_id, mode.
    let origin = "https://relay.example";
    let nonce = [0x5A; 32];
    let device = [0x01; 32];
    let mut expected = (origin.len() as u32).to_be_bytes().to_vec();
    expected.extend_from_slice(origin.as_bytes());
    expected.extend_from_slice(&nonce);
    expected.extend_from_slice(&device);
    expected.push(0);
    assert_eq!(codec::hello_tbs(origin, nonce, device, 0), expected);
}

#[test]
fn hello_sig_verifies_and_tamper_fails() {
    let a = keypair(1);
    let tbs = codec::hello_tbs("https://relay.example", [0x5A; 32], [0x01; 32], 0);
    let sig = codec::sign_hello(a.private(), &tbs, &mut rng(0xA1)).expect("sign");
    codec::verify_hello(a.public(), &tbs, &sig).expect("verify");
    // Tamper the tbs → verification fails.
    let mut tbs_t = tbs.clone();
    tbs_t[0] ^= 1;
    assert_eq!(
        codec::verify_hello(a.public(), &tbs_t, &sig),
        Err(Error::BadSignature)
    );
}
