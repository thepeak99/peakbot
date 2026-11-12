//! base64url (RFC 4648 §5) helpers.
//!
//! The 32-byte form is 43 characters, no padding. It MUST be canonical:
//! the unused low 2 bits of the last character are zero, so one secret has
//! exactly one text form (spec §6.1). Used for DeviceId text (§3) and the
//! link `b64-32` fields (§6.1).

use crate::Error;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encode exactly 32 bytes as canonical base64url (43 chars, no padding).
pub fn url_encode_32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(43);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(char::from(ALPHABET[((acc >> bits) & 0x3F) as usize]));
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        // Trailing 4 bits, padded with zeros: the canonical form (§6.1).
        out.push(char::from(ALPHABET[((acc << (6 - bits)) & 0x3F) as usize]));
    }
    out
}

/// Decode a canonical base64url token to 32 bytes.
///
/// Rejects: wrong length, non-alphabet characters, padding, and
/// non-canonical encodings (low 2 bits of the last character set) with
/// `Error::BadLink`.
pub fn url_decode_32(s: &str) -> Result<[u8; 32], Error> {
    let b = s.as_bytes();
    if b.len() != 43 {
        return Err(Error::BadLink);
    }
    let mut value = [0u8; 43];
    for (i, &c) in b.iter().enumerate() {
        value[i] = ALPHABET
            .iter()
            .position(|&a| a == c)
            .ok_or(Error::BadLink)? as u8;
    }
    // Canonical: the 4 trailing bits of the last character are zero.
    if value[42] & 0x03 != 0 {
        return Err(Error::BadLink);
    }
    let mut out = [0u8; 32];
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut n = 0usize;
    for v in value {
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if n < 32 {
                out[n] = (acc >> bits) as u8;
                n += 1;
            }
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// The short display form of a DeviceId: first 8 characters of the
/// 43-character text form. Display only; MUST NOT be used for checks
/// (spec §3).
pub fn short_id(id: &[u8; 32]) -> String {
    let full = url_encode_32(id);
    full[..8].to_string()
}
