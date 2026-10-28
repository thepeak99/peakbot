//! The startup state machine (DESIGN §4.3/§4.6/§5.3). One owner of
//! the child + phase ⇒ no locks, no races.

use std::io;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;

use semver::Version;

use crate::ca_trust::{PeakbotCa, TrustStore};
use crate::consts::{LOG_TAIL_LINES, POLL_INTERVAL, READY_TIMEOUT, STOP_GRACE, TICK};
use crate::paths::DesktopPaths;
use crate::phase::{Dialog, Phase, Platform, StartError, StartStep, tray_status};
use crate::ready::{ReadyOutcome, wait_ready};
use crate::service;
use crate::session::{self, PidRecord};
use crate::sys::{Child, Signal, SpawnSpec, Sys};
use crate::version::parse_version_output;
use crate::web_target::{WebTarget, read_web_target};

/// The UI surface the controller drives (the Tauri glue implements it).
pub trait Ui: Send {
    fn phase_changed(&self, phase: &Phase, tray_status: &str);
    /// ok → true.
    fn confirm(&self, d: &Dialog) -> bool;
}

#[derive(Debug, Clone)]
pub enum Action {
    Start,
    /// Restart only the PeakBot server so an edited `general.web` (or
    /// any boot-only config) applies (spec §8.4). Never the app.
    Restart,
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// The session sidecar this controller owns (at most one, §3.8-2).
struct Session {
    child: Box<dyn Child>,
}

/// Owns `sys, ui, paths, quitting, platform, phase, bundled, session`
/// (DESIGN §5.3). Every phase assignment goes through `set_phase`, which
/// is also where the tray status is (re)emitted.
pub struct Controller<S: Sys, U: Ui> {
    sys: S,
    ui: U,
    paths: DesktopPaths,
    quitting: Arc<AtomicBool>,
    /// The host platform (Q1): set in `new`, never changes. Chosen once
    /// by the glue via `phase::host_platform()` (windows / macOS /
    /// everything else) — the pure core never reads `cfg!` itself. On
    /// `Platform::Windows` the start path is session-only: the old
    /// background service never worked there and is never detected.
    platform: Platform,
    phase: Phase,
    /// The bundled (this app's) PeakBot version — the sidecar's
    /// `--version` output, parsed in CheckingBundled (DESIGN §1: the
    /// sidecar is the version source).
    bundled: Option<Version>,
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
        platform: Platform,
    ) -> Self {
        Self {
            sys,
            ui,
            paths,
            quitting,
            platform,
            phase: Phase::Starting(StartStep::CheckingBundled),
            bundled: None,
            session: None,
        }
    }

