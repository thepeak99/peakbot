//! Cooperative pause gate for running sub-agents.
//!
//! A [`PauseGate`] lets a sub-agent voluntarily *park* at checkpoints
//! ([`PauseGate::checkpoint`]) while a pause is requested. Time spent parked
//! is tracked on the gate's clock so that budgets can exclude it — see
//! [`pause_aware_timeout`].
//!
//! Pausing ≠ parking: [`PauseGate::request_pause`] only sets the request;
//! parked time starts counting only when a checkpoint actually waits.

use std::{future::Future, sync::Mutex, time::Duration};
use tokio::sync::watch;
use tokio::time::Instant;

/// Cooperative pause gate for a running sub-agent.
///
/// `pause_requested` is a watch channel (`true` = pause requested) so any
/// number of parked checkpoints can be woken by a single [`PauseGate::resume`].
/// The clock tracks the ongoing park (if any) and the accumulated parked time.
#[derive(Debug)]
pub struct PauseGate {
    pause_requested: watch::Sender<bool>,
    /// True while a checkpoint is actually *parked* (waiting), so observers
    /// (the UI) can show `Paused` rather than just `Pausing`.
    parked: watch::Sender<bool>,
    clock: Mutex<Clock>,
}

/// Park clock: when the ongoing park started (if any) and how much parked
/// time has accumulated from previous parks.
#[derive(Debug, Default)]
struct Clock {
    paused_since: Option<Instant>,
    accumulated: Duration,
}

/// RAII guard finalizing the park clock when a [`PauseGate::checkpoint`]
/// future ends — normally on resume, or by *drop* when Stop cancels the turn.
/// Without it a dropped parked checkpoint would leave `paused_since` set
/// forever and wedge `is_parked()` / `pause_aware_timeout`.
struct ParkGuard<'a> {
    gate: &'a PauseGate,
}

impl Drop for ParkGuard<'_> {
    fn drop(&mut self) {
        self.gate.finalize_park();
    }
}

impl PauseGate {
    /// Create a gate with no pause requested and an empty clock.
    pub fn new() -> Self {
        let (pause_requested, _rx) = watch::channel(false);
        let (parked, _rx) = watch::channel(false);
        Self {
            pause_requested,
            parked,
            clock: Mutex::new(Clock::default()),
        }
    }

    /// Request a pause. Idempotent: repeated calls do not change the state.
    pub fn request_pause(&self) {
        self.pause_requested.send_replace(true);
    }

    /// Clear the pause request and release any parked checkpoints.
    /// Idempotent: a call with no request pending is a no-op.
    pub fn resume(&self) {
        // `send_if_modified` only notifies when the flag actually flips, so a
        // redundant resume is a true no-op (no spurious wakeups).
        let _ = self.pause_requested.send_if_modified(|v| {
            if *v {
                *v = false;
                true
            } else {
                false
            }
        });
    }

    /// Whether a pause has been requested (regardless of whether anyone is parked).
    pub fn is_pause_requested(&self) -> bool {
        *self.pause_requested.borrow()
    }

    /// Whether a [`PauseGate::checkpoint`] is currently waiting (parked).
    pub fn is_parked(&self) -> bool {
        self.clock.lock().unwrap().paused_since.is_some()
    }

    /// Subscribe to parked/unparked transitions so observers (the UI) can show
    /// `Paused` once a checkpoint actually parks, not just `Pausing`.
    pub fn subscribe_parked(&self) -> watch::Receiver<bool> {
        self.parked.subscribe()
    }

