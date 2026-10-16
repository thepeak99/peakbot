//! Phase, errors, views (DESIGN §3.5). The locked strings the status page
//! and tray render live in the `t30`/`t31` tests.

use std::path::PathBuf;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::service::ServiceState;
use crate::token::Token;

/// Where the webview points. A session ALWAYS carries a token (illegal
/// state unrepresentable, §3.8-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Session {
        port: u16,
        token: Token,
    },
    /// `token: None` ⇔ web-token file absent (tokenless loopback service).
    Service {
        token: Option<Token>,
    },
}

impl Mode {
    /// `Service ⇒ SERVICE_PORT`.
    pub fn port(&self) -> u16 {
        match self {
            Mode::Session { port, .. } => *port,
            Mode::Service { .. } => crate::consts::SERVICE_PORT,
        }
    }

    /// `http://127.0.0.1:<port>/` [+ `?token=<percent-encoded>`].
    pub fn url(&self) -> Url {
        let mut u = Url::parse(&format!("http://127.0.0.1:{}/", self.port()))
            .expect("static host is a valid URL");
        if let Some(tok) = self.token() {
            u.query_pairs_mut().append_pair("token", tok.as_str());
        }
        u
    }

    /// `http://127.0.0.1:<port>/commands` [+ `?token=…`].
    pub fn probe_url(&self) -> Url {
        let mut u = Url::parse(&format!("http://127.0.0.1:{}/commands", self.port()))
            .expect("static host is a valid URL");
        if let Some(tok) = self.token() {
            u.query_pairs_mut().append_pair("token", tok.as_str());
        }
        u
    }

    /// `"http://127.0.0.1:<port>"`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }

