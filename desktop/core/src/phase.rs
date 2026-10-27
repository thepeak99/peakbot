//! Phase, errors, views (DESIGN §3.5). The locked strings the status page
//! and tray render live in the `t30`/`t31` tests.

use std::path::PathBuf;
use std::time::Duration;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::web_target::WebTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStep {
    CheckingBundled,
    CheckingService,
    StartingSession,
    WaitingForBackend,
    /// The system prompt to trust PeakBot's CA (macOS keychain /
    /// Windows certificate store) — the one start step with a `detail`.
    TrustingCertificate,
    RemovingService,
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
    TokenRejected,
    /// The user declined removing the old background service (§4.3); it
    /// would conflict with the session, so the start stops here.
    ServiceInstalled,
    ServiceCommandFailed {
        command: String,
        output: String,
    },
    /// PeakBot's CA is not in the user's trusted certificates (macOS /
    /// Windows). `detail` carries the failed `add` command + output.
    CertificateNotTrusted {
        detail: String,
    },
}

/// `Cancelled` is NOT a `StartError` (§3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Starting(StartStep),
    Ready(WebTarget),
    Failed(StartError),
}

/// The status page's button targets (DESIGN §3.5). `snake_case` wire form
/// for the `act` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    Retry,
    OpenLogs,
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
        StartStep::TrustingCertificate => "Approve PeakBot's certificate in the system prompt…",
        StartStep::RemovingService => "Removing the background service…",
        StartStep::StoppingSession => "Stopping PeakBot…",
    }
}

pub fn status_view(phase: &Phase) -> StatusView {
    match phase {
        Phase::Starting(step) => {
            let title = step_text(*step).to_string();
            // The one start step with a detail: the CA-trust prompt
            // explains WHY the window is paused (design #376b §3).
            let detail = match step {
                StartStep::TrustingCertificate => Some(
                    "PeakBot serves HTTPS with its own certificate authority (\"PeakBot Local CA\"). Approving adds it to this user account's trusted certificates so this window can open PeakBot."
                        .to_string(),
                ),
                _ => None,
            };
            StatusView {
                kind: "starting",
                title,
                detail,
                actions: Vec::new(),
            }
        }
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
        StartError::TokenRejected => StatusView {
            kind: "failed",
            title: "The backend rejected the access token".into(),
            detail: None,
            actions: vec![UiAction::Retry, UiAction::Quit],
        },
        StartError::ServiceInstalled => StatusView {
            kind: "failed",
            title: "The old PeakBot background service is still installed".into(),
            detail: Some(
                "PeakBot Desktop can't start PeakBot while it is installed. Press Retry to be asked again, or remove it yourself:\n\npeakbot service uninstall"
                    .into(),
            ),
            actions: vec![UiAction::Retry, UiAction::Quit],
        },
        StartError::ServiceCommandFailed { command, output } => StatusView {
            kind: "failed",
            title: "A background-service command failed".into(),
            detail: Some(format!("$ {command}\n{output}")),
            actions: retry_logs_quit,
        },
        StartError::CertificateNotTrusted { detail } => StatusView {
            kind: "failed",
            title: "PeakBot's HTTPS certificate isn't trusted".into(),
            detail: Some(format!(
                "This window opens PeakBot over HTTPS (general.web.tls: true) and needs PeakBot's certificate authority in your trusted certificates. Press Retry to be asked again, or set general.web.tls: false in config.yaml and press Retry.\n\n{detail}"
            )),
            actions: vec![UiAction::Retry, UiAction::Quit],
        },
    }
}

/// The host platform the app runs on. The glue decides it ONCE in
/// `host_platform` (windows / macOS / everything else) and the pure
/// core derives its per-platform behaviour from it — so the decision is
/// unit-testable on every host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// No service migration check: the service never worked on Windows
    /// (Q1), so `service status` is never run.
    Windows,
    /// Linux: the webview trusts the CA itself (design #377b) — no
    /// system-trust step is ever run.
    Linux,
    /// macOS: the CA goes into the user's login keychain.
    MacOs,
}

/// The single place the compile target becomes a `Platform`: the Tauri
/// glue calls it once and passes the result into the pure core
/// (`Controller::new`) — never a bare `cfg!`
/// inside core. CA-trust design §2: Windows ⇒ `Windows`, macOS ⇒
/// `MacOs`, everything else ⇒ `Linux`.
pub fn host_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    }
}

