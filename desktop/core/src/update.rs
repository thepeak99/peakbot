//! TDD tests for `core::update` (the desktop auto-updater core) —
//! written BEFORE the implementation against the locked plan's
//! `core::update` section (API, step table, view() table, menu_label
//! strings, wire JSON, run_loop behaviour) and spike S2 rec 4.
//!
//! RED until implemented: this file only compiles once `update.rs`
//! provides the items the plan locks in (`Bundle`, `supported`,
//! `State`, `Event`, `InstallOutcome`, `Effect`, `step`, `due`,
//! `menu_label`, `UpdateView`, `view`, `UpdateSys`, `run_loop`,
//! `run_loop_with`).
//!
//! Assumptions the implementer must honour (the plan leaves them open):
//! 1. `Version` is `String` (the plan: "implementer's choice — keep it
//!    simple").
//! 2. `pub enum Dialog { UpToDate, CheckFailed(String), InstallFailed(String) }`.
//!    The `String` payloads carry the RAW error message; the plan's
//!    "You're up to date" / "Update failed: e" are prose labels for the
//!    notification, not payload prefixes.
//! 3. `Bundle`, `State`, `Effect`, `Dialog`, `InstallOutcome` and
//!    `UpdateView` derive `Debug + PartialEq`; `State`, `Dialog` and
//!    `InstallOutcome` additionally derive `Clone`.
//! 4. `due(last, now)` treats a backwards clock (`now < last`) as DUE:
//!    a skewed clock must never block update checks forever.
//! 5. `run_loop` must be testable without waiting 1h:
//!    `pub fn run_loop_with(sys: impl UpdateSys, rx: Receiver<Event>, tick: Duration)`
//!    carries the loop body (`tick` = the `recv_timeout` interval), and
//!    `pub fn run_loop(sys: impl UpdateSys, rx: Receiver<Event>)` is the
//!    1h wrapper (`tick = Duration::from_secs(3600)`).
//! 6. The macOS `/AppTranslocation/` gate (spike S2 rec 4) lives at the
//!    glue edge, NOT in `supported(bundle, debug)`: the signature stays
//!    as the plan locked it (no path parameter), so a translocated .app
//!    simply never maps to `Bundle::MacApp`.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime};

use serde::Serialize;

/// A release version string (the plan: keep it simple — `String`).
pub type Version = String;

/// The updatable bundle kinds. Everything else (portable exe, dev
/// builds) is not updatable — the plugin would happily install a
/// second copy (spike S2 Q2), so the glue maps its bundle type here.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Bundle {
    AppImage,
    MacApp,
    Nsis,
}

/// Updatable only in a release build of a known bundle type.
pub fn supported(bundle: Option<Bundle>, debug: bool) -> bool {
    !debug && bundle.is_some()
}

/// The update state machine (plan step table). `Checking.known`
/// remembers the banner version across a re-check so a failed check
/// never hides an update we already know about.
#[derive(Debug, PartialEq, Clone)]
pub enum State {
    Idle,
    Checking {
        manual: bool,
        known: Option<Version>,
    },
    Available(Version),
    Downloading(Version),
    Installing(Version),
    Failed(Version),
}

/// One input to the state machine. `CheckDone`/`Installing`/
/// `InstallOutcome` are fed back by the loop after a `RunCheck`/
/// `RunInstall` effect; `Click`/`Install` come from the tray/banner.
pub enum Event {
    Check { manual: bool },
    Click,
    Install,
    CheckDone(Result<Option<Version>, String>),
    Installing(Version),
    InstallOutcome(InstallOutcome),
}

/// The install result. Success never returns (the process is being
/// replaced) — `Gone` is the only success-shaped outcome.
#[derive(Debug, PartialEq, Clone)]
pub enum InstallOutcome {
    Gone,
    Failed(String),
}

/// A user-facing dialog. The `String` payloads carry the raw error
/// message; the glue adds the prose.
#[derive(Debug, PartialEq, Clone)]
pub enum Dialog {
    UpToDate,
    CheckFailed(String),
    InstallFailed(String),
}

/// What the loop must do after a `step` (run inline on the loop
/// thread, in this order).
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    RunCheck,
    RunInstall,
    Notify(Dialog),
}