    fn token(&self) -> Option<&Token> {
        match self {
            Mode::Session { token, .. } => Some(token),
            Mode::Service { token } => token.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStep {
    CheckingBundled,
    CheckingService,
    StartingSession,
    WaitingForBackend,
    InstallingService,
    RemovingService,
    ConnectingService,
    StoppingSession,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    SidecarMissing {
        path: PathBuf,
    },
    /// `--version` failed or unparseable.
    SidecarBroken {
        detail: String,
    },
    /// PORT_ATTEMPTS exhausted.
    PortUnavailable,
    /// Died during startup OR while Ready.
    SessionExited {
        code: Option<i32>,
        log_tail: String,
    },
    /// Session readiness timeout.
    NotReady {
        waited: Duration,
    },
    /// Probe returned 401/403.
    TokenRejected {
        service: bool,
    },
    ServiceUnreachable {
        waited: Duration,
    },
    ServiceCommandFailed {
        command: String,
        output: String,
    },
}

/// `Cancelled` is NOT a `StartError` (§3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Starting(StartStep),
    Ready(Mode),
    Failed(StartError),
}

/// The status page's button targets (DESIGN §3.5). `snake_case` wire form
/// for the `act` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    Retry,
    OpenLogs,
    ReinstallService,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusView {
    /// `"starting" | "ready" | "failed"`.
    pub kind: &'static str,
    pub title: String,
    /// Multi-line, shown in `<pre>`.
    pub detail: Option<String>,
    /// Ordered, rendered as buttons.
    pub actions: Vec<UiAction>,
}

/// The locked status-page title for a start step (DESIGN §3.5).
fn step_text(step: StartStep) -> &'static str {
    match step {
        StartStep::CheckingBundled => "Checking the bundled PeakBot…",
        StartStep::CheckingService => "Looking for a background service…",
        StartStep::StartingSession => "Starting PeakBot…",
        StartStep::WaitingForBackend => "Waiting for PeakBot to be ready…",
        StartStep::InstallingService => "Installing the background service…",
        StartStep::RemovingService => "Removing the background service…",
        StartStep::ConnectingService => "Connecting to the background service…",
        StartStep::StoppingSession => "Stopping PeakBot…",
    }
}

pub fn status_view(phase: &Phase) -> StatusView {
    match phase {
        Phase::Starting(step) => StatusView {
            kind: "starting",
            title: step_text(*step).to_string(),
            detail: None,
            actions: Vec::new(),
        },
        Phase::Ready(_) => StatusView {
            kind: "ready",
            title: "Opening PeakBot…".to_string(),
            detail: None,
            actions: Vec::new(),
        },
        Phase::Failed(err) => failed_view(err),
    }
}

fn failed_view(err: &StartError) -> StatusView {
    let retry_logs_quit = vec![UiAction::Retry, UiAction::OpenLogs, UiAction::Quit];
    let retry_reinstall_quit = vec![UiAction::Retry, UiAction::ReinstallService, UiAction::Quit];
    match err {
        StartError::SidecarMissing { path } => StatusView {
            kind: "failed",
            title: "The PeakBot backend is missing from this app".into(),
            detail: Some(path.display().to_string()),
            actions: vec![UiAction::Quit],
        },
        StartError::SidecarBroken { .. } => StatusView {
            kind: "failed",
            title: "The bundled PeakBot backend failed to run".into(),
            detail: None,
            actions: retry_logs_quit,
        },
        StartError::PortUnavailable => StatusView {
            kind: "failed",
            title: "Couldn't find a free local port".into(),
            detail: None,
            actions: vec![UiAction::Retry, UiAction::Quit],
        },
        StartError::SessionExited { code, log_tail } => {
            let head = match code {
                Some(n) => format!("exit code {n}"),
                None => "killed by a signal".to_string(),
            };
            StatusView {
                kind: "failed",
                title: "PeakBot stopped unexpectedly".into(),
                detail: Some(format!("{head}\n\n{log_tail}")),
                actions: retry_logs_quit,
            }
        }
        StartError::NotReady { waited } => StatusView {
            kind: "failed",
            title: format!("PeakBot didn't start within {} s", waited.as_secs()),
            detail: None,
            actions: retry_logs_quit,
        },
        StartError::TokenRejected { service: false } => StatusView {
            kind: "failed",
            title: "Another program answered on PeakBot's port".into(),
            detail: None,
            actions: vec![UiAction::Retry, UiAction::Quit],
        },
        StartError::TokenRejected { service: true } => StatusView {
            kind: "failed",
            title: "The background service rejected the access token".into(),
            detail: Some(crate::service::service_log_hint().to_string()),
            actions: retry_reinstall_quit,
        },
        StartError::ServiceUnreachable { .. } => StatusView {
            kind: "failed",
            title: format!(
                "The background PeakBot service isn't responding on 127.0.0.1:{}",
                crate::consts::SERVICE_PORT
            ),
            detail: Some(crate::service::service_log_hint().to_string()),
            actions: retry_reinstall_quit,
        },
        StartError::ServiceCommandFailed { command, output } => StatusView {
            kind: "failed",
            title: "A background-service command failed".into(),
            detail: Some(format!("$ {command}\n{output}")),
            actions: vec![
                UiAction::Retry,
                UiAction::ReinstallService,
                UiAction::OpenLogs,
                UiAction::Quit,
            ],
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayModel {
    /// Disabled first item.
    pub status: String,
    pub background_checked: bool,
    pub background_enabled: bool,
    pub background_label: String,
}

pub fn tray_model(
    phase: &Phase,
    service: Option<&ServiceState>,
    bundled: Option<&Version>,
) -> TrayModel {
    let v = bundled
        .map(Version::to_string)
        .unwrap_or_else(|| "?".to_string());
    let status = match phase {
        Phase::Starting(_) => format!("PeakBot {v} · starting…"),
        Phase::Ready(Mode::Session { port, .. }) => {
            format!("PeakBot {v} · running (port {port})")
        }
        Phase::Ready(Mode::Service { .. }) => format!("PeakBot {v} · running in background"),
        Phase::Failed(_) => format!("PeakBot {v} · error"),
    };
    let background_label = match service {
        Some(ServiceState::Unavailable { .. }) => {
            "Keep PeakBot running in background (unavailable)".to_string()
        }
        _ => "Keep PeakBot running in background".to_string(),
    };
    let background_checked = matches!(service, Some(ServiceState::Installed(_)));
    let background_enabled = matches!(phase, Phase::Ready(_) | Phase::Failed(_))
        && matches!(
            service,
            Some(ServiceState::NotInstalled) | Some(ServiceState::Installed(_))
        );
    TrayModel {
        status,
        background_checked,
        background_enabled,
        background_label,
    }
}

/// A native dialog. `cancel: None` ⇒ info dialog (single OK button).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub body: String,
    pub ok: String,
    pub cancel: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{InstalledService, RunState};

    // §9.1 case 28.
    #[test]
    fn t28_mode_session_url_and_probe_url() {
        let tok = Token::parse("tok+en&1").expect("token with + & parses");
        let m = Mode::Session {
            port: 7824,
            token: tok,
        };
        assert_eq!(m.port(), 7824);
        let url = m.url();
        assert_eq!(
            url.to_string(),
            "http://127.0.0.1:7824/?token=tok%2Ben%261",
            "the token must be percent-encoded"
        );
        let probe = m.probe_url();
        assert_eq!(probe.path(), "/commands");
        assert_eq!(
            probe.to_string(),
            "http://127.0.0.1:7824/commands?token=tok%2Ben%261"
        );
    }

    // §9.1 case 29.
    #[test]
    fn t29_mode_service_url_and_origin() {
        let m = Mode::Service { token: None };
        assert_eq!(m.port(), 7823);
        assert_eq!(m.url().to_string(), "http://127.0.0.1:7823/");
        assert_eq!(m.origin(), "http://127.0.0.1:7823");
    }

    // §9.1 case 30 — one case per row of the §3.5 locked table.
    #[test]
    fn t30_status_view_titles_and_actions_per_locked_table() {
        // Starting — exact step texts.
        let start_steps = [
            (StartStep::CheckingBundled, "Checking the bundled PeakBot…"),
            (
                StartStep::CheckingService,
                "Looking for a background service…",
            ),
            (StartStep::StartingSession, "Starting PeakBot…"),
            (
                StartStep::WaitingForBackend,
                "Waiting for PeakBot to be ready…",
            ),
            (
                StartStep::InstallingService,
                "Installing the background service…",
            ),
            (
                StartStep::RemovingService,
                "Removing the background service…",
            ),
            (
                StartStep::ConnectingService,
                "Connecting to the background service…",
            ),
            (StartStep::StoppingSession, "Stopping PeakBot…"),
        ];
        for (step, title) in start_steps {
            let sv = status_view(&Phase::Starting(step));
            assert_eq!(sv.kind, "starting");
            assert_eq!(sv.title, title, "start-step title");
            assert!(sv.actions.is_empty(), "no actions while starting");
            assert_eq!(sv.detail, None);
        }

        // Ready.
        let tok = Token::parse("t").expect("parses");
        let sv = status_view(&Phase::Ready(Mode::Session {
            port: 7824,
            token: tok,
        }));
        assert_eq!(sv.kind, "ready");
        assert_eq!(sv.title, "Opening PeakBot…");
        assert!(sv.actions.is_empty());
        let sv = status_view(&Phase::Ready(Mode::Service { token: None }));
        assert_eq!(sv.kind, "ready");
        assert_eq!(sv.title, "Opening PeakBot…");
        assert!(sv.actions.is_empty());

        // Failed rows.
        let failed = |e: StartError| status_view(&Phase::Failed(e));

        let sv = failed(StartError::SidecarMissing {
            path: PathBuf::from("/app/peakbot"),
        });
        assert_eq!(sv.kind, "failed");
        assert_eq!(sv.title, "The PeakBot backend is missing from this app");
        assert_eq!(sv.actions, vec![UiAction::Quit]);
        assert_eq!(
            sv.detail.as_deref(),
            Some("/app/peakbot"),
            "detail is the path"
        );

        let sv = failed(StartError::SidecarBroken {
            detail: "boom".into(),
        });
        assert_eq!(sv.title, "The bundled PeakBot backend failed to run");
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::OpenLogs, UiAction::Quit]
        );

        let sv = failed(StartError::PortUnavailable);
        assert_eq!(sv.title, "Couldn't find a free local port");
        assert_eq!(sv.actions, vec![UiAction::Retry, UiAction::Quit]);

        let sv = failed(StartError::SessionExited {
            code: Some(1),
            log_tail: "tail-line".into(),
        });
        assert_eq!(sv.title, "PeakBot stopped unexpectedly");
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::OpenLogs, UiAction::Quit]
        );
        let detail = sv.detail.expect("SessionExited detail");
        assert!(
            detail.contains("1"),
            "detail must carry the exit code; {detail:?}"
        );
        assert!(
            detail.contains("tail-line"),
            "detail must carry the log tail; {detail:?}"
        );

