//! The startup/mode state machine (DESIGN §4.3–§4.6/§5.3). One owner of
//! the child + phase ⇒ no locks, no races.

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;

use semver::Version;

use crate::consts::{
    LOG_TAIL_LINES, POLL_INTERVAL, PORT_ATTEMPTS, READY_TIMEOUT, SERVICE_PORT,
    SESSION_PREFERRED_PORT, STOP_GRACE, TICK,
};
use crate::paths::DesktopPaths;
use crate::phase::{Dialog, Mode, Phase, StartError, StartStep, TrayModel, tray_model};
use crate::ready::{ReadyOutcome, wait_ready};
use crate::service::{self, InstalledService, LaunchPlan, RunState, ServiceState};
use crate::session::{self, EarlyExit, PidRecord};
use crate::sys::{Child, RunOutput, Signal, SpawnSpec, Sys};
use crate::token::Token;
use crate::version::{DeclinedReplace, InstalledVersion, parse_version_output};

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
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// The session sidecar this controller owns (at most one, §3.8-2).
struct Session {
    child: Box<dyn Child>,
    port: u16,
    token: Token,
}

impl Session {
    fn mode(&self) -> Mode {
        Mode::Session {
            port: self.port,
            token: self.token.clone(),
        }
    }
}

/// Owns `sys, ui, paths, quitting, phase, bundled, service, session`
/// (DESIGN §5.3). Every phase assignment goes through `set_phase`, which
/// is also where the tray model is (re)emitted — a `service` refresh
/// always lands before the next `set_phase`, so the tray checkbox mirrors
/// the last `service status`.
pub struct Controller<S: Sys, U: Ui> {
    sys: S,
    ui: U,
    paths: DesktopPaths,
    quitting: Arc<AtomicBool>,
    phase: Phase,
    /// The bundled (this app's) PeakBot version — locked equal to the
    /// sidecar's by `check-version.sh`; the `--version` run in
    /// CheckingBundled is a liveness check, not the version source.
    bundled: Option<Version>,
    service: Option<ServiceState>,
    session: Option<Session>,
}