    /// `Break` after Shutdown.
    pub fn handle(&mut self, action: Action) -> ControlFlow<()> {
        match action {
            Action::Start => self.handle_start(),
            // The full start path: stops the child, re-reads `general.web`
            // and relaunches. Ignored while a start is in flight.
            Action::Restart if !self.is_starting() => self.handle_start(),
            Action::Restart => {}
            Action::Shutdown(ack) => {
                self.stop_session();
                let _ = ack.send(());
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    /// Liveness: a Ready session whose child exited →
    /// Failed(SessionExited); a no-op otherwise.
    pub fn tick(&mut self) {
        if !matches!(self.phase, Phase::Ready(_)) {
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
    /// `phase_changed` with a fresh tray status.
    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        let tray = tray_status(&self.phase, self.bundled.as_ref());
        self.ui.phase_changed(&self.phase, &tray);
    }

    fn fail(&mut self, err: StartError) {
        self.set_phase(Phase::Failed(err));
    }

    fn is_starting(&self) -> bool {
        matches!(self.phase, Phase::Starting(_))
    }

    /// THE single exit into `Phase::Ready` (design #376b): an HTTPS
    /// target needs PeakBot's CA trusted first. Everything that makes the
    /// window open goes through here — never a bare `set_phase(Ready)`.
    fn enter_ready(&mut self, target: WebTarget) {
        if target.tls
            && let Err(detail) = self.ensure_ca_trusted()
        {
            self.fail(StartError::CertificateNotTrusted { detail });
            return;
        }
        self.set_phase(Phase::Ready(target));
    }

    /// macOS/Windows: PeakBot's CA must be in the user's trusted
    /// certificates, else the webview can't open the HTTPS URL. Check
    /// first; only a FAILED check runs the add (the OS prompt). Linux
    /// trusts the CA in the webview and touches nothing. All failures are
    /// a `String` detail for `CertificateNotTrusted` — the session child
    /// is left alive (the caller owns the child; `fail` never stops it).
    fn ensure_ca_trusted(&mut self) -> Result<(), String> {
        if self.platform == Platform::Linux {
            return Ok(());
        }
        let home = directories_next::BaseDirs::new()
            .ok_or("no home directory (directories crate unavailable)")?
            .home_dir()
            .to_path_buf();
        let Some(store) = TrustStore::for_platform(self.platform, &home) else {
            return Ok(());
        };
        let path = crate::paths::peakbot_ca_cert()
            .ok_or("cannot resolve the PeakBot CA certificate path")?;
        let pem = match self.sys.read(&path) {
            Ok(Some(pem)) => pem,
            Ok(None) => return Err(format!("{} not found", path.display())),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let ca = PeakbotCa::parse(&path, &pem)?;

        let check = store.check_cmd(&ca);
        let check_args: Vec<&str> = check.args.iter().map(String::as_str).collect();
        if let Ok(out) = self.sys.run(Path::new(check.program), &check_args)
            && store.is_trusted(&ca, &out)
        {
            return Ok(());
        }

        self.set_phase(Phase::Starting(StartStep::TrustingCertificate));
        let add = store.add_cmd(&ca);
        let add_args: Vec<&str> = add.args.iter().map(String::as_str).collect();
        match self.sys.run(Path::new(add.program), &add_args) {
            Ok(out) if out.ok() => Ok(()),
            Ok(out) => Err(format!("$ {add}\n{}", out.combined().trim())),
            Err(e) => Err(format!("$ {add}\n{e}")),
        }
    }

    // -------------------------------------------------------------------
    // Action::Start (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn handle_start(&mut self) {
        self.stop_session();
        self.reap_stale_pidfile();

        self.set_phase(Phase::Starting(StartStep::CheckingBundled));
        if let Err(err) = self.check_bundled_sidecar() {
            return self.fail(err);
        }

        // Q1 (WINDOWS-PLAN): the background service never worked on
        // Windows, so it is never looked for there: no `service status`,
        // no "Looking for a background service…" step, no prompt.
        if self.platform == Platform::Windows {
            self.launch_session();
            return;
        }

        // A failed or unrecognised status counts as "not installed": the
        // session then reports a real port conflict itself.
        let installed = self
            .sys
            .run(&self.paths.sidecar, &service::status_args())
            .is_ok_and(|out| service::is_service_installed(&out));
        self.set_phase(Phase::Starting(StartStep::CheckingService));
        if !installed {
            self.launch_session();
            return;
        }
        // Asked on every start, never remembered: the old service would
        // fight the session for the port and the config.
        if !self.ui.confirm(&service::remove_old_service_dialog()) {
            return self.fail(StartError::ServiceInstalled);
        }
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        if let Err((command, output)) = self.run_sidecar_cmd(&service::service_uninstall_args()) {
            return self.fail(StartError::ServiceCommandFailed { command, output });
        }
        self.launch_session();
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

    // -------------------------------------------------------------------
    // LAUNCH_SESSION (DESIGN §4.3)
    // -------------------------------------------------------------------

    fn launch_session(&mut self) {
        // general.web is read on every launch (spec §8.4); the child gets
        // no web flags and reads the same file itself.
        let target = self.read_web_target();
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
        let (outcome, target) = self.await_backend(target, &mut child);
        match outcome {
            ReadyOutcome::Ready => {
                self.session = Some(Session { child });
                self.enter_ready(target);
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
                self.fail(StartError::TokenRejected);
            }
            // Quitting: an internal control-flow outcome, never a
            // visible phase (§3.5).
            ReadyOutcome::Cancelled => {
                self.stop_child(&mut child);
                self.remove_pidfile();
            }
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

    /// Probe until the backend answers, then read `general.web` AGAIN
    /// (spec §8.4, Z8): the child's first boot may have auto-migrated a
    /// legacy config, moving the web-token into `general.web.token`, so
    /// the pre-launch read can be stale. A 401 also counts as "it
    /// answered". If the fresh read differs, probe once more with it and
    /// return the fresh target — that is the URL the bridge navigates to.
    fn await_backend(
        &self,
        target: WebTarget,
        child: &mut Box<dyn Child>,
    ) -> (ReadyOutcome, WebTarget) {
        let outcome = self.wait_ready_on(&target, child);
        if !matches!(outcome, ReadyOutcome::Ready | ReadyOutcome::TokenRejected) {
            return (outcome, target);
        }
        let fresh = self.read_web_target();
        if fresh == target {
            return (outcome, target);
        }
        (self.wait_ready_on(&fresh, child), fresh)
    }

    fn wait_ready_on(&self, target: &WebTarget, child: &mut Box<dyn Child>) -> ReadyOutcome {
        wait_ready(
            &self.sys,
            &target.probe_url(),
            READY_TIMEOUT,
            child.as_mut(),
            &self.quitting,
        )
    }

    /// One short-lived sidecar command. Err = (command, output) for the
    /// ServiceCommandFailed phase.
    fn run_sidecar_cmd(&self, args: &[&str]) -> Result<(), (String, String)> {
        let command = args.join(" ");
        let out = self
            .sys
            .run(&self.paths.sidecar, args)
            .map_err(|e| (command.clone(), e.to_string()))?;
        if out.ok() {
            Ok(())
        } else {
            Err((command, out.combined()))
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
    use crate::service::remove_old_service_dialog;
    use crate::session::PidRecord;
    use crate::sys::{Probe, RunOutput, Signal};
    use crate::testing::{ChildMode, Effect, FakeSys, FakeUi};
    use crate::web_target::WebTarget;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    const SIDECAR: &str = "/app/peakbot";
    const PIDFILE: &str = "/data/sidecar.pid";
    const WEB_TOKEN: &str = "/cfg/web-token";
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

    /// `Controller::new` + `handle(Start)` on `Platform::Linux` — every
    /// session-path test pins this.
    fn started(sys: FakeSys, ui: FakeUi) -> Controller<FakeSys, FakeUi> {
        started_on(sys, ui, Platform::Linux)
    }

    /// `Controller::new(…, platform)` + `handle(Start)`.
    ///
    /// CONTRACT (close-to-tray plan §C, RED until the developer lands
    /// it): the constructor takes NO replace policy and NO service
    /// field — `pub fn new(sys, ui, paths, quitting, platform: Platform)`.
    /// The glue decides the platform once via `phase::host_platform()`
    /// (windows / macOS / everything else) and passes it into the pure
    /// core.
    fn started_on(sys: FakeSys, ui: FakeUi, platform: Platform) -> Controller<FakeSys, FakeUi> {
        let mut ctrl =
            Controller::new(sys, ui, paths(), Arc::new(AtomicBool::new(false)), platform);
        let _ = ctrl.handle(Action::Start);
        ctrl
    }

    const NOT_INSTALLED: &str = "Installed: false\n";

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
            Platform::Linux,
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
            Platform::Linux,
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
                Phase::Ready(WebTarget { bind, tls, token })
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

    // §9.1 case 62.
    #[test]
    fn t62_tick_detects_session_death_and_is_a_noop_otherwise() {
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
            matches!(ctrl.phase(), Phase::Ready(_)),
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

        // (b) Ready with a LIVE child: tick is a no-op (no phase change,
        // no side effects beyond the liveness poll).
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
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
        let phases_before = ui.phases().len();
        ctrl.tick();
        assert!(matches!(ctrl.phase(), Phase::Ready(_)), "still Ready");
        assert_eq!(ui.phases().len(), phases_before, "no phase change");

        // (c) Failed (declined migration): tick is a no-op — it must not
        // poll, re-fail or touch anything.
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui.clone());
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));
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
            matches!(ctrl.phase(), Phase::Ready(_)),
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
            matches!(ctrl.phase(), Phase::Ready(_)),
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
            matches!(ctrl.phase(), Phase::Ready(_)),
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
            matches!(ctrl.phase(), Phase::Ready(_)),
            "precondition; got {:?}",
            ctrl.phase()
        );

        let _ = ctrl.handle(Action::Start); // Retry
        // Snapshot AFTER the retry: pids[0] is the stopped session,
        // pids[1] the fresh one it spawned.
        let (old_pid, new_pid) = (sys.spawned_pids()[0], sys.spawned_pids()[1]);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
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

    // -------------------------------------------------------------------
    // Close-to-tray plan §B/§D — the old-service migration on Start.
    //
    // Rule: a service that is INSTALLED (running or stopped) ⇒ ask, on
    // EVERY Start. Yes → `service uninstall` → session; No →
    // Failed(ServiceInstalled). Not installed / status unavailable ⇒
    // session, no prompt. The desktop NEVER installs anything: the
    // `install` and `service install` verbs are deliberately left
    // UNSCRIPTED in these fixtures, so a controller that tried them
    // would also trip an io error, not just the m8 assertion.
    // -------------------------------------------------------------------

    /// A sidecar script for the migration scenarios: `--version`, the
    /// given `service status` / `service uninstall` outputs (`None` ⇒
    /// unscripted ⇒ the run fails with NotFound), a ready probe and one
    /// live session child.
    fn migration_sys(status: Option<RunOutput>, uninstall: Option<RunOutput>) -> FakeSys {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        if let Some(s) = status {
            sys.set_run("peakbot", &["service", "status"], s);
        }
        if let Some(u) = uninstall {
            sys.set_run("peakbot", &["service", "uninstall"], u);
        }
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys
    }

    fn ok_out() -> RunOutput {
        run_out(Some(0), "ok\n", "")
    }

    fn arg_vec(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// m8's invariant, reusable: the desktop never ran `install` nor
    /// `service install`, and never touched anything but the sidecar
    /// (i.e. never `~/.local/bin/peakbot`).
    fn assert_never_installs(sys: &FakeSys) {
        let runs = sys.run_args();
        assert!(
            !runs.iter().any(|r| r == &arg_vec(&["install"])),
            "the desktop must never run `install`; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &arg_vec(&["service", "install"])),
            "the desktop must never run `service install`; runs: {runs:?}"
        );
        for e in sys.events() {
            if let Effect::Run { program, .. } = e {
                assert_eq!(
                    program,
                    PathBuf::from(SIDECAR),
                    "only the bundled sidecar may be run (never ~/.local/bin/peakbot)"
                );
            }
        }
    }

    fn position_of_run(sys: &FakeSys, args: &[&str]) -> Option<usize> {
        let want = arg_vec(args);
        sys.events()
            .iter()
            .position(|e| matches!(e, Effect::Run { args, .. } if *args == want))
    }

    // m1: installed + running, user says Yes ⇒ ask once, uninstall BEFORE
    // the first spawn, RemovingService step shown, ends Ready.
    #[test]
    fn m1_installed_running_asks_then_uninstalls_then_session() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("tok-000000001")),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui.clone());

        assert_eq!(
            ui.confirms(),
            vec![remove_old_service_dialog()],
            "exactly one confirm, with the locked migration dialog"
        );
        let runs = sys.run_args();
        assert!(
            runs.contains(&arg_vec(&["service", "uninstall"])),
            "Yes ⇒ `service uninstall` ran; runs: {runs:?}"
        );
        let i_status = position_of_run(&sys, &["service", "status"]).expect("status ran");
        let i_uninstall = position_of_run(&sys, &["service", "uninstall"]).expect("uninstall ran");
        let i_spawn = sys
            .events()
            .iter()
            .position(|e| matches!(e, Effect::Spawn { .. }))
            .expect("the session was spawned");
        assert!(
            i_status < i_uninstall && i_uninstall < i_spawn,
            "status → uninstall → first spawn; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "exactly one session spawn");

        let steps = ui.steps();
        let pos = |want: Phase| steps.iter().position(|p| *p == want);
        let i_check = pos(Phase::Starting(StartStep::CheckingService)).expect("CheckingService");
        let i_remove = pos(Phase::Starting(StartStep::RemovingService)).expect("RemovingService");
        let i_start = pos(Phase::Starting(StartStep::StartingSession)).expect("StartingSession");
        assert!(
            i_check < i_remove && i_remove < i_start,
            "CheckingService → RemovingService → StartingSession; steps: {steps:?}"
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(t) if t.bind.port() == 7823),
            "ends Ready(target); got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            ready_url(&ctrl).as_deref(),
            Some("http://127.0.0.1:7823/?token=tok-000000001")
        );
        assert_never_installs(&sys);
    }

    // m2: installed + STOPPED still asks (a stopped unit would still
    // conflict at the next login) — exactly one confirm.
    #[test]
    fn m2_installed_stopped_still_asks() {
        let sys = migration_sys(Some(installed_status("stopped")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui.clone());

        assert_eq!(ui.confirms().len(), 1, "a stopped service still asks");
        assert_eq!(ui.confirms(), vec![remove_old_service_dialog()]);
        assert!(
            sys.run_args().contains(&arg_vec(&["service", "uninstall"])),
            "Yes ⇒ uninstall ran"
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
        assert_never_installs(&sys);
    }

    // m2b: an `unknown` run state (exit 0, Installed: true) is installed.
    #[test]
    fn m2b_installed_unknown_state_still_asks() {
        let sys = migration_sys(Some(installed_status("unknown")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui.clone());
        assert_eq!(ui.confirms().len(), 1);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
        assert_never_installs(&sys);
    }

    // m3: user says No ⇒ Failed(ServiceInstalled): no spawn, no
    // uninstall, tray shows the error, no pidfile written.
    #[test]
    fn m3_declined_fails_service_installed() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let ctrl = started(sys.clone(), ui.clone());

        assert_eq!(
            ctrl.phase(),
            &Phase::Failed(StartError::ServiceInstalled),
            "declining lands on the dedicated declined view"
        );
        assert!(
            sys.spawned_specs().is_empty(),
            "no session may be spawned while the old service stays"
        );
        assert!(
            !sys.run_args().contains(&arg_vec(&["service", "uninstall"])),
            "No ⇒ nothing is uninstalled; runs: {:?}",
            sys.run_args()
        );
        assert!(!sys.has_file(Path::new(PIDFILE)), "no pidfile for no child");
        assert_eq!(ui.confirms(), vec![remove_old_service_dialog()]);
        assert_eq!(
            ui.last_tray(),
            "PeakBot 0.23.0 · error",
            "the tray status shows the error"
        );
        assert_never_installs(&sys);
    }

    // m4: the decline is NOT remembered — Retry (Start again) asks again.
    #[test]
    fn m4_retry_after_decline_asks_again() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui.clone());
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));
        assert_eq!(ui.confirms().len(), 1, "precondition: one ask so far");

        let _ = ctrl.handle(Action::Start); // the Retry button
        assert_eq!(ui.confirms().len(), 2, "Start asks on EVERY start");
        assert_eq!(
            ui.confirms()[0],
            ui.confirms()[1],
            "the same dialog both times"
        );
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));
        assert!(sys.spawned_specs().is_empty(), "still no spawn");
        assert_never_installs(&sys);
    }

