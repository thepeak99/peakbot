//! Optional QR/link enrollment (spec §6): link grammar, parsing, MAC.
//!
//! Grammar (ABNF, case-sensitive, spec §6.1):
//! ```text
//! link = "peakbot-cloud:v1?relay=" relay "&dev=" b64-32
//!      "&head=" seq ":" b64-32 "&s=" b64-32
//! ```
//! `relay = 1*( unreserved / pct-encoded )`: the relay https origin,
//! percent-encoded (RFC 3986) — raw `:` and `/` are not unreserved and are
//! rejected. Parameters in exactly this order, each exactly once, no
//! fragment, total length ≤ 1024 bytes. `b64-32` is canonical base64url
//! (43 chars, low 2 bits of the last character zero).

use crate::b64;
use crate::ce::push_bytes;
use crate::domain;
use crate::{DeviceId, Error, Head, Result};

const PREFIX: &str = "peakbot-cloud:v1?relay=";
/// Maximum total link length (spec §6.1).
pub const MAX_LINK: usize = 1024;

/// A parsed enrollment link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The relay https origin, e.g. `https://relay.example`.
    pub relay: String,
    /// DeviceId of E (the approver showing the link).
    pub dev: DeviceId,
    /// E's head when the link was made.
    pub head: Head,
    /// The 32-byte secret (spec §6.1).
    pub secret: [u8; 32],
}

impl Link {
    /// Parse a link string. Any grammar violation is `Error::BadLink`
    /// (spec §6.1): wrong order, duplicates, missing/extra params,
    /// fragment, >1024 bytes, non-canonical b64, leading-zero seq,
    /// non-https relay, non-ASCII.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > MAX_LINK {
            return Err(Error::BadLink);
        }
        if s.contains('#') {
            return Err(Error::BadLink);
        }
        let rest = s.strip_prefix(PREFIX).ok_or(Error::BadLink)?;
        let dev_at = rest.find("&dev=").ok_or(Error::BadLink)?;
        let relay = parse_origin(&percent_decode(&rest[..dev_at])?)?;
        let rest = &rest[dev_at + "&dev=".len()..];
        let head_at = rest.find("&head=").ok_or(Error::BadLink)?;
        let dev = b64::url_decode_32(&rest[..head_at])?;
        let rest = &rest[head_at + "&head=".len()..];
        let colon = rest.find(':').ok_or(Error::BadLink)?;
        let seq = parse_seq(&rest[..colon])?;
        let rest = &rest[colon + 1..];
        let s_at = rest.find("&s=").ok_or(Error::BadLink)?;
        let hash = b64::url_decode_32(&rest[..s_at])?;
        let secret = b64::url_decode_32(&rest[s_at + "&s=".len()..])?;
        Ok(Self {
            relay,
            dev,
            head: Head { seq, hash },
            secret,
        })
    }

    /// Serialize to the canonical link string (spec §6.1): the relay
    /// origin percent-encoded, canonical b64-32 fields.
    pub fn encode(&self) -> String {
        format!(
            "{PREFIX}{}&dev={}&head={}:{}&s={}",
            percent_encode_unreserved(&self.relay),
            b64::url_encode_32(&self.dev),
            self.head.seq,
            b64::url_encode_32(&self.head.hash),
            b64::url_encode_32(&self.secret),
        )
    }
}

/// `seq = "0" / ( %x31-39 *18DIGIT )`: decimal u64, no leading zeros.
fn parse_seq(s: &str) -> Result<u64> {
    if s.is_empty() || s.len() > 20 {
        return Err(Error::BadLink);
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(Error::BadLink);
    }
    s.parse::<u64>().map_err(|_| Error::BadLink)
}

/// Percent-decode (RFC 3986). Non-hex escapes and non-ASCII input are
/// `BadLink`.
fn percent_decode(s: &str) -> Result<String> {
    let b = s.as_bytes();
    if b.is_empty() {
        return Err(Error::BadLink);
    }
    let hex = |c: u8| -> Option<u8> { (c as char).to_digit(16).map(|d| d as u8) };
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return Err(Error::BadLink);
            }
            let hi = hex(b[i + 1]).ok_or(Error::BadLink)?;
            let lo = hex(b[i + 2]).ok_or(Error::BadLink)?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| Error::BadLink)
}

/// The decoded relay field MUST be an `https` origin (RFC 6454):
/// `https://host[:port]`, lowercase host, no path.
fn parse_origin(s: &str) -> Result<String> {
    let host_port = s.strip_prefix("https://").ok_or(Error::BadLink)?;
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (host_port, None),
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
    {
        return Err(Error::BadLink);
    }
    if let Some(p) = port {
        if p.is_empty() || p.len() > 5 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return Err(Error::BadLink);
        }
        if p.len() > 1 && p.starts_with('0') {
            return Err(Error::BadLink);
        }
    }
    Ok(s.to_string())
}

/// Percent-encode every non-unreserved byte (spec §6.1: the relay field is
/// `1*( unreserved / pct-encoded )`).
fn percent_encode_unreserved(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            out.push(c);
        } else {
            out.push('%');
            let b = (c as u8) as u32;
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

/// `mac_key = hkdf_sha3_256(secret, salt="pbc1 enroll",
/// info="pbc1 enroll mac key", 32)` (spec §6.3).
pub fn mac_key(secret: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    // A non-empty salt always succeeds; the zero fallback is unreachable.
    if let Ok(v) = v1_hkdf(
        secret,
        domain::ENROLL.as_bytes(),
        domain::ENROLL_MAC_KEY.as_bytes(),
    ) && v.len() == 32
    {
        out.copy_from_slice(&v);
    }
    out
}

fn v1_hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<Vec<u8>> {
    perma_crypto::v1::hkdf_sha3_256(ikm, salt, info, 32).map_err(crate::v1_err)
}

/// `mac = MAC(mac_key; "pbc1 enroll mac";
/// bytes(card_N) ‖ fix[32](id_E) ‖ head(head_N) ‖ head(link_head))`
/// (spec §6.3). Binds the full card, the approver, N's head and the link
/// head.
pub fn enroll_mac(
    secret: &[u8; 32],
    card_n: &[u8],
    id_e: DeviceId,
    head_n: Head,
    link_head: Head,
) -> [u8; 32] {
    let mut m = Vec::with_capacity(4 + card_n.len() + 32 + 40 + 40);
    push_bytes(&mut m, card_n);
    m.extend_from_slice(&id_e);
    m.extend_from_slice(&head_n.ce());
    m.extend_from_slice(&link_head.ce());
    crate::mac(&mac_key(secret), domain::ENROLL_MAC, &m)
}

/// Constant-time verification of an enrollment MAC (spec §1.3: MAC
/// comparisons MUST be constant-time). Mismatch is `Error::BadMac`.
pub fn verify_enroll_mac(
    secret: &[u8; 32],
    card_n: &[u8],
    id_e: DeviceId,
    head_n: Head,
    link_head: Head,
    mac: &[u8],
) -> Result<()> {
    let mut m = Vec::with_capacity(4 + card_n.len() + 32 + 40 + 40);
    push_bytes(&mut m, card_n);
    m.extend_from_slice(&id_e);
    m.extend_from_slice(&head_n.ce());
    m.extend_from_slice(&link_head.ce());
    crate::mac_verify(&mac_key(secret), domain::ENROLL_MAC, &m, mac)
}
