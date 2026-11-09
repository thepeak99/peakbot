//! Elapsed-time formatting shared by the TUI spinner and the core's
//! status messages.

use std::time::Instant;

/// Format elapsed time since `started` as `MM:SS` (under an hour) or
/// `HH:MM:SS` (one hour or more). Zero-padded, monospaced-friendly.
pub fn fmt_elapsed(started: Instant) -> String {
    let s = started.elapsed().as_secs();
    if s < 3600 {
        format!("{:02}:{:02}", s / 60, s % 60)
    } else {
        format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Helper: make an `Instant` that's `d` in the past.
    fn past(d: Duration) -> Instant {
        Instant::now().checked_sub(d).expect("time travel failed")
    }

    #[test]
    fn fmt_elapsed_zero_seconds() {
        assert_eq!(fmt_elapsed(Instant::now()), "00:00");
    }
    #[test]
    fn fmt_elapsed_under_one_minute() {
        assert_eq!(fmt_elapsed(past(Duration::from_secs(7))), "00:07");
        assert_eq!(fmt_elapsed(past(Duration::from_secs(59))), "00:59");
    }
    #[test]
    fn fmt_elapsed_minute_boundary() {
        assert_eq!(fmt_elapsed(past(Duration::from_secs(60))), "01:00");
        assert_eq!(fmt_elapsed(past(Duration::from_secs(61))), "01:01");
    }
    #[test]
    fn fmt_elapsed_just_under_an_hour() {
        assert_eq!(fmt_elapsed(past(Duration::from_secs(3599))), "59:59");
    }
    #[test]
    fn fmt_elapsed_hour_boundary_switches_to_hhmmss() {
        assert_eq!(fmt_elapsed(past(Duration::from_secs(3600))), "01:00:00");
        assert_eq!(fmt_elapsed(past(Duration::from_secs(3661))), "01:01:01");
    }
}