    // m4b: decline, then Retry + Yes ⇒ the migration completes.
    #[test]
    fn m4b_retry_after_decline_can_accept() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));

        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "Yes on the retry reaches Ready; got {:?}",
            ctrl.phase()
        );
        assert_eq!(ui.confirms().len(), 2);
        assert_eq!(sys.spawned_specs().len(), 1, "one spawn, after the Yes");
        assert_never_installs(&sys);
    }

    // m4c: Action::Restart from a Failed phase is the full start path —
    // it asks again too.
    #[test]
    fn m4c_restart_from_failed_reruns_start_and_asks_again() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));

        let flow = ctrl.handle(Action::Restart);
        assert_eq!(flow, ControlFlow::Continue(()), "the app never exits");
        assert_eq!(ui.confirms().len(), 2, "Restart re-runs handle_start");
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
    }

    // m5: `service uninstall` exits non-zero ⇒ ServiceCommandFailed with
    // the command and the output; no session spawn.
    #[test]
    fn m5_uninstall_failure_is_service_command_failed() {
        let sys = migration_sys(
            Some(installed_status("running")),
            Some(run_out(Some(1), "", "permission denied\n")),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui.clone());

        match ctrl.phase() {
            Phase::Failed(StartError::ServiceCommandFailed { command, output }) => {
                assert_eq!(command, "service uninstall");
                assert!(
                    output.contains("permission denied"),
                    "the failure output is carried; {output:?}"
                );
            }
            other => panic!("expected ServiceCommandFailed, got {other:?}"),
        }
        assert!(
            sys.spawned_specs().is_empty(),
            "a failed uninstall must not start a session next to the old service"
        );
        assert_eq!(ui.confirms().len(), 1);
        assert_never_installs(&sys);
    }

    // m5b: the uninstall cannot even be launched (io error) ⇒ the same
    // failed view.
    #[test]
    fn m5b_uninstall_not_runnable_is_service_command_failed() {
        let sys = migration_sys(Some(installed_status("running")), None);
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started(sys.clone(), ui);
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Failed(StartError::ServiceCommandFailed { command, .. })
                    if command == "service uninstall"
            ),
            "got {:?}",
            ctrl.phase()
        );
        assert!(sys.spawned_specs().is_empty());
    }

    // m6: nothing to migrate ⇒ no prompt, straight to the session —
    // `Installed: false`, a failing `service status`, a status that
    // cannot be run at all, and unrecognised output.
    #[test]
    fn m6_not_installed_or_unavailable_no_prompt() {
        let cases: Vec<(&str, Option<RunOutput>)> = vec![
            (
                "Installed: false",
                Some(run_out(Some(0), NOT_INSTALLED, "")),
            ),
            (
                "status exit 1",
                Some(run_out(Some(1), "", "systemctl --user not available\n")),
            ),
            ("status not runnable", None),
            (
                "unrecognised output",
                Some(run_out(Some(0), "Manager:   systemd-user\n", "")),
            ),
            (
                "Installed: true but exit 1",
                Some(run_out(Some(1), "Installed: true\n", "boom\n")),
            ),
        ];
        for (label, status) in cases {
            let sys = migration_sys(status, Some(ok_out()));
            let ui = FakeUi::new();
            let ctrl = started(sys.clone(), ui.clone());

            assert!(
                ui.confirms().is_empty(),
                "{label}: no prompt; got {:?}",
                ui.confirms()
            );
            assert!(
                matches!(ctrl.phase(), Phase::Ready(_)),
                "{label}: session Ready; got {:?}",
                ctrl.phase()
            );
            assert_eq!(sys.spawned_specs().len(), 1, "{label}: one spawn");
            assert!(
                !sys.run_args().contains(&arg_vec(&["service", "uninstall"])),
                "{label}: nothing to uninstall"
            );
            assert_never_installs(&sys);
        }
    }

    // m6b: `service status` is still consulted on Linux/macOS (it is how
    // the migration finds the old service) and the step is shown.
    #[test]
    fn m6b_not_installed_still_checks_service_status() {
        let sys = migration_sys(Some(run_out(Some(0), NOT_INSTALLED, "")), None);
        let ui = FakeUi::new();
        let _ = started(sys.clone(), ui.clone());
        assert!(
            sys.run_args().contains(&arg_vec(&["service", "status"])),
            "service status ran; runs: {:?}",
            sys.run_args()
        );
        assert!(
            ui.steps()
                .contains(&Phase::Starting(StartStep::CheckingService)),
            "the CheckingService step is shown on Linux; steps: {:?}",
            ui.steps()
        );
    }

    // m6c: macOS runs the same migration check as Linux.
    #[test]
    fn m6c_macos_installed_asks_like_linux() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);
        assert_eq!(ui.confirms(), vec![remove_old_service_dialog()]);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
        assert_never_installs(&sys);
    }

    // m7 (adapts q1_windows_installed_identical_…): Windows NEVER runs
    // `service status` — so it can never prompt, uninstall or show the
    // "Looking for a background service…" step — even when a service
    // would report as installed.
    #[test]
    fn m7_windows_never_runs_service_status() {
        for state in ["running", "stopped", "unknown"] {
            let sys = migration_sys(Some(installed_status(state)), Some(ok_out()));
            sys.seed_file(
                CONFIG_YAML,
                web_yaml("127.0.0.1:7823", Some("win-token-0001")),
            );
            let ui = FakeUi::new();
            ui.answer_confirm(true);
            let ctrl = started_on(sys.clone(), ui.clone(), Platform::Windows);

            assert!(
                matches!(ctrl.phase(), Phase::Ready(t) if t.bind.port() == 7823),
                "Windows lands Ready even with an installed service (state {state:?}); got {:?}",
                ctrl.phase()
            );
            assert_eq!(sys.spawned_specs().len(), 1, "one session spawn ({state})");
            assert_eq!(
                ready_url(&ctrl).as_deref(),
                Some("http://127.0.0.1:7823/?token=win-token-0001"),
                "the target is general.web ({state})"
            );
            let runs = sys.run_args();
            assert!(
                !runs
                    .iter()
                    .any(|r| r.first().is_some_and(|a| a == "service")),
                "Windows must not run any `service …` command ({state}); runs: {runs:?}"
            );
            assert!(
                ui.confirms().is_empty(),
                "Windows never prompts ({state}); got {:?}",
                ui.confirms()
            );
            assert!(
                !ui.steps().iter().any(|p| matches!(
                    p,
                    Phase::Starting(StartStep::CheckingService | StartStep::RemovingService)
                )),
                "no service steps on Windows ({state}); steps: {:?}",
                ui.steps()
            );
            assert_never_installs(&sys);
        }
    }

    // m8: across every migration outcome the desktop never installs a
    // service or copies the binary (`install` / `service install`), and
    // never runs anything but the bundled sidecar.
    #[test]
    fn m8_never_installs() {
        // (status, answer, uninstall)
        let scenarios: Vec<(&str, Option<RunOutput>, bool, Option<RunOutput>)> = vec![
            (
                "running + yes",
                Some(installed_status("running")),
                true,
                Some(ok_out()),
            ),
            (
                "stopped + yes",
                Some(installed_status("stopped")),
                true,
                Some(ok_out()),
            ),
            (
                "running + no",
                Some(installed_status("running")),
                false,
                Some(ok_out()),
            ),
            (
                "uninstall fails",
                Some(installed_status("running")),
                true,
                Some(run_out(Some(1), "", "boom")),
            ),
            (
                "not installed",
                Some(run_out(Some(0), NOT_INSTALLED, "")),
                false,
                None,
            ),
            (
                "status unavailable",
                Some(run_out(Some(1), "", "x")),
                false,
                None,
            ),
        ];
        for (label, status, answer, uninstall) in scenarios {
            let sys = migration_sys(status, uninstall);
            let ui = FakeUi::new();
            ui.answer_confirm(answer);
            let mut ctrl = started(sys.clone(), ui);
            let _ = ctrl.handle(Action::Restart); // the retry path too
            let runs = sys.run_args();
            assert!(
                !runs
                    .iter()
                    .any(|r| r == &arg_vec(&["install"]) || r == &arg_vec(&["service", "install"])),
                "{label}: no install verb may ever run; runs: {runs:?}"
            );
            assert_never_installs(&sys);
        }
    }

    // §C: `Action` is exactly {Start, Restart, Shutdown}. An exhaustive
    // match is a compile-time pin: it stops compiling while the removed
    // `ToggleBackground` / `ReinstallService` variants still exist.
    #[test]
    fn action_has_exactly_start_restart_shutdown() {
        fn label(a: &Action) -> &'static str {
            match a {
                Action::Start => "start",
                Action::Restart => "restart",
                Action::Shutdown(_) => "shutdown",
            }
        }
        let (tx, _rx) = mpsc::channel();
        assert_eq!(label(&Action::Start), "start");
        assert_eq!(label(&Action::Restart), "restart");
        assert_eq!(label(&Action::Shutdown(tx)), "shutdown");
    }

    // §C: Restart while Starting is ignored (`Restart if !starting`).
    // `quitting` makes the first start park in a Starting phase.
    #[test]
    fn restart_is_ignored_while_starting() {
        let sys = migration_sys(Some(run_out(Some(0), NOT_INSTALLED, "")), None);
        let ui = FakeUi::new();
        let mut ctrl = Controller::new(
            sys.clone(),
            ui.clone(),
            paths(),
            Arc::new(AtomicBool::new(true)), // quitting: wait_ready cancels
            Platform::Linux,
        );
        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Starting(_)),
            "precondition: Cancelled leaves a Starting phase; got {:?}",
            ctrl.phase()
        );
        let spawns_before = sys.spawned_specs().len();
        let events_before = sys.events().len();
        let phases_before = ui.phases().len();

        let flow = ctrl.handle(Action::Restart);
        assert_eq!(flow, ControlFlow::Continue(()));
        assert_eq!(sys.spawned_specs().len(), spawns_before, "no new spawn");
        assert_eq!(sys.events().len(), events_before, "no side effects");
        assert_eq!(ui.phases().len(), phases_before, "no phase change");
    }

    // Shutdown with no session (e.g. after a declined migration) still
    // acks and breaks, and touches no child.
    #[test]
    fn shutdown_without_session_acks_and_breaks() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let mut ctrl = started(sys.clone(), ui);
        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::ServiceInstalled));

        let (tx, rx) = mpsc::channel();
        assert_eq!(ctrl.handle(Action::Shutdown(tx)), ControlFlow::Break(()));
        rx.recv().expect("the ack must be sent");
        assert!(
            !sys.events().iter().any(|e| matches!(
                e,
                Effect::ChildInterrupt { .. } | Effect::ChildKill { .. } | Effect::Signal { .. }
            )),
            "nothing to stop; events: {:?}",
            sys.events()
        );
    }

    // launch_session: a 401 even after the general.web re-read ⇒ the
    // unit variant TokenRejected; the child is stopped, the pidfile gone.
    #[test]
    fn token_rejected_stops_child_and_fails_token_rejected() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(401)]);
        sys.queue_child(crate::testing::FakeChildSpec {
            mode: ChildMode::ExitsOnInterrupt,
            exit: Some(0),
            ..Default::default()
        });
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());

        assert_eq!(ctrl.phase(), &Phase::Failed(StartError::TokenRejected));
        assert!(
            sys.events()
                .iter()
                .any(|e| matches!(e, Effect::ChildInterrupt { .. })),
            "the child that answered 401 is stopped"
        );
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
        assert_eq!(ui.last_tray(), "PeakBot 0.23.0 · error");
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
            matches!(ctrl.phase(), Phase::Ready(_)),
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
        assert!(matches!(ctrl.phase(), Phase::Ready(_)));
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch needed");
    }

    // Spec §11.6: no code path reads or writes settings.json or
    // web-token — across start, session restart, the old-service
    // migration (accepted) and a retry. The token never appears in any
    // argv/env.
    #[test]
    fn no_settings_json_or_web_token_io() {
        let sys = FakeSys::new();
        sys.seed_file(
            SETTINGS_FILE,
            "{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":true}\n",
        );
        sys.seed_file(
            CONFIG_YAML,
            web_yaml("127.0.0.1:7823", Some("cfg-token-0000001")),
        );
        sys.seed_file(WEB_TOKEN, "file-token-000001\n");
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status("running"),         // start: migration offered
                run_out(Some(0), NOT_INSTALLED, ""), // session restart: gone now
            ],
        );
        sys.set_run("peakbot", &["service", "uninstall"], ok_out());
        sys.set_default_probe(vec![Probe::Status(200)]);
        for _ in 0..2 {
            sys.queue_child(crate::testing::FakeChildSpec {
                mode: ChildMode::ExitsOnInterrupt,
                exit: Some(0),
                ..Default::default()
            });
        }
        let ui = FakeUi::new();
        ui.answer_confirm(true); // remove the old service
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
            ctrl.phase()
        );
        let _ = ctrl.handle(Action::Restart);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "{:?}",
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
        // The retired `declined-replace` memory is not read or written
        // either (a stale file in data_dir is simply left alone).
        let declined = "/data/declined-replace";
        assert!(
            !sys.read_paths().iter().any(|p| p == Path::new(declined)),
            "declined-replace is retired; reads: {:?}",
            sys.read_paths()
        );
        assert!(
            sys.events().iter().all(|e| {
                !matches!(e, Effect::WritePrivate { path: p } | Effect::Remove { path: p }
                    if p == Path::new(declined))
            }),
            "declined-replace is retired; events: {:?}",
            sys.events()
        );
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

    // A declined migration must not write the retired declined-replace
    // memory (or anything else): the answer is re-asked on every Start.
    #[test]
    fn declined_migration_writes_nothing() {
        let sys = migration_sys(Some(installed_status("running")), Some(ok_out()));
        let ui = FakeUi::new();
        ui.answer_confirm(false);
        let _ = started(sys.clone(), ui);
        assert!(
            sys.private_written().is_empty(),
            "a decline persists nothing; wrote {:?}",
            sys.private_written()
        );
        assert!(!sys.has_file(Path::new("/data/declined-replace")));
    }

    // -------------------------------------------------------------------
    // CA-trust design §5 — the CA trust step right before Ready.
    //
    // Setup for every case: config `general.web.tls: true` (unless noted),
    // the CA PEM seeded at `paths::peakbot_ca_cert()`, default probe
    // ready. FakeSys keys `run` by program FILE NAME, so the trust-store
    // commands are scripted as "security" / "certutil.exe".
    // -------------------------------------------------------------------

    const TLS_YAML: &str = "general:\n  web:\n    tls: true\n";
    const HTTP_YAML: &str = "general:\n  web:\n    tls: false\n";

    /// The same real openssl fixture CA as `tests/ca_trust.rs` (PEM only,
    /// no key in the repo).
    const CA_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDJTCCAg2gAwIBAgIUWQWuHFoWUzh61plJAyaRzNKgpbUwDQYJKoZIhvcNAQEL
