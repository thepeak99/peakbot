//! Spinner frames for the "workin-baby" working indicator (the elapsed-time
//! half is `utils::fmt_elapsed`, shared with the core).
//!
//! Pure functions of `Instant` — no state, no thread, no drift. The render
//! loop (50 ms tick) is the clock; these helpers just compute what to show.
//!
//! See `workin-baby.md` §5 for the design rationale.

use std::time::Instant;

/// 10-frame braille spinner.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Which spinner frame to show for a run that started at `started`.
///
/// Advances roughly every 80 ms — fast enough to feel alive, slow enough
/// not to blur at a 50 ms render tick.
pub fn frame_for(started: Instant) -> &'static str {
    let idx = (started.elapsed().as_millis() / 80) as usize % FRAMES.len();
    FRAMES[idx]
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
    fn frame_for_returns_known_frame() {
        let f = frame_for(Instant::now());
        assert!(FRAMES.contains(&f), "frame {f:?} not in FRAMES table");
    }

    #[test]
    fn frame_for_cycles_through_all_frames() {
        // At 80 ms/frame with 10 frames, one full cycle is 800 ms. Sampling a
        // span covering that range must exercise every frame at least once.
        let mut seen = std::collections::HashSet::new();
        for ms in 0..800u64 {
            // Construct a fake "started" point `ms` milliseconds ago.
            let started = past(Duration::from_millis(ms));
            seen.insert(frame_for(started));
        }
        assert_eq!(
            seen.len(),
            FRAMES.len(),
            "expected every frame to appear across an 800ms window"
        );
    }

    #[test]
    fn frame_for_is_pure_for_same_instant() {
        // Same `Instant` input → same output. (Pure function of elapsed.)
        let started = past(Duration::from_millis(200));
        let a = frame_for(started);
        let b = frame_for(started);
        // Note: elapsed() moves forward between calls, so this can drift a
        // frame — accept either adjacent frame to keep the test non-flaky.
        let idx_a = FRAMES.iter().position(|&f| f == a).unwrap();
        let idx_b = FRAMES.iter().position(|&f| f == b).unwrap();
        let diff = (idx_b + FRAMES.len() - idx_a) % FRAMES.len();
        assert!(
            diff <= 1,
            "frames should be stable or adjacent for same `started`, got {a} then {b}"
        );
    }
}
