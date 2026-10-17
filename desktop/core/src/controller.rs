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
use crate::phase::{Dialog, Mode, Phase, Platform, StartError, StartStep, TrayModel, tray_model};
use crate::ready::{ReadyOutcome, wait_ready};
use crate::service::{self, InstalledService, LaunchPlan, ReplacePolicy, RunState, ServiceState};
use crate::session::{self, EarlyExit, PidRecord};
use crate::settings::{self, Protocol, SaveRequest, Settings};
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
    /// The validated save payload (design #377b §3.2): the settings plus
    /// an optional new token value (the IPC boundary already ran
    /// `SaveRequest::parse`).
    SaveSettings(SaveRequest),
    Shutdown(std::sync::mpsc::Sender<()>),
}

/// The session sidecar this controller owns (at most one, §3.8-2).
struct Session {
    child: Box<dyn Child>,
    port: u16,
    protocol: Protocol,
    token: Option<Token>,
}

impl Session {
    fn mode(&self) -> Mode {
        Mode::Session {
            port: self.port,
            protocol: self.protocol,
            token: self.token.clone(),
        }
    }
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
            Action::SaveSettings(new) if !self.is_starting() => self.handle_save_settings(new),
            // While Starting the tray item is disabled; ignore.
            Action::ToggleBackground | Action::ReinstallService | Action::SaveSettings(_) => {}
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
            LaunchPlan::AutoReplace => self.auto_replace_service(),
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

    /// The `TLS:` status line of the installed service (invariant I3) —
    /// the service's protocol source.
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
        // The settings + token are resolved once, before the port loop
        // (design #377 §4.4): the session's token IS the web-token
        // file's token (invariant I2).
        let s = self.settings();
        let token = self.sync_token(s);
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

