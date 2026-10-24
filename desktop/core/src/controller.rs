//! The startup/mode state machine (DESIGN §4.3–§4.6/§5.3). One owner of
//! the child + phase ⇒ no locks, no races.

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;

use semver::Version;

use crate::consts::{LOG_TAIL_LINES, POLL_INTERVAL, READY_TIMEOUT, STOP_GRACE, TICK};
use crate::paths::DesktopPaths;
use crate::phase::{Dialog, Mode, Phase, Platform, StartError, StartStep, TrayModel, tray_model};
use crate::ready::{ReadyOutcome, wait_ready};
use crate::service::{self, InstalledService, LaunchPlan, ReplacePolicy, RunState, ServiceState};
use crate::session::{self, PidRecord};
use crate::sys::{Child, RunOutput, Signal, SpawnSpec, Sys};
use crate::version::{DeclinedReplace, InstalledVersion, parse_version_output};
use crate::web_target::{WebTarget, read_web_target};

/// The UI surface the controller drives (the Tauri glue implements it).
pub trait Ui: Send {
    fn phase_changed(&self, phase: &Phase, tray: &TrayModel);
    /// ok → true.
    fn confirm(&self, d: &Dialog) -> bool;
    fn message(&self, d: &Dialog);
}

#[derive(Debug, Clone)]
pub enum Action {
    Start,
    ToggleBackground,
    ReinstallService,
    /// Restart only the PeakBot server so an edited `general.web` (or
    /// any boot-only config) applies (spec §8.4): session mode relaunches
    /// the child, service mode reinstalls the unit. Never the app.
    Restart,
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// The session sidecar this controller owns (at most one, §3.8-2).
struct Session {
    child: Box<dyn Child>,
}

/// Owns `sys, ui, paths, quitting, policy, platform, phase, bundled,
/// service, session` (DESIGN §5.3). Every phase assignment goes through
/// `set_phase`, which is also where the tray model is (re)emitted — a
/// `service` refresh always lands before the next `set_phase`, so the
/// tray checkbox mirrors the last `service status`.
pub struct Controller<S: Sys, U: Ui> {
    sys: S,
    ui: U,
    paths: DesktopPaths,
    quitting: Arc<AtomicBool>,
    /// How a build mismatch is resolved (design auto-replace §3.2): set
    /// in `new`, never changes. Chosen at the binary boundary
    /// (release ⇒ Auto, debug ⇒ Ask).
    policy: ReplacePolicy,
    /// The host platform (Q1): set in `new`, never changes. Chosen once
    /// by the glue from `cfg!(windows)` via `phase::host_platform()` —
    /// the pure core never reads `cfg!` itself. On `Platform::Windows`
    /// the start path is session-only: the background service is
    /// non-functional in v1 and is never detected.
    platform: Platform,
    phase: Phase,
    /// The bundled (this app's) PeakBot version — the sidecar's
    /// `--version` output, parsed in CheckingBundled (DESIGN §1: the
    /// sidecar is the version source).
    bundled: Option<Version>,
    service: Option<ServiceState>,
    session: Option<Session>,
}

impl<S: Sys, U: Ui> Controller<S, U> {
    /// `phase = Starting(CheckingBundled)`. No `phase_changed` is emitted
    /// here — the first emission happens on the first `handle`.
    pub fn new(
        sys: S,
        ui: U,
        paths: DesktopPaths,
        quitting: Arc<AtomicBool>,
        policy: ReplacePolicy,
        platform: Platform,
    ) -> Self {
        Self {
            sys,
            ui,
            paths,
            quitting,
            policy,
            platform,
            phase: Phase::Starting(StartStep::CheckingBundled),
            bundled: None,
            service: None,
            session: None,
        }
    }

    /// `Break` after Shutdown.
    pub fn handle(&mut self, action: Action) -> ControlFlow<()> {
        match action {
            Action::Start => self.handle_start(),
            Action::ToggleBackground if !self.is_starting() => self.handle_toggle_background(),
            Action::ReinstallService if !self.is_starting() => self.replace_service(),
            Action::Restart if !self.is_starting() => self.handle_restart(),
            // While Starting the tray item is disabled; ignore.
            Action::ToggleBackground | Action::ReinstallService | Action::Restart => {}
            Action::Shutdown(ack) => {
                self.stop_session();
                let _ = ack.send(());
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    /// Liveness: a Ready session whose child exited →
    /// Failed(SessionExited); a no-op otherwise (incl. service mode).
    pub fn tick(&mut self) {
        if !matches!(self.phase, Phase::Ready(Mode::Session { .. })) {
            return;
        }
        let Some(exit) = self
            .session
            .as_mut()
            .and_then(|sess| sess.child.try_wait().ok().flatten())
        else {
            return;
        };
        let log_tail = self.log_tail();
        self.session = None;
        self.remove_pidfile();
        self.fail(StartError::SessionExited {
            code: exit,
            log_tail,
        });
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    // -------------------------------------------------------------------
    // Phase plumbing
    // -------------------------------------------------------------------

    /// The single exit for phase changes: assign, then emit
    /// `phase_changed` with a fresh tray model.
    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        let tray = tray_model(
            &self.phase,
            self.service.as_ref(),
            self.bundled.as_ref(),
            self.platform,
        );
        self.ui.phase_changed(&self.phase, &tray);
    }

    fn fail(&mut self, err: StartError) {
        self.set_phase(Phase::Failed(err));
    }

    fn is_starting(&self) -> bool {
        matches!(self.phase, Phase::Starting(_))
    }

    // -------------------------------------------------------------------
    // Action::Start (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn handle_start(&mut self) {
        self.stop_session();
        self.reap_stale_pidfile();

        self.set_phase(Phase::Starting(StartStep::CheckingBundled));
        let bundled = match self.check_bundled_sidecar() {
            Ok(v) => v,
            Err(err) => return self.fail(err),
        };

        // Q1 (WINDOWS-PLAN): the background service is non-functional on
        // Windows in v1, so it is never detected on start: no
        // `service status`, no identity/version checks, no
        // AutoReplace/AskReplace/ConnectService, and no
        // `Starting(CheckingService)` step ("Looking for a background
        // service…" must never show on Windows). The session path is the
        // only path — exactly as when no service is installed. An old
        // installed service is left untouched (the VM finding: it used to
        // be silently replaced, then the connect hung 30 s into a dead
        // end).
        if self.platform == Platform::Windows {
            self.launch_session();
            return;
        }

        let state = self.query_service_status();
        self.service = Some(state.clone());
        self.set_phase(Phase::Starting(StartStep::CheckingService));

        let declined = self.read_declined();
        let plan = service::plan_launch(
            &state,
            |svc| self.is_bundled_build(svc),
            |svc| self.version_of(svc),
            &bundled,
            declined.as_ref(),
            self.policy,
        );
        match plan {
            LaunchPlan::Session => self.launch_session(),
            LaunchPlan::ConnectService { start_first } => self.connect_service(start_first),
            LaunchPlan::AutoReplace => self.replace_service(),
            LaunchPlan::AskReplace { installed } => {
                let dialog = service::replace_dialog(&installed, &bundled);
                if self.ui.confirm(&dialog) {
                    self.replace_service();
                } else {
                    self.write_declined(&installed, &bundled);
                    let start_first = matches!(
                        &state,
                        ServiceState::Installed(svc) if svc.run == RunState::Stopped
                    );
                    self.connect_service(start_first);
                }
            }
        }
    }

    /// `<sidecar> --version`: NotFound → SidecarMissing; failed or
    /// unparseable → SidecarBroken. On success the parsed version is
    /// stored as the bundled version (DESIGN §1) and returned.
    fn check_bundled_sidecar(&mut self) -> Result<Version, StartError> {
        let out = self
            .sys
            .run(&self.paths.sidecar, &["--version"])
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    StartError::SidecarMissing {
                        path: self.paths.sidecar.clone(),
                    }
                } else {
                    StartError::SidecarBroken {
                        detail: e.to_string(),
                    }
                }
            })?;
        let version = if out.ok() {
            parse_version_output(&out.stdout)
        } else {
            None
        };
        let Some(v) = version else {
            return Err(StartError::SidecarBroken {
                detail: format!(
                    "`--version` failed or was unparseable: {}",
                    out.combined().trim()
                ),
            });
        };
        self.bundled = Some(v.clone());
        Ok(v)
    }

    fn query_service_status(&self) -> ServiceState {
        match self.sys.run(&self.paths.sidecar, &service::status_args()) {
            Ok(out) => service::parse_service_status(&out),
            Err(e) => ServiceState::Unavailable {
                reason: e.to_string(),
            },
        }
    }

    fn refresh_service(&mut self) {
        self.service = Some(self.query_service_status());
    }

    /// A6: exe None ⇒ false; `same_contents` Err ⇒ log::warn! + false.
    fn is_bundled_build(&self, svc: &InstalledService) -> bool {
        svc.exe.as_deref().is_some_and(|exe| {
            self.sys
                .same_contents(exe, &self.paths.sidecar)
                .unwrap_or_else(|e| {
                    log::warn!(
                        "cannot compare {} with the bundled sidecar: {e}",
                        exe.display()
                    );
                    false
                })
        })
    }

    /// The `TLS:` status line of the installed service: on only when
    /// `--tls` is baked into the unit (a hand-made `service install
    /// --tls`). The desktop installs bare units, which read `general.web`.
    fn installed_tls(&self) -> bool {
        matches!(&self.service, Some(ServiceState::Installed(svc)) if svc.tls)
    }

    /// `version_of(installed)` per design auto-replace §4.1: exe None →
    /// Unknown, else `<exe> --version`.
    fn version_of(&self, svc: &InstalledService) -> InstalledVersion {
        let Some(exe) = &svc.exe else {
            return InstalledVersion::Unknown;
        };
        match self.sys.run(exe, &["--version"]) {
            Ok(out) if out.ok() => parse_version_output(&out.stdout)
                .map(InstalledVersion::Known)
                .unwrap_or(InstalledVersion::Unknown),
            _ => InstalledVersion::Unknown,
        }
    }

    fn read_declined(&self) -> Option<DeclinedReplace> {
        self.sys
            .read(&self.paths.declined_file())
            .ok()
            .flatten()
            .and_then(|s| DeclinedReplace::parse(&s))
    }

    fn write_declined(&self, installed: &InstalledVersion, bundled: &Version) {
        let d = DeclinedReplace {
            installed: installed.clone(),
            bundled: bundled.clone(),
        };
        if let Err(e) = self
            .sys
            .write_private(&self.paths.declined_file(), &d.render())
        {
            log::warn!("failed to write declined-replace: {e}");
        }
    }

