//! SAS: six-word enrollment check (spec §5).
//!
//! ```text
//! d = H("pbc1 sas"; bytes(card) ‖ head(h))
//! x = first 66 bits of d (MSB-first); w[i] = 11-bit groups of x
//! words = BIP-39-english[w[i]]
//! ```
//!
//! `SasWords` is constructible ONLY by `words` (design S3): the value
//! freezes the (card, head) pair at construction. There is no API that
//! re-derives words with a new head (spec §5.5 R2, the pure-API part of
//! the ordering rule).

use crate::ce::push_bytes;
use crate::domain;
use crate::Head;

/// Six SAS words. Constructible only by `crate::sas::words` /
/// `words_from_digest`; the fields are private, so the head used is
/// frozen into the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SasWords {
    /// The six BIP-39 indices (each 0..=2047).
    indices: [u16; 6],
}

impl SasWords {
    /// The six BIP-39 indices in order.
    pub fn as_indices(&self) -> [u16; 6] {
        self.indices
    }

    /// The six words from the embedded BIP-39 English list.
    pub fn words(&self) -> [&'static str; 6] {
        let list = bip39_english();
        [
            list[self.indices[0] as usize],
            list[self.indices[1] as usize],
            list[self.indices[2] as usize],
            list[self.indices[3] as usize],
            list[self.indices[4] as usize],
            list[self.indices[5] as usize],
        ]
    }

    /// Display string: two rows of three, numbered, e.g.
    /// `1 word  2 word  3 word\n4 word  5 word  6 word` (spec §5.3).
    /// Both screens use identical formatting.
    pub fn display(&self) -> String {
        let w = self.words();
        format!(
            "1 {}  2 {}  3 {}\n4 {}  5 {}  6 {}",
            w[0], w[1], w[2], w[3], w[4], w[5]
        )
    }
}

/// The embedded BIP-39 English word list: 2048 words, index 0 = `abandon`,
/// index 2047 = `zoo`. SHA-256 of the list joined with `\n` plus a final
/// `\n` is `2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda`
/// (spec §5.3; asserted in tests).
pub fn bip39_english() -> &'static [&'static str] {
    crate::bip39::BIP39_ENGLISH.as_slice()
}

/// The SAS digest `d` (spec §5.1):
/// `sha3_256(u32(8) ‖ "pbc1 sas" ‖ u32(len(card)) ‖ card ‖ u64(h.seq) ‖ fix[32](h.hash))`.
/// `card` is the exact CE card bytes.
pub fn digest(card: &[u8], head: &Head) -> [u8; 32] {
    let mut m = Vec::with_capacity(4 + card.len() + 40);
    push_bytes(&mut m, card);
    m.extend_from_slice(&head.ce());
    crate::h(domain::SAS, &m)
}

/// Six SAS words for a card and a head (spec §5.1–§5.3). 66-bit
/// MSB-first extraction, BIP-39 mapping.
pub fn words(card: &[u8], head: &Head) -> SasWords {
    words_from_digest(&digest(card, head))
}

/// Six SAS words directly from a digest (spec §5.2). Exposed so the bit
/// extraction can be tested against edge digests (all-zero / all-0xFF,
/// spec §15 `sas_edge`) without a full card.
pub fn words_from_digest(d: &[u8; 32]) -> SasWords {
    // x72 = d[0..9] as big-endian; x = top 66 bits (d[0] bit 7 is MSB).
    let mut x72 = 0u128;
    for b in &d[..9] {
        x72 = (x72 << 8) | u128::from(*b);
    }
    let x = x72 >> 6;
    let mut indices = [0u16; 6];
    for (i, slot) in indices.iter_mut().enumerate() {
        *slot = ((x >> (55 - 11 * i as u32)) & 0x7FF) as u16;
    }
    SasWords { indices }
}
