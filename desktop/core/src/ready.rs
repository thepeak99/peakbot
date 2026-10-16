//! Readiness wait (DESIGN §4.7).

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use url::Url;

use crate::sys::{Child, Probe, Sys};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyOutcome {
    Ready,
    ChildExited(Option<i32>),
    Timeout,
    TokenRejected,
    Cancelled,
}

/// Loop: quitting → Cancelled; child exited → ChildExited; probe
/// 200 → Ready; 401/403 → TokenRejected; else sleep POLL_INTERVAL until
/// timeout → Timeout.
pub fn wait_ready(
    sys: &impl Sys,
    url: &Url,
    timeout: Duration,
    mut child: Option<&mut dyn Child>,
    quitting: &AtomicBool,
) -> ReadyOutcome {
    use std::sync::atomic::Ordering;

    let start = sys.now();
    loop {
        if quitting.load(Ordering::Relaxed) {
            return ReadyOutcome::Cancelled;
        }
        if let Some(exit) = child
            .as_deref_mut()
            .and_then(|c| c.try_wait().ok().flatten())
        {
            return ReadyOutcome::ChildExited(exit);
        }
        match sys.probe(url) {
            // A listening socket counts as Ready for https (no TLS is
            // spoken — design #377 §6 T3).
            Probe::Status(200) | Probe::Listening => return ReadyOutcome::Ready,
            Probe::Status(401) | Probe::Status(403) => return ReadyOutcome::TokenRejected,
            Probe::Status(_) | Probe::Unreachable => {}
        }
        if sys.now() - start >= timeout {
            return ReadyOutcome::Timeout;
        }
        sys.sleep(crate::consts::POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{POLL_INTERVAL, READY_TIMEOUT};
    use crate::sys::Probe;
    use crate::testing::{ChildMode, FakeChild, FakeChildSpec, FakeSys};
    use std::sync::{Arc, atomic::AtomicBool};
    use std::time::Duration;

    fn probe_url() -> Url {
        Url::parse("http://127.0.0.1:7824/commands").unwrap()
    }

    // §9.1 case 33.
    #[test]
    fn t33_wait_ready_recovers_after_unreachable_polls() {
        let sys = FakeSys::new();
        let url = probe_url();
        sys.set_probe(
            url.to_string().as_str(),
            vec![
                Probe::Unreachable,
                Probe::Unreachable,
                Probe::Unreachable,
                Probe::Status(200),
            ],
        );
        let quitting = Arc::new(AtomicBool::new(false));
        let start = sys.now();
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::Ready);
        assert_eq!(
            sys.now() - start,
            3 * POLL_INTERVAL,
            "virtual time must be exactly 3 poll intervals (no real sleeping)"
        );
    }

    // §9.1 case 34.
    #[test]
    fn t34_wait_ready_times_out_without_real_sleeping() {
        let sys = FakeSys::new();
        let url = probe_url();
        sys.set_probe(url.to_string().as_str(), vec![Probe::Unreachable]);
        let quitting = Arc::new(AtomicBool::new(false));
        let start = sys.now();
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::Timeout);
        let elapsed = sys.now() - start;
        assert!(
            elapsed >= READY_TIMEOUT && elapsed <= READY_TIMEOUT + POLL_INTERVAL,
            "virtual elapsed {elapsed:?} must bracket READY_TIMEOUT"
        );
    }

    // §9.1 case 35.
    #[test]
    fn t35_wait_ready_token_rejected_and_503_then_ready() {
        // 401 → TokenRejected immediately.
        let sys = FakeSys::new();
        let url = probe_url();
        sys.set_probe(url.to_string().as_str(), vec![Probe::Status(401)]);
        let quitting = Arc::new(AtomicBool::new(false));
        let start = sys.now();
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::TokenRejected);
        assert_eq!(
            sys.now() - start,
            Duration::ZERO,
            "no poll may precede the 401"
        );

        // 503 then 200 → Ready.
        let sys = FakeSys::new();
        let url = probe_url();
        sys.set_probe(
            url.to_string().as_str(),
            vec![Probe::Status(503), Probe::Status(200)],
        );
        let quitting = Arc::new(AtomicBool::new(false));
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::Ready);
    }

    // §9.1 case 36.
    #[test]
    fn t36_wait_ready_child_exit_wins_before_timeout() {
        let sys = FakeSys::new();
        let url = probe_url();
        sys.set_probe(url.to_string().as_str(), vec![Probe::Unreachable]);
        let mut child = FakeChild::new(FakeChildSpec {
            mode: ChildMode::ExitsAfter(1),
            exit: Some(1),
            ..Default::default()
        });
        let quitting = Arc::new(AtomicBool::new(false));
        let out = wait_ready(&sys, &url, READY_TIMEOUT, Some(&mut child), &quitting);
        assert_eq!(out, ReadyOutcome::ChildExited(Some(1)));
    }

    // §9.1 case 37.
    #[test]
    fn t37_wait_ready_cancels_when_quitting() {
        let sys = FakeSys::new();
        let url = probe_url();
        let quitting = Arc::new(AtomicBool::new(true));
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::Cancelled);
    }

    // Design #377 t75: for https URLs the probe is a TCP connect (no TLS
    // is spoken — core ureq has no TLS, until #374); a listening socket
    // counts as Ready.
    #[test]
    fn t75_wait_ready_listening_counts_as_ready_for_https() {
        let sys = FakeSys::new();
        let url = Url::parse("https://127.0.0.1:7824/commands?token=tok").unwrap();
        sys.set_probe(
            url.to_string().as_str(),
            vec![Probe::Unreachable, Probe::Listening],
        );
        let quitting = Arc::new(AtomicBool::new(false));
        let start = sys.now();
        let out = wait_ready(&sys, &url, READY_TIMEOUT, None, &quitting);
        assert_eq!(out, ReadyOutcome::Ready, "Listening ⇒ Ready");
        assert_eq!(
            sys.now() - start,
            POLL_INTERVAL,
            "exactly one poll interval before the Listening answer"
        );
    }
}