    /// Park while a pause is requested.
    ///
    /// Returns immediately if no pause is requested. Otherwise records the
    /// park start, waits until [`PauseGate::resume`] (or `reset`), and adds
    /// the parked time to the clock. The clock is finalized by an RAII guard,
    /// so dropping this future while parked (Stop cancels the turn) still
    /// unwinds the gate and clears [`PauseGate::is_parked`].
    pub async fn checkpoint(&self) {
        if !self.is_pause_requested() {
            return;
        }
        // Start the clock (first parker wins) and tell observers we're parked.
        {
            let mut clock = self.clock.lock().unwrap();
            if clock.paused_since.is_none() {
                clock.paused_since = Some(Instant::now());
            }
        }
        let _ = self.parked.send_if_modified(|p| {
            if !*p {
                *p = true;
                true
            } else {
                false
            }
        });

        // Finalizes on every exit — resume, reset, or a drop of this future.
        let _guard = ParkGuard { gate: self };

        let mut rx = self.pause_requested.subscribe();
        while *rx.borrow_and_update() {
            // Sender dropped (gate gone) — stop waiting; the guard finalizes.
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Total parked time: accumulated parks plus the ongoing park (if any).
    pub fn paused_total(&self) -> Duration {
        let clock = self.clock.lock().unwrap();
        let ongoing = clock
            .paused_since
            .map(|since| since.elapsed())
            .unwrap_or_default();
        clock.accumulated + ongoing
    }

    /// Clear the pause request and zero the clock.
    pub fn reset(&self) {
        let mut clock = self.clock.lock().unwrap();
        // Discard any ongoing park without accumulating: a reset means the run
        // is over, so its parked time must not leak into the next run's budget.
        clock.paused_since = None;
        clock.accumulated = Duration::ZERO;
        drop(clock);
        self.pause_requested.send_replace(false);
        let _ = self.parked.send_if_modified(|p| {
            if *p {
                *p = false;
                true
            } else {
                false
            }
        });
    }

    /// Fold the ongoing park into `accumulated` and clear `paused_since`.
    /// Called by [`ParkGuard`] on every exit of [`PauseGate::checkpoint`];
    /// no-op if the clock was already cleared (e.g. by [`PauseGate::reset`]).
    fn finalize_park(&self) {
        let ended_park = {
            let mut clock = self.clock.lock().unwrap();
            let since = clock.paused_since.take();
            if let Some(since) = since {
                clock.accumulated += since.elapsed();
            }
            since.is_some()
        };
        // Only signal unpark if we actually ended a park, so a concurrent
        // parker's still-active park is not reported as unparked.
        if ended_park {
            let _ = self.parked.send_if_modified(|p| {
                if *p {
                    *p = false;
                    true
                } else {
                    false
                }
            });
        }
    }

    /// Wait until no checkpoint is parked. Lets [`pause_aware_timeout`] honor
    /// "never expire while parked"; returns immediately if already unparked.
    async fn wait_while_parked(&self) {
        let mut rx = self.parked.subscribe();
        while *rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return; // gate dropped
            }
        }
    }
}

impl Default for PauseGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned by [`pause_aware_timeout`] when the budget is exhausted.
#[derive(Debug, PartialEq, Eq)]
pub struct PauseAwareElapsed;