/// The plan's step table; unlisted (state, event) pairs are ignored
/// (single-flight: one check and one install at a time).
pub fn step(s: &State, e: Event) -> (State, Effect) {
    match (s, e) {
        (State::Idle, Event::Check { manual }) => (
            State::Checking {
                manual,
                known: None,
            },
            Effect::RunCheck,
        ),
        (State::Available(v) | State::Failed(v), Event::Check { manual }) => (
            State::Checking {
                manual,
                known: Some(v.clone()),
            },
            Effect::RunCheck,
        ),
        // Single-flight: the running check continues; a manual check
        // only upgrades `manual` so its result may notify.
        (State::Checking { manual, known }, Event::Check { manual: promote })
            if promote && !*manual =>
        {
            (
                State::Checking {
                    manual: true,
                    known: known.clone(),
                },
                Effect::None,
            )
        }
        (State::Checking { .. }, Event::CheckDone(Ok(Some(v)))) => {
            (State::Available(v), Effect::None)
        }
        (State::Checking { manual, .. }, Event::CheckDone(Ok(None))) => {
            let effect = if *manual {
                Effect::Notify(Dialog::UpToDate)
            } else {
                Effect::None
            };
            (State::Idle, effect)
        }
        (State::Checking { manual, known }, Event::CheckDone(Err(e))) => {
            let state = match known {
                Some(v) => State::Available(v.clone()),
                None => State::Idle,
            };
            let effect = if *manual {
                Effect::Notify(Dialog::CheckFailed(e))
            } else {
                Effect::None
            };
            (state, effect)
        }
        (State::Idle, Event::Click) => (
            State::Checking {
                manual: true,
                known: None,
            },
            Effect::RunCheck,
        ),
        // Click = the tray item; Install = the web banner button. Both
        // start the same download+install on Available/Failed.
        (State::Available(v) | State::Failed(v), Event::Click | Event::Install) => {
            (State::Downloading(v.clone()), Effect::RunInstall)
        }
        (State::Downloading(_), Event::Installing(v)) => (State::Installing(v), Effect::None),
        (
            State::Downloading(_) | State::Installing(_),
            Event::InstallOutcome(InstallOutcome::Gone),
        ) => (State::Idle, Effect::None),
        (
            State::Downloading(v) | State::Installing(v),
            Event::InstallOutcome(InstallOutcome::Failed(e)),
        ) => (
            State::Failed(v.clone()),
            Effect::Notify(Dialog::InstallFailed(e)),
        ),
        _ => (s.clone(), Effect::None),
    }
}

/// The 24h check cadence. `now < last` (a skewed clock) counts as due:
/// a backwards clock must never block update checks forever.
pub fn due(last: SystemTime, now: SystemTime) -> bool {
    match now.duration_since(last) {
        Ok(elapsed) => elapsed >= Duration::from_secs(24 * 3600),
        Err(_) => true,
    }
}

/// The tray menu item's `(label, enabled)` for a state (plan table).
pub fn menu_label(s: &State) -> (String, bool) {
    match s {
        State::Idle => ("Check for updates…".to_string(), true),
        State::Checking { .. } => ("Checking for updates…".to_string(), false),
        State::Available(v) => (format!("Update available — v{v}"), true),
        State::Downloading(v) => (format!("Downloading v{v}…"), false),
        State::Installing(v) => (format!("Installing v{v}…"), false),
        State::Failed(v) => (format!("Update to v{v} failed — retry"), true),
    }
}

/// The web banner's view (wire: `null` | tagged object, `state` in
/// lowercase — plan). `Checking{known:Some(v)}` keeps the banner up
/// during a re-check.
#[derive(Debug, PartialEq, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum UpdateView {
    Available { version: String },
    Installing,
    Failed { version: String },
}

/// The plan's view table.
pub fn view(s: &State) -> Option<UpdateView> {
    match s {
        State::Idle => None,
        State::Checking { known, .. } => known
            .as_ref()
            .map(|v| UpdateView::Available { version: v.clone() }),
        State::Available(v) => Some(UpdateView::Available { version: v.clone() }),
        State::Downloading(_) | State::Installing(_) => Some(UpdateView::Installing),
        State::Failed(v) => Some(UpdateView::Failed { version: v.clone() }),
    }
}

/// The I/O the loop drives (glue: tauri-plugin-updater + tray + banner
/// state). All calls are blocking; the loop owns its own thread.
pub trait UpdateSys {
    fn check(&self) -> Result<Option<Version>, String>;
    fn install(&self, on_installing: &mut dyn FnMut(Version)) -> InstallOutcome;
    fn notify(&self, d: &Dialog);
    fn render(&self, s: &State);
    fn now(&self) -> SystemTime;
}

