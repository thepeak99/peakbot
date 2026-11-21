//! Foreground-resume planning (p3-design §E.4).
//!
//! On mobile the runtime only delivers `WindowEvent::Resumed` as a
//! foreground signal. That must resume the cloud link (foreground) AND
//! nudge the SPA (nudge) — wiring only to `Focused(true)` leaves the
//! cloud link parked in `Suspended` on Android (device never reconnects,
//! `/api/cloud` returns 409 not_active, chats stay "Loading…").

#[derive(Debug, PartialEq)]
pub enum ForegroundSignal {
    /// `WindowEvent::Resumed` — the sole foreground signal on Android.
    Resumed,
    /// `WindowEvent::Focused(true)` — the foreground signal on iOS.
    Focused,
}

#[derive(Debug, PartialEq)]
pub enum ResumeStep {
    /// Tell the core the app is in the foreground so it can resume its
    /// cloud socket after the background debounce.
    Foreground,
    /// Re-serve the Local core if its listener was reclaimed and nudge
    /// the SPA to re-dial its socket.
    Nudge,
}

const RESUMED_PLAN: [ResumeStep; 2] = [ResumeStep::Foreground, ResumeStep::Nudge];

const FOCUSED_PLAN: [ResumeStep; 1] = [ResumeStep::Foreground];

/// The foreground-resume plan, pure. Takes the foreground signal and
/// returns the ordered list of actions the shell must perform. This is
/// host-testable and used by the mobile shell's event wiring.
pub fn resume_steps(signal: ForegroundSignal) -> &'static [ResumeStep] {
    match signal {
        ForegroundSignal::Resumed => &RESUMED_PLAN,
        ForegroundSignal::Focused => &FOCUSED_PLAN,
    }
}