/// The tray's disabled first item (§B).
pub fn tray_status(phase: &Phase, bundled: Option<&Version>) -> String {
    let v = bundled
        .map(Version::to_string)
        .unwrap_or_else(|| "?".to_string());
    match phase {
        Phase::Starting(_) => format!("PeakBot {v} · starting…"),
        Phase::Ready(target) => format!("PeakBot {v} · running (port {})", target.bind.port()),
        Phase::Failed(_) => format!("PeakBot {v} · error"),
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

/// Shown on ✕ when no tray host can bring the window back (§B): closing
/// then quits, which stops the server.
pub fn close_app_dialog() -> Dialog {
    Dialog {
        title: "Close PeakBot?".into(),
        body: "Closing PeakBot will stop the server and any running jobs. Close anyway?".into(),
        ok: "Close PeakBot".into(),
        cancel: Some("Cancel".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn target(bind: &str, tls: bool, token: Option<Token>) -> WebTarget {
        WebTarget {
            bind: bind.parse().expect("test bind parses"),
            tls,
            token,
        }
    }

    // §9.1 case 30 — one case per row of the §3.5 locked table
    // (close-to-tray plan: the service-mode rows are gone — no
    // `InstallingService`/`ConnectingService` steps, no
    // `ServiceUnreachable`, no `ReinstallService` action;
    // `TokenRejected` is a unit variant; the new `ServiceInstalled`
    // declined view has its locked strings).
    #[test]
    fn t30_status_view_titles_and_actions_per_locked_table() {
        // Starting — exact step texts (the seven surviving steps;
        // TrustingCertificate, the one with a detail, is checked below).
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
                StartStep::RemovingService,
                "Removing the background service…",
            ),
            (StartStep::StoppingSession, "Stopping PeakBot…"),
        ];
        for (step, title) in start_steps {
            let sv = status_view(&Phase::Starting(step));
            assert_eq!(sv.kind, "starting");
            assert_eq!(sv.title, title, "start-step title");
            assert!(sv.actions.is_empty(), "no actions while starting");
            assert_eq!(
                sv.detail, None,
                "every start step except TrustingCertificate keeps detail None"
            );
        }

        // TrustingCertificate is the ONE start step with a detail.
        let sv = status_view(&Phase::Starting(StartStep::TrustingCertificate));
        assert_eq!(sv.kind, "starting");
        assert_eq!(
            sv.title, "Approve PeakBot's certificate in the system prompt…",
            "the ellipsis is U+2026, matching the other step titles"
        );
        assert_eq!(
            sv.detail.as_deref(),
            Some(
                "PeakBot serves HTTPS with its own certificate authority (\"PeakBot Local CA\"). Approving adds it to this user account's trusted certificates so this window can open PeakBot."
            )
        );
        assert!(sv.actions.is_empty(), "no actions while starting");

        // Ready(WebTarget) — the service mode is gone, there is one shape.
        let tok = Token::parse("t").expect("parses");
        let sv = status_view(&Phase::Ready(target("127.0.0.1:7824", false, Some(tok))));
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

        // TokenRejected is a unit variant: one locked title, one action
        // row (no service-mode twin, no `ReinstallService`).
        let sv = failed(StartError::TokenRejected);
        assert_eq!(sv.title, "The backend rejected the access token");
        assert_eq!(sv.actions, vec![UiAction::Retry, UiAction::Quit]);

        // The declined-migration view (§B): the exact locked title,
        // detail and [Retry, Quit] actions.
        let sv = failed(StartError::ServiceInstalled);
        assert_eq!(sv.kind, "failed");
        assert_eq!(
            sv.title, "The old PeakBot background service is still installed",
            "the exact locked title"
        );
        assert_eq!(
            sv.detail.as_deref(),
            Some(
                "PeakBot Desktop can't start PeakBot while it is installed. Press Retry to be asked again, or remove it yourself:\n\npeakbot service uninstall"
            ),
            "the exact locked detail"
        );
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::Quit],
            "declined view: [Retry, Quit] — no OpenLogs, no ReinstallService"
        );

        let sv = failed(StartError::ServiceCommandFailed {
            command: "service uninstall".into(),
            output: "boom".into(),
        });
        assert_eq!(sv.title, "A background-service command failed");
        assert_eq!(
            sv.actions,
            vec![UiAction::Retry, UiAction::OpenLogs, UiAction::Quit],
            "ServiceCommandFailed: [Retry, OpenLogs, Quit] — no ReinstallService"
        );
        assert!(
            sv.detail
                .as_deref()
                .is_some_and(|d| d.contains("$ service uninstall")),
            "detail is `$ <command>` + output; {:?}",
            sv.detail
        );

        let sv = failed(StartError::CertificateNotTrusted {
            detail: "macOS refused the certificate".into(),
        });
        assert_eq!(sv.kind, "failed");
        assert_eq!(
            sv.title, "PeakBot's HTTPS certificate isn't trusted",
            "the exact locked title"
        );
        assert_eq!(
            sv.detail.as_deref(),
            Some(
                "This window opens PeakBot over HTTPS (general.web.tls: true) and needs PeakBot's certificate authority in your trusted certificates. Press Retry to be asked again, or set general.web.tls: false in config.yaml and press Retry.\n\nmacOS refused the certificate"
            ),
            "detail is the locked prefix + the raw failure detail"
        );
        assert_eq!(sv.actions, vec![UiAction::Retry, UiAction::Quit]);
    }

    // CA-trust design §2: the compile target decides the Platform once.
    // On Linux CI this asserts `host_platform() == Platform::Linux`.
    #[test]
    fn host_platform_follows_the_build_target() {
        let expected = if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        };
        assert_eq!(
            host_platform(),
            expected,
            "host_platform must mirror the build target (Linux CI ⇒ Linux)"
        );
    }

    // §B: the tray's disabled first item, per phase. `TrayModel` is gone —
    // `tray_status` returns the String the tray applies.
    #[test]
    fn t31_tray_status() {
        let v = Version::parse("0.23.0").unwrap();
        let tok = Token::parse("t").expect("parses");
        let ready = Phase::Ready(target("127.0.0.1:7824", false, Some(tok)));

        assert_eq!(
            tray_status(&Phase::Starting(StartStep::CheckingBundled), Some(&v)),
            "PeakBot 0.23.0 · starting…"
        );
        assert_eq!(
            tray_status(&Phase::Starting(StartStep::RemovingService), Some(&v)),
            "PeakBot 0.23.0 · starting…"
        );
        assert_eq!(
            tray_status(&ready, Some(&v)),
            "PeakBot 0.23.0 · running (port 7824)"
        );
        assert_eq!(
            tray_status(
                &Phase::Failed(StartError::NotReady {
                    waited: Duration::from_secs(30)
                }),
                Some(&v)
            ),
            "PeakBot 0.23.0 · error"
        );

        // bundled None → `?`.
        assert_eq!(
            tray_status(&Phase::Starting(StartStep::CheckingBundled), None),
            "PeakBot ? · starting…"
        );
        assert_eq!(tray_status(&ready, None), "PeakBot ? · running (port 7824)");
        assert_eq!(
            tray_status(&Phase::Failed(StartError::ServiceInstalled), None),
            "PeakBot ? · error"
        );
    }

    // §C: `UiAction` lost `ReinstallService` and `Back` — exactly
    // retry / open_logs / quit remain, and nothing else deserializes.
    #[test]
    fn t32_ui_action_serde_wire_names() {
        for (wire, action) in [
            ("retry", UiAction::Retry),
            ("open_logs", UiAction::OpenLogs),
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
        for gone in ["reinstall_service", "back", "toggle_background", "rm_rf"] {
            assert!(
                serde_json::from_str::<UiAction>(&format!("\"{gone}\"")).is_err(),
                "\"{gone}\" must not deserialize (the action was removed)"
            );
        }
    }

    // §B: the close-without-tray dialog's locked strings.
    #[test]
    fn close_app_dialog_locked_strings() {
        let d = close_app_dialog();
        assert_eq!(d.title, "Close PeakBot?");
        assert_eq!(
            d.body,
            "Closing PeakBot will stop the server and any running jobs. Close anyway?"
        );
        assert_eq!(d.ok, "Close PeakBot");
        assert_eq!(d.cancel.as_deref(), Some("Cancel"));
    }
}