            let spec = SpawnSpec {
                program: self.paths.sidecar.clone(),
                args: session::session_args(port, s),
                extra_env: session::session_env(token.as_ref()),
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
            let mut sess = Session {
                child,
                port,
                protocol: s.protocol(),
                token: token.clone(),
            };
            let mode = sess.mode();
            let rec = PidRecord {
                pid: sess.child.pid(),
                port,
            };
            if let Err(e) = self.sys.write_private(&self.paths.pidfile(), &rec.render()) {
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

    fn connect_service(&mut self, start_first: bool) {
        self.set_phase(Phase::Starting(StartStep::ConnectingService));
        let s = self.settings();
        let token = self.read_web_token();
        if start_first {
            let args = service::service_install_args(s);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err((command, output)) = self.run_sidecar_cmd(&args) {
                return self.fail(StartError::ServiceCommandFailed { command, output });
            }
        }
        // The protocol is what the service actually runs with — the
        // `TLS:` status line (invariant I3), never settings.json.
        let protocol = if self.installed_tls() {
            Protocol::Https
        } else {
            Protocol::Http
        };
        let mode = Mode::Service { protocol, token };
        match wait_ready(
            &self.sys,
            &mode.probe_url(),
            READY_TIMEOUT,
            None,
            &self.quitting,
        ) {
            ReadyOutcome::Ready => self.set_phase(Phase::Ready(mode)),
            ReadyOutcome::TokenRejected => self.fail(StartError::TokenRejected { service: true }),
            ReadyOutcome::Timeout => self.fail(StartError::ServiceUnreachable {
                waited: READY_TIMEOUT,
            }),
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

    /// The saved settings (design #377 §4.4): the file is the single
    /// source and is read whenever it's needed; corrupt/missing ⇒
    /// defaults.
    fn settings(&self) -> Settings {
        let text = self.sys.read(&self.paths.settings_file()).ok().flatten();
        settings::load_settings(text.as_deref())
    }

    /// `"token on" ⇔ the web-token file holds a parseable token`
    /// (invariant I2): reuse the file's token, generate + write 0600 if
    /// absent, remove the file when the token is off. The session's
    /// token IS the file's token.
    fn sync_token(&self, s: Settings) -> Option<Token> {
        let path = self.paths.web_token();
        if !s.token() {
            if let Err(e) = self.sys.remove(&path) {
                log::warn!("failed to remove web-token: {e}");
            }
            return None;
        }
        if let Some(t) = self
            .sys
            .read(&path)
            .ok()
            .flatten()
            .and_then(|s| Token::parse(&s))
        {
            return Some(t);
        }
        let token = Token::generate(&self.sys);
        if let Err(e) = self.sys.write_private(&path, token.as_str()) {
            log::warn!("failed to write web-token: {e}");
        }
        Some(token)
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

    /// User-confirmed replace (dialog "Replace" / tray ReinstallService,
    /// #377 semantics): the SAVED settings apply, and the token file is
    /// synced to them.
    fn replace_service(&mut self) {
        let s = self.settings();
        self.sync_token(s);
        self.swap_binary_and_reinstall(s);
    }

    /// `LaunchPlan::AutoReplace` — silent: it reproduces the RUNNING
    /// service (protocol from the `TLS:` line, A4) and never calls
    /// `sync_token`, so the `web-token` file is untouched and the
    /// `service install` argv carries no `--token` (I7).
    fn auto_replace_service(&mut self) {
        let s = service::running_settings(self.settings(), self.installed_tls());
        self.swap_binary_and_reinstall(s);
    }

    /// The shared replace sequence (A5): the binary copy runs FIRST, so
    /// a failed copy leaves the service and the old binary intact.
    /// `enable --now` doesn't restart a running unit, so the uninstall +
    /// install pair is still what restarts the service on the new binary.
    fn swap_binary_and_reinstall(&mut self, s: Settings) {
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
        let args = service::service_install_args(s);
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
        if !self.sys.port_is_free(SERVICE_PORT) {
            self.ui.message(&service::port_busy_dialog());
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
        let s = self.settings();
        self.sync_token(s);
        let args = service::service_install_args(s);
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
    // Action::SaveSettings (design #377b §4.4)
    // -------------------------------------------------------------------

    /// Write-then-apply: the token file FIRST (it is the single source of
    /// the token), then the settings file, then the existing relaunch /
    /// reconfigure path — `sync_token` re-reads the file, so the new
    /// value goes live. A failed write leaves everything else untouched
    /// (no rollback machinery, T7). `reconfigure_service` IS the restart
    /// for a token-only change in service mode (T-A5: no `service
    /// restart` verb exists).
    fn handle_save_settings(&mut self, req: SaveRequest) {
        let new = req.settings();
        let current = self.settings();
        let tok_new = req
            .new_token()
            .filter(|t| Some(*t) != self.read_web_token().as_ref())
            .cloned();
        if new == current && tok_new.is_none() {
            return; // nothing to apply; no dialog
        }
        let service_mode = matches!(self.service, Some(ServiceState::Installed(_)));
        let dialog = if service_mode {
            service::settings_reinstall_dialog()
        } else {
            service::settings_relaunch_dialog()
        };
        if !self.ui.confirm(&dialog) {
            return; // nothing written, nothing stopped
        }
        if let Some(t) = tok_new {
            let path = self.paths.web_token();
            if let Err(e) = self.sys.write_private(&path, t.as_str()) {
                self.ui
                    .message(&service::save_failed_dialog(&path, &e.to_string()));
                return; // nothing else written
            }
        }
        if new != current {
            let path = self.paths.settings_file();
            if let Err(e) = self
                .sys
                .write_private(&path, &settings::render_settings_file(new))
            {
                self.ui
                    .message(&service::save_failed_dialog(&path, &e.to_string()));
                return;
            }
        }
        if service_mode {
            self.reconfigure_service();
        } else {
            // The fully-tested stop → version → status → plan → launch
            // path, with the new settings (design §6 T5).
            self.handle_start();
        }
    }

    /// Reconfigure the installed service with the saved settings. The
    /// binary is NOT replaced (design §6 T6). Ordering matters:
    /// `sync_token` runs before `service install` (a remote plan needs
    /// the file; token off must remove it before the restarted service
    /// reads it).
    fn reconfigure_service(&mut self) {
        self.set_phase(Phase::Starting(StartStep::RemovingService));
        // Non-zero tolerated: the unit may not be loaded.
        let _ = self
            .sys
            .run(&self.paths.sidecar, &service::service_uninstall_args());

        self.set_phase(Phase::Starting(StartStep::InstallingService));
        let s = self.settings();
        self.sync_token(s);
        let args = service::service_install_args(s);
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

    /// Stale pidfile reaper: identity-checked (cmdline) SIGINT → poll up
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
    use crate::consts::{PORT_ATTEMPTS, READY_TIMEOUT, SERVICE_PORT, SESSION_PREFERRED_PORT};
    use crate::paths::DesktopPaths;
    use crate::phase::{Mode, Phase, Platform, StartError, StartStep};
    use crate::service::ReplacePolicy;
    use crate::session::PidRecord;
    use crate::settings::{Protocol, SaveRequest, Settings, SettingsWire, render_settings_file};
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

    // §9.1 case 40 (updated for #377: the session's token IS the
    // web-token file's token — design T1).
    #[test]
    fn t40_happy_session() {
        // (a) A valid web-token file: the session reuses it; no new token.
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            run_out(Some(0), NOT_INSTALLED, ""),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        sys.seed_file(WEB_TOKEN, "seeded-token\n");
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui.clone());

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session {
                    port,
                    protocol,
                    token
                }) if *port == SESSION_PREFERRED_PORT
                    && *protocol == Protocol::Http
                    && token.as_ref().map(|t| t.as_str()) == Some("seeded-token")
            ),
            "the session token is the file's token; got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 1, "exactly one spawn");
        assert_eq!(
            spawns[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()]
        );
        assert!(
            spawns[0]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == "seeded-token"),
            "spawn env must carry the file's token; got {:?}",
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
        assert!(
            sys.events()
                .iter()
                .all(|e| !matches!(e, Effect::RandomToken)),
            "no RandomToken when the file holds a valid token; events: {:?}",
            sys.events()
        );
        assert_eq!(
            sys.file(Path::new(WEB_TOKEN)).as_deref(),
            Some("seeded-token\n"),
            "the file is untouched"
        );

        // Pidfile: pid/port.
        let pidfile = sys.file(Path::new(PIDFILE)).expect("pidfile written");
        let rec = PidRecord::parse(&pidfile).expect("pidfile parses");
        assert_eq!(rec.port, SESSION_PREFERRED_PORT);

        // (b) No file: a token is generated + written 0600, and the env
        // carries it.
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

        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { port, .. }) if *port == SESSION_PREFERRED_PORT
            ),
            "got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
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
            "spawn env must carry the generated token; got {:?}",
            spawns[0].extra_env
        );
        assert_eq!(
            sys.file(Path::new(WEB_TOKEN)).as_deref(),
            Some(token.as_str()),
            "the generated token is exactly the file's contents"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the generated web-token must be written 0600"
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
            Phase::Ready(Mode::Service { token, .. }) => {
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
            Phase::Ready(Mode::Service { token, .. }) => {
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
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        // CONNECT after a successful install is proven by the probe
        // (DESIGN §4.3) — script a 200, never rely on the install command.
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        ui.answer_confirm(true); // Replace
        let ctrl = started(sys.clone(), ui);

        let tok = sys
            .tokens_generated()
            .first()
            .cloned()
            .expect("ensure_token generated a token");
        match ctrl.phase() {
            Phase::Ready(Mode::Service { token, .. }) => {
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

        // Command order (invariant A5, design auto-replace §4.3): the
        // binary copy runs FIRST, so a failed copy leaves the service
        // untouched: install → service uninstall → service install.
        assert_order_ok(&sys.run_args());

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
        // Bundled version = the sidecar's `--version` output (DESIGN §1),
        // scripted distinct from the app's crate version on purpose.
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
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
        let v24 = semver::Version::parse("0.24.0").unwrap();
        assert!(
            declined.matches(&InstalledVersion::Known(v22.clone()), &v24.clone()),
            "the exact (installed, bundled) pair must be recorded; bundled = the sidecar's --version"
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
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_at(INSTALLED_EXE, "running"),
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
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
            &["service", "install", "--bind", "127.0.0.1:7823"],
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

    // §9.1 case 57 (updated for #377: enable reuses the web-token file —
    // the session already used it, so no `preferred` token is passed).
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

        // The session launch already created the web-token (default
        // settings: token on) — the session's token IS the file's token.
        let session_file = sys
            .file(Path::new(WEB_TOKEN))
            .expect("web-token created at launch");
        assert_eq!(
            session_file.trim(),
            sys.tokens_generated()[0].as_str(),
            "the session token is the generated file token"
        );

        let _ = ctrl.handle(Action::ToggleBackground);

        match ctrl.phase() {
            Phase::Ready(Mode::Service { token, .. }) => {
                let file = sys.file(Path::new(WEB_TOKEN)).expect("web-token written");
                let file_tok = Token::parse(&file).expect("web-token parses");
                assert_eq!(
                    token.as_ref(),
                    Some(&file_tok),
                    "Ready's service token must equal the web-token file's contents"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
        // Exactly one token was generated for the whole flow: the enable
        // reused the file (no `preferred`, no regeneration).
        assert_eq!(
            sys.tokens_generated().len(),
            1,
            "the file token must be reused by the enable; generated {:?}",
            sys.tokens_generated()
        );
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
        // The token file was written 0600 (at the session launch).
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the web-token was written 0600"
        );
        // The pidfile is gone; no new sidecar spawn; tray checked.
        assert!(!sys.has_file(Path::new(PIDFILE)), "pidfile removed");
        assert_eq!(
            sys.spawned_specs().len(),
            1,
            "no new sidecar spawn in service mode"
        );
        assert!(ui.last_tray().background_checked, "tray checkbox checked");
        // Default settings ⇒ the service install has no --tls.
        assert!(
            sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                ]
            }),
            "default settings install loopback without --tls; runs: {:?}",
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
                rec.port, SESSION_PREFERRED_PORT,
                "pidfile names the new session's port"
            );
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
        // The stale record is reaped, but the fresh session replaces it.
        fresh_pidfile(&sys);

        // (b) Mismatched cmdline (PID reuse) → no signal; the fresh
        // session rewrites the pidfile.
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
        fresh_pidfile(&sys);

        // (c) Garbage pidfile → removed, no signal; the fresh session
        // rewrites the pidfile.
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
        assert_eq!(rec.pid, new_pid, "pidfile names the new session's pid");
        assert_eq!(rec.port, SESSION_PREFERRED_PORT);
    }

    // -------------------------------------------------------------------
    // Design #377 — controller plumbing (task 7) and SaveSettings
    // (task 8)
    // -------------------------------------------------------------------

    /// A fresh session-mode controller, Ready, with a default settings
    /// (no file) launch. The session child exits on SIGINT.
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

    // Design #377 t84 (invariant I3): the service protocol comes from
    // the `TLS:` status line, never from settings.json.
    #[test]
    fn t84_start_with_tls_service_probes_https_and_ready_https() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_tls("running", "on"),
        );
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_default_probe(vec![Probe::Status(200)]);
        let ui = FakeUi::new();
        let ctrl = started(sys.clone(), ui);
        match ctrl.phase() {
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(
                    *protocol,
                    Protocol::Https,
                    "the protocol comes from the TLS: line; got {:?}",
                    ctrl.phase()
                );
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is the file's token"
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

    // Design #377 t85: enabling background with a Remote settings file
    // installs the remote service.
    #[test]
    fn t85_enable_with_remote_file_installs_remote_service() {
        let sys = FakeSys::new();
        sys.seed_file(SETTINGS_FILE, render_settings_file(Settings::Remote));
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        // Start sees NotInstalled; the post-enable refresh sees Installed.
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                run_out(Some(0), NOT_INSTALLED, ""),
                installed_status_tls("running", "on"),
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
            &["service", "install", "--bind", "0.0.0.0:7823", "--tls"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        ui.answer_confirm(true);
        let mut ctrl = started(sys.clone(), ui.clone());

        // The session itself runs with the remote settings (invariant I4).
        assert_eq!(
            sys.spawned_specs()[0].args,
            vec![
                "--bind".to_string(),
                "0.0.0.0:7824".to_string(),
                "--tls".to_string()
            ],
            "the session bind/flags come from settings.json"
        );

        let _ = ctrl.handle(Action::ToggleBackground);
        assert!(
            sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "0.0.0.0:7823".to_string(),
                    "--tls".to_string(),
                ]
            }),
            "enable must install the remote service; runs: {:?}",
            sys.run_args()
        );
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Service { protocol, token, .. })
                    if *protocol == Protocol::Https && token.is_some()
            ),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design #377 t86: Replace with an https settings file — the binary
    // IS replaced (unlike reconfigure) and the service install gets
    // --tls.
    #[test]
    fn t86_replace_with_https_file_installs_tls_and_binary() {
        let sys = FakeSys::new();
        sys.seed_file(
            SETTINGS_FILE,
            render_settings_file(Settings::Loopback {
                protocol: Protocol::Https,
                token: true,
            }),
        );
        sys.set_run("peakbot", &["--version"], versioned("0.24.0"));
        // Start sees the OLD installed service (no TLS line); the
        // post-replace refresh sees the reinstalled one (TLS on).
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status_at(INSTALLED_EXE, "running"),
                installed_status_tls("running", "on"),
            ],
        );
        sys.set_run("peakbot-0.22.0", &["--version"], versioned("0.22.0"));
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823", "--tls"],
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
            sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                    "--tls".to_string(),
                ]
            }),
            "the https settings file must reach `service install --tls`; runs: {:?}",
            sys.run_args()
        );
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Service { protocol, .. })
                    if *protocol == Protocol::Https
            ),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design #377 t87 (criterion 9): a corrupt settings.json at Start
    // loads as defaults; load never rewrites the file.
    #[test]
    fn t87_corrupt_settings_file_at_start_loads_defaults_without_rewrite() {
        let sys = FakeSys::new();
        sys.seed_file(SETTINGS_FILE, "not json");
        let ui = FakeUi::new();
        let ctrl = ready_session(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, .. })
                    if *protocol == Protocol::Http
            ),
            "a corrupt file loads as the default (http); got {:?}",
            ctrl.phase()
        );
        assert_eq!(
            sys.spawned_specs()[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()],
            "default session: no --tls"
        );
        assert!(
            !sys.private_written().contains(Path::new(SETTINGS_FILE)),
            "load must never write the settings file"
        );
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some("not json"),
            "the corrupt file is left as-is"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "default token-on ⇒ the web-token is written 0600"
        );
    }

    // Design #377 t76 (criterion 3): session mode, saving https — the
    // session is relaunched with --tls and the webview URL is https.
    #[test]
    fn t76_save_settings_session_https() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let old_pid = sys.spawned_pids()[0];
        // The relaunch (handle_start) spawns a new session child.
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        ui.answer_confirm(true);

        let _ = ctrl.handle(Action::SaveSettings(
            Settings::Loopback {
                protocol: Protocol::Https,
                token: true,
            }
            .into(),
        ));

        // The file holds the new settings, byte-exact.
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some("{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":false}\n"),
            "settings.json must hold the rendered new settings"
        );
        // The old child was interrupted.
        let evts = sys.events();
        assert!(
            evts.iter()
                .any(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == old_pid)),
            "the old session must be interrupted; events: {evts:?}"
        );
        // The new spawn's args end with --tls.
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 2, "one relaunch");
        assert_eq!(
            spawns[1].args,
            vec![
                "--bind".to_string(),
                "127.0.0.1:7824".to_string(),
                "--tls".to_string()
            ],
            "the relaunched session must carry --tls"
        );
        // Ready(Session{Https}) and the URL is https with the token.
        let mode = match ctrl.phase() {
            Phase::Ready(m) => m.clone(),
            other => panic!("expected Ready, got {other:?}"),
        };
        assert!(
            matches!(
                mode,
                Mode::Session {
                    protocol: Protocol::Https,
                    token: Some(_),
                    ..
                }
            ),
            "got {mode:?}"
        );
        assert!(
            mode.url()
                .to_string()
                .starts_with("https://127.0.0.1:7824/?token="),
            "the webview URL must be https with the token; got {:?}",
            mode.url()
        );
    }

    // Design #377 t77 (criterion 4): session mode, saving token off —
    // the web-token file is removed before the relaunch and the new
    // session has no PEAKBOT_WEB_TOKEN and no ?token= URL.
    #[test]
    fn t77_save_settings_session_token_off() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        // The default launch created the web-token file.
        assert!(sys.has_file(Path::new(WEB_TOKEN)), "precondition");
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        ui.answer_confirm(true);

        let _ = ctrl.handle(Action::SaveSettings(
            Settings::Loopback {
                protocol: Protocol::Http,
                token: false,
            }
            .into(),
        ));

        // The token file was removed BEFORE the new spawn.
        let evts = sys.events();
        let i_rm = evts
            .iter()
            .position(|e| matches!(e, Effect::Remove { path } if path == Path::new(WEB_TOKEN)))
            .expect("the web-token file must be removed");
        let new_pid = sys.spawned_pids()[1];
        let i_spawn = evts
            .iter()
            .position(|e| matches!(e, Effect::Spawn { pid, .. } if *pid == new_pid))
            .expect("the relaunched session");
        assert!(
            i_rm < i_spawn,
            "the token file must be removed before the relaunched session reads it; events: {evts:?}"
        );
        // The new spawn has no PEAKBOT_WEB_TOKEN.
        let spawns = sys.spawned_specs();
        assert!(
            !spawns[1]
                .extra_env
                .iter()
                .any(|(k, _)| k == "PEAKBOT_WEB_TOKEN"),
            "no PEAKBOT_WEB_TOKEN when the token is off; got {:?}",
            spawns[1].extra_env
        );
        assert!(
            spawns[1]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_NO_OPEN" && v == "1"),
            "PEAKBOT_NO_OPEN=1 stays; got {:?}",
            spawns[1].extra_env
        );
        // Ready(Session{Http, None}) and the URL has no query.
        let mode = match ctrl.phase() {
            Phase::Ready(m) => m.clone(),
            other => panic!("expected Ready, got {other:?}"),
        };
        assert!(
            matches!(
                mode,
                Mode::Session {
                    protocol: Protocol::Http,
                    token: None,
                    ..
                }
            ),
            "got {mode:?}"
        );
        assert_eq!(
            mode.url().to_string(),
            "http://127.0.0.1:7824/",
            "no ?token= without a token"
        );
        assert!(
            !sys.has_file(Path::new(WEB_TOKEN)),
            "the token file is gone"
        );
    }

    // Design #377 t78 (criterion 10): cancelling the confirm changes
    // nothing.
    #[test]
    fn t78_save_settings_confirm_cancel_changes_nothing() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let evts_before = sys.events().len();
        ui.answer_confirm(false);

        let _ = ctrl.handle(Action::SaveSettings(
            Settings::Loopback {
                protocol: Protocol::Https,
                token: true,
            }
            .into(),
        ));

        assert!(
            ui.confirms().len() == 1,
            "the confirm dialog was shown; got {:?}",
            ui.confirms()
        );
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects after a cancel; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch");
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "nothing was written"
        );
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, .. })
                    if *protocol == Protocol::Http
            ),
            "the phase is unchanged; got {:?}",
            ctrl.phase()
        );
    }

    // Design #377 t79: saving the same settings is a no-op — no dialog,
    // no effects.
    #[test]
    fn t79_save_settings_unchanged_is_a_noop() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let evts_before = sys.events().len();

        let _ = ctrl.handle(Action::SaveSettings(Settings::default().into()));

        assert!(ui.confirms().is_empty(), "no dialog for an unchanged save");
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects for an unchanged save; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch");
        assert!(!sys.has_file(Path::new(SETTINGS_FILE)), "no file write");
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design #377 t80 (criterion 5): service mode, saving Remote —
    // uninstall + `service install --bind 0.0.0.0:7823 --tls`, NO
    // `install`, web-token written 0600 before the install.
    #[test]
    fn t80_save_settings_service_remote_reconfigures() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        // Start: TLS off; the post-reconfigure refresh: TLS on.
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status_tls("running", "off"),
                installed_status_tls("running", "on"),
            ],
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "0.0.0.0:7823", "--tls"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Service { protocol, token, .. })
                    if *protocol == Protocol::Http && token.is_none()
            ),
            "precondition: http tokenless service; got {:?}",
            ctrl.phase()
        );
        ui.answer_confirm(true);

        let _ = ctrl.handle(Action::SaveSettings(Settings::Remote.into()));

        // Command order: service uninstall → service install (remote).
        let runs = sys.run_args();
        let i_uninstall = runs
            .iter()
            .position(|r| r == &["service".to_string(), "uninstall".to_string()])
            .expect("service uninstall ran");
        let i_svc = runs
            .iter()
            .position(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "0.0.0.0:7823".to_string(),
                    "--tls".to_string(),
                ]
            })
            .expect("service install --bind 0.0.0.0:7823 --tls ran");
        assert!(
            i_uninstall < i_svc,
            "uninstall before install; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "reconfigure must NOT run `install` (no binary swap); runs: {runs:?}"
        );
        // The web-token was written 0600 BEFORE the service install ran.
        let evts = sys.events();
        let i_tok = evts
            .iter()
            .position(
                |e| matches!(e, Effect::WritePrivate { path } if path == Path::new(WEB_TOKEN)),
            )
            .expect("the web-token was written");
        let i_install = evts
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Run { args, .. }
                        if args == &["service".to_string(), "install".to_string(), "--bind".to_string(), "0.0.0.0:7823".to_string(), "--tls".to_string()]
                )
            })
            .expect("the service install ran");
        assert!(
            i_tok < i_install,
            "the token file must exist before the restarted service reads it; events: {evts:?}"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the web-token was written 0600"
        );
        // The file holds the new settings.
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some("{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":true}\n")
        );
        // Ready(Service{Https, Some(file token)}).
        let tok = sys
            .tokens_generated()
            .first()
            .cloned()
            .expect("a token was generated for the remote service");
        match ctrl.phase() {
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(*protocol, Protocol::Https, "status seq off→on");
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some(tok.as_str()),
                    "Ready with the file token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
    }

    // Design #377 t81 (criterion 6): service mode, saving loopback
    // token-off from a Remote file — the web-token is removed before the
    // loopback reinstall.
    #[test]
    fn t81_save_settings_service_token_off_removes_token() {
        let sys = FakeSys::new();
        sys.seed_file(SETTINGS_FILE, render_settings_file(Settings::Remote));
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        // Start: TLS on (the remote service); the post-reconfigure
        // refresh: TLS off.
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![
                installed_status_tls("running", "on"),
                installed_status_tls("running", "off"),
            ],
        );
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Service { protocol, .. })
                    if *protocol == Protocol::Https
            ),
            "precondition: https service; got {:?}",
            ctrl.phase()
        );
        ui.answer_confirm(true);

        let _ = ctrl.handle(Action::SaveSettings(
            Settings::Loopback {
                protocol: Protocol::Http,
                token: false,
            }
            .into(),
        ));

        // The web-token was removed BEFORE the service install ran.
        let evts = sys.events();
        let i_rm = evts
            .iter()
            .position(|e| matches!(e, Effect::Remove { path } if path == Path::new(WEB_TOKEN)))
            .expect("the web-token file must be removed");
        let i_install = evts
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Run { args, .. }
                        if args == &["service".to_string(), "install".to_string(), "--bind".to_string(), "127.0.0.1:7823".to_string()]
                )
            })
            .expect("service install --bind 127.0.0.1:7823 (no --tls) ran");
        assert!(
            i_rm < i_install,
            "the token file must be removed before the restarted service reads it; events: {evts:?}"
        );
        let runs = sys.run_args();
        assert!(
            runs.iter()
                .any(|r| r == &["service".to_string(), "uninstall".to_string()]),
            "the service was uninstalled first; runs: {runs:?}"
        );
        assert!(
            !runs.iter().any(|r| r == &["install".to_string()]),
            "no binary swap; runs: {runs:?}"
        );
        assert!(
            !sys.private_written().contains(Path::new(WEB_TOKEN)),
            "token off ⇒ no web-token write"
        );
        assert!(
            !sys.has_file(Path::new(WEB_TOKEN)),
            "the token file is gone"
        );
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some("{\"v\":1,\"protocol\":\"http\",\"token\":false,\"remote\":false}\n")
        );
        match ctrl.phase() {
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(*protocol, Protocol::Http, "status seq on→off");
                assert!(token.is_none(), "no token");
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
    }

    // Design #377 t82: a failing reconfigure install →
    // Failed(ServiceCommandFailed); the file keeps the new settings
    // (write-then-apply).
    #[test]
    fn t82_save_settings_reconfigure_install_failure() {
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_tls("running", "off"),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "0.0.0.0:7823", "--tls"],
            run_out(Some(1), "", "reinstall failed\n"),
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        ui.answer_confirm(true);

        let _ = ctrl.handle(Action::SaveSettings(Settings::Remote.into()));

        match ctrl.phase() {
            Phase::Failed(StartError::ServiceCommandFailed { command, output }) => {
                assert!(
                    command.contains("service install"),
                    "command must name the failing verb; {command:?}"
                );
                assert!(
                    output.contains("reinstall failed"),
                    "output must carry the child's stderr; {output:?}"
                );
            }
            other => panic!("expected ServiceCommandFailed, got {other:?}"),
        }
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some(render_settings_file(Settings::Remote).as_str()),
            "write-then-apply: the file keeps the new settings"
        );
    }

    // Design #377 t83: a failing settings write → the save-failed
    // dialog; the session/service is untouched.
    #[test]
    fn t83_save_settings_write_failure_shows_dialog_and_changes_nothing() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        sys.fail_write(PathBuf::from(SETTINGS_FILE));
        ui.answer_confirm(true);
        let evts_before = sys.events().len();

        let _ = ctrl.handle(Action::SaveSettings(
            Settings::Loopback {
                protocol: Protocol::Https,
                token: true,
            }
            .into(),
        ));

        assert!(
            ui.messages()
                .iter()
                .any(|d| { d.title == "Couldn't save settings" && d.body.contains(SETTINGS_FILE) }),
            "the save-failed info dialog must show; got {:?}",
            ui.messages()
        );
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects after a failed write; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "the session is untouched");
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "nothing was written"
        );
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, .. })
                    if *protocol == Protocol::Http
            ),
            "the phase is unchanged; got {:?}",
            ctrl.phase()
        );
    }

    // Design #377 t89: SaveSettings while Starting is ignored, like its
    // siblings.
    #[test]
    fn t89_save_settings_while_starting_is_ignored() {
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
        let _ = ctrl.handle(Action::SaveSettings(Settings::Remote.into()));
        assert!(
            matches!(ctrl.phase(), Phase::Starting(StartStep::CheckingBundled)),
            "phase must be untouched; got {:?}",
            ctrl.phase()
        );
        assert!(sys.run_args().is_empty(), "no commands may run");
        assert!(sys.spawned_specs().is_empty());
        assert!(ui.confirms().is_empty());
        assert!(ui.messages().is_empty());
        assert!(sys.events().is_empty(), "no side effects at all");
    }

    // Design #377b t94: session mode, same settings + a new token — the
    // web-token is written 0600 BEFORE the relaunch, settings.json is
    // NOT written, and the new session runs with the user's token
    // (nothing generated).
    #[test]
    fn t94_save_settings_session_token_only_relaunches() {
        let new_tok = "new-user-token-123456";
        let sys = FakeSys::new();
        // Seed the file token so the INITIAL launch generates nothing —
        // the "no RandomToken" assertion below checks the whole log.
        sys.seed_file(WEB_TOKEN, "old-session-token-123456\n");
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let old_pid = sys.spawned_pids()[0];
        // The relaunch (handle_start) spawns a new session child.
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        ui.answer_confirm(true);

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Http,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        // One relaunch dialog.
        assert_eq!(ui.confirms().len(), 1, "one save ⇒ one dialog");
        assert_eq!(ui.confirms()[0].title, "Apply settings?");
        assert_eq!(ui.confirms()[0].ok, "Restart");

        // The web-token was written 0600 BEFORE the old child was
        // interrupted and the new one spawned.
        let evts = sys.events();
        let i_tok = evts
            .iter()
            .position(
                |e| matches!(e, Effect::WritePrivate { path } if path == Path::new(WEB_TOKEN)),
            )
            .expect("the web-token was written");
        let i_intr = evts
            .iter()
            .position(|e| matches!(e, Effect::ChildInterrupt { pid } if *pid == old_pid))
            .expect("the old child was interrupted");
        let new_pid = sys.spawned_pids()[1];
        let i_spawn = evts
            .iter()
            .position(|e| matches!(e, Effect::Spawn { pid, .. } if *pid == new_pid))
            .expect("the relaunched session");
        assert!(
            i_tok < i_intr,
            "the token file must be written before the relaunch; events: {evts:?}"
        );
        assert!(
            i_tok < i_spawn,
            "the token file must be written before the new session reads it; events: {evts:?}"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the web-token was written 0600"
        );
        assert_eq!(sys.file(Path::new(WEB_TOKEN)).as_deref(), Some(new_tok));

        // settings.json is NOT written (the settings are unchanged).
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "a token-only change must not touch settings.json"
        );

        // The new session runs with the user's token; nothing generated.
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 2, "one relaunch");
        assert!(
            spawns[1]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == new_tok),
            "the spawn env carries the user's token; got {:?}",
            spawns[1].extra_env
        );
        assert!(
            evts.iter().all(|e| !matches!(e, Effect::RandomToken)),
            "no token generation; events: {evts:?}"
        );

        // Ready with the new token in the URL.
        let mode = match ctrl.phase() {
            Phase::Ready(m) => m.clone(),
            other => panic!("expected Ready, got {other:?}"),
        };
        assert!(
            matches!(
                mode,
                Mode::Session {
                    token: Some(ref t),
                    ..
                } if t.as_str() == new_tok
            ),
            "got {mode:?}"
        );
        assert!(
            mode.url()
                .to_string()
                .ends_with(&format!("?token={new_tok}")),
            "the webview URL carries the new token; got {:?}",
            mode.url()
        );
    }

    // Design #377b t95: service mode, a token-only change — the
    // reinstall dialog, the web-token is written 0600 BEFORE
    // `service uninstall`, the token never reaches any argv, and
    // Ready(Service{…, Some(new)}).
    #[test]
    fn t95_save_settings_service_token_only_reconfigures() {
        let new_tok = "svc-user-token-12345678";
        let sys = FakeSys::new();
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        sys.set_run(
            "peakbot",
            &["service", "status"],
            installed_status_tls("running", "off"),
        );
        sys.set_default_probe(vec![Probe::Status(200)]);
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823"],
            run_out(Some(0), "ok\n", ""),
        );
        let ui = FakeUi::new();
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Service { protocol, token, .. })
                    if *protocol == Protocol::Http && token.is_none()
            ),
            "precondition: a tokenless http service; got {:?}",
            ctrl.phase()
        );
        assert!(!sys.has_file(Path::new(WEB_TOKEN)), "precondition: no file");
        ui.answer_confirm(true);

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Http,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        // The reinstall dialog (no `service restart` verb exists;
        // the reinstall IS the restart).
        assert_eq!(ui.confirms().len(), 1, "one save ⇒ one dialog");
        assert_eq!(ui.confirms()[0].title, "Apply settings?");
        assert_eq!(ui.confirms()[0].ok, "Reinstall");

        // The web-token was written 0600 BEFORE the service uninstall.
        let evts = sys.events();
        let i_tok = evts
            .iter()
            .position(
                |e| matches!(e, Effect::WritePrivate { path } if path == Path::new(WEB_TOKEN)),
            )
            .expect("the web-token was written");
        let i_uninst = evts
            .iter()
            .position(|e| {
                matches!(
                    e,
                    Effect::Run { args, .. }
                        if args == &["service".to_string(), "uninstall".to_string()]
                )
            })
            .expect("service uninstall ran");
        assert!(
            i_tok < i_uninst,
            "the token file must exist before the restarted service reads it; events: {evts:?}"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the web-token was written 0600"
        );
        assert_eq!(sys.file(Path::new(WEB_TOKEN)).as_deref(), Some(new_tok));

        // No settings.json write; the token never reaches any argv.
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "a token-only change must not touch settings.json"
        );
        for run in sys.run_args() {
            assert!(
                run.iter().all(|a| a != new_tok),
                "the token must not appear in any argv; run: {run:?}"
            );
        }
        assert!(
            sys.spawned_specs().is_empty(),
            "no session spawn in service mode"
        );
        assert!(
            evts.iter().all(|e| !matches!(e, Effect::RandomToken)),
            "the user's token is kept, nothing generated; events: {evts:?}"
        );

        // Ready(Service{Http, Some(new)}).
        match ctrl.phase() {
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(*protocol, Protocol::Http);
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some(new_tok),
                    "Ready with the user's token"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }
    }

    // Design #377b t96: the same settings with a token equal to the
    // file's token is a no-op — no dialog, no effects.
    #[test]
    fn t96_save_settings_same_settings_same_token_is_a_noop() {
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let file_tok = sys
            .file(Path::new(WEB_TOKEN))
            .expect("the launch created the web-token")
            .trim()
            .to_string();
        let evts_before = sys.events().len();

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Http,
                token: true,
                remote: false,
            },
            &file_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        assert!(
            ui.confirms().is_empty(),
            "no dialog for the same settings + the same token"
        );
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch");
        assert!(!sys.has_file(Path::new(SETTINGS_FILE)), "no file write");
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
            "got {:?}",
            ctrl.phase()
        );
    }

    // Design #377b t97: cancelling the confirm leaves the web-token
    // and the settings file untouched, and nothing is spawned.
    #[test]
    fn t97_save_settings_cancel_writes_nothing() {
        let new_tok = "cancel-me-token-123456789";
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        let token_before = sys
            .file(Path::new(WEB_TOKEN))
            .expect("the launch created the web-token");
        let evts_before = sys.events().len();
        ui.answer_confirm(false);

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Https,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        assert_eq!(ui.confirms().len(), 1, "the confirm dialog was shown");
        assert_eq!(
            sys.file(Path::new(WEB_TOKEN)),
            Some(token_before),
            "the web-token is untouched"
        );
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "nothing was written"
        );
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects after a cancel; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch");
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, .. }) if *protocol == Protocol::Http
            ),
            "the phase is unchanged; got {:?}",
            ctrl.phase()
        );
    }

    // Design #377b t98: Http→Https + a new token in ONE save — one
    // dialog, the token write BEFORE the settings write, one relaunch
    // with --tls carrying the user's token.
    #[test]
    fn t98_save_settings_protocol_and_token_together() {
        let new_tok = "https-user-token-1234567";
        let sys = FakeSys::new();
        // Seed the file token so the INITIAL launch generates nothing —
        // the "no RandomToken" assertion below checks the whole log.
        sys.seed_file(WEB_TOKEN, "old-session-token-123456\n");
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        ui.answer_confirm(true);

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Https,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        assert_eq!(ui.confirms().len(), 1, "one save ⇒ one dialog");

        // The token write comes first, the settings write second.
        let evts = sys.events();
        let i_tok = evts
            .iter()
            .position(
                |e| matches!(e, Effect::WritePrivate { path } if path == Path::new(WEB_TOKEN)),
            )
            .expect("the web-token was written");
        let i_file = evts
            .iter()
            .position(
                |e| matches!(e, Effect::WritePrivate { path } if path == Path::new(SETTINGS_FILE)),
            )
            .expect("settings.json was written");
        assert!(
            i_tok < i_file,
            "the token file must be written before the settings file; events: {evts:?}"
        );
        assert_eq!(sys.file(Path::new(WEB_TOKEN)).as_deref(), Some(new_tok));
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some("{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":false}\n")
        );

        // One relaunch, with --tls and the user's token.
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 2, "one relaunch");
        assert_eq!(
            spawns[1].args,
            vec![
                "--bind".to_string(),
                "127.0.0.1:7824".to_string(),
                "--tls".to_string()
            ],
            "the relaunched session must carry --tls"
        );
        assert!(
            spawns[1]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == new_tok),
            "the spawn env carries the user's token; got {:?}",
            spawns[1].extra_env
        );
        assert!(
            evts.iter().all(|e| !matches!(e, Effect::RandomToken)),
            "the user's token is kept, nothing generated; events: {evts:?}"
        );

        let mode = match ctrl.phase() {
            Phase::Ready(m) => m.clone(),
            other => panic!("expected Ready, got {other:?}"),
        };
        assert!(
            matches!(
                mode,
                Mode::Session {
                    protocol: Protocol::Https,
                    token: Some(ref t),
                    ..
                } if t.as_str() == new_tok
            ),
            "got {mode:?}"
        );
    }

    // Design #377b t99: a failing web-token write → the save-failed
    // dialog naming the web-token path; nothing else is written,
    // nothing restarts.
    #[test]
    fn t99_save_settings_token_write_failure_changes_nothing() {
        let new_tok = "fail-write-token-1234567";
        let sys = FakeSys::new();
        let ui = FakeUi::new();
        let mut ctrl = ready_session(sys.clone(), ui.clone());
        sys.fail_write(PathBuf::from(WEB_TOKEN));
        ui.answer_confirm(true);
        let evts_before = sys.events().len();

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Https,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        assert!(
            ui.messages()
                .iter()
                .any(|d| d.title == "Couldn't save settings" && d.body.contains(WEB_TOKEN)),
            "the save-failed dialog must name the web-token path; got {:?}",
            ui.messages()
        );
        assert!(
            !sys.has_file(Path::new(SETTINGS_FILE)),
            "settings.json must not be written after a failed token write"
        );
        assert_eq!(
            sys.events().len(),
            evts_before,
            "no side effects after the failed write; events: {:?}",
            sys.events()
        );
        assert_eq!(sys.spawned_specs().len(), 1, "no relaunch");
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, .. }) if *protocol == Protocol::Http
            ),
            "the phase is unchanged; got {:?}",
            ctrl.phase()
        );
    }

    // Design #377b t100: token off→on with a value — the file holds
    // the user's value, and no token is generated (the empty→generate
    // path stays locked by t40).
    #[test]
    fn t100_save_settings_token_off_to_on_with_value() {
        let new_tok = "off-to-on-token-123456789";
        let sys = FakeSys::new();
        sys.seed_file(
            SETTINGS_FILE,
            render_settings_file(Settings::Loopback {
                protocol: Protocol::Http,
                token: false,
            }),
        );
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
        let mut ctrl = started(sys.clone(), ui.clone());
        assert!(
            matches!(
                ctrl.phase(),
                Phase::Ready(Mode::Session { protocol, token, .. })
                    if *protocol == Protocol::Http && token.is_none()
            ),
            "precondition: a tokenless session; got {:?}",
            ctrl.phase()
        );
        assert!(!sys.has_file(Path::new(WEB_TOKEN)), "token off ⇒ no file");
        sys.queue_child(crate::testing::FakeChildSpec::alive());
        ui.answer_confirm(true);

        let req = SaveRequest::parse(
            SettingsWire {
                protocol: Protocol::Http,
                token: true,
                remote: false,
            },
            new_tok,
        )
        .expect("a valid token parses");
        let _ = ctrl.handle(Action::SaveSettings(req));

        assert_eq!(
            sys.file(Path::new(WEB_TOKEN)).as_deref(),
            Some(new_tok),
            "the file holds the user's value"
        );
        assert!(
            sys.private_written().contains(Path::new(WEB_TOKEN)),
            "the web-token was written 0600"
        );
        assert_eq!(
            sys.file(Path::new(SETTINGS_FILE)).as_deref(),
            Some(
                render_settings_file(Settings::Loopback {
                    protocol: Protocol::Http,
                    token: true,
                })
                .as_str()
            )
        );
        let evts = sys.events();
        assert!(
            evts.iter().all(|e| !matches!(e, Effect::RandomToken)),
            "the user's token is kept, nothing generated; events: {evts:?}"
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 2, "one relaunch");
        assert!(
            spawns[1]
                .extra_env
                .iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == new_tok),
            "the spawn env carries the user's token; got {:?}",
            spawns[1].extra_env
        );
        match ctrl.phase() {
            Phase::Ready(Mode::Session { token, .. }) => {
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some(new_tok),
                    "Ready with the user's token"
                );
            }
            other => panic!("expected Ready(Session), got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // Design auto-replace §7.1 — t107–t120.
    //
    // Fixture conventions: `SIDECAR` is the bundled sidecar; the
    // installed exe's file contents are seeded with `sys.seed_file`.
    // "build-A" vs "build-B" ⇒ the builds differ; both "build-A" ⇒
    // identical. "Replace scripts" = `install`, `service uninstall` and
    // `service install --bind 127.0.0.1:7823 [--tls]` scripted ok, plus
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
            &["service", "install", "--bind", "127.0.0.1:7823"],
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(*protocol, Protocol::Http, "http service (no TLS line)");
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is the web-token file's"
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "got {:?}",
            ctrl.phase()
        );
        let runs = sys.run_args();
        assert_eq!(
            runs.iter()
                .filter(|r| {
                    *r == &[
                        "service".to_string(),
                        "install".to_string(),
                        "--bind".to_string(),
                        "127.0.0.1:7823".to_string(),
                    ]
                })
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        ui.answer_confirm(false); // Keep
        let mut ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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

    // Design auto-replace t114 (SC6/A4): the silent replace reproduces
    // the RUNNING service — protocol from the TLS line, token from the
    // web-token file, which is never touched.
    #[test]
    fn t114_auto_replace_preserves_tls_and_token() {
        // (a) No settings file at all, service runs with TLS: the
        // reinstall gets `--tls`, the token file is untouched, and no
        // token is minted.
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "127.0.0.1:7823", "--tls"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_probe(
            "https://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Listening],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "127.0.0.1:7823".to_string(),
                    "--tls".to_string(),
                ]
            }),
            "the TLS line must reach `service install --tls`; runs: {:?}",
            sys.run_args()
        );
        assert_eq!(
            sys.file(Path::new(WEB_TOKEN)).as_deref(),
            Some("svc-token\n"),
            "the web-token file is untouched"
        );
        let evts = sys.events();
        assert!(
            evts.iter().all(|e| {
                !matches!(e, Effect::WritePrivate { path } if path == Path::new(WEB_TOKEN))
                    && !matches!(e, Effect::Remove { path } if path == Path::new(WEB_TOKEN))
            }),
            "no WritePrivate/Remove on the web-token; events: {evts:?}"
        );
        assert!(
            evts.iter().all(|e| !matches!(e, Effect::RandomToken)),
            "no token may be minted; events: {evts:?}"
        );
        match ctrl.phase() {
            Phase::Ready(Mode::Service {
                protocol, token, ..
            }) => {
                assert_eq!(*protocol, Protocol::Https, "the TLS line ⇒ https");
                assert_eq!(
                    token.as_ref().map(|t| t.as_str()),
                    Some("svc-token"),
                    "the token is the file's"
                );
            }
            other => panic!("expected Ready(Service), got {other:?}"),
        }

        // (b) Settings with token OFF + a TLS-off service: the reinstall
        // has no `--tls`, and the web-token file is NOT removed (the
        // silent path never syncs the token).
        let sys = FakeSys::new();
        sys.seed_file(
            SETTINGS_FILE,
            render_settings_file(Settings::Loopback {
                protocol: Protocol::Http,
                token: false,
            }),
        );
        sys.set_run("peakbot", &["--version"], versioned("0.23.0"));
        let pre = installed_status_at(INSTALLED_SAME, "running");
        sys.set_run_seq(
            "peakbot",
            &["service", "status"],
            vec![pre, installed_status_tls("running", "off")],
        );
        sys.set_run("peakbot-same", &["--version"], versioned("0.23.0"));
        seed_builds_differ(&sys, INSTALLED_SAME);
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        script_replace_http(&sys);
        sys.set_probe(
            "http://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Status(200)],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

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
            "a TLS-off service is reinstalled without --tls; runs: {runs:?}"
        );
        assert!(
            runs.iter().all(|r| !r.iter().any(|a| a == "--tls")),
            "no --tls anywhere; runs: {runs:?}"
        );
        assert!(
            sys.has_file(Path::new(WEB_TOKEN)),
            "the silent path must NOT remove the web-token (token off in settings.json is not applied)"
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "got {:?}",
            ctrl.phase()
        );
        assert!(ui.confirms().is_empty(), "no dialog");

        // (c) Saved settings Remote + TLS on: bind 0.0.0.0 with --tls.
        let sys = FakeSys::new();
        sys.seed_file(SETTINGS_FILE, render_settings_file(Settings::Remote));
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
        sys.set_run("peakbot", &["install"], run_out(Some(0), "ok\n", ""));
        sys.set_run(
            "peakbot",
            &["service", "uninstall"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_run(
            "peakbot",
            &["service", "install", "--bind", "0.0.0.0:7823", "--tls"],
            run_out(Some(0), "ok\n", ""),
        );
        sys.set_probe(
            "https://127.0.0.1:7823/commands?token=svc-token",
            vec![Probe::Listening],
        );
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            sys.run_args().iter().any(|r| {
                r == &[
                    "service".to_string(),
                    "install".to_string(),
                    "--bind".to_string(),
                    "0.0.0.0:7823".to_string(),
                    "--tls".to_string(),
                ]
            }),
            "Remote + TLS ⇒ 0.0.0.0 with --tls; runs: {:?}",
            sys.run_args()
        );
        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
            "got {:?}",
            ctrl.phase()
        );
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
            &["service", "install", "--bind", "127.0.0.1:7823"],
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
        sys.seed_file(WEB_TOKEN, "seeded-token\n");
        let ui = FakeUi::new();
        let ctrl = started_with(sys.clone(), ui.clone(), ReplacePolicy::Auto);

        assert!(
            matches!(ctrl.phase(), Phase::Ready(Mode::Session { .. })),
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
        sys.seed_file(WEB_TOKEN, "svc-token\n");
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
            matches!(ctrl.phase(), Phase::Ready(Mode::Service { .. })),
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
            sys.seed_file(WEB_TOKEN, "win-token\n");
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
                    Phase::Ready(Mode::Session { port, .. })
                        if *port == SESSION_PREFERRED_PORT
                ),
                "Windows start must land Ready(Session) even with an installed service (state {state:?}); got {:?}",
                ctrl.phase()
            );
            let spawns = sys.spawned_specs();
            assert_eq!(spawns.len(), 1, "the session sidecar was spawned once");
            assert_eq!(
                spawns[0].args,
                vec!["--bind".to_string(), "127.0.0.1:7824".to_string()],
                "the spawn is exactly the session path's (state {state:?}); got {:?}",
                spawns[0].args
            );
            assert!(
                spawns[0]
                    .extra_env
                    .iter()
                    .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == "win-token"),
                "the session token plumbing is unchanged (state {state:?}); got {:?}",
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
        sys.seed_file(WEB_TOKEN, "win-token\n");
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
                Phase::Ready(Mode::Session { port, .. })
                    if *port == SESSION_PREFERRED_PORT
            ),
            "the session path must be taken; got {:?}",
            ctrl.phase()
        );
        let spawns = sys.spawned_specs();
        assert_eq!(spawns.len(), 1, "the session sidecar was spawned");
        assert_eq!(
            spawns[0].args,
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()],
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
}
