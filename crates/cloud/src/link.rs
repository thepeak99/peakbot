//! Optional QR/link enrollment (spec §6): link grammar, parsing, MAC.
//!
//! Grammar (ABNF, case-sensitive, spec §6.1):
//! ```text
//! link = "peakbot-cloud:v1?relay=" relay "&dev=" b64-32
//!      "&head=" seq ":" b64-32 "&s=" b64-32
//! ```
//! Parameters in exactly this order, each exactly once, no fragment,
//! total length ≤ 1024 bytes. `b64-32` is canonical base64url (43 chars,
//! low 2 bits of the last character zero). `relay` decodes to an https
//! origin (RFC 6454).

use crate::Head;
use crate::{DeviceId, Result};

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
        todo!()
    }

    /// Serialize to the canonical link string (spec §6.1).
    pub fn encode(&self) -> String {
        todo!()
    }
}

/// `mac_key = hkdf_sha3_256(secret, salt="pbc1 enroll",
/// info="pbc1 enroll mac key", 32)` (spec §6.3).
pub fn mac_key(secret: &[u8; 32]) -> [u8; 32] {
    todo!()
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
    todo!()
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
    todo!()
}