        let sv = failed(StartError::SessionExited {
            code: None,
            log_tail: String::new(),
        });
        assert!(
            sv.detail.as_deref().is_some_and(|d| d.contains("signal")),
            "signal exit must say so; {:?}",
            sv.detail
        );

        let sv = failed(StartError::NotReady {
            waited: Duration::from_secs(30),
        });
        assert_eq!(sv.title, "PeakBot didn't start within 30 s");
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::OpenLogs, UiAction::Quit]
        );

        let sv = failed(StartError::TokenRejected { service: false });
        assert_eq!(sv.title, "Another program answered on PeakBot's port");
        assert_eq!(sv.actions, vec![UiAction::Retry, UiAction::Quit]);

        let sv = failed(StartError::TokenRejected { service: true });
        assert_eq!(sv.title, "The background service rejected the access token");
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::ReinstallService, UiAction::Quit]
        );
        assert!(
            sv.detail.is_some(),
            "service-mode rejection carries the service log hint"
        );

        let sv = failed(StartError::ServiceUnreachable {
            waited: Duration::from_secs(30),
        });
        assert_eq!(
            sv.title,
            "The background PeakBot service isn't responding on 127.0.0.1:7823"
        );
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::ReinstallService, UiAction::Quit]
        );
        assert!(
            sv.detail.is_some(),
            "ServiceUnreachable carries the service log hint"
        );

        let sv = failed(StartError::ServiceCommandFailed {
            command: "peakbot service install".into(),
            output: "boom".into(),
        });
        assert_eq!(sv.title, "A background-service command failed");
        assert_eq!(
            sv.actions,
            vec![
                UiAction::Retry,
                UiAction::ReinstallService,
                UiAction::OpenLogs,
                UiAction::Quit
            ]
        );
        assert!(
            sv.detail
                .as_deref()
                .is_some_and(|d| d.contains("$ peakbot service install")),
            "detail is `$ <command>` + output; {:?}",
            sv.detail
        );
    }

    // §9.1 case 31.
    #[test]
    fn t31_tray_model() {
        let v = Version::parse("0.23.0").unwrap();
        let tok = Token::parse("t").expect("parses");
        let ready_session = Phase::Ready(Mode::Session {
            port: 7824,
            token: tok,
        });
        let ready_service = Phase::Ready(Mode::Service { token: None });
        let installed = ServiceState::Installed(InstalledService {
            exe: None,
            run: RunState::Running,
        });

        // Starting → background disabled.
        let t = tray_model(
            &Phase::Starting(StartStep::CheckingBundled),
            Some(&ServiceState::NotInstalled),
            Some(&v),
        );
        assert_eq!(t.status, "PeakBot 0.23.0 · starting…");
        assert!(!t.background_checked);
        assert!(!t.background_enabled);
        assert_eq!(t.background_label, "Keep PeakBot running in background");

        // Ready(Session) + NotInstalled → unchecked, enabled.
        let t = tray_model(&ready_session, Some(&ServiceState::NotInstalled), Some(&v));
        assert_eq!(t.status, "PeakBot 0.23.0 · running (port 7824)");
        assert!(!t.background_checked);
        assert!(t.background_enabled);

        // Ready(Service) + Installed → checked, enabled.
        let t = tray_model(&ready_service, Some(&installed), Some(&v));
        assert_eq!(t.status, "PeakBot 0.23.0 · running in background");
        assert!(t.background_checked);
        assert!(t.background_enabled);

        // Failed + Installed → checked, enabled.
        let t = tray_model(
            &Phase::Failed(StartError::PortUnavailable),
            Some(&installed),
            Some(&v),
        );
        assert_eq!(t.status, "PeakBot 0.23.0 · error");
        assert!(t.background_checked);
        assert!(t.background_enabled);

        // Unavailable → label suffix + disabled.
        let unavail = ServiceState::Unavailable {
            reason: "no bus".into(),
        };
        let t = tray_model(&ready_session, Some(&unavail), Some(&v));
        assert_eq!(
            t.background_label,
            "Keep PeakBot running in background (unavailable)"
        );
        assert!(!t.background_enabled);

        // service None → disabled.
        let t = tray_model(&ready_service, None, Some(&v));
        assert!(!t.background_enabled);

        // bundled None → `?`.
        let t = tray_model(&Phase::Starting(StartStep::CheckingBundled), None, None);
        assert_eq!(t.status, "PeakBot ? · starting…");
    }

    // §9.1 case 32.
    #[test]
    fn t32_ui_action_serde_wire_names() {
        for (wire, action) in [
            ("retry", UiAction::Retry),
            ("open_logs", UiAction::OpenLogs),
            ("reinstall_service", UiAction::ReinstallService),
            ("quit", UiAction::Quit),
        ] {
            let d: UiAction = serde_json::from_str(&format!("\"{wire}\""))
                .unwrap_or_else(|e| panic!("{wire}: {e}"));
            assert_eq!(d, action);
            assert_eq!(
                serde_json::to_string(&action).unwrap(),
                format!("\"{wire}\"")
            );
        }
        assert!(
            serde_json::from_str::<UiAction>("\"rm_rf\"").is_err(),
            "unknown actions must not deserialize"
        );
    }
}
