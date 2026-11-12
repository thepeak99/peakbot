//! base64url (RFC 4648 §5) helpers.
//!
//! The 32-byte form is 43 characters, no padding. It MUST be canonical:
//! the unused low 2 bits of the last character are zero, so one secret has
//! exactly one text form (spec §6.1). Used for DeviceId text (§3) and the
//! link `b64-32` fields (§6.1).

use crate::Error;

/// Encode exactly 32 bytes as canonical base64url (43 chars, no padding).
pub fn url_encode_32(bytes: &[u8; 32]) -> String {
    todo!()
}

/// Decode a canonical base64url token to 32 bytes.
///
/// Rejects: wrong length, non-alphabet characters, padding, and
/// non-canonical encodings (low 2 bits of the last character set) with
/// `Error::BadLink`.
pub fn url_decode_32(s: &str) -> Result<[u8; 32], Error> {
    todo!()
}

/// The short display form of a DeviceId: first 8 characters of the
/// 43-character text form. Display only; MUST NOT be used for checks
/// (spec §3).
pub fn short_id(id: &[u8; 32]) -> &str {
    todo!()
}