/// The production loop: a 1h `tick`, re-checking only when `due`.
pub fn run_loop(sys: impl UpdateSys, rx: Receiver<Event>) {
    run_loop_with(sys, rx, Duration::from_secs(3600));
}

/// The loop body (`tick` = the `recv_timeout` interval, so tests run a
/// short tick against a faked `now()`). Sends itself the startup
/// `Check{manual:false}`; after every step it renders, and effects run
/// inline — a check's result is fed back through `step` as `CheckDone`,
/// an install's through `Installing`/`InstallOutcome`.
pub fn run_loop_with(sys: impl UpdateSys, rx: Receiver<Event>, tick: Duration) {
    let mut state = State::Idle;
    sys.render(&state);
    let mut last_check = sys.now();

    // The startup check enters through the same path as any other event.
    let mut pending = Some(Event::Check { manual: false });
    loop {
        if pending.is_none() {
            match rx.recv_timeout(tick) {
                Ok(e) => pending = Some(e),
                // The tick only re-checks when the 24h cadence is due;
                // otherwise it is a no-op (state unchanged, no render).
                Err(RecvTimeoutError::Timeout) => {
                    if due(last_check, sys.now()) {
                        pending = Some(Event::Check { manual: false });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let Some(event) = pending.take() else {
            continue;
        };
        let (next, effect) = step(&state, event);
        state = next;
        sys.render(&state);
        process_effect(&sys, &mut state, &mut last_check, effect);
    }
}

/// Run one effect inline; the fed-back result step may yield a
/// `Notify` (never another `Run*`), so this terminates.
fn process_effect(
    sys: &impl UpdateSys,
    state: &mut State,
    last_check: &mut SystemTime,
    effect: Effect,
) {
    match effect {
        Effect::None => {}
        Effect::Notify(d) => sys.notify(&d),
        Effect::RunCheck => {
            *last_check = sys.now();
            let (next, effect) = step(state, Event::CheckDone(sys.check()));
            *state = next;
            sys.render(state);
            process_effect(sys, state, last_check, effect);
        }
        Effect::RunInstall => {
            let outcome = sys.install(&mut |v| {
                let (next, _) = step(state, Event::Installing(v));
                *state = next;
                sys.render(state);
            });
            let (next, effect) = step(state, Event::InstallOutcome(outcome));
            *state = next;
            sys.render(state);
            process_effect(sys, state, last_check, effect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    /// A point of the fake clock, in seconds since the Unix epoch.
    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Run `f` (polling every 5ms) until it holds, failing after 10s —
    /// logical-state polling, so these tests never depend on exact
    /// timing, only on the loop eventually doing the right thing.
    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_step(state: &State, event: Event, want: (State, Effect)) {
        let got = step(state, event);
        assert_eq!(got, want, "step from {state:?}");
    }

    // ── supported() ─────────────────────────────────────────────────────

    #[test]
    fn supported_true_for_appimage_macapp_and_nsis_in_release() {
        for b in [Bundle::AppImage, Bundle::MacApp, Bundle::Nsis] {
            assert!(
                supported(Some(b), false),
                "{b:?} in a release build is updatable"
            );
        }
    }

    #[test]
    fn supported_false_when_there_is_no_bundle() {
        // Windows portable exe / unrecognised bundle: no checks, no tray
        // item, no banner (the updater would happily install a second
        // copy — spike S2 Q2).
        assert!(!supported(None, false));
    }

    #[test]
    fn supported_false_for_every_bundle_in_a_debug_build() {
        for b in [Bundle::AppImage, Bundle::MacApp, Bundle::Nsis] {
            assert!(
                !supported(Some(b), true),
                "{b:?}: dev builds never auto-update"
            );
        }
        assert!(!supported(None, true));
    }

    // ── step(): the full plan table ─────────────────────────────────────

    // Check — from the resting states.
    #[test]
    fn step_check_from_idle_starts_a_background_check() {
        assert_step(
            &State::Idle,
            Event::Check { manual: false },
            (
                State::Checking {
                    manual: false,
                    known: None,
                },
                Effect::RunCheck,
            ),
        );
    }

    #[test]
    fn step_check_from_available_remembers_the_version_as_known() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Available(v.clone()),
            Event::Check { manual: false },
            (
                State::Checking {
                    manual: false,
                    known: Some(v.clone()),
                },
                Effect::RunCheck,
            ),
        );
    }

    #[test]
    fn step_check_from_failed_remembers_the_version_as_known() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Failed(v.clone()),
            Event::Check { manual: true },
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Effect::RunCheck,
            ),
        );
    }

    #[test]
    fn step_manual_check_upgrades_a_running_background_check() {
        let v = "0.25.0".to_string();
        // Single-flight: the running check continues, only `manual`
        // flips (so its result may notify).
        assert_step(
            &State::Checking {
                manual: false,
                known: Some(v.clone()),
            },
            Event::Check { manual: true },
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Effect::None,
            ),
        );
    }

    #[test]
    fn step_check_is_ignored_while_a_check_is_running() {
        assert_step(
            &State::Checking {
                manual: true,
                known: None,
            },
            Event::Check { manual: false },
            (
                State::Checking {
                    manual: true,
                    known: None,
                },
                Effect::None,
            ),
        );
        assert_step(
            &State::Checking {
                manual: false,
                known: None,
            },
            Event::Check { manual: false },
            (
                State::Checking {
                    manual: false,
                    known: None,
                },
                Effect::None,
            ),
        );
    }

    #[test]
    fn step_check_is_ignored_while_downloading_or_installing() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Downloading(v.clone()),
            Event::Check { manual: false },
            (State::Downloading(v.clone()), Effect::None),
        );
        assert_step(
            &State::Installing(v.clone()),
            Event::Check { manual: true },
            (State::Installing(v.clone()), Effect::None),
        );
    }

    // CheckDone — the check result lands.
    #[test]
    fn step_check_done_fresh_result_finds_the_update() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Checking {
                manual: false,
                known: None,
            },
            Event::CheckDone(Ok(Some(v.clone()))),
            (State::Available(v.clone()), Effect::None),
        );
    }

    #[test]
    fn step_check_done_up_to_date_notifies_only_when_manual() {
        assert_step(
            &State::Checking {
                manual: true,
                known: None,
            },
            Event::CheckDone(Ok(None)),
            (State::Idle, Effect::Notify(Dialog::UpToDate)),
        );
        assert_step(
            &State::Checking {
                manual: false,
                known: None,
            },
            Event::CheckDone(Ok(None)),
            (State::Idle, Effect::None),
        );
    }

    #[test]
    fn step_check_done_up_to_date_drops_a_stale_known_version() {
        // The server says "no update" — the remembered banner version
        // goes away with it.
        let v = "0.25.0".to_string();
        assert_step(
            &State::Checking {
                manual: false,
                known: Some(v),
            },
            Event::CheckDone(Ok(None)),
            (State::Idle, Effect::None),
        );
    }

    #[test]
    fn step_check_done_error_falls_back_to_the_known_version() {
        // A failed check never hides an update we already know about.
        let v = "0.25.0".to_string();
        assert_step(
            &State::Checking {
                manual: false,
                known: Some(v.clone()),
            },
            Event::CheckDone(Err("network down".to_string())),
            (State::Available(v.clone()), Effect::None),
        );
        assert_step(
            &State::Checking {
                manual: false,
                known: None,
            },
            Event::CheckDone(Err("network down".to_string())),
            (State::Idle, Effect::None),
        );
    }

    #[test]
    fn step_check_done_error_notifies_only_when_manual() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Checking {
                manual: true,
                known: Some(v.clone()),
            },
            Event::CheckDone(Err("network down".to_string())),
            (
                State::Available(v.clone()),
                Effect::Notify(Dialog::CheckFailed("network down".to_string())),
            ),
        );
        assert_step(
            &State::Checking {
                manual: true,
                known: None,
            },
            Event::CheckDone(Err("network down".to_string())),
            (
                State::Idle,
                Effect::Notify(Dialog::CheckFailed("network down".to_string())),
            ),
        );
    }

    // Click — the tray "Check for updates…" / banner button.
    #[test]
    fn step_click_from_idle_triggers_a_manual_check() {
        assert_step(
            &State::Idle,
            Event::Click,
            (
                State::Checking {
                    manual: true,
                    known: None,
                },
                Effect::RunCheck,
            ),
        );
    }

    #[test]
    fn step_click_on_available_starts_the_install() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Available(v.clone()),
            Event::Click,
            (State::Downloading(v.clone()), Effect::RunInstall),
        );
    }

    #[test]
    fn step_click_on_failed_retries_the_install() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Failed(v.clone()),
            Event::Click,
            (State::Downloading(v.clone()), Effect::RunInstall),
        );
    }

    #[test]
    fn step_click_is_ignored_while_checking_downloading_or_installing() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Checking {
                manual: true,
                known: Some(v.clone()),
            },
            Event::Click,
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Effect::None,
            ),
        );
        assert_step(
            &State::Downloading(v.clone()),
            Event::Click,
            (State::Downloading(v.clone()), Effect::None),
        );
        assert_step(
            &State::Installing(v.clone()),
            Event::Click,
            (State::Installing(v.clone()), Effect::None),
        );
    }

    // Install — the web UI's "Restart to update" (and retry) button.
    #[test]
    fn step_install_on_available_starts_the_install() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Available(v.clone()),
            Event::Install,
            (State::Downloading(v.clone()), Effect::RunInstall),
        );
    }

    #[test]
    fn step_install_on_failed_retries_the_install() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Failed(v.clone()),
            Event::Install,
            (State::Downloading(v.clone()), Effect::RunInstall),
        );
    }

    #[test]
    fn step_install_is_ignored_outside_available_and_failed() {
        let v = "0.25.0".to_string();
        assert_step(&State::Idle, Event::Install, (State::Idle, Effect::None));
        assert_step(
            &State::Checking {
                manual: true,
                known: Some(v.clone()),
            },
            Event::Install,
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Effect::None,
            ),
        );
        assert_step(
            &State::Downloading(v.clone()),
            Event::Install,
            (State::Downloading(v.clone()), Effect::None),
        );
        assert_step(
            &State::Installing(v.clone()),
            Event::Install,
            (State::Installing(v.clone()), Effect::None),
        );
    }

    // Installing / InstallOutcome — the install in flight.
    #[test]
    fn step_installing_moves_downloading_to_installing() {
        let v = "0.25.0".to_string();
        let v2 = "0.25.1".to_string();
        assert_step(
            &State::Downloading(v),
            Event::Installing(v2.clone()),
            (State::Installing(v2), Effect::None),
        );
    }

    #[test]
    fn step_installing_is_ignored_outside_downloading() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Idle,
            Event::Installing(v.clone()),
            (State::Idle, Effect::None),
        );
        assert_step(
            &State::Available(v.clone()),
            Event::Installing(v.clone()),
            (State::Available(v.clone()), Effect::None),
        );
        assert_step(
            &State::Installing(v.clone()),
            Event::Installing(v.clone()),
            (State::Installing(v.clone()), Effect::None),
        );
        assert_step(
            &State::Failed(v.clone()),
            Event::Installing(v.clone()),
            (State::Failed(v.clone()), Effect::None),
        );
    }

    #[test]
    fn step_install_gone_returns_to_idle() {
        // Success never returns (the process is being replaced) — "Gone"
        // is the only success-shaped outcome.
        let v = "0.25.0".to_string();
        assert_step(
            &State::Downloading(v.clone()),
            Event::InstallOutcome(InstallOutcome::Gone),
            (State::Idle, Effect::None),
        );
        assert_step(
            &State::Installing(v.clone()),
            Event::InstallOutcome(InstallOutcome::Gone),
            (State::Idle, Effect::None),
        );
    }

    #[test]
    fn step_install_failure_lands_in_failed_and_notifies() {
        let v = "0.25.0".to_string();
        for s in [State::Downloading(v.clone()), State::Installing(v.clone())] {
            assert_step(
                &s,
                Event::InstallOutcome(InstallOutcome::Failed("no disk space".to_string())),
                (
                    State::Failed(v.clone()),
                    Effect::Notify(Dialog::InstallFailed("no disk space".to_string())),
                ),
            );
        }
    }

    #[test]
    fn step_install_outcome_is_ignored_outside_downloading_and_installing() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Idle,
            Event::InstallOutcome(InstallOutcome::Gone),
            (State::Idle, Effect::None),
        );
        assert_step(
            &State::Checking {
                manual: true,
                known: Some(v.clone()),
            },
            Event::InstallOutcome(InstallOutcome::Gone),
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Effect::None,
            ),
        );
        assert_step(
            &State::Available(v.clone()),
            Event::InstallOutcome(InstallOutcome::Failed("boom".to_string())),
            (State::Available(v.clone()), Effect::None),
        );
        assert_step(
            &State::Failed(v.clone()),
            Event::InstallOutcome(InstallOutcome::Gone),
            (State::Failed(v.clone()), Effect::None),
        );
    }

    #[test]
    fn step_check_done_is_ignored_outside_checking() {
        let v = "0.25.0".to_string();
        assert_step(
            &State::Idle,
            Event::CheckDone(Ok(Some(v.clone()))),
            (State::Idle, Effect::None),
        );
        assert_step(
            &State::Available(v.clone()),
            Event::CheckDone(Ok(None)),
            (State::Available(v.clone()), Effect::None),
        );
    }

    // ── due() ───────────────────────────────────────────────────────────

    #[test]
    fn due_at_exactly_24_hours() {
        assert!(due(at(0), at(86_400)), "exactly 24h is due (>=, not >)");
    }

    #[test]
    fn due_just_under_24_hours_is_not_due() {
        assert!(!due(at(0), at(86_399)), "23h59m59s is not yet due");
    }

    #[test]
    fn due_well_over_24_hours_is_due() {
        assert!(due(at(0), at(86_401)));
        assert!(due(at(0), at(24 * 86_400)), "a week late is definitely due");
    }

    #[test]
    fn due_with_zero_elapsed_time_is_not_due() {
        assert!(!due(at(1000), at(1000)));
    }

    #[test]
    fn due_when_the_clock_went_backwards() {
        // Documented behaviour (assumption 4): a skewed clock must never
        // block update checks forever — `now < last` counts as due.
        assert!(due(at(1000), at(500)));
        assert!(due(at(2 * 86_400), at(0)));
    }

    // ── menu_label() — the exact six strings ────────────────────────────

    #[test]
    fn menu_label_idle_offers_the_manual_check() {
        assert_eq!(
            menu_label(&State::Idle),
            ("Check for updates…".to_string(), true)
        );
    }

    #[test]
    fn menu_label_checking_is_disabled() {
        let s1 = State::Checking {
            manual: false,
            known: None,
        };
        let s2 = State::Checking {
            manual: true,
            known: Some("0.25.0".to_string()),
        };
        for s in [s1, s2] {
            assert_eq!(menu_label(&s), ("Checking for updates…".to_string(), false));
        }
    }

    #[test]
    fn menu_label_available_shows_the_version() {
        assert_eq!(
            menu_label(&State::Available("0.25.0".to_string())),
            ("Update available — v0.25.0".to_string(), true)
        );
    }

    #[test]
    fn menu_label_downloading_shows_the_version_disabled() {
        assert_eq!(
            menu_label(&State::Downloading("0.25.0".to_string())),
            ("Downloading v0.25.0…".to_string(), false)
        );
    }

    #[test]
    fn menu_label_installing_shows_the_version_disabled() {
        assert_eq!(
            menu_label(&State::Installing("0.25.0".to_string())),
            ("Installing v0.25.0…".to_string(), false)
        );
    }

    #[test]
    fn menu_label_failed_offers_the_retry() {
        assert_eq!(
            menu_label(&State::Failed("0.25.0".to_string())),
            ("Update to v0.25.0 failed — retry".to_string(), true)
        );
    }

    // ── view() + the wire JSON ──────────────────────────────────────────

    #[test]
    fn view_maps_every_state_per_the_plan_table() {
        let v = "0.25.0".to_string();
        let cases: Vec<(State, Option<UpdateView>)> = vec![
            (State::Idle, None),
            (
                State::Checking {
                    manual: false,
                    known: None,
                },
                None,
            ),
            (
                State::Checking {
                    manual: true,
                    known: None,
                },
                None,
            ),
            (
                State::Checking {
                    manual: true,
                    known: Some(v.clone()),
                },
                Some(UpdateView::Available { version: v.clone() }),
            ),
            (
                State::Available(v.clone()),
                Some(UpdateView::Available { version: v.clone() }),
            ),
            (State::Downloading(v.clone()), Some(UpdateView::Installing)),
            (State::Installing(v.clone()), Some(UpdateView::Installing)),
            (
                State::Failed(v.clone()),
                Some(UpdateView::Failed { version: v }),
            ),
        ];
        for (s, want) in cases {
            assert_eq!(view(&s), want, "view({s:?})");
        }
    }

    #[test]
    fn wire_none_serializes_to_json_null() {
        for s in [
            State::Idle,
            State::Checking {
                manual: false,
                known: None,
            },
        ] {
            assert_eq!(
                serde_json::to_value(view(&s)).expect("serialize"),
                serde_json::Value::Null,
                "view({s:?}) must be null on the wire"
            );
        }
    }

    #[test]
    fn wire_available_serializes_exactly() {
        let v = view(&State::Available("0.25.0".to_string())).expect("available has a view");
        assert_eq!(
            serde_json::to_value(v).expect("serialize"),
            serde_json::json!({ "state": "available", "version": "0.25.0" })
        );
    }

    #[test]
    fn wire_installing_serializes_exactly() {
        let v = view(&State::Installing("0.25.0".to_string())).expect("installing has a view");
        assert_eq!(
            serde_json::to_value(v).expect("serialize"),
            serde_json::json!({ "state": "installing" })
        );
    }

    #[test]
    fn wire_failed_serializes_exactly() {
        let v = view(&State::Failed("0.25.0".to_string())).expect("failed has a view");
        assert_eq!(
            serde_json::to_value(v).expect("serialize"),
            serde_json::json!({ "state": "failed", "version": "0.25.0" })
        );
    }

    // ── run_loop() — via FakeUpdateSys + run_loop_with(tick) ────────────
    //
    // The fake records every call, scripts the check results, and owns a
    // controllable clock (`now()`), so the 24h tick test never sleeps
    // 24h. `run_loop_with(sys, rx, tick)` (assumption 5) is what makes
    // the loop testable at a 30ms tick; `run_loop` is its 1h wrapper.

    struct FakeState {
        checks: usize,
        script: VecDeque<Result<Option<String>, String>>,
        last_checked: Option<String>,
        install_calls: usize,
        install_result: InstallOutcome,
        notifications: Vec<Dialog>,
        renders: Vec<State>,
        clock: SystemTime,
    }

    struct FakeUpdateSys {
        inner: Arc<Mutex<FakeState>>,
    }

    fn make_fake(
        first_check: Result<Option<String>, String>,
        install: InstallOutcome,
    ) -> FakeUpdateSys {
        FakeUpdateSys {
            inner: Arc::new(Mutex::new(FakeState {
                checks: 0,
                script: vec![first_check].into(),
                last_checked: None,
                install_calls: 0,
                install_result: install,
                notifications: vec![],
                renders: vec![],
                clock: at(0),
            })),
        }
    }

    impl UpdateSys for FakeUpdateSys {
        fn check(&self) -> Result<Option<String>, String> {
            let mut st = self.inner.lock().unwrap();
            st.checks += 1;
            let r = st.script.pop_front().unwrap_or(Ok(None));
            st.last_checked = r.clone().ok().flatten();
            r
        }

        fn install(&self, on_installing: &mut dyn FnMut(String)) -> InstallOutcome {
            // Gather, drop the lock, THEN call on_installing: the loop's
            // closure renders, which comes back into this fake.
            let (version, result) = {
                let mut st = self.inner.lock().unwrap();
                st.install_calls += 1;
                (st.last_checked.clone(), st.install_result.clone())
            };
            if let Some(v) = version {
                on_installing(v);
            }
            result
        }

        fn notify(&self, d: &Dialog) {
            self.inner.lock().unwrap().notifications.push(d.clone());
        }

        fn render(&self, s: &State) {
            self.inner.lock().unwrap().renders.push(s.clone());
        }

        fn now(&self) -> SystemTime {
            self.inner.lock().unwrap().clock
        }
    }

    const TICK: Duration = Duration::from_millis(30);

    /// Run the loop on a worker thread; returns the event sender, a
    /// "the loop returned" receiver, and a handle into the fake's state.
    fn spawn_loop(
        sys: FakeUpdateSys,
    ) -> (
        mpsc::Sender<Event>,
        mpsc::Receiver<()>,
        Arc<Mutex<FakeState>>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let inner = sys.inner.clone();
        std::thread::spawn(move || {
            run_loop_with(sys, rx, TICK);
            let _ = done_tx.send(());
        });
        (tx, done_rx, inner)
    }

    #[test]
    fn run_loop_performs_the_startup_check_and_renders_the_result() {
        let (tx, done, st) = spawn_loop(make_fake(
            Ok(Some("0.25.0".to_string())),
            InstallOutcome::Gone,
        ));
        wait_for("the startup check", || st.lock().unwrap().checks == 1);
        wait_for("the Available render", || {
            st.lock().unwrap().renders.last() == Some(&State::Available("0.25.0".to_string()))
        });
        assert!(
            st.lock().unwrap().notifications.is_empty(),
            "a successful startup check is silent (the banner carries the news)"
        );
        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("the loop exits when the sender is dropped");
    }

    #[test]
    fn run_loop_click_while_available_triggers_exactly_one_install() {
        let (tx, done, st) = spawn_loop(make_fake(
            Ok(Some("0.25.0".to_string())),
            InstallOutcome::Gone,
        ));
        wait_for("the Available state", || {
            st.lock().unwrap().renders.last() == Some(&State::Available("0.25.0".to_string()))
        });
        tx.send(Event::Click).expect("the loop is alive");
        wait_for("the install to run", || {
            st.lock().unwrap().install_calls == 1
        });
        // Let several ticks pass: a buggy double-install would show up.
        std::thread::sleep(Duration::from_millis(150));
        let st = st.lock().unwrap();
        assert_eq!(st.install_calls, 1, "one Click → exactly one install");
        assert!(st.notifications.is_empty(), "a clean install is silent");
        assert_eq!(
            st.renders.last(),
            Some(&State::Idle),
            "Gone (replaced) → back to Idle"
        );
        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("the loop exits");
    }

    #[test]
    fn run_loop_install_event_in_idle_does_nothing() {
        let (tx, done, st) = spawn_loop(make_fake(Ok(None), InstallOutcome::Gone));
        wait_for("the startup check", || st.lock().unwrap().checks == 1);
        tx.send(Event::Install).expect("the loop is alive");
        std::thread::sleep(Duration::from_millis(150)); // several ticks
        let st = st.lock().unwrap();
        assert_eq!(
            st.install_calls, 0,
            "Install in Idle must not start an install"
        );
        assert!(st.notifications.is_empty(), "and it must not notify");
        assert_eq!(
            st.renders.last(),
            Some(&State::Idle),
            "the state stays Idle"
        );
        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("the loop exits");
    }

    #[test]
    fn run_loop_failed_install_renders_failed_and_notifies() {
        let (tx, done, st) = spawn_loop(make_fake(
            Ok(Some("0.25.0".to_string())),
            InstallOutcome::Failed("no disk space".to_string()),
        ));
        wait_for("the Available state", || {
            st.lock().unwrap().renders.last() == Some(&State::Available("0.25.0".to_string()))
        });
        tx.send(Event::Click).expect("the loop is alive");
        wait_for("the Failed render", || {
            st.lock().unwrap().renders.last() == Some(&State::Failed("0.25.0".to_string()))
        });
        wait_for("the InstallFailed notification", || {
            st.lock().unwrap().notifications
                == vec![Dialog::InstallFailed("no disk space".to_string())]
        });
        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("the loop exits");
    }

    #[test]
    fn run_loop_tick_rechecks_only_when_due() {
        let (tx, done, st) = spawn_loop(make_fake(Ok(None), InstallOutcome::Gone));
        wait_for("the startup check", || st.lock().unwrap().checks == 1);

        // 23h59m later: under 24h — the ticking loop must NOT recheck.
        {
            let mut st = st.lock().unwrap();
            st.clock = at(23 * 3600 + 59 * 60);
        }
        std::thread::sleep(Duration::from_millis(200)); // many 30ms ticks
        assert_eq!(
            st.lock().unwrap().checks,
            1,
            "23h59m < 24h — the tick must not recheck"
        );

        // Cross the 24h boundary: the next tick MUST recheck.
        {
            let mut st = st.lock().unwrap();
            st.clock = at(25 * 3600);
        }
        wait_for("the 24h recheck", || st.lock().unwrap().checks == 2);

        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("the loop exits");
    }

    #[test]
    fn run_loop_exits_when_the_sender_is_dropped() {
        let (tx, done, _st) = spawn_loop(make_fake(Ok(None), InstallOutcome::Gone));
        drop(tx);
        done.recv_timeout(Duration::from_secs(5))
            .expect("run_loop must return (not block forever) when the sender is dropped");
    }
}
