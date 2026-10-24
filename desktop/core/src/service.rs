//! Service state, launch planning, dialogs (DESIGN §3.4/§4.4/§5.2).

use std::path::{Path, PathBuf};

use semver::Version;

use crate::phase::Dialog;
use crate::settings::{Protocol, Settings};
use crate::sys::RunOutput;
use crate::version::{DeclinedReplace, InstalledVersion};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledService {
    pub exe: Option<PathBuf>,
    pub run: RunState,
    /// The `TLS:` status line (added by #375) — the service's protocol
    /// source (invariant I3): `on` ⇒ https.
    pub tls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// `service status` exited non-zero.
    Unavailable {
        reason: String,
    },
    NotInstalled,
    Installed(InstalledService),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacePolicy {
    /// Replace silently unless it would be a downgrade.
    Auto,
    /// Always ask (dev builds — a debug sidecar must never silently
    /// overwrite the user's ~/.local/bin/peakbot).
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlan {
    Session,
    /// The installed exe is byte-identical to the bundled sidecar, or the
    /// user declined this exact (installed, bundled) pair.
    /// `start_first ⇔ run == Stopped`.
    ConnectService {
        start_first: bool,
    },
    /// The builds differ, it is not a downgrade, and the policy is Auto:
    /// replace the binary without asking, reproducing the running
    /// service's protocol and token.
    AutoReplace,
    /// The builds differ, and (policy is Ask, or installed > bundled), and
    /// the pair was not declined.
    AskReplace {
        installed: InstalledVersion,
    },
}

pub fn status_args() -> [&'static str; 2] {
    ["service", "status"]
}

/// `["service","install"]` — bare, no `--bind`, no `--tls` (spec §8.4:
/// the unit reads `general.web` at every start, so a Settings edit plus
/// a restart applies). The token never appears in argv (I7).
pub fn service_install_args() -> Vec<String> {
    vec!["service".to_string(), "install".to_string()]
}

pub fn service_uninstall_args() -> [&'static str; 2] {
    ["service", "uninstall"]
}

pub fn install_args() -> [&'static str; 1] {
    ["install"]
}

/// Parse the stdout of `peakbot service status` (the B4-pinned text
/// contract, DESIGN §5.2).
pub fn parse_service_status(out: &RunOutput) -> ServiceState {
    if !out.ok() {
        // Non-zero exit: reason = first non-empty line of stderr, else
        // stdout, else the fixed fallback.
        let reason = out
            .stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .or_else(|| out.stdout.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("service status failed")
            .to_string();
        return ServiceState::Unavailable { reason };
    }
    let mut installed: Option<bool> = None;
    let mut exe: Option<PathBuf> = None;
    let mut run: Option<RunState> = None;
    let mut tls = false;
    for line in out.stdout.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Installed:") {
            installed = Some(v.trim() == "true");
        } else if let Some(v) = line.strip_prefix("Exec:") {
            let p = v.trim();
            if !p.is_empty() {
                exe = Some(PathBuf::from(p));
            }
        } else if let Some(v) = line.strip_prefix("State:") {
            run = Some(match v.trim() {
                "running" => RunState::Running,
                "stopped" => RunState::Stopped,
                // `unknown` or anything unrecognised.
                _ => RunState::Unknown,
            });
        } else if let Some(v) = line.strip_prefix("TLS:") {
            tls = v.trim() == "on";
        }
    }
    match installed {
        Some(true) => ServiceState::Installed(InstalledService {
            exe,
            run: run.unwrap_or(RunState::Unknown),
            tls,
        }),
        Some(false) => ServiceState::NotInstalled,
        None => ServiceState::Unavailable {
            reason: "unrecognised service status output".into(),
        },
    }
}

/// The §4.2 truth table (design auto-replace; replaces DESIGN §4.4).
/// `identical` = the installed exe's bytes equal the bundled sidecar's
/// (A6: "cannot compare" is false); it runs BEFORE `version_of`, and
/// neither runs for NotInstalled/Unavailable (A3). `newer` = the
/// installed version is Known and above the bundled one (A2).
pub fn plan_launch(
    state: &ServiceState,
    identical: impl FnOnce(&InstalledService) -> bool,
    version_of: impl FnOnce(&InstalledService) -> InstalledVersion,
    bundled: &Version,
    declined: Option<&DeclinedReplace>,
    policy: ReplacePolicy,
) -> LaunchPlan {
    let ServiceState::Installed(svc) = state else {
        // Unavailable / NotInstalled ⇒ session; identical/version_of are
        // not called.
        return LaunchPlan::Session;
    };
    let start_first = svc.run == RunState::Stopped;
    if identical(svc) {
        return LaunchPlan::ConnectService { start_first };
    }
    let installed = version_of(svc);
    let newer = matches!(&installed, InstalledVersion::Known(v) if v > bundled);
    if policy == ReplacePolicy::Auto && !newer {
        return LaunchPlan::AutoReplace;
    }
    if declined.is_some_and(|d| d.matches(&installed, bundled)) {
        return LaunchPlan::ConnectService { start_first };
    }
    LaunchPlan::AskReplace { installed }
}

/// The settings that reproduce the RUNNING service for a silent replace
/// (A4). Protocol from the `TLS:` line (I3); Remote only if saved is
/// Remote AND the service runs TLS (I1). The loopback `token` flag is
/// carried from `saved` but is not used by `service_install_args`. The
/// token itself lives in `web-token`, which the silent path never
/// touches.
pub fn running_settings(saved: Settings, tls: bool) -> Settings {
    match (saved, tls) {
        (Settings::Remote, true) => Settings::Remote,
        (Settings::Remote, false) => Settings::Loopback {
            protocol: Protocol::Http,
            token: true,
        },
        (Settings::Loopback { token, .. }, tls) => Settings::Loopback {
            protocol: if tls { Protocol::Https } else { Protocol::Http },
            token,
        },
    }
}

pub fn replace_dialog(installed: &InstalledVersion, bundled: &Version) -> Dialog {
    let i = match installed {
        InstalledVersion::Known(v) => v.to_string(),
        InstalledVersion::Unknown => "an unknown version".to_string(),
    };
    let mut body = format!(
        "A PeakBot background service ({i}) is already installed. \
         This app includes PeakBot {bundled}.\n\n\
         Replace the service with {bundled}? This also replaces \
         ~/.local/bin/peakbot (your \"peakbot\" command)."
    );
    if let InstalledVersion::Known(v) = installed
        && v > bundled
    {
        body.push_str(&format!(
            "\n\nWarning: {bundled} is OLDER than the installed {v}."
        ));
    }
    let cancel = match installed {
        InstalledVersion::Known(v) => format!("Keep {v}"),
        InstalledVersion::Unknown => "Keep current".to_string(),
    };
    Dialog {
        title: "Replace PeakBot background service?".into(),
        body,
        ok: "Replace".into(),
        cancel: Some(cancel),
    }
}

pub fn enable_dialog() -> Dialog {
    Dialog {
        title: "Keep PeakBot running in background?".into(),
        body: "PeakBot will be installed to ~/.local/bin/peakbot and start automatically when \
               you log in. It keeps running after you close this window."
            .into(),
        ok: "Enable".into(),
        cancel: Some("Cancel".into()),
    }
}

pub fn disable_dialog() -> Dialog {
    Dialog {
        title: "Remove background service?".into(),
        body: "PeakBot will stop running in the background and will only run while this window \
               is open. ~/.local/bin/peakbot and your settings are kept."
            .into(),
        ok: "Remove".into(),
        cancel: Some("Cancel".into()),
    }
}

pub fn finish_setup_dialog() -> Dialog {
    Dialog {
        title: "Finish setup first".into(),
        body: "Complete the PeakBot setup in this window, then enable background mode.".into(),
        ok: "OK".into(),
        cancel: None,
    }
}

pub fn port_busy_dialog() -> Dialog {
    Dialog {
        title: "Port 7823 is busy".into(),
        body: "Another program (maybe a PeakBot started from a terminal) is using \
               127.0.0.1:7823. Stop it and try again."
            .into(),
        ok: "OK".into(),
        cancel: None,
    }
}

/// `Action::SaveSettings` confirm — session mode (design #377 §5.1).
pub fn settings_relaunch_dialog() -> Dialog {
    Dialog {
        title: "Apply settings?".into(),
        body: "PeakBot will restart with the new settings. \
               Conversations that are running now will be interrupted."
            .into(),
        ok: "Restart".into(),
        cancel: Some("Cancel".into()),
    }
}

/// `Action::SaveSettings` confirm — service mode (design #377 §5.1).
pub fn settings_reinstall_dialog() -> Dialog {
    Dialog {
        title: "Apply settings?".into(),
        body: "The background service will be reinstalled with the new settings \
               and restarted. Conversations that are running now will be interrupted."
            .into(),
        ok: "Reinstall".into(),
        cancel: Some("Cancel".into()),
    }
}

/// The settings file write failed (design #377 §5.1); nothing was applied.
pub fn save_failed_dialog(path: &Path, err: &str) -> Dialog {
    Dialog {
        title: "Couldn't save settings".into(),
        body: format!("{}\n{}", path.display(), err),
        ok: "OK".into(),
        cancel: None,
    }
}

pub fn command_failed_dialog(command: &str, output: &str) -> Dialog {
    Dialog {
        title: "Background service error".into(),
        body: format!("$ {command}\n{output}"),
        ok: "OK".into(),
        cancel: None,
    }
}

/// Per target_os: Linux `journalctl --user -u peakbot.service`,
/// macOS `~/Library/Logs/peakbot.log`.
pub fn service_log_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "~/Library/Logs/peakbot.log"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "journalctl --user -u peakbot.service"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn run_out(code: Option<i32>, stdout: &str, stderr: &str) -> RunOutput {
        RunOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    // §9.1 case 7 (updated for #377: `InstalledService` carries `tls`).
    #[test]
    fn t07_parse_service_status_installed_running_sample() {
        let out = run_out(
            Some(0),
            "Installed: true\nExec:      /home/u/.local/bin/peakbot\nState:     running\n",
            "",
        );
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: Some(PathBuf::from("/home/u/.local/bin/peakbot")),
                run: RunState::Running,
                tls: false,
            })
        );
    }

    // §9.1 case 8 (updated for #377: `InstalledService` carries `tls`).
    #[test]
    fn t08_exec_path_with_spaces_and_missing_exec() {
        let out = run_out(
            Some(0),
            "Installed: true\nExec:      /home/user with space/.local/bin/peakbot\nState:     running\n",
            "",
        );
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: Some(PathBuf::from("/home/user with space/.local/bin/peakbot")),
                run: RunState::Running,
                tls: false,
            }),
            "a space in the Exec path must be preserved"
        );

        let out = run_out(Some(0), "Installed: true\nState:     running\n", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Running,
                tls: false,
            })
        );
    }

    // §9.1 case 9 (updated for #377: `InstalledService` carries `tls`).
    #[test]
    fn t09_state_words() {
        let base = |state: &str| run_out(Some(0), &format!("Installed: true\n{state}\n"), "");
        assert_eq!(
            parse_service_status(&base("State:     stopped")),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Stopped,
                tls: false,
            })
        );
        assert_eq!(
            parse_service_status(&base("State:     unknown")),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Unknown,
                tls: false,
            })
        );
        // Missing State line → Unknown.
        let out = run_out(Some(0), "Installed: true\n", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Unknown,
                tls: false,
            })
        );
    }

    // §9.1 case 10.
    #[test]
    fn t10_installed_false_is_not_installed() {
        let out = run_out(Some(0), "Installed: false\n", "");
        assert_eq!(parse_service_status(&out), ServiceState::NotInstalled);
    }

    // §9.1 case 11.
    #[test]
    fn t11_nonzero_exit_is_unavailable_with_first_stderr_line() {
        let out = run_out(
            Some(1),
            "",
            "service status failed: systemctl --user not available\nsecond line\n",
        );
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Unavailable {
                reason: "service status failed: systemctl --user not available".into(),
            }
        );

        let out = run_out(Some(1), "", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Unavailable {
                reason: "service status failed".into(),
            },
            "no output ⇒ the fixed fallback reason"
        );
    }

    // §9.1 case 12.
    #[test]
    fn t12_exit_zero_without_installed_line_is_unavailable() {
        let out = run_out(Some(0), "Manager:   systemd-user\n", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Unavailable {
                reason: "unrecognised service status output".into(),
            }
        );
    }

    fn bundled() -> Version {
        Version::parse("0.23.0").unwrap()
    }

    // Design auto-replace §4.2 — every row of the truth table (t13,
    // rewritten). `identical` is the byte-equality check; it runs
    // before the version, and only for Installed.
    #[test]
    fn t13_plan_launch_truth_table() {
        let b = bundled();
        let v22 = InstalledVersion::Known(Version::parse("0.22.0").unwrap());
        let v24 = InstalledVersion::Known(Version::parse("0.24.0").unwrap());
        let ident = |same: bool| move |_s: &InstalledService| same;
        let vo = |i: InstalledVersion| move |_s: &InstalledService| i;
        let svc = |run: RunState| InstalledService {
            exe: Some(PathBuf::from("/x/peakbot")),
            run,
            tls: false,
        };

        // Row 1: Unavailable → Session (both policies).
        for policy in [ReplacePolicy::Auto, ReplacePolicy::Ask] {
            assert_eq!(
                plan_launch(
                    &ServiceState::Unavailable { reason: "r".into() },
                    ident(true),
                    vo(InstalledVersion::Unknown),
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::Session,
                "row 1 (policy {policy:?})"
            );
        }
        // Row 2: NotInstalled → Session (both policies).
        for policy in [ReplacePolicy::Auto, ReplacePolicy::Ask] {
            assert_eq!(
                plan_launch(
                    &ServiceState::NotInstalled,
                    ident(true),
                    vo(InstalledVersion::Unknown),
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::Session,
                "row 2 (policy {policy:?})"
            );
        }
        // Row 3: identical → ConnectService{start_first: run==Stopped},
        // both policies, all three run states (Unknown counts as running).
        for policy in [ReplacePolicy::Auto, ReplacePolicy::Ask] {
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(RunState::Running)),
                    ident(true),
                    vo(InstalledVersion::Unknown),
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::ConnectService { start_first: false },
                "row 3 Running (policy {policy:?})"
            );
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(RunState::Stopped)),
                    ident(true),
                    vo(InstalledVersion::Unknown),
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::ConnectService { start_first: true },
                "row 3 Stopped (policy {policy:?})"
            );
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(RunState::Unknown)),
                    ident(true),
                    vo(InstalledVersion::Unknown),
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::ConnectService { start_first: false },
                "row 3 Unknown (policy {policy:?})"
            );
        }
        // Row 4: not identical, Known == bundled, Auto → AutoReplace.
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "row 4: same version, different build ⇒ silent replace"
        );
        // Row 5: not identical, Known < bundled, Auto → AutoReplace.
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(v22.clone()),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "row 5: older installed ⇒ silent replace"
        );
        // Row 6: not identical, Unknown, Auto → AutoReplace.
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(InstalledVersion::Unknown),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "row 6: unknown version ⇒ silent replace"
        );
        // Row 7: not identical, Known > bundled, declined pair matches,
        // Auto → ConnectService, all three run states.
        let declined_24 = DeclinedReplace {
            installed: v24.clone(),
            bundled: b.clone(),
        };
        for run in [RunState::Running, RunState::Stopped, RunState::Unknown] {
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(run)),
                    ident(false),
                    vo(v24.clone()),
                    &b,
                    Some(&declined_24),
                    ReplacePolicy::Auto
                ),
                LaunchPlan::ConnectService {
                    start_first: run == RunState::Stopped
                },
                "row 7 {run:?}: newer + declined ⇒ connect"
            );
        }
        // Row 8: not identical, Known > bundled, no decline, Auto →
        // AskReplace (a downgrade of the user's CLI is never silent).
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(v24.clone()),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AskReplace {
                installed: v24.clone()
            },
            "row 8: newer installed ⇒ the OLDER dialog"
        );
        // Row 9: not identical, any version, declined pair matches, Ask
        // → ConnectService, all three run states.
        let declined_22 = DeclinedReplace {
            installed: v22.clone(),
            bundled: b.clone(),
        };
        for run in [RunState::Running, RunState::Stopped, RunState::Unknown] {
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(run)),
                    ident(false),
                    vo(v22.clone()),
                    &b,
                    Some(&declined_22),
                    ReplacePolicy::Ask
                ),
                LaunchPlan::ConnectService {
                    start_first: run == RunState::Stopped
                },
                "row 9 {run:?}: declined ⇒ connect (Ask)"
            );
        }
        // Row 10: not identical, any version, no decline, Ask →
        // AskReplace (dev builds keep the prompt).
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(v22.clone()),
                &b,
                None,
                ReplacePolicy::Ask
            ),
            LaunchPlan::AskReplace {
                installed: v22.clone()
            },
            "row 10: Ask + Known ⇒ dialog"
        );
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc(RunState::Running)),
                ident(false),
                vo(InstalledVersion::Unknown),
                &b,
                None,
                ReplacePolicy::Ask
            ),
            LaunchPlan::AskReplace {
                installed: InstalledVersion::Unknown
            },
            "row 10: Ask + Unknown ⇒ dialog"
        );
    }

    // Design auto-replace §7.1 (t14, updated): neither `identical` nor
    // `version_of` runs for NotInstalled/Unavailable (A3).
    #[test]
    fn t14_plan_launch_skips_identity_and_version_for_not_installed_and_unavailable() {
        let b = bundled();
        let panic_identical = |_s: &InstalledService| -> bool {
            panic!("identical must not be called for NotInstalled/Unavailable")
        };
        let panic_version_of = |_s: &InstalledService| -> InstalledVersion {
            panic!("version_of must not be called for NotInstalled/Unavailable")
        };
        for policy in [ReplacePolicy::Auto, ReplacePolicy::Ask] {
            assert_eq!(
                plan_launch(
                    &ServiceState::NotInstalled,
                    panic_identical,
                    panic_version_of,
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::Session,
                "NotInstalled (policy {policy:?})"
            );
            assert_eq!(
                plan_launch(
                    &ServiceState::Unavailable { reason: "r".into() },
                    panic_identical,
                    panic_version_of,
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::Session,
                "Unavailable (policy {policy:?})"
            );
        }
    }

    // Design auto-replace §7.1: an identical build goes straight to
    // ConnectService and `version_of` is never called (A3/T5) — both
    // policies.
    #[test]
    fn t103_identical_build_never_queries_version() {
        let b = bundled();
        let svc = |run: RunState| InstalledService {
            exe: Some(PathBuf::from("/x/peakbot")),
            run,
            tls: false,
        };
        let panic_version_of = |_s: &InstalledService| -> InstalledVersion {
            panic!("version_of must not run for an identical build")
        };
        for policy in [ReplacePolicy::Auto, ReplacePolicy::Ask] {
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(RunState::Running)),
                    |_| true,
                    panic_version_of,
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::ConnectService { start_first: false },
                "Running identical (policy {policy:?})"
            );
            assert_eq!(
                plan_launch(
                    &ServiceState::Installed(svc(RunState::Stopped)),
                    |_| true,
                    panic_version_of,
                    &b,
                    None,
                    policy
                ),
                LaunchPlan::ConnectService { start_first: true },
                "Stopped identical (policy {policy:?})"
            );
        }
    }

    // Design auto-replace §7.1: the Auto policy rows — a differing build
    // is replaced silently unless the installed version is newer (A2);
    // the declined pair only gates the dialog, never AutoReplace.
    #[test]
    fn t104_auto_policy_rows() {
        let b = bundled(); // 0.23.0
        let svc = InstalledService {
            exe: Some(PathBuf::from("/x/peakbot")),
            run: RunState::Running,
            tls: false,
        };
        let ident_false = |_s: &InstalledService| false;
        let vo = |i: InstalledVersion| move |_s: &InstalledService| i;
        let state = ServiceState::Installed(svc);

        // Known == bundled → AutoReplace.
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "same version, different build ⇒ silent"
        );
        // Known < bundled → AutoReplace.
        let v22 = InstalledVersion::Known(Version::parse("0.22.0").unwrap());
        assert_eq!(
            plan_launch(&state, ident_false, vo(v22), &b, None, ReplacePolicy::Auto),
            LaunchPlan::AutoReplace,
            "older installed ⇒ silent"
        );
        // A pre-release of the bundled version is NOT newer (semver
        // order) → AutoReplace.
        let v_rc = InstalledVersion::Known(Version::parse("0.23.0-rc.1").unwrap());
        assert_eq!(
            plan_launch(&state, ident_false, vo(v_rc), &b, None, ReplacePolicy::Auto),
            LaunchPlan::AutoReplace,
            "0.23.0-rc.1 < 0.23.0 ⇒ not a downgrade ⇒ silent"
        );
        // Unknown → AutoReplace.
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Unknown),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "unknown version ⇒ silent"
        );
        // Known > bundled (0.23.1) → AskReplace.
        let v231 = InstalledVersion::Known(Version::parse("0.23.1").unwrap());
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(v231.clone()),
                &b,
                None,
                ReplacePolicy::Auto
            ),
            LaunchPlan::AskReplace { installed: v231 },
            "a newer installed version is never silent (A2)"
        );
        // Known > bundled + declined pair matches → ConnectService.
        let v24 = InstalledVersion::Known(Version::parse("0.24.0").unwrap());
        let declined_24 = DeclinedReplace {
            installed: v24.clone(),
            bundled: b.clone(),
        };
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(v24),
                &b,
                Some(&declined_24),
                ReplacePolicy::Auto
            ),
            LaunchPlan::ConnectService { start_first: false },
            "newer + declined ⇒ connect without re-asking"
        );
        // Known == bundled + a declined (same, same) pair → STILL
        // AutoReplace: the memory only silences the dialog, it never
        // blocks the silent replace.
        let declined_same = DeclinedReplace {
            installed: InstalledVersion::Known(b.clone()),
            bundled: b.clone(),
        };
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Known(b.clone())),
                &b,
                Some(&declined_same),
                ReplacePolicy::Auto
            ),
            LaunchPlan::AutoReplace,
            "declined only gates the downgrade dialog"
        );
    }

    // Design auto-replace §7.1: the Ask policy (dev builds) keeps the
    // prompt on ANY build difference, whatever the version.
    #[test]
    fn t105_ask_policy_prompts_on_any_difference() {
        let b = bundled(); // 0.23.0
        let svc = InstalledService {
            exe: Some(PathBuf::from("/x/peakbot")),
            run: RunState::Running,
            tls: false,
        };
        let ident_false = |_s: &InstalledService| false;
        let vo = |i: InstalledVersion| move |_s: &InstalledService| i;
        let state = ServiceState::Installed(svc);

        // Same version, different build → AskReplace (dev builds never
        // silently overwrite ~/.local/bin/peakbot).
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None,
                ReplacePolicy::Ask
            ),
            LaunchPlan::AskReplace {
                installed: InstalledVersion::Known(b.clone())
            },
            "Ask + same version ⇒ dialog"
        );
        // Older → AskReplace.
        let v22 = InstalledVersion::Known(Version::parse("0.22.0").unwrap());
        assert_eq!(
            plan_launch(&state, ident_false, vo(v22), &b, None, ReplacePolicy::Ask),
            LaunchPlan::AskReplace {
                installed: InstalledVersion::Known(Version::parse("0.22.0").unwrap())
            },
            "Ask + older ⇒ dialog"
        );
        // Unknown → AskReplace{Unknown}.
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Unknown),
                &b,
                None,
                ReplacePolicy::Ask
            ),
            LaunchPlan::AskReplace {
                installed: InstalledVersion::Unknown
            },
            "Ask + unknown ⇒ dialog"
        );
        // Declined pair matches → ConnectService (the memory still works).
        let declined = DeclinedReplace {
            installed: InstalledVersion::Known(b.clone()),
            bundled: b.clone(),
        };
        assert_eq!(
            plan_launch(
                &state,
                ident_false,
                vo(InstalledVersion::Known(b.clone())),
                &b,
                Some(&declined),
                ReplacePolicy::Ask
            ),
            LaunchPlan::ConnectService { start_first: false },
            "Ask + declined ⇒ connect"
        );
    }

    // Design auto-replace §7.1: `running_settings` reproduces the
    // RUNNING service for a silent replace (A4/SC6): the protocol comes
    // from the TLS line, and Remote only survives when the service runs
    // TLS (I1: Remote ⇒ HTTPS).
    #[test]
    fn t106_running_settings_reproduces_service() {
        let cases: [(Settings, bool, Settings); 4] = [
            (
                Settings::Loopback {
                    protocol: Protocol::Http,
                    token: true,
                },
                true,
                Settings::Loopback {
                    protocol: Protocol::Https,
                    token: true,
                },
            ),
            (
                Settings::Loopback {
                    protocol: Protocol::Https,
                    token: false,
                },
                false,
                Settings::Loopback {
                    protocol: Protocol::Http,
                    token: false,
                },
            ),
            (Settings::Remote, true, Settings::Remote),
            (
                Settings::Remote,
                false,
                Settings::Loopback {
                    protocol: Protocol::Http,
                    token: true,
                },
            ),
        ];
        for (saved, tls, expected) in cases {
            assert_eq!(
                running_settings(saved, tls),
                expected,
                "saved {saved:?}, tls {tls}"
            );
        }
        // (T15a: the `service_install_args` argv assertions moved to
        // `service_install_args_bare` — the install is now bare.)
    }

    // §9.1 case 15.
    #[test]
    fn t15_replace_dialog_variants() {
        let b = Version::parse("0.23.0").unwrap();

        // Installed OLDER than bundled → no warning.
        let d = replace_dialog(
            &InstalledVersion::Known(Version::parse("0.22.0").unwrap()),
            &b,
        );
        assert_eq!(d.title, "Replace PeakBot background service?");
        assert!(
            !d.body.contains("OLDER"),
            "no warning when the installed version is older; {d:?}"
        );
        assert_eq!(d.ok, "Replace");
        assert_eq!(d.cancel.as_deref(), Some("Keep 0.22.0"));

        // Installed NEWER than bundled → OLDER warning.
        let d = replace_dialog(
            &InstalledVersion::Known(Version::parse("0.24.0").unwrap()),
            &b,
        );
        assert!(
            d.body.contains("OLDER"),
            "warning required when the bundled is older; {d:?}"
        );
        assert_eq!(d.cancel.as_deref(), Some("Keep 0.24.0"));

        // Unknown → "an unknown version" + "Keep current".
        let d = replace_dialog(&InstalledVersion::Unknown, &b);
        assert!(d.body.contains("an unknown version"), "{d:?}");
        assert_eq!(d.cancel.as_deref(), Some("Keep current"));
    }

    // §9.1 case 16 (T15a: `service_install_args` is now bare — see
    // `service_install_args_bare`).
    #[test]
    fn t16_arg_builders_are_exact() {
        assert_eq!(status_args(), ["service", "status"]);
        assert_eq!(service_uninstall_args(), ["service", "uninstall"]);
        assert_eq!(install_args(), ["install"]);
    }

    // T15a (0.24 config, spec §8.4): `service install` with NO web flags —
    // no `--bind`, no `--tls`. The unit reads `general.web` at every start,
    // so a Settings edit plus a restart applies. The token never appears in
    // argv (I7). ASSUMED new signature:
    // `pub fn service_install_args() -> Vec<String>` (the old `Settings`
    // form is deleted by T15b).
    #[test]
    fn service_install_args_bare() {
        assert_eq!(
            service_install_args(),
            vec!["service".to_string(), "install".to_string()],
            "bare `service install`; no --bind/--tls, token never in argv"
        );
    }

    // Design #377 t71: the `TLS:` status line (added by #375) is the
    // service's protocol source (invariant I3).
    #[test]
    fn t71_tls_status_line_parses() {
        let status = |tls: &str| {
            run_out(
                Some(0),
                &format!("Installed: true\nExec:      /x/peakbot\nState:     running\n{tls}\n"),
                "",
            )
        };
        assert_eq!(
            parse_service_status(&status("TLS:       on")),
            ServiceState::Installed(InstalledService {
                exe: Some(PathBuf::from("/x/peakbot")),
                run: RunState::Running,
                tls: true,
            }),
            "`TLS:       on` ⇒ tls: true"
        );
        assert_eq!(
            parse_service_status(&status("TLS:       off")),
            ServiceState::Installed(InstalledService {
                exe: Some(PathBuf::from("/x/peakbot")),
                run: RunState::Running,
                tls: false,
            }),
            "`TLS:       off` ⇒ tls: false"
        );
        // A pre-#375 status output has no TLS line at all.
        let out = run_out(
            Some(0),
            "Installed: true\nExec:      /x/peakbot\nState:     running\n",
            "",
        );
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: Some(PathBuf::from("/x/peakbot")),
                run: RunState::Running,
                tls: false,
            }),
            "missing TLS line ⇒ tls: false"
        );
    }

    // Design #377 t72: the three new SaveSettings dialogs, verbatim.
    #[test]
    fn t72_settings_dialog_strings_are_exact() {
        let d = settings_relaunch_dialog();
        assert_eq!(d.title, "Apply settings?");
        assert_eq!(
            d.body,
            "PeakBot will restart with the new settings. \
             Conversations that are running now will be interrupted."
        );
        assert_eq!(d.ok, "Restart");
        assert_eq!(d.cancel.as_deref(), Some("Cancel"));

        let d = settings_reinstall_dialog();
        assert_eq!(d.title, "Apply settings?");
        assert_eq!(
            d.body,
            "The background service will be reinstalled with the new settings \
             and restarted. Conversations that are running now will be interrupted."
        );
        assert_eq!(d.ok, "Reinstall");
        assert_eq!(d.cancel.as_deref(), Some("Cancel"));

        let d = save_failed_dialog(Path::new("/data/settings.json"), "disk full");
        assert_eq!(d.title, "Couldn't save settings");
        assert_eq!(d.body, "/data/settings.json\ndisk full");
        assert_eq!(d.ok, "OK");
        assert_eq!(d.cancel, None, "info dialog (no cancel)");
    }
}
