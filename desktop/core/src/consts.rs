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
/// Feed connect timeout (loopback / LAN).
pub const NOTIFY_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Feed idle limit: 3 missed 15 s server keepalives ⇒ the stream is dead.
pub const NOTIFY_READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Reconnect backoff cap.
pub const NOTIFY_RETRY_MAX: Duration = Duration::from_secs(30);
