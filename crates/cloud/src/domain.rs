//! Domain-separation tag registry (spec §1.4). One module, one tag per
//! use. A tag MUST NOT be used for any purpose other than the one listed.

/// T1 — ChainEntry signature (§4.3).
pub const CHAIN_SIG: &str = "pbc1 chain sig";
/// T2 — ChainEntry hash / chain head (§4.3).
pub const CHAIN_HASH: &str = "pbc1 chain hash";
/// T3 — SAS words (§5).
pub const SAS: &str = "pbc1 sas";
/// T4 — QR/link MAC key, HKDF salt (§6.3).
pub const ENROLL: &str = "pbc1 enroll";
/// T5 — QR/link MAC key, HKDF info (§6.3).
pub const ENROLL_MAC_KEY: &str = "pbc1 enroll mac key";
/// T6 — QR/link MAC input (§6.3).
pub const ENROLL_MAC: &str = "pbc1 enroll mac";
/// T7 — SignedLetter (§7.2).
pub const LETTER_SIG: &str = "pbc1 letter sig";
/// T8 — `v1::seal` AAD prefix for letters (§7.3).
pub const LETTER_AAD: &str = "pbc1 letter aad";
/// T9 — Relay authentication (§10.2).
pub const RELAY_HELLO: &str = "pbc1 relay hello";
/// T10 — Transcript hash after Hs1 (§8.3).
pub const HS_TH1: &str = "pbc1 hs th1";
/// T11 — Transcript hash after Hs2 body (§8.3).
pub const HS_TH2: &str = "pbc1 hs th2";
/// T12 — Transcript hash after sig_R (§8.3).
pub const HS_TH3: &str = "pbc1 hs th3";
/// T13 — Responder signature (§8.3).
pub const HS_SIG_R: &str = "pbc1 hs sig R";
/// T14 — Initiator signature (§8.3).
pub const HS_SIG_I: &str = "pbc1 hs sig I";
/// T15 — Initiator→Responder data key, HKDF info (§8.4).
pub const KEY_IR: &str = "pbc1 key IR";
/// T16 — Responder→Initiator data key, HKDF info (§8.4).
pub const KEY_RI: &str = "pbc1 key RI";