    // -------------------------------------------------------------------
    // LAUNCH_SESSION (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn launch_session(&mut self) {
        // general.web is read on every launch (spec §8.4); the child gets
        // no web flags and reads the same file itself.
        let mode = Mode::Session(self.read_web_target());
        self.set_phase(Phase::Starting(StartStep::StartingSession));
        let spec = SpawnSpec {
            program: self.paths.sidecar.clone(),
            args: session::session_args(),
            extra_env: session::session_env(&std::env::vars().collect::<Vec<_>>()),
            log: self.paths.session_log(),
        };
        let mut child = match self.sys.spawn(spec) {
            Ok(c) => c,
            Err(e) => {
                return self.fail(StartError::SidecarBroken {
                    detail: e.to_string(),
                });
            }
        };
        let rec = PidRecord {
            pid: child.pid(),
            exe: self.paths.sidecar.clone(),
        };
        if let Err(e) = self.sys.write_private(&self.paths.pidfile(), &rec.render()) {
            log::warn!("failed to write pidfile: {e}");
        }

        self.set_phase(Phase::Starting(StartStep::WaitingForBackend));
        let (outcome, mode) = self.await_backend(mode, Some(&mut child));
        match outcome {
            ReadyOutcome::Ready => {
                self.session = Some(Session { child });
                self.set_phase(Phase::Ready(mode));
            }
            // A busy bind lands here too: the child exits and the log
            // tail says why (spec §8.4 — no port fallback).
            ReadyOutcome::ChildExited(code) => {
                let log_tail = self.log_tail();
                self.remove_pidfile();
                self.fail(StartError::SessionExited { code, log_tail });
            }
            ReadyOutcome::Timeout => {
                self.stop_child(&mut child);
                self.remove_pidfile();
                self.fail(StartError::NotReady {
                    waited: READY_TIMEOUT,
                });
            }
            ReadyOutcome::TokenRejected => {
                self.stop_child(&mut child);
                self.remove_pidfile();
                self.fail(StartError::TokenRejected { service: false });
            }
            // Quitting: an internal control-flow outcome, never a
            // visible phase (§3.5).
            ReadyOutcome::Cancelled => {
                self.stop_child(&mut child);
                self.remove_pidfile();
            }
        }
    }

    // -------------------------------------------------------------------
    // CONNECT (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn connect_service(&mut self, start_first: bool) {
        self.set_phase(Phase::Starting(StartStep::ConnectingService));
        let mode = self.service_mode();
        if start_first {
            let args = service::service_install_args();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err((command, output)) = self.run_sidecar_cmd(&args) {
                return self.fail(StartError::ServiceCommandFailed { command, output });
            }
        }
        match self.await_backend(mode, None) {
            (ReadyOutcome::Ready, mode) => self.set_phase(Phase::Ready(mode)),
            (ReadyOutcome::TokenRejected, _) => {
                self.fail(StartError::TokenRejected { service: true })
            }
            (ReadyOutcome::Timeout, mode) => self.fail(StartError::ServiceUnreachable {
                waited: READY_TIMEOUT,
                addr: mode.target().host_port(),
            }),
            (ReadyOutcome::Cancelled, _) => {}
            (ReadyOutcome::ChildExited(_), _) => {} // no child in service mode
        }
    }

    /// `general.web` from config.yaml (spec §8.4). Missing, legacy or
    /// unparsable ⇒ the defaults; PeakBot reports real problems itself.
    fn read_web_target(&self) -> WebTarget {
        let text = match self.sys.read(&self.paths.peakbot_config()) {
            Ok(text) => text,
            Err(e) => {
                log::warn!("failed to read config.yaml: {e}");
                None
            }
        };
        read_web_target(text.as_deref())
    }

    /// The service serves `general.web`, plus TLS when the unit itself
    /// carries `--tls` (the server's rule is `--tls || general.web.tls`).
    fn service_mode(&self) -> Mode {
        let mut target = self.read_web_target();
        target.tls |= self.installed_tls();
        Mode::Service(target)
    }

    /// Probe until the backend answers, then read `general.web` AGAIN
    /// (spec §8.4, Z8): the child's first boot may have auto-migrated a
    /// legacy config, moving the web-token into `general.web.token`, so
    /// the pre-launch read can be stale. A 401 also counts as "it
    /// answered". If the fresh read differs, probe once more with it and
    /// return the fresh mode — that is the URL the bridge navigates to.
    fn await_backend(
        &self,
        mode: Mode,
        mut child: Option<&mut Box<dyn Child>>,
    ) -> (ReadyOutcome, Mode) {
        let outcome = self.wait_ready_on(&mode, child.as_deref_mut());
        if !matches!(outcome, ReadyOutcome::Ready | ReadyOutcome::TokenRejected) {
            return (outcome, mode);
        }
        let fresh = match mode {
            Mode::Session(_) => Mode::Session(self.read_web_target()),
            Mode::Service(_) => self.service_mode(),
        };
        if fresh == mode {
            return (outcome, mode);
        }
        (self.wait_ready_on(&fresh, child), fresh)
    }

    fn wait_ready_on(&self, mode: &Mode, child: Option<&mut Box<dyn Child>>) -> ReadyOutcome {
        // Written as a match so the `Box<dyn Child + 'static>` borrow is
        // coerced to the short-lived trait object `wait_ready` takes.
        let child: Option<&mut dyn Child> = match child {
            Some(c) => Some(c.as_mut()),
            None => None,
        };
        wait_ready(
            &self.sys,
            &mode.probe_url(),
            READY_TIMEOUT,
            child,
            &self.quitting,
        )
    }

    /// One short-lived sidecar command. Err = (command, output) for the
    /// ServiceCommandFailed dialog/phase.
    fn run_sidecar_cmd(&self, args: &[&str]) -> Result<RunOutput, (String, String)> {
        let command = args.join(" ");
        let out = self
            .sys
            .run(&self.paths.sidecar, args)
            .map_err(|e| (command.clone(), e.to_string()))?;
        if out.ok() {
            Ok(out)
        } else {
            Err((command, out.combined()))
        }
    }

    // -------------------------------------------------------------------
    // REPLACE (design auto-replace §4.3) — also Action::ReinstallService
    // -------------------------------------------------------------------

    /// Replace the installed service with the bundled build (dialog
    /// "Replace", tray ReinstallService, or a silent AutoReplace). A5:
    /// the binary copy runs FIRST, so a failed copy leaves the service
    /// and the old binary intact. `enable --now` doesn't restart a
    /// running unit, so the uninstall + install pair is still what
    /// restarts the service on the new binary. The install is bare: the
    /// unit reads `general.web` (spec §8.4).
    fn replace_service(&mut self) {
        self.set_phase(Phase::Starting(StartStep::InstallingService));
        if let Err((command, output)) = self.run_sidecar_cmd(&service::install_args()) {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }

        self.set_phase(Phase::Starting(StartStep::RemovingService));
        // Non-zero tolerated: the unit may not be loaded.
        let _ = self
            .sys
            .run(&self.paths.sidecar, &service::service_uninstall_args());

        self.set_phase(Phase::Starting(StartStep::InstallingService));
        let args = service::service_install_args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err((command, output)) = self.run_sidecar_cmd(&args) {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }

        self.refresh_service();
        if let Err(e) = self.sys.remove(&self.paths.declined_file()) {
            log::warn!("failed to remove declined-replace: {e}");
        }
        self.connect_service(false);
    }

    // -------------------------------------------------------------------
    // Mode toggle (DESIGN §4.5)
    // -------------------------------------------------------------------

    fn handle_toggle_background(&mut self) {
        match self.service.as_ref() {
            Some(ServiceState::NotInstalled) => self.enable_background(),
            Some(ServiceState::Installed(_)) => self.disable_background(),
            // Unavailable (or no status yet): the tray item is disabled.
            _ => {}
        }
    }

    fn enable_background(&mut self) {
        if !self.sys.exists(&self.paths.peakbot_config()) {
            self.ui.message(&service::finish_setup_dialog());
            return;
        }
        // The service will bind general.web.bind. Our own session holds
        // that port right now and is stopped below, so only check it when
        // there is no session.
        let bind = self.read_web_target().bind;
        if self.session.is_none() && !self.sys.port_is_free(bind.port()) {
            self.ui.message(&service::port_busy_dialog(bind));
            return;
        }
        if !self.ui.confirm(&service::enable_dialog()) {
            return;
        }
        // Stop the session BEFORE installing: two backends must never
        // share the config/DB.
        self.stop_session();

        // REPLACE steps minus the uninstall.
        self.set_phase(Phase::Starting(StartStep::InstallingService));
        let args = service::service_install_args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err((command, output)) = self
            .run_sidecar_cmd(&service::install_args())
            .and_then(|_| self.run_sidecar_cmd(&args))
        {
            return self.enable_failed(&command, &output);
        }
        self.refresh_service();
        self.connect_service(false);
    }

    /// Enable failure: best-effort rollback, then the user is back where
    /// they were (session mode).
    fn enable_failed(&mut self, command: &str, output: &str) {
        let _ = self
            .sys
            .run(&self.paths.sidecar, &service::service_uninstall_args());
        self.refresh_service();
        self.ui
            .message(&service::command_failed_dialog(command, output));
        self.launch_session();
    }

    fn disable_background(&mut self) {
        if !self.ui.confirm(&service::disable_dialog()) {
            return;
        }
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        match self.run_sidecar_cmd(&service::service_uninstall_args()) {
            Ok(_) => {
                self.refresh_service();
                self.launch_session();
            }
            Err((command, output)) => {
                self.refresh_service();
                self.ui
                    .message(&service::command_failed_dialog(&command, &output));
                // Unchanged mode: reconnect to the still-installed service.
                self.connect_service(false);
            }
        }
    }

    // -------------------------------------------------------------------
    // Action::Restart (spec §8.4)
    // -------------------------------------------------------------------

    /// Service mode: `reconfigure_service` — there is no `service
    /// restart` verb, the reinstall IS the restart. Otherwise (session
    /// mode, or a failed start with no service): the full start path,
    /// which stops the child, re-reads `general.web` and relaunches.
    fn handle_restart(&mut self) {
        if matches!(self.service, Some(ServiceState::Installed(_))) {
            self.reconfigure_service();
        } else {
            self.handle_start();
        }
    }

    /// Reinstall the installed unit so the restarted service reads the
    /// current config. The binary is NOT replaced (design §6 T6).
    fn reconfigure_service(&mut self) {
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        // Non-zero tolerated: the unit may not be loaded.
        let _ = self
            .sys
            .run(&self.paths.sidecar, &service::service_uninstall_args());

        self.set_phase(Phase::Starting(StartStep::InstallingService));
        let args = service::service_install_args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if let Err((command, output)) = self.run_sidecar_cmd(&args) {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }
        self.refresh_service();
        self.connect_service(false);
    }

    // -------------------------------------------------------------------
    // Stopping + reaping (DESIGN §4.6)
    // -------------------------------------------------------------------

    /// SIGINT → poll `try_wait` up to STOP_GRACE → SIGKILL → remove
    /// pidfile. A no-op when there is no session.
    fn stop_session(&mut self) {
        let Some(mut sess) = self.session.take() else {
            return;
        };
        self.set_phase(Phase::Starting(StartStep::StoppingSession));
        self.stop_child(&mut sess.child);
        self.remove_pidfile();
    }

    fn stop_child(&mut self, child: &mut Box<dyn Child>) {
        child.interrupt();
        let deadline = self.sys.now() + STOP_GRACE;
        loop {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            if self.sys.now() >= deadline {
                child.kill();
                let _ = child.try_wait();
                return;
            }
            self.sys.sleep(POLL_INTERVAL);
        }
    }

    /// Stale pidfile reaper: identity-checked (cmdline contains the
    /// recorded sidecar path) SIGINT → poll up
    /// to STOP_GRACE → SIGKILL; the pidfile is always removed. The fresh
    /// session then writes its own pidfile (DESIGN §3.8: pidfile exists
    /// ⇔ a session child was spawned and not yet confirmed exited).
    fn reap_stale_pidfile(&mut self) {
        let pidfile = self.paths.pidfile();
        let Some(text) = self.sys.read(&pidfile).ok().flatten() else {
            return;
        };
        if let Some(rec) = PidRecord::parse(&text)
            && let Some(cmd) = self.sys.cmdline_of(rec.pid)
            && session::is_our_sidecar(&cmd, &rec)
        {
            self.sys.signal(rec.pid, Signal::Interrupt);
            let deadline = self.sys.now() + STOP_GRACE;
            while self.sys.cmdline_of(rec.pid).is_some() && self.sys.now() < deadline {
                self.sys.sleep(POLL_INTERVAL);
            }
            if self.sys.cmdline_of(rec.pid).is_some() {
                self.sys.signal(rec.pid, Signal::Kill);
            }
        }
        if let Err(e) = self.sys.remove(&pidfile) {
            log::warn!("failed to remove stale pidfile: {e}");
        }
    }

    fn remove_pidfile(&self) {
        if let Err(e) = self.sys.remove(&self.paths.pidfile()) {
            log::warn!("failed to remove pidfile: {e}");
        }
    }

    /// Last LOG_TAIL_LINES of the session log ("" if absent).
    fn log_tail(&self) -> String {
        match self.sys.read(&self.paths.session_log()) {
            Ok(Some(text)) => session::tail_lines(&text, LOG_TAIL_LINES),
            Ok(None) => String::new(),
            Err(e) => {
                log::warn!("failed to read session log: {e}");
                String::new()
            }
        }
    }
}

