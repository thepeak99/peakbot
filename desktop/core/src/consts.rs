//! Constants (DESIGN §3.1). Values are locked; no logic.
use std::time::Duration;

/// peakbot default; a desktop-installed service always binds 127.0.0.1:7823.
pub const SERVICE_PORT: u16 = 7823;
/// Tried first so the webview origin (localStorage/cookies) is stable.
pub const SESSION_PREFERRED_PORT: u16 = 7824;
/// Session spawn retries on "already in use".
pub const PORT_ATTEMPTS: u32 = 3;
/// Session readiness timeout.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Readiness / stop-poll interval.
pub const POLL_INTERVAL: Duration = Duration::from_millis(150);
/// Probe timeout.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// SIGINT → SIGKILL grace.
pub const STOP_GRACE: Duration = Duration::from_secs(3);
/// Short-lived sidecar commands.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Controller liveness check.
pub const TICK: Duration = Duration::from_millis(500);
/// Lines of session log kept for `SessionExited` details.
pub const LOG_TAIL_LINES: usize = 20;
