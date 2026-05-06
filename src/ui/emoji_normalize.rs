//! Normalise text for terminal rendering.
//!
//! ## Why this module exists
//!
//! `unicode-width 0.2` (the cell-width oracle ratatui consults for layout)
//! disagrees with most Linux terminal emulators (vte / gnome-terminal,
//! Alacritty, kitty, foot) on three classes of glyphs. Each disagreement
//! is a +1 column drift on the line containing the glyph. When the user
//! scrolls, ratatui re-emits visible rows from a clean slate, so the
//! drift becomes visible as "garbled columns" / displaced borders /
//! scrollbar smear. macOS Terminal.app and iTerm2 use width tables that
//! match `unicode-width`, so the bug only manifests on Linux. See
//! `garbled.md` at the repo root for the full triage.
//!
//! ## What this module does
//!
//! Strips presentation-selector and zero-width "joiner-class" codepoints
//! at the renderer input boundary so the *bytes* ratatui measures match
//! what the terminal actually advances by. Callers feed every chunk of
//! text destined for a `Span` / `Line` through [`normalize_for_terminal`]
//! before construction. The result is a `Cow<'_, str>` so ASCII-only
//! input (the common case) costs zero allocations.
//!
//! ### Stripped codepoints
//!
//! | CP        | Name                              | Why strip |
//! |-----------|-----------------------------------|-----------|
//! | `U+200C`  | Zero-Width Non-Joiner (ZWNJ)      | width disagreement |
//! | `U+200D`  | Zero-Width Joiner (ZWJ)           | width disagreement |
//! | `U+FE0E`  | Variation Selector-15 (text)      | redundant; some terminals advance |
//! | `U+FE0F`  | Variation Selector-16 (emoji)     | the prime offender |
//! | `U+20E3`  | Combining Enclosing Keycap        | rendered, never advances correctly |
//!
//! Bidi controls (`U+202A`–`U+202E`, `U+2066`–`U+2069`) are *not*
//! stripped — they're a different class of bug and removing them can
//! mangle RTL text. Out of scope.
//!
//! ## Performance
//!
//! Fast path: a single byte-scan. The codepoints we strip all live in
//! the `U+2000–U+FFFF` BMP range, which means UTF-8 byte sequences of
//! the form `0xE2 0x80 0xXX` (ZWNJ/ZWJ), `0xE2 0x83 0xA3` (keycap), or
//! `0xEF 0xB8 0x8E/0x8F` (VS15/VS16). If neither `0xE2` nor `0xEF` is
//! present in the input, we hand back `Cow::Borrowed` immediately —
//! no allocation, no codepoint walk. The slow path runs only on input
//! that *might* contain a disagreement byte.

use std::borrow::Cow;

/// Strip presentation-selectors and zero-width joiner codepoints that
/// cause `unicode-width` ↔ terminal disagreement.
///
/// See the module docs for the why and the codepoint list. Returns
/// `Cow::Borrowed(input)` when the input cannot contain any of the
/// stripped codepoints (fast path); otherwise returns
/// `Cow::Owned(filtered)`.
pub fn normalize_for_terminal(input: &str) -> Cow<'_, str> {
    // Fast path: every stripped codepoint encodes to UTF-8 starting
    // with either 0xE2 or 0xEF. If neither byte appears, the input
    // is guaranteed clean — no allocation, no char walk.
    if !input.bytes().any(|b| b == 0xE2 || b == 0xEF) {
        return Cow::Borrowed(input);
    }

    // Slow path: filter codepoints. Single allocation sized to the
    // input length (the output is always ≤ the input).
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if !is_stripped(ch) {
            out.push(ch);
        }
    }
    Cow::Owned(out)
}

