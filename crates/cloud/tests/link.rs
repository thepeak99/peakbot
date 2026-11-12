//! RED tests: optional QR/link enrollment (spec §6) — grammar
//! success/failure, MAC verify, tamper.
mod common;

use cloud::Error;
use cloud::Head;
use cloud::link::{self, Link};
use common::*;

fn parsed() -> Link {
    Link::parse(&good_link()).expect("valid link parses")
}

// -- grammar: success ----------------------------------------------------------

#[test]
fn valid_link_parses_fields() {
    let l = parsed();
    assert_eq!(l.relay, LINK_RELAY);
    assert_eq!(l.dev, LINK_DEV);
    assert_eq!(l.head, link_head());
    assert_eq!(l.secret, LINK_SECRET);
}

#[test]
fn roundtrip_encode_parse() {
    let l = parsed();
    let s = l.encode();
    assert_eq!(s, good_link());
    assert_eq!(Link::parse(&s).expect("roundtrip"), l);
}

// -- grammar: failures (all BadLink, spec §6.1) --------------------------------

#[test]
fn rejects_reordered_params() {
    // dev before relay.
    let s = format!(
        "peakbot-cloud:v1?dev={dev}&relay={relay}&head=42:{h}&s={s2}",
        dev = b64url32(&LINK_DEV),
        relay = LINK_RELAY,
        h = b64url32(&link_head().hash),
        s2 = b64url32(&LINK_SECRET),
    );
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_extra_param() {
    assert_eq!(
        Link::parse(&format!("{}&x=1", good_link())),
        Err(Error::BadLink)
    );
}

#[test]
fn rejects_missing_param() {
    // Drop the s= secret.
    let s = good_link();
    let cut = s.find("&s=").expect("s param present");
    assert_eq!(Link::parse(&s[..cut]), Err(Error::BadLink));
}

#[test]
fn rejects_fragment() {
    assert_eq!(
        Link::parse(&format!("{}#f", good_link())),
        Err(Error::BadLink)
    );
}

#[test]
fn rejects_wrong_version_token() {
    let s = good_link().replacen("peakbot-cloud:v1", "peakbot-cloud:v2", 1);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_case_sensitive_scheme() {
    let s = good_link().replacen("peakbot-cloud", "Peakbot-cloud", 1);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_non_canonical_b64() {
    // LINK_SECRET = 0x22×32 → canonical last char 'I' ((0x22 & 0xF) << 2 = 8).
    // 'J' (value 9) has the low 2 bits set → non-canonical.
    let s = good_link();
    let enc = b64url32(&LINK_SECRET);
    assert!(enc.ends_with('I'));
    let s = s.replacen(&enc, &format!("{}J", &enc[..enc.len() - 1]), 1);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_b64_wrong_length() {
    let enc = b64url32(&LINK_SECRET);
    let s = good_link().replacen(&enc, &enc[..42], 1); // 42 chars
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_seq_leading_zero() {
    let s = good_link().replacen("head=42:", "head=042:", 1);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_seq_zero_is_allowed() {
    // seq = "0" is the one permitted zero form.
    let mut h = link_head();
    h.seq = 0;
    let s = format!(
        "peakbot-cloud:v1?relay={}&dev={}&head=0:{}&s={}",
        encoded_relay(),
        b64url32(&LINK_DEV),
        b64url32(&h.hash),
        b64url32(&LINK_SECRET),
    );
    let l = Link::parse(&s).expect("seq 0 is allowed");
    assert_eq!(l.head.seq, 0);
}

#[test]
fn rejects_seq_u64_overflow() {
    let s = good_link().replacen("head=42:", "head=18446744073709551616:", 1);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_http_relay() {
    // relay must decode to an https origin (RFC 6454).
    let s = format!(
        "peakbot-cloud:v1?relay=http%3A%2F%2Frelay.example&dev={}&head=42:{}&s={}",
        b64url32(&LINK_DEV),
        b64url32(&link_head().hash),
        b64url32(&LINK_SECRET),
    );
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_relay_with_path() {
    // No path in the origin.
    let s = format!(
        "peakbot-cloud:v1?relay=https%3A%2F%2Frelay.example%2Fx&dev={}&head=42:{}&s={}",
        b64url32(&LINK_DEV),
        b64url32(&link_head().hash),
        b64url32(&LINK_SECRET),
    );
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

#[test]
fn rejects_overlong_link() {
    // Total length > 1024 bytes: pad the host (unreserved chars).
    let host = "a".repeat(1200);
    let s = format!(
        "peakbot-cloud:v1?relay=https://{host}&dev={}&head=42:{}&s={}",
        b64url32(&LINK_DEV),
        b64url32(&link_head().hash),
        b64url32(&LINK_SECRET),
    );
    assert!(s.len() > 1024);
    assert_eq!(Link::parse(&s), Err(Error::BadLink));
}

// -- MAC (spec §6.3) ---------------------------------------------------------------

fn mac_args() -> (&'static [u8; 32], Vec<u8>, [u8; 32], Head, Head) {
    let card_n = vec![0xAB; 512];
    let head_n = Head {
        seq: 3,
        hash: [0x0C; 32],
    };
    let link_head = link_head();
    (&LINK_SECRET, card_n, LINK_DEV, head_n, link_head)
}

#[test]
fn mac_roundtrip_verifies() {
    let (secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(secret, &card_n, id_e, head_n, link_head);
    link::verify_enroll_mac(secret, &card_n, id_e, head_n, link_head, &mac).expect("MAC verifies");
}

#[test]
fn mac_tamper_card_fails() {
    let (secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(secret, &card_n, id_e, head_n, link_head);
    let mut card_t = card_n.clone();
    card_t[0] ^= 1;
    assert_eq!(
        link::verify_enroll_mac(secret, &card_t, id_e, head_n, link_head, &mac),
        Err(Error::BadMac)
    );
}

#[test]
fn mac_tamper_id_e_fails() {
    let (secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(secret, &card_n, id_e, head_n, link_head);
    let mut id_t = id_e;
    id_t[0] ^= 1;
    assert_eq!(
        link::verify_enroll_mac(secret, &card_n, id_t, head_n, link_head, &mac),
        Err(Error::BadMac)
    );
}

#[test]
fn mac_tamper_head_n_fails() {
    let (secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(secret, &card_n, id_e, head_n, link_head);
    let mut h = head_n;
    h.seq += 1;
    assert_eq!(
        link::verify_enroll_mac(secret, &card_n, id_e, h, link_head, &mac),
        Err(Error::BadMac)
    );
}

#[test]
fn mac_tamper_link_head_fails() {
    let (secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(secret, &card_n, id_e, head_n, link_head);
    let mut h = link_head;
    h.hash[31] ^= 1;
    assert_eq!(
        link::verify_enroll_mac(secret, &card_n, id_e, head_n, h, &mac),
        Err(Error::BadMac)
    );
}

#[test]
fn mac_wrong_secret_fails() {
    let (_secret, card_n, id_e, head_n, link_head) = mac_args();
    let mac = link::enroll_mac(&LINK_SECRET, &card_n, id_e, head_n, link_head);
    let wrong = [0x23; 32];
    assert_eq!(
        link::verify_enroll_mac(&wrong, &card_n, id_e, head_n, link_head, &mac),
        Err(Error::BadMac)
    );
}
