//! Q1 — background-service toggle on Windows (DESIGN amendment
//! (windows-portable), user decision:
//! **hide it on Windows in v1**).
//!
//! WHERE THE DECISION LIVES. The background row is not modeled in the Tauri
//! glue: it is produced by two PURE functions in the Tauri-free core —
//! `phase::tray_model()` (the tray's background checkbox) and
//! `settings::settings_view()` (the settings page's background row, which reads
//! the `TrayModel`). Today both emit the row unconditionally. So the "is the
//! background service offered on this host?" question has a natural home in
//! core, and it can be specified as a pure, platform-parameterized decision —
//! which means it is unit-testable on every host (no `cfg!(windows)` glue that
//! a Linux CI could never exercise).
//!
//! THE CONTRACT (RED until the developer lands it in `core/src/phase.rs`):
//!
//! ```rust
//! /// The host platform the app runs on. The glue decides it ONCE from
//! /// `cfg!(windows)` and passes it into the pure core.
//! #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//! pub enum Platform {
//!     Windows, // background service is non-functional in v1 (Q1)
//!     Unix,    // Linux/macOS — unchanged
//! }
//!
//! /// Q1: the background-service toggle is NOT offered on Windows in v1; on
//! /// Unix (Linux/macOS) it stays offered, unchanged.
//! pub fn background_offered(p: Platform) -> bool;
//! ```
//!
//! Wiring the developer completes (covered by the manual W11 check, not here):
//! the glue computes `Platform` from `cfg!(windows)` once; `TrayModel` carries
//! a `background_visible: bool` derived from `background_offered`; the tray
//! item and the settings row are hidden when it is false. `Platform::Unix`
//! behaviour must be byte-for-byte today's (this file pins the offer decision;
//! the existing `t31`/`t74` tests pin the rest).

use peakbot_desktop_core::phase::{Platform, background_offered};

#[test]
fn q1_background_service_is_not_offered_on_windows() {
    assert!(
        !background_offered(Platform::Windows),
        "Q1: the background-service toggle must NOT be offered on Windows in v1"
    );
}

#[test]
fn q1_background_service_is_still_offered_on_unix() {
    assert!(
        background_offered(Platform::Unix),
        "Q1: the background-service toggle must stay offered on Linux/macOS (Unix)"
    );
}
