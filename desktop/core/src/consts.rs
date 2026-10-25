//! Constants (DESIGN §3.1). Values are locked; no logic.
use std::time::Duration;

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
