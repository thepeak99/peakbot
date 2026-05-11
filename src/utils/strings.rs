//! String utilities for safe UTF-8 handling.

// ─── Truncation helpers ─────────────────────────────────────────────────────

/// Truncate `s` to at most `max_bytes`, snapping to the nearest UTF-8
/// character boundary. Returns the full slice if `s` is already short enough.
///
/// Never panics on multi-byte content (unlike `&s[..max_bytes]`).
#[inline]
pub fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // floor_char_boundary is stable in Rust 1.86+.
    let end = s.floor_char_boundary(max_bytes);
    &s[..end]
}

/// Truncate `s` to at most `max_bytes` bytes, appending `suffix` if truncation
/// occurred. The `suffix` length is accounted for — if `suffix.len() > max_bytes`,
/// the suffix is returned as-is (the caller should guard against this).
///
/// Never panics on multi-byte content.
#[inline]
pub fn truncate_with_suffix(s: &str, max_bytes: usize, suffix: &str) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let snapped = truncate_to_char_boundary(s, max_bytes);
    format!("{}{suffix}", snapped)
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{truncate_to_char_boundary, truncate_with_suffix};

    // Pin for gitea issue #9.

    #[test]
    fn truncate_snap_on_multibyte_boundary() {
        // "🦀" is 4 bytes. One ASCII + 75 crabs = 301 bytes, boundaries at
        // 0, 1, 5, 9, …, 297, 301. Byte 300 lands mid-codepoint.
        let s = format!("a{}", "🦀".repeat(75));
        assert_eq!(s.len(), 301);
        assert!(!s.is_char_boundary(300));

        let out = truncate_to_char_boundary(&s, 300);
        assert_eq!(out.len(), 297);
        assert!(s.is_char_boundary(out.len()));
        assert!(s.starts_with(out));
    }

    #[test]
    fn truncate_passthrough_when_short_enough() {
        assert_eq!(truncate_to_char_boundary("hello", 300), "hello");
        assert_eq!(truncate_to_char_boundary("", 10), "");
    }

    #[test]
    fn truncate_ascii_exact_cut() {
        let s = "a".repeat(500);
        let out = truncate_to_char_boundary(&s, 300);
        assert_eq!(out.len(), 300);
    }

    #[test]
    fn truncate_zero_budget_returns_empty() {
        assert_eq!(truncate_to_char_boundary("hello", 0), "");
    }

    #[test]
    fn truncate_mixed_ascii_and_multibyte() {
        // 250 ASCII + 50 × 4-byte emoji = 450 bytes. Byte 300 lands inside
        // the emoji starting at byte 298.
        let s = format!("{}{}", "a".repeat(250), "🦀".repeat(50));
        assert!(s.len() > 300);
        let out = truncate_to_char_boundary(&s, 300);
        assert!(out.len() <= 300);
        assert!(s.is_char_boundary(out.len()));
        assert!(s.starts_with(out));
    }

    #[test]
    fn truncate_with_suffix_passthrough() {
        assert_eq!(truncate_with_suffix("hello", 300, "..."), "hello");
    }

    #[test]
    fn truncate_with_suffix_appends_suffix() {
        let s = "a".repeat(500);
        let out = truncate_with_suffix(&s, 300, "...");
        assert!(out.ends_with("..."));
        // Suffix is appended after the snap, so total bytes ≈ 300 + "...".len()
        assert!(out.len() <= 303);
    }

    #[test]
    fn truncate_with_suffix_multibyte() {
        let s = format!("a{}", "🦀".repeat(75));
        let out = truncate_with_suffix(&s, 300, "...");
        assert!(out.ends_with("..."));
        // Last bytes before "..." are always on a valid boundary.
        let without_suffix = &out[..out.len() - 3];
        assert!(s.is_char_boundary(without_suffix.len()));
        assert!(s.starts_with(without_suffix));
    }
}
