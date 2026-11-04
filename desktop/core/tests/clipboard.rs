//! Tests for `peakbot_desktop_core::clipboard::checked_png` — the 50 MB
//! bound on the clipboard PNG crossing IPC (DESIGN §15 / design §5.4):
//! the bound is on the ENCODED PNG, over-cap is an `Err` and never empty
//! (invariant I1: empty must mean only "no image"), and empty input
//! stays the no-image wire value. Pure logic — no GTK, no webkit.
//!
//! RED before implementation: `peakbot_desktop_core::clipboard` does
//! not exist yet, so this file fails to compile for exactly the
//! "feature absent" reason.

use peakbot_desktop_core::clipboard::{MAX_PNG_MB, checked_png};

#[test]
fn checked_png_passes_bytes_at_the_cap() {
    let at_cap = vec![0u8; MAX_PNG_MB * 1024 * 1024];

    let got = checked_png(at_cap.clone()).expect("exactly at the cap passes");

    assert_eq!(
        got.len(),
        MAX_PNG_MB * 1024 * 1024,
        "same length, unchanged"
    );
    assert_eq!(got, at_cap);
}

#[test]
fn checked_png_rejects_one_byte_over_the_cap() {
    let over = vec![0u8; MAX_PNG_MB * 1024 * 1024 + 1];

    let err = checked_png(over).expect_err("one byte over the cap is refused");

    assert_eq!(
        err, "clipboard image is larger than 50 MB",
        "the reason is built from MAX_PNG_MB (invariant I5)"
    );
}

#[test]
fn checked_png_passes_empty_through() {
    // Empty is the "no image" wire value — it must stay empty, never
    // become an error (otherwise "nothing there" would look like a
    // failure).
    let got = checked_png(Vec::new()).expect("empty passes through");
    assert!(got.is_empty());
}