/// Like `tokio::time::timeout`, but time spent parked in `gate` doesn't count
/// toward `budget`.
///
/// The deadline is frozen while a checkpoint is parked and resumes counting
/// when the gate is released; work done after a resume still counts. A parked
/// sub-agent can be paused indefinitely without ever expiring.
pub async fn pause_aware_timeout<F: Future>(
    gate: &PauseGate,
    budget: Duration,
    fut: F,
) -> Result<F::Output, PauseAwareElapsed> {
    let start = Instant::now();
    let mut fut = Box::pin(fut);
    loop {
        // Never sleep while parked. While a checkpoint waits, the deadline must
        // not be enforced; wait for the park to end or the future to finish.
        //
        // Polling `fut` here — not just waiting on the `parked` watch — avoids
        // the deadlock where `fut` *is* the parked checkpoint. The park can only
        // end by polling `fut` (that's where `ParkGuard` drops), and waiting
        // solely for the `parked`-watch flip would block the very poll that
        // releases it. `tokio::select!` over both branches resolves this.
        if gate.is_parked() {
            tokio::select! {
                result = &mut fut => return Ok(result),
                _ = gate.wait_while_parked() => {}
            }
        }

        // Parked time extends the deadline, so it never counts against budget.
        let deadline = start + budget + gate.paused_total();
        tokio::select! {
            result = &mut fut => return Ok(result),
            _ = tokio::time::sleep_until(deadline) => {
                if gate.is_parked() {
                    // Still parked: loop back; the top waits for unpark.
                    continue;
                }
                // A park that just ended pushed the deadline out — recompute.
                if start + budget + gate.paused_total() > Instant::now() {
                    continue;
                }
                return Err(PauseAwareElapsed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Spawn a `checkpoint()` task on `gate` and yield once so the spawned
    /// task is polled and (with a pause requested) actually parks, recording
    /// `paused_since` at the current virtual time.
    async fn spawn_parked_checkpoint(gate: &Arc<PauseGate>) -> tokio::task::JoinHandle<()> {
        let g = gate.clone();
        let handle = tokio::spawn(async move { g.checkpoint().await });
        tokio::task::yield_now().await;
        handle
    }

    /// Happy path: no pause requested → checkpoint returns without parking.
    #[tokio::test(start_paused = true)]
    async fn checkpoint_returns_immediately_when_not_paused() {
        let gate = Arc::new(PauseGate::new());

        let result = tokio::time::timeout(Duration::from_millis(1), gate.checkpoint()).await;

        assert!(
            result.is_ok(),
            "checkpoint must return immediately when no pause is requested"
        );
    }

    /// After `request_pause`, a checkpoint parks: it does not complete on its
    /// own, `is_parked()` is true, and a single `resume()` releases it.
    #[tokio::test(start_paused = true)]
    async fn checkpoint_parks_after_request_pause_and_releases_on_resume() {
        let gate = Arc::new(PauseGate::new());
        gate.request_pause();
        assert!(
            gate.is_pause_requested(),
            "request_pause must be observable"
        );

        let h = spawn_parked_checkpoint(&gate).await;
        assert!(
            gate.is_parked(),
            "checkpoint must report parked while waiting"
        );

        // Virtual time passing must not release a parked checkpoint.
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(
            !h.is_finished(),
            "parked checkpoint must not complete before resume"
        );

        gate.resume();
        h.await
            .expect("checkpoint task must complete after resume without panicking");

        assert!(
            !gate.is_parked(),
            "is_parked must be false once the checkpoint is released"
        );
    }

    /// Double `request_pause` and double `resume` must not panic or corrupt
    /// state, and a single resume must be enough to release the gate.
    #[tokio::test(start_paused = true)]
    async fn request_pause_and_resume_are_idempotent() {
        let gate = Arc::new(PauseGate::new());

        gate.request_pause();
        gate.request_pause();
        assert!(
            gate.is_pause_requested(),
            "state must stay requested after a double request"
        );

        gate.resume();
        gate.resume();
        assert!(
            !gate.is_pause_requested(),
            "a second resume must not re-pause the gate"
        );

        // The gate must be clean: a checkpoint returns immediately.
        let result = tokio::time::timeout(Duration::from_millis(1), gate.checkpoint()).await;
        assert!(
            result.is_ok(),
            "checkpoint after double resume must return immediately"
        );
    }

    /// Park 10s, resume, park 5s, resume → paused_total == 15s. While a park
    /// is ongoing, paused_total must include the ongoing portion.
    #[tokio::test(start_paused = true)]
    async fn paused_total_accumulates_across_park_periods_and_includes_ongoing() {
        let gate = Arc::new(PauseGate::new());

        // Park 1 — 10 virtual seconds.
        gate.request_pause();
        let h = spawn_parked_checkpoint(&gate).await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(
            gate.paused_total(),
            Duration::from_secs(10),
            "an ongoing park must count toward paused_total"
        );
        gate.resume();
        h.await.expect("checkpoint task must not panic");

        // Park 2 — 5 virtual seconds.
        gate.request_pause();
        let h = spawn_parked_checkpoint(&gate).await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            gate.paused_total(),
            Duration::from_secs(15),
            "paused_total must be accumulated + ongoing"
        );
        gate.resume();
        h.await.expect("checkpoint task must not panic");

        assert_eq!(
            gate.paused_total(),
            Duration::from_secs(15),
            "total parked time across both parks must be 15s"
        );
    }

    /// Requesting a pause does not start the clock: with no parked checkpoint,
    /// paused_total stays zero no matter how much time passes.
    #[tokio::test(start_paused = true)]
    async fn paused_total_is_zero_when_request_pause_never_parked() {
        let gate = PauseGate::new();
        gate.request_pause();

        tokio::time::advance(Duration::from_secs(5)).await;

        assert_eq!(
            gate.paused_total(),
            Duration::ZERO,
            "pausing is not parking: no checkpoint waited, so no time may count"
        );
    }

    /// `reset` clears a pending request (a subsequent checkpoint returns
    /// immediately) and zeroes the clock, even after real park time.
    #[tokio::test(start_paused = true)]
    async fn reset_clears_pending_request_and_zeroes_paused_total() {
        let gate = Arc::new(PauseGate::new());

        // Build up 3s of real park time first.
        gate.request_pause();
        let h = spawn_parked_checkpoint(&gate).await;
        tokio::time::advance(Duration::from_secs(3)).await;
        gate.resume();
        h.await.expect("checkpoint task must not panic");
        assert_eq!(
            gate.paused_total(),
            Duration::from_secs(3),
            "sanity: park time recorded"
        );

        // A pending request with no parked checkpoint.
        gate.request_pause();
        assert!(gate.is_pause_requested());

        gate.reset();

        assert!(
            !gate.is_pause_requested(),
            "reset must clear the pause request"
        );
        assert_eq!(
            gate.paused_total(),
            Duration::ZERO,
            "reset must zero the clock"
        );

        let result = tokio::time::timeout(Duration::from_millis(1), gate.checkpoint()).await;
        assert!(
            result.is_ok(),
            "checkpoint after reset must return immediately"
        );
    }

    /// Happy path: the future finishes within the budget → Ok(output).
    #[tokio::test(start_paused = true)]
    async fn pause_aware_timeout_ok_when_future_finishes_within_budget() {
        let gate = PauseGate::new();

        let fut = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            42
        };

        let result = pause_aware_timeout(&gate, Duration::from_secs(1), fut).await;

        assert_eq!(result, Ok(42));
    }

    /// No pause anywhere: the budget expires and the error is
    /// `PauseAwareElapsed`, not a panic or a hang.
    #[tokio::test(start_paused = true)]
    async fn pause_aware_timeout_errs_when_budget_expires_without_pause() {
        let gate = PauseGate::new();

        let fut = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            42
        };

        let result = pause_aware_timeout(&gate, Duration::from_secs(1), fut).await;

        assert!(
            matches!(result, Err(PauseAwareElapsed)),
            "an over-budget future must yield Err(PauseAwareElapsed)"
        );
    }

    /// The core contract: with a 1s budget, a future that parks at the gate
    /// for 1 virtual hour and then does 100ms of work must still succeed —
    /// parked time does not count against the budget.
    #[tokio::test(start_paused = true)]
    async fn pause_aware_timeout_excludes_time_parked_in_gate() {
        let gate = Arc::new(PauseGate::new());
        gate.request_pause();

        let inner_gate = gate.clone();
        let fut = async move {
            inner_gate.checkpoint().await; // parks for 1 virtual hour
            tokio::time::sleep(Duration::from_millis(100)).await;
            "done"
        };

        // A side task parks the future for 1 virtual hour, then resumes. It
        // waits for `is_parked()` so the clock only advances after the future
        // has actually reached the gate.
        let side_gate = gate.clone();
        let waiter = tokio::spawn(async move {
            while !side_gate.is_parked() {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_secs(3600)).await;
            side_gate.resume();
        });

        let result = pause_aware_timeout(&gate, Duration::from_secs(1), fut).await;

        assert_eq!(
            result,
            Ok("done"),
            "time parked in the gate must not count against the budget"
        );
        waiter.await.expect("waiter task must not panic");
    }

    /// Parked time is excluded, but work still counts: 500ms of work, a 1-hour
    /// park, then 700ms of work with a 1s budget must time out (500ms + 700ms
    /// > 1s of real work).
    #[tokio::test(start_paused = true)]
    async fn pause_aware_timeout_still_expires_after_resume_when_work_exceeds_budget() {
        let gate = Arc::new(PauseGate::new());
        gate.request_pause();

        let inner_gate = gate.clone();
        let fut = async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            inner_gate.checkpoint().await; // parks for 1 virtual hour
            tokio::time::sleep(Duration::from_millis(700)).await;
            "done"
        };

        let side_gate = gate.clone();
        let waiter = tokio::spawn(async move {
            // Block on the `parked` watch — do NOT spin with `yield_now`.
            // Under `start_paused = true` the clock only auto-advances when
            // no task is ready to run; a yield-spin keeps a task perpetually
            // woken, which suppresses the very advance that lets the 500ms of
            // pre-park work elapse, so the future would never reach the
            // checkpoint and this test could never finish.
            let mut rx = side_gate.subscribe_parked();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    return; // gate dropped
                }
            }
            tokio::time::advance(Duration::from_secs(3600)).await;
            side_gate.resume();
        });

        let result = pause_aware_timeout(&gate, Duration::from_secs(1), fut).await;

        assert!(
            matches!(result, Err(PauseAwareElapsed)),
            "work after resume still counts against the budget (500ms + 700ms > 1s)"
        );
        waiter.await.expect("waiter task must not panic");
    }
}