BQAwIjEgMB4GA1UEAwwXUGVha0JvdCBUZXN0IEZpeHR1cmUgQ0EwHhcNMjYxMDI3
MTQwNzA4WhcNMzYxMDI0MTQwNzA4WjAiMSAwHgYDVQQDDBdQZWFrQm90IFRlc3Qg
Rml4dHVyZSBDQTCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAIXf4iIa
Z9Z0QXgRZD9ccGVEilX09MUaJcKnNo9RR9h/5rgWEn0sPbVDz/0IySxICzjdL5ek
F30Xu/E13nXCcL5yFxBTA9VmkW2ZLj5Bn/pNZGfWeQeqZ7oFdOR8Iw75XeAjowjI
1g0ZB8iM1JyuuaGJjtHWlJ8vpOY0VZEB/jvlSkVZ4NqvjT6f3h7S6USUhaNW6EFI
LBKycWUZ8mWoL9Ly5jB+C6kR5geGHl8lK/1Z7RyqyEh2J6sp1iUvtqEOZR6NFfCy
5eBE+BdUgvO0Gz6vfC4tXvpxWYSc0/dtSkq1rsT3HBoXuIgEo7n/LZ6pQqqBip1b
tBZbjSzAE8t0AwECAwEAAaNTMFEwHQYDVR0OBBYEFBay38etvAkZau3PFkDys+zy
0y3pMB8GA1UdIwQYMBaAFBay38etvAkZau3PFkDys+zy0y3pMA8GA1UdEwEB/wQF
MAMBAf8wDQYJKoZIhvcNAQELBQADggEBAAw/cvPHEKiatiif+8XMoTyvTAG0nvma
hhpnmeaQz3It2hnnJrgiEVY4m16o4AMjU9CZmE/isCpJKMVbmK4aRW+G4FyaLtF/
rjD4gl0IRzgmzEDOOLhqpH34nliJNANHTWJxSE3qXTiACxxEAkrnijaihCCU/2Ta
M2J9RlmrmvFDXhAO4y6VGXRhAxPrBs5yEQ5kvlON17L61ChdmIuH9YdlopsQiJEX
zUm22DjFCRmsEF7T2nVXKFKuWmRc2DcTfK+jfPmJL27yTPso1ICK30wPJpDsGCkI
Mau1Qxq3S7SfaVNWM0OeBd9HV3682/Fu/7MHE+ATQV5Ved7dQCfVoVc=
-----END CERTIFICATE-----
"#;

    /// SHA-1 of the fixture CA (the thumbprint the Windows store uses).
    const CT_CA_SHA1: &str = "FC78374057D7B930128238B5AC29E5531816611E";

    fn ca_cert_path() -> PathBuf {
        crate::paths::peakbot_ca_cert().expect("the cache dir resolves")
    }

    /// The login keychain the controller is expected to use in tests.
    fn login_keychain() -> String {
        directories_next::BaseDirs::new()
            .expect("base dirs resolve")
            .home_dir()
            .join("Library/Keychains/login.keychain-db")
            .to_string_lossy()
            .into_owned()
    }

    fn ca_path_str() -> String {
        ca_cert_path().to_string_lossy().into_owned()
    }

    /// `spaced_lower("ABCDEF")` ⇒ `"ab cd ef"` (certutil's hash style).
    fn spaced_lower(hash: &str) -> String {
        hash.as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn macos_verify_args() -> Vec<String> {
        vec![
            "verify-cert".to_string(),
            "-c".to_string(),
            ca_path_str(),
            "-p".to_string(),
            "ssl".to_string(),
            "-l".to_string(),
            "-L".to_string(),
            "-R".to_string(),
            "offline".to_string(),
        ]
    }

    /// `&[&str]` view of a `Vec<String>` (the `set_run` shape).
    fn refs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    /// How many recorded `run` calls had exactly these args.
    fn count_runs(runs: &[Vec<String>], args: &[String]) -> usize {
        runs.iter().filter(|r| r.as_slice() == args).count()
    }

    fn macos_add_args() -> Vec<String> {
        vec![
            "add-trusted-cert".to_string(),
            "-r".to_string(),
            "trustRoot".to_string(),
            "-p".to_string(),
            "ssl".to_string(),
            "-k".to_string(),
            login_keychain(),
            ca_path_str(),
        ]
    }

    fn windows_check_args() -> Vec<String> {
        vec![
            "-user".to_string(),
            "-store".to_string(),
            "Root".to_string(),
            CT_CA_SHA1.to_string(),
        ]
    }

    fn windows_add_args() -> Vec<String> {
        vec![
            "-user".to_string(),
            "-addstore".to_string(),
            "Root".to_string(),
            ca_path_str(),
        ]
    }

    /// Base start script: versioned sidecar, no service, tls/http
    /// config, live session child, probe 200, CA seeded (unless
    /// `seed_ca` is false).
    fn ct_base(tls: bool, seed_ca: bool) -> (FakeSys, FakeUi) {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(CONFIG_YAML, if tls { TLS_YAML } else { HTTP_YAML });
        if seed_ca {
            sys.seed_file(ca_cert_path(), CA_PEM);
        }
        (sys, FakeUi::new())
    }

    /// No `security` / `certutil.exe` run at all (the trust step never
    /// ran).
    fn assert_no_trust_store_runs(sys: &FakeSys) {
        assert!(
            sys.events().iter().all(|e| {
                !matches!(
                    e,
                    Effect::Run { program, .. }
                        if program
                            .file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n == "security" || n == "certutil.exe")
                )
            }),
            "no security/certutil runs; events: {:?}",
            sys.events()
        );
    }

    // CT1: MacOs session, the CA is already trusted ⇒ Ready with no
    // add-trusted-cert and no TrustingCertificate step.
    #[test]
    fn ct1_macos_already_trusted_ready_without_add() {
        let (sys, ui) = ct_base(true, true);
        sys.set_run(
            "security",
            &refs(&macos_verify_args()),
            run_out(Some(0), "", ""),
        );
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "Ready(Session, tls); got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            count_runs(&runs, &macos_verify_args()),
            1,
            "the trust check ran exactly once; runs: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r.first().is_some_and(|a| a == "add-trusted-cert")),
            "no add-trusted-cert run; runs: {runs:?}"
        );
        assert!(
            !ui.steps()
                .iter()
                .any(|p| { matches!(p, Phase::Starting(StartStep::TrustingCertificate)) }),
            "TrustingCertificate must never be emitted; steps: {:?}",
            ui.steps()
        );
    }

    // CT2: MacOs, not trusted ⇒ the add runs exactly once, the phase
    // goes … WaitingForBackend → TrustingCertificate → Ready, and the
    // verify run happens AFTER the successful probe.
    #[test]
    fn ct2_macos_not_trusted_adds_then_ready() {
        let (sys, ui) = ct_base(true, true);
        sys.set_run(
            "security",
            &refs(&macos_verify_args()),
            run_out(Some(1), "", ""),
        );
        let add_args = macos_add_args();
        sys.set_run("security", &refs(&add_args), run_out(Some(0), "ok\n", ""));
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "Ready(Session, tls); got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            count_runs(&runs, &add_args),
            1,
            "the add ran exactly once; runs: {runs:?}"
        );

        // Phases: … WaitingForBackend → TrustingCertificate → Ready.
        let steps = ui.steps();
        let i_wait = steps
            .iter()
            .position(|p| matches!(p, Phase::Starting(StartStep::WaitingForBackend)))
            .expect("WaitingForBackend emitted");
        let i_trust = steps
            .iter()
            .position(|p| matches!(p, Phase::Starting(StartStep::TrustingCertificate)))
            .expect("TrustingCertificate emitted");
        let i_ready = steps
            .iter()
            .position(|p| matches!(p, Phase::Ready(_)))
            .expect("Ready emitted");
        assert!(
            i_wait < i_trust && i_trust + 1 == i_ready,
            "WaitingForBackend → TrustingCertificate → Ready; steps: {steps:?}"
        );

        // The verify run happens after the successful probe.
        let evts = sys.events();
        let i_probe = evts
            .iter()
            .position(|e| matches!(e, Effect::Probe { .. }))
            .expect("a probe ran");
        let i_verify = evts
            .iter()
            .position(|e| {
                matches!(e, Effect::Run { args, .. }
                    if args.first().is_some_and(|a| a == "verify-cert"))
            })
            .expect("verify ran");
        assert!(
            i_probe < i_verify,
            "the verify run happens after the successful probe; events: {evts:?}"
        );
    }

    // CT3: MacOs, the add is refused ⇒ Failed(CertificateNotTrusted)
    // with the failed command in the detail — the session child stays
    // alive (no interrupt, pidfile present). Retry interrupts the old
    // child before spawning and checks again.
    #[test]
    fn ct3_macos_add_failure_fails_and_retry_rechecks() {
        let (sys, ui) = ct_base(true, true);
        // verify: 1st call Some(1) (first start), 2nd Some(0) (retry —
        // the cert is in by then).
        sys.set_run_seq(
            "security",
            &refs(&macos_verify_args()),
            vec![run_out(Some(1), "", ""), run_out(Some(0), "", "")],
        );
        sys.set_run(
            "security",
            &refs(&macos_add_args()),
            run_out(Some(1), "", "User canceled\n"),
        );
        sys.queue_child(crate::testing::FakeChildSpec::alive()); // the retry's child
        let mut ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);

        match ctrl.phase() {
            Phase::Failed(StartError::CertificateNotTrusted { detail }) => {
                assert!(
                    detail.starts_with("$ /usr/bin/security add-trusted-cert"),
                    "detail names the failed add command; {detail:?}"
                );
                assert!(
                    detail.contains("User canceled"),
                    "detail carries the add output; {detail:?}"
                );
            }
            other => panic!("expected CertificateNotTrusted, got {other:?}"),
        }
        assert!(
            !sys.events()
                .iter()
                .any(|e| matches!(e, Effect::ChildInterrupt { .. })),
            "no child interrupt on a trust failure; events: {:?}",
            sys.events()
        );
        assert!(
            sys.has_file(Path::new(PIDFILE)),
            "the pidfile stays: the session child is alive"
        );

        // Retry: the old pid is interrupted before the new spawn, and
        // the check runs again.
        let _ = ctrl.handle(Action::Start);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(_)),
            "the retry reaches Ready; got {:?}",
            ctrl.phase()
        );
        let pids = sys.spawned_pids();
        assert_eq!(pids.len(), 2, "one child per start");
        let evts = sys.events();
        let i_int = evts
            .iter()
            .position(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == pids[0]))
            .expect("the old child was interrupted");
        let i_spawn2 = evts
            .iter()
            .position(|e| matches!(e, Effect::Spawn { pid, .. } if *pid == pids[1]))
            .expect("the new child was spawned");
        assert!(
            i_int < i_spawn2,
            "the old pid is interrupted before the new spawn; events: {evts:?}"
        );
        let verifies = evts
            .iter()
            .filter(|e| {
                matches!(e, Effect::Run { args, .. }
                    if args.first().is_some_and(|a| a == "verify-cert"))
            })
            .count();
        assert_eq!(
            verifies, 2,
            "the check runs again on the retry; events: {evts:?}"
        );
    }

    // CT4: MacOs, the check command itself errors (unscripted ⇒
    // FakeSys NotFound) ⇒ treated as NOT trusted, the add runs, Ready.
    #[test]
    fn ct4_macos_check_error_treated_as_not_trusted() {
        let (sys, ui) = ct_base(true, true);
        // `verify-cert` is deliberately NOT scripted: run → Err.
        sys.set_run(
            "security",
            &refs(&macos_add_args()),
            run_out(Some(0), "ok\n", ""),
        );
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "a check error is not a hard failure; got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            count_runs(&runs, &macos_add_args()),
            1,
            "the add ran exactly once; runs: {runs:?}"
        );
    }

    // CT5: Windows, the store check says trusted (exit 0 + thumbprint
    // in stdout) ⇒ Ready, no -addstore.
    #[test]
    fn ct5_windows_already_trusted_ready_without_addstore() {
        let (sys, ui) = ct_base(true, true);
        let out = format!("Cert Hash(sha1): {}\n", spaced_lower(CT_CA_SHA1));
        sys.set_run(
            "certutil.exe",
            &refs(&windows_check_args()),
            run_out(Some(0), &out, ""),
        );
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::Windows);

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "Ready(Session, tls); got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            count_runs(&runs, &windows_check_args()),
            1,
            "the store check ran exactly once; runs: {runs:?}"
        );
        assert!(
            !runs
                .iter()
                .any(|r| r.get(1).is_some_and(|a| a == "-addstore")),
            "no -addstore run; runs: {runs:?}"
        );
    }

    // CT6: Windows, not trusted ⇒ addstore runs, Ready.
    #[test]
    fn ct6_windows_not_trusted_addstore_then_ready() {
        let (sys, ui) = ct_base(true, true);
        sys.set_run(
            "certutil.exe",
            &refs(&windows_check_args()),
            run_out(Some(1), "", ""),
        );
        sys.set_run(
            "certutil.exe",
            &refs(&windows_add_args()),
            run_out(Some(0), "Command completed successfully.\n", ""),
        );
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::Windows);

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "Ready(Session, tls); got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            count_runs(&runs, &windows_add_args()),
            1,
            "the addstore ran exactly once; runs: {runs:?}"
        );
    }

    // CT7: Windows, the addstore is refused ⇒ Failed(CertificateNotTrusted).
    #[test]
    fn ct7_windows_addstore_failure_fails_certificate_not_trusted() {
        let (sys, ui) = ct_base(true, true);
        sys.set_run(
            "certutil.exe",
            &refs(&windows_check_args()),
            run_out(Some(1), "", ""),
        );
        sys.set_run(
            "certutil.exe",
            &refs(&windows_add_args()),
            run_out(Some(1), "", "The system cannot find the file specified.\n"),
        );
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::Windows);

        match ctrl.phase() {
            Phase::Failed(StartError::CertificateNotTrusted { detail }) => {
                assert!(
                    detail.starts_with("$ certutil.exe -user -addstore"),
                    "detail names the failed addstore command; {detail:?}"
                );
            }
            other => panic!("expected CertificateNotTrusted, got {other:?}"),
        }
    }

    // CT8: tls false ⇒ no trust step at all, on MacOs AND Windows.
    #[test]
    fn ct8_tls_false_no_trust_step_on_macos_or_windows() {
        for platform in [Platform::MacOs, Platform::Windows] {
            let (sys, ui) = ct_base(false, true);
            let ctrl = started_on(sys.clone(), ui.clone(), platform);
            assert!(
                matches!(
                    ctrl.phase(),
                    Phase::Ready(t) if !t.tls
                ),
                "Ready(Session, http) on {platform:?}; got {:?}",
                ctrl.phase()
            );
            assert_no_trust_store_runs(&sys);
        }
    }

    // CT9: Linux with tls ⇒ the webview trusts the CA itself; no
    // security/certutil runs, Ready(https).
    #[test]
    fn ct9_linux_tls_true_no_trust_step() {
        let (sys, ui) = ct_base(true, true);
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::Linux);
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(t) if t.tls
            ),
            "Ready(Session, https); got {:?}",
            ctrl.phase()
        );
        assert_no_trust_store_runs(&sys);
    }

    // CT10: MacOs, tls, the CA file is missing ⇒ Failed, the detail
    // names the file and "not found"; zero trust-store runs.
    #[test]
    fn ct10_macos_ca_missing_fails_not_found() {
        let (sys, ui) = ct_base(true, false); // CA NOT seeded
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);
        match ctrl.phase() {
            Phase::Failed(StartError::CertificateNotTrusted { detail }) => {
                assert!(
                    detail.contains("ca-cert.pem"),
                    "detail names the CA file; {detail:?}"
                );
                assert!(
                    detail.contains("not found"),
                    "detail says the file is missing; {detail:?}"
                );
            }
            other => panic!("expected CertificateNotTrusted, got {other:?}"),
        }
        assert_no_trust_store_runs(&sys);
    }

    // CT11: MacOs, the CA file is unparseable ⇒ Failed; zero
    // trust-store runs.
    #[test]
    fn ct11_macos_ca_garbage_fails_without_trust_store_runs() {
        let (sys, ui) = ct_base(true, false);
        sys.seed_file(ca_cert_path(), "this is not a pem\n");
        let ctrl = started_on(sys.clone(), ui.clone(), Platform::MacOs);
        match ctrl.phase() {
            Phase::Failed(StartError::CertificateNotTrusted { detail }) => {
                assert!(
                    !detail.is_empty(),
                    "detail describes the parse failure; {detail:?}"
                );
            }
            other => panic!("expected CertificateNotTrusted, got {other:?}"),
        }
        assert_no_trust_store_runs(&sys);
    }

    /// The URL the bridge would navigate to (`None` unless Ready).
    fn ready_url(ctrl: &Controller<FakeSys, FakeUi>) -> Option<String> {
        match ctrl.phase() {
            Phase::Ready(target) => Some(target.url().to_string()),
            _ => None,
        }
    }
}