/// Whether a codepoint should be stripped from rendered text.
///
/// Keep this `#[inline]` and branchless-ish — it runs once per
/// codepoint on the slow path.
#[inline]
fn is_stripped(c: char) -> bool {
    matches!(
        c,
        '\u{200C}'   // ZERO WIDTH NON-JOINER
        | '\u{200D}' // ZERO WIDTH JOINER
        | '\u{FE0E}' // VARIATION SELECTOR-15 (text presentation)
        | '\u{FE0F}' // VARIATION SELECTOR-16 (emoji presentation)
        | '\u{20E3}' // COMBINING ENCLOSING KEYCAP
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────── Fast-path proofs ───────────────────────────
    //
    // ASCII-only and "BMP-but-no-suspect-bytes" inputs must hand back
    // a `Cow::Borrowed`. If these fail we've regressed to allocating
    // on every render call.

    #[test]
    fn ascii_only_is_borrowed() {
        let input = "hello world";
        let out = normalize_for_terminal(input);
        assert!(matches!(out, Cow::Borrowed(_)), "ASCII must not allocate");
        assert_eq!(out, "hello world");
    }

    #[test]
    fn empty_is_borrowed() {
        let out = normalize_for_terminal("");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "");
    }

    #[test]
    fn supplementary_plane_emoji_is_borrowed() {
        // 🤖 = U+1F916 = F0 9F A4 96 — no 0xE2/0xEF, must take the
        // fast path. This is critical: the most common emoji in our
        // codebase (🤖, 👤, 🔧, 📋, 📝) all live in U+1Fxxx and must
        // not pay the slow-path tax.
        let input = "🤖 Agent: hi";
        let out = normalize_for_terminal(input);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "supplementary-plane emoji without VS16 must not allocate, got Owned({out:?})"
        );
        assert_eq!(out, "🤖 Agent: hi");
    }

    // ─────────────────────────── Stripping behaviour ────────────────────────

    #[test]
    fn vs16_is_stripped_from_warning_emoji() {
        // ⚠️ = U+26A0 + U+FE0F. VS16 is the prime garbling offender.
        // After stripping, only U+26A0 remains.
        let input = "\u{26A0}\u{FE0F}";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "\u{26A0}");
        assert!(!out.contains('\u{FE0F}'));
    }

    #[test]
    fn vs16_is_stripped_from_info_emoji() {
        // ℹ️ = U+2139 + U+FE0F
        let input = "\u{2139}\u{FE0F}";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "\u{2139}");
    }

    #[test]
    fn vs16_is_stripped_from_gear_emoji() {
        // ⚙️ = U+2699 + U+FE0F
        let input = "\u{2699}\u{FE0F}";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "\u{2699}");
    }

    #[test]
    fn zwj_is_stripped() {
        // ZWJ alone — strips cleanly.
        let input = "a\u{200D}b";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "ab");
    }

    #[test]
    fn zwnj_is_stripped() {
        let input = "a\u{200C}b";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "ab");
    }

    #[test]
    fn vs15_is_stripped() {
        let input = "x\u{FE0E}y";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "xy");
    }

    #[test]
    fn keycap_combiner_is_stripped() {
        // 1️⃣ = '1' + VS16 + COMBINING ENCLOSING KEYCAP
        // After stripping VS16 and the keycap combiner, just '1' remains.
        let input = "1\u{FE0F}\u{20E3}";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "1");
    }

    #[test]
    fn mixed_content_preserves_safe_chars() {
        // The "hard test": real-world string with a Class-A emoji
        // mid-line, plus ASCII context, plus a fast-path emoji.
        // Only the VS16 must vanish.
        let input = "[!] \u{26A0}\u{FE0F} warning from 🤖";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "[!] \u{26A0} warning from 🤖");
    }

    #[test]
    fn slow_path_returns_owned() {
        // Sanity: when we DO strip, the result must be Cow::Owned.
        let input = "\u{26A0}\u{FE0F}";
        let out = normalize_for_terminal(input);
        assert!(matches!(out, Cow::Owned(_)));
    }

    // ───────────────────────── False-positive coverage ──────────────────────
    //
    // The fast path triggers on 0xE2 or 0xEF — which means *any* string
    // containing a codepoint encoded with those leading bytes will take
    // the slow path even if no actual stripping happens. That's fine
    // (still correct output), but pin the behaviour so future "optimise
    // the byte-scan" PRs don't break correctness.

    #[test]
    fn false_positive_em_dash_is_unchanged() {
        // — = U+2014 = E2 80 94 — triggers the slow path but nothing strips.
        let input = "hello — world";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "hello — world");
    }

    #[test]
    fn false_positive_check_mark_is_unchanged() {
        // ✓ = U+2713 = E2 9C 93 — no VS16, terminal disagreement is
        // a Class-B problem (kitty), out of scope for this normaliser.
        let input = "ok \u{2713}";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "ok \u{2713}");
    }

    #[test]
    fn check_emoji_is_unchanged() {
        // ✅ = U+2705 — no VS16, Class-B; we deliberately don't touch it.
        let input = "done ✅";
        let out = normalize_for_terminal(input);
        assert_eq!(out, "done ✅");
    }

    // ───────────────────────────── Stress cases ─────────────────────────────

    #[test]
    fn vs16_at_string_boundaries() {
        // VS16 at start / end / both — all must vanish.
        assert_eq!(normalize_for_terminal("\u{FE0F}hello"), "hello");
        assert_eq!(normalize_for_terminal("hello\u{FE0F}"), "hello");
        assert_eq!(normalize_for_terminal("\u{FE0F}\u{FE0F}"), "");
    }

    #[test]
    fn many_vs16_in_row() {
        // Pathological input: 100 VS16s. Must not panic, must produce
        // empty string.
        let input: String = "\u{FE0F}".repeat(100);
        let out = normalize_for_terminal(&input);
        assert_eq!(out, "");
    }
}
