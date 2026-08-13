//! Reasoning/thinking-block carrier for Anthropic (and Anthropic-compatible
//! via the Messages API) provider responses.
//!
//! Sibling to `src/vision.rs` (which models `ImageAttachment` — the
//! existing precedent for "opaque provider payload that rides the
//! transcript to the wire").
//!
//! One Anthropic thinking block, preserved verbatim. The signature is a
//! provider-issued MAC over the thinking text: Anthropic rejects (400)
//! any replay where it is missing, re-encoded or re-wrapped, so it is
//! stored as an opaque `String` and never trimmed, lowercased, logged
//! or summarised.

use serde::{Deserialize, Serialize};

/// One Anthropic reasoning block, preserved verbatim.
///
/// Variants are an enum rather than `{ text, signature: Option<String> }`
/// so the redacted case has no text — making it a type-level distinction
/// means the display path and the summariser-exclusion boundary are
/// enforced by `match`, not by discipline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThinkingBlock {
    /// `thinking` block: visible-to-us reasoning text + its signature.
    Thinking {
        /// The reasoning text. May be empty (Anthropic's "display: omitted"
        /// variant, where the full reasoning rides in `signature`).
        text: String,
        /// Provider-issued MAC. MUST round-trip verbatim — never trimmed,
        /// re-encoded, logged, or normalised. Empty when the source
        /// `ReasoningContent::Text` had `signature: None`; the wire seam
        /// then drops the block (replaying an unsigned block is a 400).
        signature: String,
    },
    /// `redacted_thinking` block: opaque ciphertext. Must be replayed
    /// as-is; there is nothing inside for us to display, log, summarise,
    /// or pass to the summariser.
    Redacted {
        /// Opaque ciphertext payload.
        data: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enum must serialise with a `kind` tag and the variant's
    /// fields under the same object — that's the wire shape the
    /// summariser-skip and the JSON-round-trip paths both rely on.
    #[test]
    fn thinking_block_serialises_with_kind_tag() {
        let block = ThinkingBlock::Thinking {
            text: "t".to_string(),
            signature: "SIG".to_string(),
        };
        let s = serde_json::to_string(&block).expect("encode");
        // Tag must be `kind` (design §1.1); variant name in snake_case.
        assert!(s.contains("\"kind\":\"thinking\""), "got: {s}");
        assert!(s.contains("\"text\":\"t\""), "got: {s}");
        assert!(s.contains("\"signature\":\"SIG\""), "got: {s}");
    }

    /// The signature is the safety-sensitive part of the contract. The
    /// byte content must survive a JSON round-trip — whitespace,
    /// encoding, non-ASCII, edge characters all preserved.
    #[test]
    fn thinking_block_signature_round_trips_byte_identical() {
        let original = ThinkingBlock::Thinking {
            text: "reasoning line 1\nline 2 with \"quotes\" and \\backslash".to_string(),
            signature: "sig.《αβγ》-unicode-edges-==".to_string(),
        };

        let json = serde_json::to_string(&original).expect("encode");
        let restored: ThinkingBlock = serde_json::from_str(&json).expect("decode");

        assert_eq!(
            restored, original,
            "ThinkingBlock must round-trip byte-identical through serde",
        );
    }

    /// The Redacted variant preserves the opaque payload; it contributes
    /// no text. The wall between "an opaque thing to replay" and "a
    /// thing a renderer could leak" is the type level.
    #[test]
    fn redacted_block_preserves_data_through_round_trip() {
        let original = ThinkingBlock::Redacted {
            data: "opaque-payload-77a1".to_string(),
        };

        let json = serde_json::to_string(&original).expect("encode");
        assert!(
            json.contains("\"kind\":\"redacted\""),
            "redacted variant must tag as `redacted`; got: {json}",
        );
        assert!(
            !json.contains("\"text\""),
            "redacted variant must NOT carry a `text` field; got: {json}",
        );

        let restored: ThinkingBlock = serde_json::from_str(&json).expect("decode");
        assert_eq!(restored, original);
    }

    /// Thinking-without-signature is the wire-strip seam: empty sig →
    /// dropped at the wire rebuild. The enum permits it (the capture
    /// path produces it from `ReasoningContent::Text { signature: None }`)
    /// but the *type* flags it so the wire seam can filter on it.
    #[test]
    fn thinking_block_with_empty_signature_is_constructible() {
        let b = ThinkingBlock::Thinking {
            text: "visible-only thinking".to_string(),
            signature: String::new(),
        };
        assert!(matches!(
            b,
            ThinkingBlock::Thinking { ref signature, .. } if signature.is_empty()
        ));
    }

    /// Cross-variant disambiguation: a `Thinking` block and a `Redacted`
    /// block round-trip to the *same* wire kind-tag space without
    /// one being misinterpreted as the other.
    #[test]
    fn variants_are_disambiguated_by_kind_tag() {
        let t = ThinkingBlock::Thinking {
            text: "x".into(),
            signature: "s".into(),
        };
        let r = ThinkingBlock::Redacted { data: "x".into() };

        let t_json = serde_json::to_string(&t).unwrap();
        let r_json = serde_json::to_string(&r).unwrap();

        assert!(t_json.contains("\"kind\":\"thinking\""));
        assert!(r_json.contains("\"kind\":\"redacted\""));

        // Redacted-shaped JSON decodes as the `Redacted` variant (not the
        // `Thinking` variant). That's the disambiguation contract — the
        // kind tag routes to the matching struct shape, and the missing
        // `text`/`signature` fields on `Thinking` are what make a
        // deserialised-as-Thinking impossible here.
        let decoded: ThinkingBlock = serde_json::from_str(&r_json)
            .expect("redacted-kind JSON must decode as a ThinkingBlock");
        assert!(
            matches!(decoded, ThinkingBlock::Redacted { .. }),
            "a redacted-kind JSON must decode as Redacted, not Thinking; got {decoded:?}",
        );
    }
}