impl<S: Sys, U: Ui> Controller<S, U> {
    /// `phase = Starting(CheckingBundled)`. No `phase_changed` is emitted
    /// here — the first emission happens on the first `handle`.
    pub fn new(sys: S, ui: U, paths: DesktopPaths, quitting: Arc<AtomicBool>) -> Self {
        Self {
            sys,
            ui,
            paths,
            quitting,
            phase: Phase::Starting(StartStep::CheckingBundled),
            bundled: Version::parse(env!("CARGO_PKG_VERSION")).ok(),
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
            // While Starting the tray item is disabled; ignore.
            Action::ToggleBackground | Action::ReinstallService => {}
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
        let tray = tray_model(&self.phase, self.service.as_ref(), self.bundled.as_ref());
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
        let reaped_stale = self.reap_stale_pidfile();

        self.set_phase(Phase::Starting(StartStep::CheckingBundled));
        if let Err(err) = self.check_bundled_sidecar() {
            return self.fail(err);
        }

        let state = self.query_service_status();
        self.service = Some(state.clone());
        self.set_phase(Phase::Starting(StartStep::CheckingService));

        let Some(bundled) = self.bundled.clone() else {
            return self.fail(StartError::SidecarBroken {
                detail: "bundled version unavailable".to_string(),
            });
        };
        let declined = self.read_declined();
        let plan = service::plan_launch(
            &state,
            |svc| self.version_of(svc),
            &bundled,
            declined.as_ref(),
        );
        match plan {
            LaunchPlan::Session => self.launch_session(reaped_stale),
            LaunchPlan::ConnectService { start_first } => self.connect_service(start_first, false),
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
                    self.connect_service(start_first, false);
                }
            }
        }
    }

    /// `<sidecar> --version`: NotFound → SidecarMissing; failed or
    /// unparseable → SidecarBroken.
    fn check_bundled_sidecar(&self) -> Result<(), StartError> {
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
        if !out.ok() || parse_version_output(&out.stdout).is_none() {
            return Err(StartError::SidecarBroken {
                detail: format!(
                    "`--version` failed or was unparseable: {}",
                    out.combined().trim()
                ),
            });
        }
        Ok(())
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

    /// `version_of(installed)` per DESIGN §4.4: exe None → Unknown, else
    /// `<exe> --version`.
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

    fn launch_session(&mut self, suppress_pidfile: bool) {
        let mut tried: Vec<u16> = Vec::new();
        for attempt in 1..=PORT_ATTEMPTS {
            self.set_phase(Phase::Starting(StartStep::StartingSession));
            // 7824 if free (and not already burned by a "port taken"
            // exit), else a fresh port from the OS.
            let port = if self.sys.port_is_free(SESSION_PREFERRED_PORT)
                && !tried.contains(&SESSION_PREFERRED_PORT)
            {
                SESSION_PREFERRED_PORT
            } else {
                match self.sys.free_port() {
                    Ok(p) => p,
                    Err(_) => return self.fail(StartError::PortUnavailable),
                }
            };
            tried.push(port);

            let token = Token::generate(&self.sys);
            let spec = SpawnSpec {
                program: self.paths.sidecar.clone(),
                args: session::session_args(port),
                extra_env: session::session_env(&token),
                log: self.paths.session_log(),
            };
            let child = match self.sys.spawn(spec) {
                Ok(c) => c,
                Err(e) => {
                    return self.fail(StartError::SidecarBroken {
                        detail: e.to_string(),
                    });
                }
            };
            let mut sess = Session { child, port, token };
            let mode = sess.mode();
            let rec = PidRecord {
                pid: sess.child.pid(),
                port,
            };
            if !suppress_pidfile
                && let Err(e) = self.sys.write_private(&self.paths.pidfile(), &rec.render())
            {
                log::warn!("failed to write pidfile: {e}");
            }

            self.set_phase(Phase::Starting(StartStep::WaitingForBackend));
            match wait_ready(
                &self.sys,
                &mode.probe_url(),
                READY_TIMEOUT,
                Some(sess.child.as_mut()),
                &self.quitting,
            ) {
                ReadyOutcome::Ready => {
                    self.session = Some(sess);
                    self.set_phase(Phase::Ready(mode));
                    return;
                }
                ReadyOutcome::ChildExited(code) => {
                    let log_tail = self.log_tail();
                    self.remove_pidfile();
                    if session::classify_early_exit(code, &log_tail) == EarlyExit::PortTaken {
                        if attempt < PORT_ATTEMPTS {
                            // The TOCTOU between free_port() and the
                            // child's bind: retry on a new port.
                            continue;
                        }
                        // Port taken on the final attempt = exhausted.
                        self.fail(StartError::PortUnavailable);
                        return;
                    }
                    self.fail(StartError::SessionExited { code, log_tail });
                    return;
                }
                ReadyOutcome::Timeout => {
                    self.stop_child(&mut sess.child);
                    self.remove_pidfile();
                    self.fail(StartError::NotReady {
                        waited: READY_TIMEOUT,
                    });
                    return;
                }
                ReadyOutcome::TokenRejected => {
                    self.stop_child(&mut sess.child);
                    self.remove_pidfile();
                    self.fail(StartError::TokenRejected { service: false });
                    return;
                }
                // Quitting: an internal control-flow outcome, never a
                // visible phase (§3.5).
                ReadyOutcome::Cancelled => {
                    self.stop_child(&mut sess.child);
                    self.remove_pidfile();
                    return;
                }
            }
        }
        self.fail(StartError::PortUnavailable);
    }

    // -------------------------------------------------------------------
    // CONNECT (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn connect_service(&mut self, start_first: bool, trust_install: bool) {
        self.set_phase(Phase::Starting(StartStep::ConnectingService));
        let token = self.read_web_token();
        if start_first
            && let Err((command, output)) = self.run_sidecar_cmd(&service::service_install_args())
        {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }
        let mode = Mode::Service { token };
        match wait_ready(
            &self.sys,
            &mode.probe_url(),
            READY_TIMEOUT,
            None,
            &self.quitting,
        ) {
            ReadyOutcome::Ready => self.set_phase(Phase::Ready(mode)),
            ReadyOutcome::TokenRejected => self.fail(StartError::TokenRejected { service: true }),
            ReadyOutcome::Timeout => {
                if trust_install {
                    // `service install` starts the unit itself (DESIGN
                    // §4.3); right after a successful install the probe is
                    // a sanity check, the command is the source of truth.
                    self.set_phase(Phase::Ready(mode));
                } else {
                    self.fail(StartError::ServiceUnreachable {
                        waited: READY_TIMEOUT,
                    })
                }
            }
            ReadyOutcome::Cancelled => {}
            ReadyOutcome::ChildExited(_) => {} // no child in service mode
        }
    }

    /// The web-token is exactly what the file holds — never generated
    /// here (a generated token would not match the running service).
    fn read_web_token(&self) -> Option<Token> {
        self.sys
            .read(&self.paths.web_token())
            .ok()
            .flatten()
            .and_then(|s| Token::parse(&s))
    }

    /// Generate + write the web-token 0600 iff absent. When a session
    /// token is passed (mode toggle), it is reused — the webview's token
    /// stays stable across the port switch.
    fn ensure_token(&self, preferred: Option<&Token>) {
        if self
            .sys
            .read(&self.paths.web_token())
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        let token = preferred
            .cloned()
            .unwrap_or_else(|| Token::generate(&self.sys));
        if let Err(e) = self
            .sys
            .write_private(&self.paths.web_token(), token.as_str())
        {
            log::warn!("failed to write web-token: {e}");
        }
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
    // REPLACE (DESIGN §4.3) — also Action::ReinstallService
    // -------------------------------------------------------------------

    fn replace_service(&mut self) {
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        // Non-zero tolerated: the unit may not be loaded.
        let _ = self
            .sys
            .run(&self.paths.sidecar, &service::service_uninstall_args());

        self.set_phase(Phase::Starting(StartStep::InstallingService));
        self.ensure_token(None);
        if let Err((command, output)) = self
            .run_sidecar_cmd(&service::install_args())
            .and_then(|_| self.run_sidecar_cmd(&service::service_install_args()))
        {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }

        self.refresh_service();
        if let Err(e) = self.sys.remove(&self.paths.declined_file()) {
            log::warn!("failed to remove declined-replace: {e}");
        }
        self.connect_service(false, true);
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
        if !self.sys.port_is_free(SERVICE_PORT) {
            self.ui.message(&service::port_busy_dialog());
            return;
        }
        if !self.ui.confirm(&service::enable_dialog()) {
            return;
        }
        // The service reuses the session's token when it takes over
        // (stable webview token across the port switch) — capture it
        // before the session goes away.
        let session_token = self.session.as_ref().map(|s| s.token.clone());
        // Stop the session BEFORE installing: two backends must never
        // share the config/DB.
        self.stop_session();

        // REPLACE steps minus the uninstall.
        self.set_phase(Phase::Starting(StartStep::InstallingService));
        self.ensure_token(session_token.as_ref());
        if let Err((command, output)) = self
            .run_sidecar_cmd(&service::install_args())
            .and_then(|_| self.run_sidecar_cmd(&service::service_install_args()))
        {
            return self.enable_failed(&command, &output);
        }
        self.refresh_service();
        self.connect_service(false, true);
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
        self.launch_session(false);
    }

    fn disable_background(&mut self) {
        if !self.ui.confirm(&service::disable_dialog()) {
            return;
        }
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        match self.run_sidecar_cmd(&service::service_uninstall_args()) {
            Ok(_) => {
                self.refresh_service();
                self.launch_session(false);
            }
            Err((command, output)) => {
                self.refresh_service();
                self.ui
                    .message(&service::command_failed_dialog(&command, &output));
                // Unchanged mode: reconnect to the still-installed service.
                self.connect_service(false, false);
            }
        }
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

    /// Stale pidfile reaper: identity-checked (cmdline) SIGINT → poll up
    /// to STOP_GRACE → SIGKILL; the pidfile is always removed. Returns
    /// true when a stale pidfile was found and reaped (the fresh
    /// session then skips its pidfile write — the reaped record was the
    /// bookkeeping for the previous crash).
    fn reap_stale_pidfile(&mut self) -> bool {
        let pidfile = self.paths.pidfile();
        let Some(text) = self.sys.read(&pidfile).ok().flatten() else {
            return false;
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
        true
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
    use crate::consts::{PORT_ATTEMPTS, READY_TIMEOUT, SERVICE_PORT, SESSION_PREFERRED_PORT};
    use crate::paths::DesktopPaths;
    use crate::phase::{Mode, Phase, StartError, StartStep};
    use crate::session::PidRecord;
    use crate::sys::{Probe, RunOutput, Signal};
    use crate::testing::{ChildMode, Effect, FakeSys, FakeUi};
    use crate::version::{DeclinedReplace, InstalledVersion};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    const SIDECAR: &str = "/app/peakbot";
    const PIDFILE: &str = "/data/sidecar.pid";
    const WEB_TOKEN: &str = "/cfg/web-token";
    const DECLINED: &str = "/data/declined-replace";
    const CONFIG_YAML: &str = "/cfg/config.yaml";

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

    const NOT_INSTALLED: &str = "Installed: false\n";

    /// `Controller::new` + `handle(Start)`, asserting we land Ready.
    fn started(sys: FakeSys, ui: FakeUi) -> Controller<FakeSys, FakeUi> {
        let mut ctrl = Controller::new(sys, ui, paths(), Arc::new(AtomicBool::new(false)));
        let _ = ctrl.handle(Action::Start);
        ctrl
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
        );
        let _ = ctrl.handle(Action::Start);
        match ctrl.phase() {
            Phase::Failed(StartError::SidecarBroken { detail }) => {
                assert!(!detail.is_empty(), "detail must describe the failure");
            }
            other => panic!("expected SidecarBroken, got {other:?}"),
        }
    }

    // §9.1 case 40.
    #[test]
    fn t40_happy_session() {
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

        let port = match ctrl.phase() {
            Phase::Ready(Mode::Session { port, .. }) => *port,
            other => panic!("expected Ready(Session), got {other:?}"),
        };
        assert_eq!(
            port, SESSION_PREFERRED_PORT,
            "7824 is free ⇒ preferred port"
        );

        // Spawn: args + env.
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 1, "exactly one spawn");
        assert_eq!(
            spawns[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()]
        );
        let token = sys
            .tokens_generated()
            .first()
            .cloned()
            .expect("a session token was generated");
        assert!(
            spawns[0]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == &token),
            "spawn env must carry the token; got {:?}",
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

        // Pidfile: pid/port.
        let pidfile = sys.file(Path::new(PIDFILE)).expect("pidfile written");
        let rec = PidRecord::parse(&pidfile).expect("pidfile parses");
        assert_eq!(rec.port, SESSION_PREFERRED_PORT);

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

    // §9.1 case 41.
    #[test]
    fn t41_busy_preferred_port_falls_back_to_free_port() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_port_free(SESSION_PREFERRED_PORT, false);
        sys.set_free_port(7999);
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { port, .. }) if *port == 7999),
            "got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
        assert_eq!(
            spawns[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7999".to_string()]
        );
        assert!(
            sys.events().iter().any(|e| matches!(e, Effect::FreePort)),
            "free_port() must be consulted"
        );
    }

    // §9.1 case 42.
    #[test]
    fn t42_port_taken_retries_then_gives_up() {
        let port_taken_child = || crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsAfter(1),
            exit: Some(1),
            log_write: Some("127.0.0.1:7824 is already in use".to_string()),
            ..Default::default()
        };

        // Scenario A: first attempt hits a taken port; the second succeeds.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.set_free_port(7999);
        sys.queue_child(port_taken_child());
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { port, .. }) if *port == 7999),
            "second attempt must use the free port; got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 2, "two attempts");
        assert_eq!(
            spawns[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()]
        );
        assert_eq!(
            spawns[1].args,
            vec!["--bind".to_string(), "127.0.0.1:7999".to_string()]
        );

        // Scenario B: PORT_ATTEMPTS taken-port exits ⇒ PortUnavailable.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_free_port(7999);
        for _ in 0..PORT_ATTEMPTS {
            sys.queue_child(port_taken_child());
        }
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Failed(StartError::PortUnavailable)),
            "got {:?}",
            ctrl.phase()
        );
        assert_eq!(sys.spawned_specs().len(), PORT_ATTEMPTS as usize);
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service { token }) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is exactly what the web-token file holds"
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
            !runs.iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                ]
            }),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert!(
            runs.iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                ]
            }),
            "`service install --bind 127.0.0.1:7823` must run; runs: {runs:?}"
        );
    }

    // §9.1 case 48.
    #[test]
    fn t48_installed_tokenless_service_ready_without_token_file() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        // No web-token file.
        sys.set_probe("http://127.0.0.1:7823/commands", vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service { token }) => {
                assert!(token.is_none(), "absent web-token ⇒ Service with no token");
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            !sys.has_file(Path::new(WEB_TOKEN)),
            "the token file must NOT be created in CONNECT (it would not match the running service)"
        );
    }

    // §9.1 case 49.
    #[test]
    fn t49_replace_flow_command_order_and_cleanup() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(DECLINED, "installed=0.20.0\nbundled=0.21.0\n"); // stale; must be removed
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started(sys.clone(), ui);

        let tok = sys
            .tokens_generated()
            .first()
            .cloned()
            .expect("ensure_token generated a token");
        match ctrl.phase() {
            Phase::Ready(Mode::Service { token }) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some(tok.as_str()),
                    "Ready with the generated token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        assert!(
            sys.events()
                .iter()
                .any(|e| { matches!(e, Effect::Probe { url } if url.to_string().contains(&tok)) }),
            "the readiness probe must use the generated token"
        );

        // Command order: service uninstall → install → service install.
        let runs = sys.run_args();
        let i_uninstall = runs
            .iter()
            .position(|r| r == &["service".to_string(), "uninstall".to_string()])
            .expect("service uninstall ran");
        let i_install = runs
            .iter()
            .position(|r| r == &["install".to_string()])
            .expect("install ran");
        let i_svc = runs
            .iter()
            .position(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                ]
            })
            .expect("service install ran");
        assert!(
            i_uninstall < i_install && i_install < i_svc,
            "order must be uninstall → install → service install; runs: {runs:?}"
        );

        // ensure_token: the absent web-token was written 0600.
        assert!(sys.has_file(Path::new(WEB_TOKEN)), "web-token created");
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "web-token must be written private (0600)"
        );
        // The stale declined file is removed.
        assert!(!sys.has_file(Path::new(DECLINED)), "declined file removed");
    }

    // §9.1 case 50.
    #[test]
    fn t50_keep_flow_writes_declined_pair_and_silences_next_start() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false); // Keep
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "got {:?}",
            ctrl.phase()
        );

        // The declined file carries the exact pair.
        let content = sys
            .file(Path::new(DECLINED))
            .expect("declined-replace written");
        let declined = DeclinedReplace::parse(&content).expect("declined file parses");
        let v22 = semver::Version::parse("0.22.0").unwrap();
        let v23 = semver::Version::parse("0.23.0").unwrap();
        assert!(
            declined.matches(&InstalledVersion::Known(v22.clone()), &v23.clone()),
            "the exact (installed, bundled) pair must be recorded"
        );
        assert!(
            !sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                ]
            }),
            "Keep must not (re)install the service"
        );

        // Next Start: no dialog.
        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.set_run("peakbot", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        // Declined for an OLDER bundled version — the pair no longer matches.
        sys.seed_file(DECLINED, "installed=0.22.0\nbundled=0.21.0\n");
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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

    // §9.1 case 53.
    #[test]
    fn t53_replace_service_install_failure_is_servicecommandfailed() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.22.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status("running"),
        );
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "precondition; got {:?}",
            ctrl.phase()
        );
        // config.yaml absent.
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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

    // §9.1 case 55.
    #[test]
    fn t55_enable_with_busy_port_shows_port_busy() {
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
        sys.set_port_free(SERVICE_PORT, false);
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "got {:?}",
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
        assert_eq!(
            sys.spawned_specs().len(),
            1,
            "the running session is untouched"
        );
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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

    // §9.1 case 57.
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
        sys.seed_file(CONFIG_YAML, "provider:\n  type: openrouter\n");
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        let session_pid = sys.spawned_pids()[0];

        let _ = ctrl.handle(Action::ToggleBackground);

        match ctrl.phase() {
            Phase::Ready(Mode::Service { token }) => {
                let tok = sys
                    .tokens_generated()
                    .first()
                    .cloned()
                    .expect("ensure_token generated a token");
                assert_eq!(token.as_ref().map(|t| t.as_str()), Some(tok.as_str()));
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
        // The token was written 0600 (absent before).
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "ensure_token wrote the absent web-token 0600"
        );
        // The pidfile is gone; no new sidecar spawn; tray checked.
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
        assert_eq!(
            sys.spawned_specs().len(),
            1,
            "no new sidecar spawn in service mode"
        );
        assert!(ui.last_tray().background_checked, "tray checkbox checked");
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
            &["service", "install", "--bind", "127.0.0.1:7823"],
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "precondition; got {:?}",
            ctrl.phase()
        );
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui.clone());
        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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

        // (a) Matching cmdline → Interrupt, then Kill (stays "alive"); pidfile removed.
        let sys = base_sys();
        sys.seed_file(PIDFILE, "pid=1234\nport=7824\n");
        sys.set_cmdline(1234, "/tmp/.mount_x/usr/bin/peakbot --bind 127.0.0.1:7824");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");

        // (b) Mismatched cmdline (PID reuse) → no signal, pidfile removed.
        let sys = base_sys();
        sys.seed_file(PIDFILE, "pid=1234\nport=7824\n");
        sys.set_cmdline(1234, "/usr/bin/vim --bind 127.0.0.1:7824");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");

        // (c) Garbage pidfile → removed, no signal.
        let sys = base_sys();
        sys.seed_file(PIDFILE, "pid=notanumber\nport=7824\n");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "got {:?}",
            ctrl.phase()
        );
        assert!(
            !sys.events()
                .iter()
                .any(|e| matches!(e, Effect::Signal { .. })),
            "no signal for an unparseable pidfile"
        );
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "precondition; got {:?}",
            ctrl.phase()
        );

        let _ = ctrl.handle(Action::Start); // Retry
        // Snapshot AFTER the retry: pids[0] is the stopped session,
        // pids[1] the fresh one it spawned.
        let (old_pid, new_pid) = (sys.spawned_pids()[0], sys.spawned_pids()[1]);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        assert_eq!(rec.port, SESSION_PREFERRED_PORT);
    }
}