/// `rx.recv_timeout(TICK)` → `handle(action)` or `tick()` (DESIGN §4.1).
/// The loop ends on a Shutdown Break or a disconnected channel.
pub fn run_loop<S: Sys, U: Ui>(mut ctrl: Controller<S, U>, rx: Receiver<Action>) {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(action) => {
                if ctrl.handle(action) == ControlFlow::Break(()) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => ctrl.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::READY_TIMEOUT;
    use crate::paths::DesktopPaths;
    use crate::phase::{Mode, Phase, Platform, StartError, StartStep};
    use crate::service::ReplacePolicy;
    use crate::session::PidRecord;
    use crate::sys::{Probe, RunOutput, Signal};
    use crate::testing::{ChildMode, Effect, FakeSys, FakeUi};
    use crate::version::{DeclinedReplace, InstalledVersion};
    use crate::web_target::WebTarget;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    const SIDECAR: &str = "/app/peakbot";
    const PIDFILE: &str = "/data/sidecar.pid";
    const WEB_TOKEN: &str = "/cfg/web-token";
    const DECLINED: &str = "/data/declined-replace";
    const CONFIG_YAML: &str = "/cfg/config.yaml";
    const SETTINGS_FILE: &str = "/data/settings.json";

    fn paths() -> DesktopPaths {
        DesktopPaths {
            sidecar: PathBuf::from(SIDECAR),
            data_dir: PathBuf::from("/data"),
            log_dir: PathBuf::from("/logs"),
            peakbot_config_dir: PathBuf::from("/cfg"),
        }
    }

    fn run_out(code: Option<i32>, stdout: &str, stderr: &str) -> RunOutput {
        RunOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    fn versioned(version: &str) -> RunOutput {
        run_out(Some(0), &format!("peakbot {version}\n"), "")
    }

    fn installed_status(state: &str) -> RunOutput {
        run_out(
            Some(0),
            &format!("Installed: true\nExec:      {SIDECAR}\nState:     {state}\n"),
            "",
        )
    }

    /// Exec path of an installed service whose binary is NOT the bundled
    /// sidecar. The file name is deliberately distinct: FakeSys scripts
    /// `run` by (file name, args), so the installed binary can report a
    /// `--version` different from the sidecar's.
    const INSTALLED_EXE: &str = "/home/u/.local/bin/peakbot-0.22.0";

    /// Design auto-replace §7.1 fixtures: installed exes, each with a
    /// distinct file name because FakeSys keys `run` by file name.
    /// `--version` → `peakbot 0.23.0` (same as the bundled sidecar).
    const INSTALLED_SAME: &str = "/home/u/.local/bin/peakbot-same";
    /// `--version` → `peakbot 0.24.0` (newer than the bundled 0.23.0).
    const INSTALLED_NEWER: &str = "/home/u/.local/bin/peakbot-0.24.0";
    /// `--version` → `hello` (unparseable ⇒ Unknown).
    const INSTALLED_GARBAGE: &str = "/home/u/.local/bin/peakbot-garbage";

    /// `service status` for a service whose Exec path is `exe`.
    fn installed_status_at(exe: &str, state: &str) -> RunOutput {
        run_out(
            Some(0),
            &format!("Installed: true\nExec:      {exe}\nState:     {state}\n"),
            "",
        )
    }

    /// `service status` with a `TLS:` line (added by #375; parsed into
    /// `InstalledService.tls` by #377).
    fn installed_status_tls(state: &str, tls: &str) -> RunOutput {
        run_out(
            Some(0),
            &format!(
                "Installed: true\nExec:      {SIDECAR}\nState:     {state}\nTLS:       {tls}\n"
            ),
            "",
        )
    }

    const NOT_INSTALLED: &str = "Installed: false\n";

    /// `Controller::new` + `handle(Start)`, asserting we land Ready.
    /// Existing tests run under `ReplacePolicy::Ask` (the dev-build
    /// policy): they exercise the prompt path, which is unchanged for
    /// differing builds (design auto-replace §6 T6).
    fn started(sys: FakeSys, ui: FakeUi) -> Controller<FakeSys, FakeUi> {
        started_with(sys, ui, ReplacePolicy::Ask)
    }

    /// `Controller::new(…, policy)` + `handle(Start)` on `Platform::Unix`
    /// — the pre-Q1 behaviour every existing test pins.
    fn started_with(
        sys: FakeSys,
        ui: FakeUi,
        policy: ReplacePolicy,
    ) -> Controller<FakeSys, FakeUi> {
        started_on(sys, ui, policy, Platform::Unix)
    }

    /// `Controller::new(…, policy, platform)` + `handle(Start)`.
    ///
    /// CONTRACT (Q1, RED until the developer lands it): the constructor
    /// takes the host platform as its LAST parameter — the glue decides
    /// it once from `cfg!(windows)` via `phase::host_platform()` and
    /// passes it into the pure core, exactly like `policy`.
    /// `pub fn new(sys, ui, paths, quitting, policy, platform: Platform)`.
    fn started_on(
        sys: FakeSys,
        ui: FakeUi,
        policy: ReplacePolicy,
        platform: Platform,
    ) -> Controller<FakeSys, FakeUi> {
        let mut ctrl = Controller::new(
            sys,
            ui,
            paths(),
            Arc::new(AtomicBool::new(false)),
            policy,
            platform,
        );
        let _ = ctrl.handle(Action::Start);
        ctrl
    }

    /// Design auto-replace §7.1 "Order OK" (invariant A5): in
    /// `run_args()`, the binary copy (`install`) runs BEFORE
    /// `service uninstall`, which runs before `service install`.
    fn assert_order_ok(runs: &[Vec<String>]) {
        let i_install = runs
            .iter()
            .position(|r| r == &["install".to_string()])
            .expect("`install` (the binary copy) ran");
        let i_uninstall = runs
            .iter()
            .position(|r| r == &["service".to_string(), "uninstall".to_string()])
            .expect("`service uninstall` ran");
        let i_svc = runs
            .iter()
            .position(|r| {
                r.first().is_some_and(|s| s == "service")
                    && r.get(1).is_some_and(|s| s == "install")
            })
            .expect("`service install` ran");
        assert!(
            i_install < i_uninstall && i_uninstall < i_svc,
            "order must be install → service uninstall → service install; runs: {runs:?}"
        );
    }

    // §9.1 case 38.
    #[test]
    fn t38_sidecar_missing_fails_without_spawn() {
        let sys = FakeSys::new(); // no `--version` script ⇒ NotFound
        let ui = FakeUi::new();
        let mut ctrl = Controller::new(
            sys.clone(),
            ui.clone(),
            paths(),
            Arc::new(AtomicBool::new(false)),
            ReplacePolicy::Ask,
            Platform::Unix,
        );
        let _ = ctrl.handle(Action::Start);
        match ctrl.phase() {
            Phase::Failed(StartError::SidecarMissing { path }) => {
                assert_eq!(*path, PathBuf::from(SIDECAR));
            }
            other => panic!("expected SidecarMissing, got {other:?}"),
        }
        assert!(
            sys.spawned_specs().is_empty(),
            "no sidecar spawn may happen when the sidecar is missing"
        );
    }

    // §9.1 case 39.
    #[test]
    fn t39_broken_sidecar_version_fails() {
        let sys = FakeSys::new();
        sys.set_run(
            "peakbot",
            &["--version"],
            run_out(Some(0), "peakbot not-a-version\n", ""),
        );
        let ui = FakeUi::new();
        let mut ctrl = Controller::new(
            sys.clone(),
            ui.clone(),
            paths(),
            Arc::new(AtomicBool::new(false)),
            ReplacePolicy::Ask,
            Platform::Unix,
        );
        let _ = ctrl.handle(Action::Start);
        match ctrl.phase() {
            Phase::Failed(StartError::SidecarBroken { detail }) => {
                assert!(!detail.is_empty(), "detail must describe the failure");
            }
            other => panic!("expected SidecarBroken, got {other:?}"),
        }
    }

    // §9.1 case 40 (0.24, spec §8.4: the session's bind + token come
    // from config.yaml general.web; the child gets no web flags and no
    // token in its env).
    #[test]
    fn t40_happy_session() {
        // (a) general.web with a token: the URL carries it; the child
        // reads the same file itself.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("seeded-token")),
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session(WebTarget { bind, tls, token }))
                    if bind.port() == 7823
                    && !*tls
                    && token.as_ref().map(|t| t.as_str()) == Some("seeded-token")
            ),
            "the session target is general.web; got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/?token=seeded-token")
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 1, "exactly one spawn");
        assert!(spawns[0].args.is_empty(), "no --bind/--tls/--token");
        assert!(
            spawns[0]
                .extra_env
                .iter()
                .all(|(k, _)| k != "PEAKBOT_WEB_TOKEN"),
            "the child env carries no token; got {:?}",
            spawns[0].extra_env
        );
        assert!(
            spawns[0]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_NO_OPEN" && v == "1"),
            "spawn env must carry PEAKBOT_NO_OPEN=1; got {:?}",
            spawns[0].extra_env
        );
        assert_eq!(
            sys.file(Path::new(CONFIG_YAML)),
            Some(web_yaml("127.0.0.1:7823", Some("seeded-token"))),
            "config.yaml is read, never written"
        );

        // Pidfile: pid + the sidecar path.
        let pidfile = sys.file(Path::new(PIDFILE)).expect("pidfile written");
        let rec = PidRecord::parse(&pidfile).expect("pidfile parses");
        assert_eq!(rec.pid, sys.spawned_pids()[0]);
        assert_eq!(rec.exe, PathBuf::from(SIDECAR));

        // (b) No config.yaml: the defaults (127.0.0.1:7823, http, no
        // token) — nothing is generated or written.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());

        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/"),
            "defaults without config.yaml; got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            sys.private_written().into_iter().collect::<Vec<_>>(),
            vec![PathBuf::from(PIDFILE)],
            "only the pidfile is written"
        );

        // Phases emitted in order.
        let steps: Vec<String> = ui
            .steps()
            .iter()
            .map(|p| match p {
                Phase::Starting(s) => format!("starting:{s:?}"),
                Phase::Ready(_) => "ready".to_string(),
                Phase::Failed(e) => format!("failed:{e:?}"),
            })
            .collect();
        assert_eq!(
            steps,
            vec![
                "starting:CheckingBundled",
                "starting:CheckingService",
                "starting:StartingSession",
                "starting:WaitingForBackend",
                "ready"
            ]
        );
    }

    // Spec §8.4 (replaces t41/t42, the deleted port fallback): a busy
    // bind is not retried on another port — the child exits and the
    // status page shows the failure with the log tail.
    #[test]
    fn busy_bind_fails_with_log_tail_without_fallback() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_port_free(7823, false);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsAfter(1),
            exit: Some(1),
            log_write: Some("127.0.0.1:7823 is already in use".to_string()),
            ..Default::default()
        });
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Failed(StartError::SessionExited { code, log_tail }) => {
                assert_eq!(*code, Some(1));
                assert!(log_tail.contains("already in use"), "{log_tail:?}");
            }
            other => panic!("expected SessionExited, got {other:?}"),
        }
        assert_eq!(sys.spawned_specs().len(), 1, "no retry on another port");
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
    }

    // §9.1 case 43.
    #[test]
    fn t43_session_crash_fails_with_log_tail_and_removes_pidfile() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsAfter(1),
            exit: Some(1),
            log_write: Some("fatal: database locked".to_string()),
            ..Default::default()
        });
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Failed(StartError::SessionExited { code, log_tail }) => {
                assert_eq!(*code, Some(1));
                assert!(
                    log_tail.contains("database locked"),
                    "log tail must carry the child's output; {log_tail:?}"
                );
            }
            other => panic!("expected SessionExited, got {other:?}"),
        }
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile must be removed");
    }

    // §9.1 case 44.
    #[test]
    fn t44_session_readiness_timeout_stops_child_and_fails_notready() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        // No probe script ⇒ Unreachable; child ignores SIGINT.
        sys.queue_child(crate::testing::FakeChildSpec::never());
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Failed(StartError::NotReady { waited }) => {
                assert_eq!(*waited, READY_TIMEOUT);
            }
            other => panic!("expected NotReady, got {other:?}"),
        }
        let evts = sys.events();
        let interrupted: Vec<u32> = evts
            .iter()
            .filter_map(|e| match e {
                Effect::ChildInterrupt { pid } => Some(*pid),
                _ => None,
            })
            .collect();
        assert!(!interrupted.is_empty(), "the child must be interrupted");
        let killed: Vec<u32> = evts
            .iter()
            .filter_map(|e| match e {
                Effect::ChildKill { pid } => Some(*pid),
                _ => None,
            })
            .collect();
        assert_eq!(
            killed, interrupted,
            "a child that ignores SIGINT must be SIGKILLed after STOP_GRACE"
        );
    }

    // §9.1 case 45.
    #[test]
    fn t45_unavailable_service_falls_back_to_session_without_prompt() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(1), "", "service status failed: no user bus\n"),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(ui.confirms().is_empty(), "no dialog may be shown");
        assert_eq!(sys.spawned_specs().len(), 1);
    }

    // §9.1 case 46.
    #[test]
    fn t46_installed_same_version_running_connects_without_spawn() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { token, .. })) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is exactly general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            sys.spawned_specs().is_empty(),
            "no sidecar spawn in service mode"
        );
        let runs = sys.run_args();
        assert!(
            !runs
                .iter()
                .any(|r| { r == &["service".to_string(), "install".to_string(),] }),
            "a running same-version service must not be re-installed; runs: {runs:?}"
        );
    }

    // §9.1 case 47.
    #[test]
    fn t47_installed_same_version_stopped_starts_then_connects() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("stopped"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert!(
            runs.iter()
                .any(|r| { r == &["service".to_string(), "install".to_string(),] }),
            "bare `service install` must run; runs: {runs:?}"
        );
    }

    // §9.1 case 48.
    #[test]
    fn t48_installed_tokenless_service_ready_without_general_web_token() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        // No general.web.token.
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", None));
        sys.set_probe("http://127.0.0.1:7823/commands", vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { token, .. })) => {
                assert!(
                    token.is_none(),
                    "no general.web.token ⇒ Service with no token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            sys.private_written().is_empty(),
            "CONNECT writes nothing (a token is never minted by the desktop)"
        );
    }

    // §9.1 case 49.
    #[test]
    fn t49_replace_flow_command_order_and_cleanup() {
        let sys = FakeSys::new();
        // Bundled version = the sidecar's `--version` output (DESIGN §1,
        // §4.3 CheckingBundled) — scripted distinct from the app's crate
        // version on purpose, so the test pins the source of truth.
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.seed_file(DECLINED, "installed=0.20.0\nbundled=0.21.0\n"); // stale; must be removed
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("cfg-token")));
        // CONNECT after a successful install is proven by the probe
        // (DESIGN §4.3) — script a 200, never rely on the install command.
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started(sys.clone(), ui);

        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { token, .. })) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("cfg-token"),
                    "Ready with general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            sys.events().iter().any(|e| {
                matches!(e, Effect::Probe { url } if url.to_string().contains("cfg-token"))
            }),
            "the readiness probe must use general.web.token"
        );

        // Command order (invariant A5, design auto-replace §4.3): the
        // binary copy runs FIRST, so a failed copy leaves the service
        // untouched: install → service uninstall → service install.
        assert_order_ok(&sys.run_args());

        // The install is bare: the unit reads general.web itself.
        assert!(
            sys.run_args()
                .iter()
                .all(|r| r.first().map(String::as_str) != Some("service") || r.len() == 2),
            "no web flags on any service verb; runs: {:?}",
            sys.run_args()
        );
        // The stale declined file is removed.
        assert!(!sys.has_file(Path::new(DECLINED)), "declined file removed");
    }

    // §9.1 case 50.
    #[test]
    fn t50_keep_flow_writes_declined_pair_and_silences_next_start() {
        let sys = FakeSys::new();
        // Bundled version = the sidecar's `--version` output (DESIGN §1),
        // scripted distinct from the app's crate version on purpose.
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false); // Keep
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );

        // The declined file carries the exact pair.
        let content = sys
            .file(Path::new(DECLINED))
            .expect("declined-replace written");
        let declined = DeclinedReplace::parse(&content).expect("declined file parses");
        let v22 = semver::Version::parse("0.22.0").unwrap();
        let v24 = semver::Version::parse("0.24.0").unwrap();
        assert!(
            declined.matches(&InstalledVersion::Known(v22.clone()), &v24.clone()),
            "the exact (installed, bundled) pair must be recorded; bundled = the sidecar's --version"
        );
        assert!(
            !sys.run_args()
                .iter()
                .any(|r| { r == &["service".to_string(), "install".to_string(),] }),
            "Keep must not (re)install the service"
        );

        // Next Start: no dialog.
        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ui.confirms().len(),
            1,
            "the second Start must not re-ask; got {:?}",
            ui.confirms()
        );
    }

    // §9.1 case 51.
    #[test]
    fn t51_stale_declined_pair_reasks() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        // Declined for an OLDER bundled version — the pair no longer matches.
        sys.seed_file(DECLINED, "installed=0.22.0\nbundled=0.21.0\n");
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ui.confirms().len(),
            1,
            "a mismatched declined pair must re-ask the dialog"
        );
    }

    // §9.1 case 52.
    #[test]
    fn t52_service_probe_401_and_timeout() {
        // 401 → TokenRejected{service:true}.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(401)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Failed(StartError::TokenRejected { service }) => {
                assert!(service, "service-mode rejection");
            }
            other => panic!("expected TokenRejected{{service:true}}, got {other:?}"),
        }

        // Always unreachable → ServiceUnreachable.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        // No probe script ⇒ Unreachable.
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Failed(StartError::ServiceUnreachable { .. })
            ),
            "got {:?}",
            ctrl.phase()
        );
    }

    // DESIGN §4.3: CONNECT after a successful `service install` (Replace)
    // still reaches Ready only via the probe — a probe timeout is
    // Failed(ServiceUnreachable), NEVER Ready (the install command is not
    // proof the service answers).
    #[test]
    fn t52b_post_install_probe_timeout_is_serviceunreachable() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        // No probe script ⇒ every probe Unreachable.
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Failed(StartError::ServiceUnreachable { .. })
            ),
            "a probe timeout after install must be ServiceUnreachable, never Ready; got {:?}",
            ctrl.phase()
        );
    }

    // §9.1 case 53.
    #[test]
    fn t53_replace_service_install_failure_is_servicecommandfailed() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(1), "", "unit failed to start\n"),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Failed(StartError::ServiceCommandFailed { command, output }) => {
                assert!(
                    command.contains("service install"),
                    "command must name the failing verb; {command:?}"
                );
                assert!(
                    output.contains("unit failed to start"),
                    "output must carry the child's stderr; {output:?}"
                );
            }
            other => panic!("expected ServiceCommandFailed, got {other:?}"),
        }
    }

    // §9.1 case 54.
    #[test]
    fn t54_enable_without_config_shows_finish_setup_and_changes_nothing() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );
        // config.yaml absent.
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "phase must be unchanged; got {:?}",
            ctrl.phase()
        );
        assert!(
            ui.messages()
                .iter()
                .any(|d| d.title == "Finish setup first"),
            "the finish-setup info dialog must show; got {:?}",
            ui.messages()
        );
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]),
            "no install may run; runs: {:?}",
            sys.run_args()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no new session spawn");
    }

    // §9.1 case 55 (0.24: the service binds general.web.bind, the same
    // port our session uses — so the check only applies when no session
    // of ours holds it, e.g. after the session failed on a busy bind).
    #[test]
    fn t55_enable_with_busy_port_shows_port_busy() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsAfter(1),
            exit: Some(1),
            log_write: Some("127.0.0.1:7823 is already in use".to_string()),
            ..Default::default()
        });
        sys.seed_file(CONFIG_YAML, "provider:\n  type: openrouter\n");
        sys.set_port_free(7823, false);
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Failed(StartError::SessionExited { .. })
            ),
            "precondition: the session failed on the busy bind; got {:?}",
            ctrl.phase()
        );
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Failed(StartError::SessionExited { .. })
            ),
            "phase unchanged; got {:?}",
            ctrl.phase()
        );
        assert!(
            ui.messages().iter().any(|d| d.title == "Port 7823 is busy"),
            "got {:?}",
            ui.messages()
        );
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]),
            "no install may run"
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no new session spawn");
    }

    // §9.1 case 55 (0.24 companion): our own running session holds
    // general.web.bind — that is not "busy"; the enable proceeds.
    #[test]
    fn t55b_enable_from_session_ignores_its_own_port() {
        let sys = FakeSys::new();
        sys.set_port_free(7823, false); // held by our session
        sys.seed_file(CONFIG_YAML, "provider:\n  type: openrouter\n");
        let ui = FakeUi::new();
        ui.answer_confirm(false); // stop at the confirm: the check passed
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            ui.messages().is_empty(),
            "no port-busy dialog; got {:?}",
            ui.messages()
        );
        assert_eq!(ui.confirms().len(), 1, "the enable confirm was reached");
    }

    // §9.1 case 56.
    #[test]
    fn t56_enable_confirm_cancel_does_nothing() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(CONFIG_YAML, "provider:\n  type: openrouter\n");
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(ui.confirms().len(), 1, "the enable confirm was shown");
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]
                || r == &["service".to_string(), "uninstall".to_string()]),
            "no commands may run; runs: {:?}",
            sys.run_args()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no stop, no new spawn");
    }

    // §9.1 case 57 (0.24: the enable installs a bare unit; the service
    // reads general.web — the desktop passes and writes no token).
    #[test]
    fn t57_enable_happy_path() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        // Start sees NotInstalled; the post-enable refresh sees Installed.
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                run_out(Some(0), NOT_INSTALLED, ""),
                installed_status("running"),
            ],
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("cfg-token")));
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        let session_pid = sys.spawned_pids()[0];

        let _ = ctrl.handle(Action::ToggleBackground);

        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { token, .. })) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("cfg-token"),
                    "Ready's service token is general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        // The session child was interrupted BEFORE `install` ran.
        let evts = sys.events();
        let i_int = evts
            .iter()
            .position(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == session_pid))
            .expect("the session child was interrupted");
        let i_install = evts
            .iter()
            .position(|e| matches!(e, Effect::Run { args, .. } if args == &["install".to_string()]))
            .expect("`install` ran");
        assert!(
            i_int < i_install,
            "the session must be stopped BEFORE installing (no two backends on one config); events: {evts:?}"
        );
        // The pidfile is gone; no new sidecar spawn; tray checked.
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
        assert_eq!(
            sys.spawned_specs().len(),
            1,
            "no new sidecar spawn in service mode"
        );
        assert!(ui.last_tray().background_checked, "tray checkbox checked");
        // The install is bare: no --bind, no --tls, no --token.
        assert!(
            sys.run_args()
                .iter()
                .any(|r| { r == &["service".to_string(), "install".to_string(),] }),
            "bare service install; runs: {:?}",
            sys.run_args()
        );
    }

    // §9.1 case 58.
    #[test]
    fn t58_enable_failure_falls_back_to_session() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                run_out(Some(0), NOT_INSTALLED, ""),
                run_out(Some(0), NOT_INSTALLED, ""), // after the failed enable
            ],
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        sys.queue_child(crate::testing::FakeChildSpec::alive()); // fallback session
        sys.seed_file(CONFIG_YAML, "provider:\n  type: openrouter\n");
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(1), "", "enable --now failed\n"),
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "the user must be back in session mode; got {:?}",
            ctrl.phase()
        );
        assert!(
            sys.run_args()
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "the failed service must be uninstalled (best effort); runs: {:?}",
            sys.run_args()
        );
        assert!(
            ui.messages().iter().any(|d| {
                d.title == "Background service error" && d.body.contains("enable --now failed")
            }),
            "the error dialog must show the command output; got {:?}",
            ui.messages()
        );
        assert_eq!(sys.spawned_specs().len(), 2, "a new session was launched");
    }

    // §9.1 case 59.
    #[test]
    fn t59_disable_flow() {
        // Happy: service mode → confirm → uninstall → session.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status("running"),
                run_out(Some(0), NOT_INSTALLED, ""),
            ],
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            sys.run_args()
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "service uninstall ran"
        );
        assert_eq!(sys.spawned_specs().len(), 1, "the session spawn");
        assert!(
            !ui.last_tray().background_checked,
            "tray checkbox unchecked"
        );

        // Cancel: nothing.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            !sys.run_args()
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "Cancel must not uninstall; runs: {:?}",
            sys.run_args()
        );
    }

    // §9.1 case 60.
    #[test]
    fn t60_disable_uninstall_failure_reconnects_service() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![installed_status("running"), installed_status("running")],
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(1), "", "disable failed\n"),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "unchanged mode; got {:?}",
            ctrl.phase()
        );
        assert!(
            ui.messages().iter().any(|d| {
                d.title == "Background service error" && d.body.contains("disable failed")
            }),
            "the error dialog must show; got {:?}",
            ui.messages()
        );
        assert_eq!(sys.spawned_specs().len(), 0, "no session spawn");
    }

    // §9.1 case 61.
    #[test]
    fn t61_toggle_and_reinstall_ignored_while_starting() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = Controller::new(
            sys.clone(),
            ui.clone(),
            paths(),
            Arc::new(AtomicBool::new(false)),
            ReplacePolicy::Ask,
            Platform::Unix,
        );
        let _ = ctrl.handle(Action::ToggleBackground);
        let _ = ctrl.handle(Action::ReinstallService);
        assert!(
            matches!(ctrl.phase(), Phase::Starting(StartStep::CheckingBundled)),
            "phase must be untouched; got {:?}",
            ctrl.phase()
        );
        assert!(
            sys.run_args().is_empty(),
            "no commands may run; got {:?}",
            sys.run_args()
        );
        assert!(sys.spawned_specs().is_empty());
        assert!(ui.confirms().is_empty());
        assert!(ui.messages().is_empty());
    }

    // §9.1 case 62.
    #[test]
    fn t62_tick_detects_session_death_and_is_a_noop_in_service() {
        // (a) A session child that dies after Ready.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsAfter(2),
            exit: Some(1),
            log_write: Some("late crash".to_string()),
            ..Default::default()
        });
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );
        ctrl.tick();
        match ctrl.phase() {
            Phase::Failed(StartError::SessionExited { code, log_tail }) => {
                assert_eq!(*code, Some(1));
                assert!(log_tail.contains("late crash"), "{log_tail:?}");
            }
            other => panic!("expected SessionExited, got {other:?}"),
        }
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");

        // (b) Service mode: tick is a no-op.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );
        let phases_before = ui.phases().len();
        let events_before = sys.events().len();
        ctrl.tick();
        assert_eq!(ui.phases().len(), phases_before, "no phase change");
        assert_eq!(sys.events().len(), events_before, "no side effects");
    }

    // §9.1 case 63.
    #[test]
    fn t63_shutdown_session() {
        // (a) The child exits on SIGINT → no kill, ack sent, Break.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui);
        let (tx, rx) = mpsc::channel();
        let flow = ctrl.handle(Action::Shutdown(tx));
        assert_eq!(flow, std::ops::ControlFlow::Break(()));
        rx.recv().expect("the ack must be sent");
        assert!(
            !sys.events()
                .iter()
                .any(|e| matches!(e, Effect::ChildKill { .. })),
            "a cooperative child must not be killed"
        );
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");

        // (b) The child ignores SIGINT → SIGKILL after STOP_GRACE.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::never());
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui);
        let (tx, rx) = mpsc::channel();
        assert_eq!(
            ctrl.handle(Action::Shutdown(tx)),
            std::ops::ControlFlow::Break(())
        );
        rx.recv().expect("ack");
        let evts = sys.events();
        assert!(
            evts.iter()
                .any(|e| matches!(e, Effect::ChildInterrupt { .. })),
            "interrupted first"
        );
        assert!(
            evts.iter().any(|e| matches!(e, Effect::ChildKill { .. })),
            "killed after the grace"
        );
    }

    // §9.1 case 64.
    #[test]
    fn t64_stale_pidfile_reaper() {
        fn base_sys() -> FakeSys {
            let sys = FakeSys::new();
            sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
            sys.set_run(
                "peakbot",
                &["service", "status"],
                run_out(Some(0), NOT_INSTALLED, ""),
            );
            sys.set_default_probe(vec![Probe::Status(200)]);
            sys.queue_child(crate::testing::FakeChildSpec::alive());
            sys
        }

        /// Invariant 4 (DESIGN §3.8): after a Start that spawns a session,
        /// the pidfile exists and names the NEW session's pid + port — even
        /// when a stale pidfile was reaped just before.
        fn fresh_pidfile(sys: &FakeSys) {
            let rec = PidRecord::parse(
                &sys.file(Path::new(PIDFILE))
                    .expect("pidfile written for the new session"),
            )
            .expect("pidfile parses");
            assert_eq!(
                rec.pid,
                sys.spawned_pids()[0],
                "pidfile names the new session's pid"
            );
            assert_eq!(
                rec.exe,
                PathBuf::from(SIDECAR),
                "pidfile names the new session's executable"
            );
        }

        // (a) cmdline matches the RECORDED exe → Interrupt, then Kill
        // (stays "alive"); pidfile removed. The record is from an older
        // AppImage mount (not today's SIDECAR path): identity is the
        // recorded path, so a crash-orphaned child is still reaped.
        let sys = base_sys();
        sys.seed_file(PIDFILE, "pid=1234\nexe=/tmp/.mount_old/usr/bin/peakbot\n");
        sys.set_cmdline(1234, "/tmp/.mount_old/usr/bin/peakbot");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        let evts = sys.events();
        let i_int = evts
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Signal {
                        pid: 1234,
                        sig: Signal::Interrupt
                    }
                )
            })
            .expect("Interrupt sent to the stale pid");
        let i_kill = evts
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Signal {
                        pid: 1234,
                        sig: Signal::Kill
                    }
                )
            })
            .expect("Kill after STOP_GRACE (the stale process stays alive in the fake)");
        assert!(i_int < i_kill, "interrupt before kill");
        // The stale record is reaped, but the fresh session replaces it.
        fresh_pidfile(&sys);

        // (b) Mismatched cmdline (PID reuse by another program) → no
        // signal; the fresh
        // session rewrites the pidfile.
        let sys = base_sys();
        sys.seed_file(PIDFILE, "pid=1234\nexe=/tmp/.mount_old/usr/bin/peakbot\n");
        sys.set_cmdline(1234, "/usr/bin/vim");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            !sys.events()
                .iter()
                .any(|e| matches!(e, Effect::Signal { .. })),
            "no signal for a reused PID; events: {:?}",
            sys.events()
        );
        fresh_pidfile(&sys);

        // (c) Garbage pidfile → removed, no signal; the fresh session
        // rewrites the pidfile.
        let sys = base_sys();
        sys.seed_file(
            PIDFILE,
            "pid=notanumber\nexe=/tmp/.mount_old/usr/bin/peakbot\n",
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            !sys.events()
                .iter()
                .any(|e| matches!(e, Effect::Signal { .. })),
            "no signal for an unparseable pidfile"
        );
        fresh_pidfile(&sys);
    }

    // §9.1 case 65.
    #[test]
    fn t65_retry_stops_the_old_session_before_spawning() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );

        let _ = ctrl.handle(Action::Start); // Retry
        // Snapshot AFTER the retry: pids[0] is the stopped session,
        // pids[1] the fresh one it spawned.
        let (old_pid, new_pid) = (sys.spawned_pids()[0], sys.spawned_pids()[1]);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        let evts = sys.events();
        let i_stop = evts
            .iter()
            .position(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == old_pid))
            .expect("the old child must be interrupted");
        let i_spawn2 = evts
            .iter()
            .position(|e| matches!(e, Effect::Spawn { pid, .. } if *pid == new_pid))
            .expect("the new child was spawned");
        assert!(
            i_stop < i_spawn2,
            "the old session must be stopped before the new spawn; events: {evts:?}"
        );
        // The pidfile now names the new session on the preferred port.
        let rec = PidRecord::parse(&sys.file(Path::new(PIDFILE)).expect("pidfile rewritten"))
            .expect("pidfile parses");
        assert_eq!(rec.pid, new_pid, "pidfile names the new session's pid");
        assert_eq!(rec.exe, PathBuf::from(SIDECAR));
    }

    // -------------------------------------------------------------------
    // Design #377 — controller plumbing (task 7); TLS from general.web
    // or the unit's `TLS:` line (0.24)
    // -------------------------------------------------------------------

    /// A fresh session-mode controller, Ready, on whatever config.yaml the
    /// test seeded (none ⇒ the general.web defaults). The session child
    /// exits on SIGINT.
    fn ready_session(sys: FakeSys, ui: FakeUi) -> Controller<FakeSys, FakeUi> {
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        started(sys, ui)
    }

    // Design #377 t84 (0.24): a unit with `--tls` baked in (`TLS: on`)
    // serves https even when general.web.tls is false — the server's
    // rule is `--tls || general.web.tls`.
    #[test]
    fn t84_start_with_tls_service_probes_https_and_ready_https() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_tls("running", "on"),
        );
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { tls, token, .. })) => {
                assert!(
                    *tls,
                    "the protocol comes from the TLS: line; got {:?}",
                    ctrl.phase()
                );
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            sys.events()
                .iter()
                .any(|e| matches!(e, Effect::Probe { url } if url.to_string().starts_with("https://127.0.0.1:7823/commands"))),
            "the readiness probe must hit the https URL; events: {:?}",
            sys.events()
        );
    }

    // Design #377 t86 (0.24): Replace with `general.web.tls: true` — the
    // binary IS replaced (unlike reconfigure), the unit is installed
    // bare (it reads general.web itself) and the desktop connects https.
    #[test]
    fn t86_replace_with_tls_config_installs_bare_and_connects_https() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        // Start sees the OLD installed service; the post-replace refresh
        // sees the bare unit (no TLS line).
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status_at(INSTALLED_EXE, "running"),
                installed_status("running"),
            ],
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.seed_file(
            CONFIG_YAML,
            "general:\n  web:\n    tls: true\n    token: svc-token\n",
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started(sys.clone(), ui);

        // Command order (invariant A5, design auto-replace §4.3):
        // install → service uninstall → service install.
        assert_order_ok(&sys.run_args());
        assert!(
            sys.run_args()
                .iter()
                .any(|r| { r == &["service".to_string(), "install".to_string(),] }),
            "a bare `service install`; runs: {:?}",
            sys.run_args()
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("https://127.0.0.1:7823/?token=svc-token"),
            "general.web.tls ⇒ https; got {:?}",
            ctrl.phase()
        );
    }

    // -------------------------------------------------------------------
    // Design auto-replace §7.1 — t107–t120.
    //
    // Fixture conventions: `SIDECAR` is the bundled sidecar; the
    // installed exe's file contents are seeded with `sys.seed_file`.
    // "build-A" vs "build-B" ⇒ the builds differ; both "build-A" ⇒
    // identical. "Replace scripts" = `install`, `service uninstall` and
    // the bare `service install` scripted ok, plus
    // `service status` as a [pre, post] sequence where post =
    // `installed_status_tls("running", …)` (Exec = SIDECAR).
    // -------------------------------------------------------------------

    /// The three replace commands scripted ok (http, no TLS).
    fn script_replace_http(sys: &FakeSys) {
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
    }

    /// Seed the installed exe and the sidecar with DIFFERENT contents.
    fn seed_builds_differ(sys: &FakeSys, installed_exe: &str) {
        sys.seed_file(installed_exe, "build-A");
        sys.seed_file(SIDECAR, "build-B");
    }

    /// Seed the installed exe and the sidecar with IDENTICAL contents.
    fn seed_builds_identical(sys: &FakeSys, installed_exe: &str) {
        sys.seed_file(installed_exe, "build-A");
        sys.seed_file(SIDECAR, "build-A");
    }

    /// No `Effect::Run` whose program is `exe` (the installed binary
    /// was never executed — e.g. no `--version` query).
    fn assert_never_ran(sys: &FakeSys, exe: &str) {
        assert!(
            sys.events().iter().all(|e| {
                !matches!(e, Effect::Run { program, .. } if program == Path::new(exe))
            }),
            "`{exe}` must never be executed; events: {:?}",
            sys.events()
        );
    }

    // Design auto-replace t107 (SC1): same version, DIFFERENT build,
    // policy Auto ⇒ the replace runs silently — no dialog.
    #[test]
    fn t107_auto_same_version_different_build_replaces_silently() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = installed_status_at(INSTALLED_SAME, "running");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.seed_file(DECLINED, "installed=0.20.0\nbundled=0.21.0\n"); // stale; must be removed
        script_replace_http(&sys);
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            ui.confirms().is_empty(),
            "the silent replace must not show a dialog; got {:?}",
            ui.confirms()
        );
        // A5: copy → uninstall → install.
        assert_order_ok(&sys.run_args());
        // The content comparison ran, and BEFORE the version query.
        let evts = sys.events();
        let i_same = evts
            .iter()
            .position(|e| {
                matches!(e, Effect::SameContents { a, b }
                    if a == Path::new(INSTALLED_SAME) && b == Path::new(SIDECAR))
            })
            .expect("same_contents(installed exe, sidecar) must run");
        let i_version = evts
            .iter()
            .position(|e| {
                matches!(e, Effect::Run { program, args }
                    if program == Path::new(INSTALLED_SAME)
                        && args == &["--version".to_string()])
            })
            .expect("the installed exe's --version ran (builds differ)");
        assert!(
            i_same < i_version,
            "identity is checked before the version; events: {evts:?}"
        );
        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { tls, token, .. })) => {
                assert!(!*tls, "http service (no TLS line)");
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(!sys.has_file(Path::new(DECLINED)), "declined file removed");
    }

    // Design auto-replace t108 (SC2): identical build ⇒ connect,
    // WITHOUT any replace — and without a version query.
    #[test]
    fn t108_auto_identical_build_connects_without_replace() {
        // (a) Running: no service install at all.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_SAME, "running"),
        );
        seed_builds_identical(&sys, INSTALLED_SAME);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "no binary copy; runs: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "no service uninstall; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| {
                r.first().is_some_and(|s| s == "service")
                    && r.get(1).is_some_and(|s| s == "install")
            }),
            "a running identical service must not be re-installed; runs: {runs:?}"
        );
        assert_never_ran(&sys, INSTALLED_SAME);
        assert!(ui.confirms().is_empty(), "no dialog");

        // (b) Stopped: `service install` runs ONCE (start_first), no
        // binary copy, no uninstall.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_SAME, "stopped"),
        );
        seed_builds_identical(&sys, INSTALLED_SAME);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            runs.iter()
                .filter(|r| { *r == &["service".to_string(), "install".to_string(),] })
                .count(),
            1,
            "a stopped identical service is (re)started once; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "no binary copy; runs: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "no service uninstall; runs: {runs:?}"
        );
        assert_never_ran(&sys, INSTALLED_SAME);
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t109 (SC3): an OLDER installed build is
    // replaced silently under Auto.
    #[test]
    fn t109_auto_older_installed_replaces_silently() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.24.0")); // bundled
        let pre = installed_status_at(INSTALLED_EXE, "running");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        seed_builds_differ(&sys, INSTALLED_EXE);
        script_replace_http(&sys);
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            ui.confirms().is_empty(),
            "an older installed build is replaced silently; got {:?}",
            ui.confirms()
        );
        assert_order_ok(&sys.run_args());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design auto-replace t110 (SC4): a NEWER installed build is never
    // silent — the OLDER warning dialog appears; confirming replaces.
    #[test]
    fn t110_auto_newer_installed_asks_then_replace() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0")); // bundled
        let pre = installed_status_at(INSTALLED_NEWER, "running");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run("peakbot-0.24.0", &["--version"], versioned("0.24.0"));
        seed_builds_differ(&sys, INSTALLED_NEWER);
        script_replace_http(&sys);
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        let confirms = ui.confirms();
        assert_eq!(confirms.len(), 1, "exactly one dialog; got {confirms:?}");
        assert!(
            confirms[0].body.contains("OLDER"),
            "the downgrade warning must be shown; got {:?}",
            confirms[0].body
        );
        assert_order_ok(&sys.run_args());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design auto-replace t111 (SC4): declining the NEWER dialog
    // records the (installed, bundled) pair and silences the re-ask.
    #[test]
    fn t111_auto_newer_installed_keep_records_pair_and_does_not_reask() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0")); // bundled
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_NEWER, "running"),
        );
        sys.set_run("peakbot-0.24.0", &["--version"], versioned("0.24.0"));
        seed_builds_differ(&sys, INSTALLED_NEWER);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false); // Keep
        let mut ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );

        // The declined file carries the exact (0.24.0, 0.23.0) pair.
        let content = sys
            .file(Path::new(DECLINED))
            .expect("declined-replace written");
        let declined = DeclinedReplace::parse(&content).expect("declined file parses");
        let v24 = semver::Version::parse("0.24.0").unwrap();
        let v23 = semver::Version::parse("0.23.0").unwrap();
        assert!(
            declined.matches(&InstalledVersion::Known(v24), &v23),
            "the exact (installed 0.24.0, bundled 0.23.0) pair must be recorded"
        );
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]),
            "Keep must not copy a binary; runs: {:?}",
            sys.run_args()
        );

        // Next Start: no dialog, connects.
        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ui.confirms().len(),
            1,
            "the second Start must not re-ask; got {:?}",
            ui.confirms()
        );
    }

    // Design auto-replace t112 (SC5a): an unparseable `--version`
    // (Unknown) is replaced silently under Auto.
    #[test]
    fn t112_auto_unknown_version_different_build_replaces_silently() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = installed_status_at(INSTALLED_GARBAGE, "running");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run(
            "peakbot-garbage",
            &["--version"],
            run_out(Some(0), "hello\n", ""),
        );
        seed_builds_differ(&sys, INSTALLED_GARBAGE);
        script_replace_http(&sys);
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            ui.confirms().is_empty(),
            "an unknown version is replaced silently under Auto; got {:?}",
            ui.confirms()
        );
        assert_order_ok(&sys.run_args());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design auto-replace t113 (SC5b): an unparseable `--version` on an
    // IDENTICAL build ⇒ connect, with no version query at all.
    #[test]
    fn t113_auto_unknown_version_identical_build_connects() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_GARBAGE, "running"),
        );
        seed_builds_identical(&sys, INSTALLED_GARBAGE);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_never_ran(&sys, INSTALLED_GARBAGE);
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]),
            "no binary copy; runs: {:?}",
            sys.run_args()
        );
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t114 (SC6/A4, 0.24): the silent replace
    // installs a bare unit and the desktop writes nothing — the token is
    // general.web.token; https follows the unit's `TLS:` line.
    #[test]
    fn t114_auto_replace_installs_bare_and_writes_nothing() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = run_out(
            Some(0),
            &format!(
                "Installed: true\nExec:      {INSTALLED_SAME}\nState:     running\nTLS:       on\n"
            ),
            "",
        );
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "on")],
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        script_replace_http(&sys);
        sys.set_probe(
            "https://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Listening],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        let runs = sys.run_args();
        assert!(
            runs.iter()
                .any(|r| r == &["service".to_string(), "install".to_string()]),
            "a bare `service install`; runs: {runs:?}"
        );
        assert!(
            runs.iter().all(|r| !r
                .iter()
                .any(|a| ["--bind", "--tls", "--token"].contains(&a.as_str()))),
            "no --bind/--tls/--token on any command; runs: {runs:?}"
        );
        let writes: Vec<PathBuf> = sys
            .events()
            .iter()
            .filter_map(|e| match e {
                Effect::WritePrivate { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert!(
            writes.is_empty(),
            "the silent path writes no file; wrote {writes:?}"
        );
        match ctrl.phase() {
            Phase::Ready(Mode::Service(WebTarget { tls, token, .. })) => {
                assert!(*tls, "the TLS line ⇒ https");
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is general.web.token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t115 (SC7): a STOPPED service is replaced —
    // each of the three commands runs exactly once, then it connects.
    #[test]
    fn t115_auto_replace_stopped_service_starts_and_connects() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = installed_status_at(INSTALLED_SAME, "stopped");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        script_replace_http(&sys);
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        let runs = sys.run_args();
        assert_eq!(
            runs.iter()
                .filter(|r| *r == &["install".to_string()])
                .count(),
            1,
            "the binary copy runs exactly once; runs: {runs:?}"
        );
        assert_eq!(
            runs.iter()
                .filter(|r| *r == &["service".to_string(), "uninstall".to_string()])
                .count(),
            1,
            "service uninstall runs exactly once; runs: {runs:?}"
        );
        assert_eq!(
            runs.iter()
                .filter(|r| {
                    r.first().is_some_and(|s| s == "service")
                        && r.get(1).is_some_and(|s| s == "install")
                })
                .count(),
            1,
            "service install runs exactly once (it (re)starts the unit); runs: {runs:?}"
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t116 (SC8): the COPY fails ⇒ the service is
    // left running and untouched — no uninstall, no reinstall.
    #[test]
    fn t116_auto_replace_copy_failure_leaves_service_untouched() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_SAME, "running"),
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.set_run(
            "peakbot",
            &["install"],
            run_out(
                Some(1),
                "",
                "install failed: install I/O error: Permission denied (os error 13)\n",
            ),
        );
        // `service uninstall` / `service install` are deliberately NOT
        // scripted: if they ran, the failure would look different.
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        match ctrl.phase() {
            Phase::Failed(StartError::ServiceCommandFailed { command, output }) => {
                assert_eq!(
                    command, "install",
                    "the failing command is the copy; {command:?}"
                );
                assert!(
                    output.contains("Permission denied"),
                    "the output must carry the copy's error; {output:?}"
                );
            }
            other => panic!("expected ServiceCommandFailed{{\"install\"}}, got {other:?}"),
        }
        let runs = sys.run_args();
        assert!(
            !runs
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "the unit must not be removed when the copy fails; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| {
                r.first().is_some_and(|s| s == "service")
                    && r.get(1).is_some_and(|s| s == "install")
            }),
            "no reinstall may follow a failed copy; runs: {runs:?}"
        );
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t117 (SC8): the copy succeeds but
    // `service install` fails ⇒ ServiceCommandFailed naming the verb.
    #[test]
    fn t117_auto_replace_service_install_failure_is_servicecommandfailed() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_SAME, "running"),
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(1), "", "unit failed to start\n"),
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        match ctrl.phase() {
            Phase::Failed(StartError::ServiceCommandFailed { command, output }) => {
                assert!(
                    command.contains("service install"),
                    "command must name the failing verb; {command:?}"
                );
                assert!(
                    output.contains("unit failed to start"),
                    "output must carry the child's stderr; {output:?}"
                );
            }
            other => panic!("expected ServiceCommandFailed, got {other:?}"),
        }
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t118 (A6): no `Exec:` line ⇒ "cannot compare"
    // counts as NOT identical ⇒ the silent replace goes ahead.
    #[test]
    fn t118_auto_missing_exec_line_replaces_silently() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = run_out(Some(0), "Installed: true\nState:     running\n", ""); // no Exec
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        script_replace_http(&sys);
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            sys.events()
                .iter()
                .all(|e| !matches!(e, Effect::SameContents { .. })),
            "without an Exec path there is nothing to compare; events: {:?}",
            sys.events()
        );
        assert!(ui.confirms().is_empty(), "no dialog");
        assert_order_ok(&sys.run_args());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design auto-replace t119 (SC9): no service installed ⇒ the
    // session path is unchanged, and the content comparison never runs.
    #[test]
    fn t119_no_service_session_unchanged_and_never_compares() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("seeded-token")),
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            sys.events()
                .iter()
                .all(|e| !matches!(e, Effect::SameContents { .. })),
            "no content comparison without an installed exe; events: {:?}",
            sys.events()
        );
        let runs = sys.run_args();
        assert!(
            runs.iter().all(|r| {
                r == &["--version".to_string()]
                    || r == &["service".to_string(), "status".to_string()]
            }),
            "only `--version` and `service status` may run; runs: {runs:?}"
        );
        assert!(ui.confirms().is_empty(), "no dialog");
    }

    // Design auto-replace t120 (dev default): policy Ask with the SAME
    // version but a different build ⇒ the prompt stays, and "Keep"
    // records the (0.23.0, 0.23.0) pair.
    #[test]
    fn t120_ask_policy_same_version_different_build_prompts() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_SAME, "running"),
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("svc-token")));
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false); // Keep
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Ask);

        let confirms = ui.confirms();
        assert_eq!(
            confirms.len(),
            1,
            "dev builds keep the prompt; got {confirms:?}"
        );
        let content = sys
            .file(Path::new(DECLINED))
            .expect("declined-replace written");
        let declined = DeclinedReplace::parse(&content).expect("declined file parses");
        let v23 = semver::Version::parse("0.23.0").unwrap();
        assert!(
            declined.matches(&InstalledVersion::Known(v23.clone()), &v23),
            "the (0.23.0, 0.23.0) pair must be recorded"
        );
        assert!(
            !sys.run_args().iter().any(|r| r == &["install".to_string()]),
            "Keep must not copy a binary; runs: {:?}",
            sys.run_args()
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
    }

    // -------------------------------------------------------------------
    // Q1 (WINDOWS-PLAN) — the Windows session-only start.
    //
    // The background service is non-functional on Windows in v1, so
    // `handle_start` on `Platform::Windows` must NOT query the service
    // status at all — no `service status` sidecar invocation, no
    // `same_contents`/`--version` comparison against an installed
    // service, no AutoReplace/install/uninstall — and must ALWAYS take
    // the session path, spawning the session sidecar exactly as when no
    // service is installed. This holds even when the fake reports a
    // service as installed (identical binary, an older version, running
    // or unknown state). `Platform::Unix` behaviour is unchanged and is
    // pinned by the existing tests (t46/t47: installed ⇒ ConnectService).
    // -------------------------------------------------------------------

    // Q1: installed service whose binary is byte-IDENTICAL to the
    // bundled sidecar (running, and unknown state) — the session path
    // is taken and the service is never queried.
    #[test]
    fn q1_windows_installed_identical_start_spawns_session_without_service_status() {
        for state in ["running", "unknown"] {
            let sys = FakeSys::new();
            sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
            // The fake reports an installed service (the real-VM
            // scenario) — on Windows it must never be queried.
            sys.set_run(
                "peakbot",
                &["service", "status"],
                installed_status_at(INSTALLED_SAME, state),
            );
            seed_builds_identical(&sys, INSTALLED_SAME);
            sys.set_default_probe(vec![Probe::Status(200)]);
            sys.queue_child(crate::testing::FakeChildSpec::alive());
            sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("win-token")));
            let ui = FakeUi::new();
            let ctrl = started_on(
                sys.clone(),
                ui.clone(),
                ReplacePolicy::Ask,
                Platform::Windows,
            );

            // (1) The session path was taken, exactly like NotInstalled.
            assert!(
                matches!(
                    ctrl.phase(),
                    Phase::Ready(Mode::Session(t)) if t.bind.port() == 7823
                ),
                "Windows start must land Ready(Session) even with an installed service (state {state:?}); got {:?}",
                ctrl.phase()
            );
            let spawns = sys.spawned_specs();
            assert_eq!(spawns.len(), 1, "the session sidecar was spawned once");
            assert_eq!(
                spawns[0].args,
                Vec::<String>::new(),
                "the spawn is exactly the session path's (state {state:?}); got {:?}",
                spawns[0].args
            );
            assert_eq!(
                ready_url(&ctrl).as_deref(),
                Some("http://127.0.0.1:7823/?token=win-token"),
                "the session target is general.web (state {state:?})"
            );
            assert!(
                spawns[0]
                    .extra_env
                    .iter()
                    .all(|(k, _)| k != "PEAKBOT_WEB_TOKEN"),
                "no token in the child env (state {state:?}); got {:?}",
                spawns[0].extra_env
            );

            // (2) No service sidecar invocation at all.
            let runs = sys.run_args();
            assert!(
                !runs
                    .iter()
                    .any(|r| r == &["service".to_string(), "status".to_string()]),
                "Windows must not run `service status` (state {state:?}); runs: {runs:?}"
            );
            assert!(
                !runs.iter().any(|r| r == &["install".to_string()]),
                "no binary copy (state {state:?}); runs: {runs:?}"
            );
            assert!(
                !runs
                    .iter()
                    .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
                "no service uninstall (state {state:?}); runs: {runs:?}"
            );
            assert!(
                !runs.iter().any(|r| {
                    r.first().is_some_and(|s| s == "service")
                        && r.get(1).is_some_and(|s| s == "install")
                }),
                "no service install (state {state:?}); runs: {runs:?}"
            );

            // (3) No identity/version comparison against the installed exe.
            assert!(
                sys.events()
                    .iter()
                    .all(|e| !matches!(e, Effect::SameContents { .. })),
                "no content comparison (state {state:?}); events: {:?}",
                sys.events()
            );
            assert_never_ran(&sys, INSTALLED_SAME);

            // (4) The phase never becomes ConnectingService; no dialog.
            assert!(
                !ui.steps()
                    .iter()
                    .any(|p| matches!(p, Phase::Starting(StartStep::ConnectingService))),
                "the phase must never become ConnectingService (state {state:?}); steps: {:?}",
                ui.steps()
            );
            assert!(
                ui.confirms().is_empty(),
                "no dialog (state {state:?}); got {:?}",
                ui.confirms()
            );
        }
    }

    // Q1: the real-VM dead-end — an OLDER service (0.22.0) installed
    // with the release policy (Auto). Unfixed, `plan_launch` routes to
    // AutoReplace and silently overwrites the installed binary, then
    // ConnectService times out after 30 s. On Windows the start must
    // spawn the session and touch the service in no way: no auto-
    // replace, no replace dialog.
    #[test]
    fn q1_windows_installed_older_start_never_auto_replaces() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0")); // bundled
        // Installed 0.22.0, running, a DIFFERENT build.
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        seed_builds_differ(&sys, INSTALLED_EXE);
        // The replace commands are deliberately NOT scripted: if the
        // controller tries to replace the service, it fails loudly
        // (like t116) — the session path must not need them.
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", Some("win-token")));
        let ui = FakeUi::new();
        let ctrl = started_on(
            sys.clone(),
            ui.clone(),
            ReplacePolicy::Auto,
            Platform::Windows,
        );

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session(t)) if t.bind.port() == 7823
            ),
            "the session path must be taken; got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 1, "the session sidecar was spawned");
        assert_eq!(
            spawns[0].args,
            Vec::<String>::new(),
            "the spawn is exactly the session path's; got {:?}",
            spawns[0].args
        );

        let runs = sys.run_args();
        assert!(
            !runs
                .iter()
                .any(|r| r == &["service".to_string(), "status".to_string()]),
            "Windows must not run `service status`; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "no silent binary copy (the VM bug); runs: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "no service uninstall; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| {
                r.first().is_some_and(|s| s == "service")
                    && r.get(1).is_some_and(|s| s == "install")
            }),
            "no service install; runs: {runs:?}"
        );
        assert!(
            sys.events()
                .iter()
                .all(|e| !matches!(e, Effect::SameContents { .. })),
            "no content comparison; events: {:?}",
            sys.events()
        );
        assert_never_ran(&sys, INSTALLED_EXE);
        assert!(
            !ui.steps()
                .iter()
                .any(|p| matches!(p, Phase::Starting(StartStep::ConnectingService))),
            "the phase must never become ConnectingService; steps: {:?}",
            ui.steps()
        );
        assert!(
            ui.confirms().is_empty(),
            "no replace dialog; got {:?}",
            ui.confirms()
        );
    }

    // -------------------------------------------------------------------
    // T15b (0.24 config, spec §8.4/§11.6) — general.web is the desktop's
    // only web config: read on every (re)start and again on Ready;
    // Action::Restart restarts only the server; no settings.json or
    // web-token file I/O anywhere.
    // -------------------------------------------------------------------

    /// A config.yaml whose `general.web` carries `bind` and an optional
    /// token (the rest of the file is irrelevant to the desktop).
    fn web_yaml(bind: &str, token: Option<&str>) -> String {
        let mut s = format!("providers: {{}}\ngeneral:\n  web:\n    bind: \"{bind}\"\n");
        if let Some(t) = token {
            s.push_str(&format!("    token: {t}\n"));
        }
        s
    }

    // Spec §11.6: session-mode Restart stops the child, re-reads
    // general.web (a changed bind ⇒ a new URL) and relaunches; no dialog,
    // the app keeps running.
    #[test]
    fn restart_session_rereads_bind() {
        let sys = FakeSys::new();
        sys.seed_file(CONFIG_YAML, web_yaml("127.0.0.1:7823", None));
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/"),
            "precondition: Ready on the configured bind"
        );
        let old_pid = sys.spawned_pids()[0];

        // The user edits general.web in Settings, then clicks Restart now.
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:9100", Some("restart-token-0001")),
        );
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let flow = ctrl.handle(Action::Restart);
        assert_eq!(flow, ControlFlow::Continue(()), "the app never exits");

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:9100/?token=restart-token-0001"),
            "the relaunch re-reads general.web"
        );
        let pids = sys.spawned_pids();
        assert_eq!(pids.len(), 2, "exactly one relaunch");
        let evts = sys.events();
        let i_stop = evts
            .iter()
            .position(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == old_pid))
            .expect("the old child was stopped");
        let i_spawn = evts
            .iter()
            .position(|e| matches!(e, Effect::Spawn { pid, .. } if *pid == pids[1]))
            .expect("the new child was spawned");
        assert!(i_stop < i_spawn, "stop before relaunch; events: {evts:?}");
        assert!(
            sys.spawned_specs()[1].args.is_empty(),
            "the relaunch passes no web flags"
        );
        assert!(ui.confirms().is_empty(), "Restart asks nothing");
    }

    // Spec §11.6: service-mode Restart runs the reinstall path
    // (`reconfigure_service`: uninstall + bare install, NO binary copy),
    // then reconnects on the re-read general.web.
    #[test]
    fn restart_service_reinstalls() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("svc-token-000001")),
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "precondition; got {:?}",
            ctrl.phase()
        );
        let before = sys.run_args().len();

        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7900", Some("svc-token-000001")),
        );
        let flow = ctrl.handle(Action::Restart);
        assert_eq!(flow, ControlFlow::Continue(()), "the app never exits");

        let runs = sys.run_args()[before..].to_vec();
        let i_uninstall = runs
            .iter()
            .position(|r| r == &["service".to_string(), "uninstall".to_string()])
            .expect("service uninstall ran");
        let i_install = runs
            .iter()
            .position(|r| r == &["service".to_string(), "install".to_string()])
            .expect("bare service install ran");
        assert!(
            i_uninstall < i_install,
            "uninstall → install; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "Restart reinstalls the unit, it never replaces the binary; runs: {runs:?}"
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7900/?token=svc-token-000001"),
            "reconnects on the re-read general.web; got {:?}",
            ctrl.phase()
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "got {:?}",
            ctrl.phase()
        );
        assert!(sys.spawned_specs().is_empty(), "no session spawn");
        assert!(ui.confirms().is_empty(), "Restart asks nothing");
    }

    // Spec §8.4/§11.6: the child's first boot auto-migrates a legacy
    // config, moving the web-token into general.web.token. The pre-launch
    // read saw no token; the re-read when the child answers picks it up,
    // so the URL the bridge navigates to carries it.
    #[test]
    fn ready_reread_picks_migrated_token() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        // Legacy 0.23 layout: no general.web yet.
        sys.seed_file(CONFIG_YAML, "default_model: gpt\n");
        sys.queue_child(crate::testing::FakeChildSpec {
            boot_write: Some((
                PathBuf::from(CONFIG_YAML),
                web_yaml("127.0.0.1:7823", Some("migrated-token-01")),
            )),
            ..crate::testing::FakeChildSpec::alive()
        });
        // The migrated server rejects the tokenless probe.
        sys.set_probe("http://127.0.0.1:7823/commands", vec![Probe::Status(401)]);
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=migrated-token-01",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/?token=migrated-token-01"),
            "the Ready URL comes from the fresh read; got {:?}",
            ctrl.phase()
        );
        assert!(matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))));
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch needed");
    }

    // Spec §11.6: no code path reads or writes settings.json or
    // web-token — across start, session restart, enable, service
    // restart and disable. The token never appears in any argv/env.
    #[test]
    fn no_settings_json_or_web_token_io() {
        let sys = FakeSys::new();
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("file-token-000001")),
        );
        sys.seed_file(
            SETTINGS_FILE,
            "{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":true}\n",
        );
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("cfg-token-0000001")),
        );
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                run_out(Some(0), NOT_INSTALLED, ""), // start
                run_out(Some(0), NOT_INSTALLED, ""), // session restart
                installed_status("running"),         // after enable
                installed_status("running"),         // after service restart
                run_out(Some(0), NOT_INSTALLED, ""), // after disable
            ],
        );
        for args in [
            &["install"][..],
            &["service", "install"][..],
            &["service", "uninstall"][..],
        ] {
            sys.set_run("peakbot", args, run_out(Some(0), "ok\n", ""));
        }
        sys.set_default_probe(vec![Probe::Status(200)]);
        for _ in 0..3 {
            sys.queue_child(crate::testing::FakeChildSpec {
                mode: ChildMode::ExitsOnInterrupt,
                exit: Some(0),
                ..Default::default()
            });
        }
        let ui = FakeUi::new();
        ui.answer_confirm(true); // enable
        ui.answer_confirm(true); // disable
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))));
        let _ = ctrl.handle(Action::Restart);
        assert!(matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))));
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))),
            "enabled; got {:?}",
            ctrl.phase()
        );
        let _ = ctrl.handle(Action::Restart);
        assert!(matches!(ctrl.phase(), Phase::Ready(Mode::Service(_))));
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session(_))),
            "disabled; got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/?token=cfg-token-0000001"),
            "the token is general.web.token, never the web-token file's"
        );

        for path in [WEB_TOKEN, SETTINGS_FILE] {
            assert!(
                !sys.read_paths().iter().any(|p| p == Path::new(path)),
                "{path} must never be read; reads: {:?}",
                sys.read_paths()
            );
            assert!(
                sys.events().iter().all(|e| {
                    !matches!(e, Effect::WritePrivate { path: p } | Effect::Remove { path: p }
                        if p == Path::new(path))
                }),
                "{path} must never be written or removed; events: {:?}",
                sys.events()
            );
        }
        for spec in sys.spawned_specs() {
            assert!(spec.args.is_empty(), "no web flags; got {:?}", spec.args);
            assert!(
                spec.extra_env.iter().all(|(k, v)| k != "PEAKBOT_WEB_TOKEN"
                    && !v.contains("cfg-token")
                    && !v.contains("file-token")),
                "no token in the child env; got {:?}",
                spec.extra_env
            );
        }
        for r in sys.run_args() {
            assert!(
                r.iter().all(|a| !a.contains("token")),
                "no token in any argv (I7); got {r:?}"
            );
        }
    }

    /// The URL the bridge would navigate to (`None` unless Ready).
    fn ready_url(ctrl: &Controller<FakeSys, FakeUi>) -> Option<String> {
        match ctrl.phase() {
            Phase::Ready(mode) => Some(mode.url().to_string()),
            _ => None,
        }
    }
}
