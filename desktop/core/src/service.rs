//! Service state, launch planning, dialogs (DESIGN §3.4/§4.4/§5.2).

use std::path::PathBuf;

use semver::Version;

use crate::phase::Dialog;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlan {
    Session,
    /// `start_first ⇔ run == Stopped`.
    ConnectService {
        start_first: bool,
    },
    /// Versions differ (or unknown) and not previously declined.
    AskReplace {
        installed: InstalledVersion,
    },
}

pub fn status_args() -> [&'static str; 2] {
    todo!()
}

pub fn service_install_args() -> [&'static str; 4] {
    todo!()
}

pub fn service_uninstall_args() -> [&'static str; 2] {
    todo!()
}

pub fn install_args() -> [&'static str; 1] {
    todo!()
}

/// Parse the stdout of `peakbot service status` (the B4-pinned text
/// contract, DESIGN §5.2).
pub fn parse_service_status(out: &RunOutput) -> ServiceState {
    todo!()
}

/// The §4.4 truth table. `version_of` runs `<exe> --version`.
pub fn plan_launch(
    state: &ServiceState,
    version_of: impl FnOnce(&InstalledService) -> InstalledVersion,
    bundled: &Version,
    declined: Option<&DeclinedReplace>,
) -> LaunchPlan {
    todo!()
}

pub fn replace_dialog(installed: &InstalledVersion, bundled: &Version) -> Dialog {
    todo!()
}

pub fn enable_dialog() -> Dialog {
    todo!()
}

pub fn disable_dialog() -> Dialog {
    todo!()
}

pub fn finish_setup_dialog() -> Dialog {
    todo!()
}

pub fn port_busy_dialog() -> Dialog {
    todo!()
}

pub fn command_failed_dialog(command: &str, output: &str) -> Dialog {
    todo!()
}

/// Per target_os: Linux `journalctl --user -u peakbot.service`,
/// macOS `~/Library/Logs/peakbot.log`.
pub fn service_log_hint() -> &'static str {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_out(code: Option<i32>, stdout: &str, stderr: &str) -> RunOutput {
        RunOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    // §9.1 case 7.
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
            })
        );
    }

    // §9.1 case 8.
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
            }),
            "a space in the Exec path must be preserved"
        );

        let out = run_out(Some(0), "Installed: true\nState:     running\n", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Running,
            })
        );
    }

    // §9.1 case 9.
    #[test]
    fn t09_state_words() {
        let base = |state: &str| run_out(Some(0), &format!("Installed: true\n{state}\n"), "");
        assert_eq!(
            parse_service_status(&base("State:     stopped")),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Stopped,
            })
        );
        assert_eq!(
            parse_service_status(&base("State:     unknown")),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Unknown,
            })
        );
        // Missing State line → Unknown.
        let out = run_out(Some(0), "Installed: true\n", "");
        assert_eq!(
            parse_service_status(&out),
            ServiceState::Installed(InstalledService {
                exe: None,
                run: RunState::Unknown,
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

    // §9.1 case 13 — every row of the §4.4 truth table.
    #[test]
    fn t13_plan_launch_truth_table() {
        let b = bundled();
        let v22 = InstalledVersion::Known(Version::parse("0.22.0").unwrap());
        let vo = |i: InstalledVersion| move |_s: &InstalledService| i;
        let mut svc = InstalledService {
            exe: Some(PathBuf::from("/x/peakbot")),
            run: RunState::Running,
        };

        // Row: Unavailable → Session.
        assert_eq!(
            plan_launch(
                &ServiceState::Unavailable { reason: "r".into() },
                vo(InstalledVersion::Unknown),
                &b,
                None
            ),
            LaunchPlan::Session
        );
        // Row: NotInstalled → Session.
        assert_eq!(
            plan_launch(
                &ServiceState::NotInstalled,
                vo(InstalledVersion::Unknown),
                &b,
                None
            ),
            LaunchPlan::Session
        );
        // Row: Installed, equal, Running → ConnectService{start_first:false}.
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None
            ),
            LaunchPlan::ConnectService { start_first: false }
        );
        // Same, Stopped → start_first:true.
        svc.run = RunState::Stopped;
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None
            ),
            LaunchPlan::ConnectService { start_first: true }
        );
        // RunState::Unknown counts as running → start_first:false.
        svc.run = RunState::Unknown;
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(InstalledVersion::Known(b.clone())),
                &b,
                None
            ),
            LaunchPlan::ConnectService { start_first: false }
        );
        // Row: Installed, different Known, not declined → AskReplace.
        svc.run = RunState::Running;
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(v22.clone()),
                &b,
                None
            ),
            LaunchPlan::AskReplace {
                installed: v22.clone()
            }
        );
        // Row: different Known, declined pair matches → ConnectService.
        let declined = DeclinedReplace {
            installed: v22.clone(),
            bundled: b.clone(),
        };
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(v22.clone()),
                &b,
                Some(&declined)
            ),
            LaunchPlan::ConnectService { start_first: false }
        );
        // Row: Installed, Unknown, not declined → AskReplace.
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(InstalledVersion::Unknown),
                &b,
                None
            ),
            LaunchPlan::AskReplace {
                installed: InstalledVersion::Unknown
            }
        );
        // Row: Unknown, declined pair matches → ConnectService.
        let declined_u = DeclinedReplace {
            installed: InstalledVersion::Unknown,
            bundled: b.clone(),
        };
        assert_eq!(
            plan_launch(
                &ServiceState::Installed(svc.clone()),
                vo(InstalledVersion::Unknown),
                &b,
                Some(&declined_u)
            ),
            LaunchPlan::ConnectService { start_first: false }
        );
    }

    // §9.1 case 14.
    #[test]
    fn t14_plan_launch_skips_version_of_for_not_installed_and_unavailable() {
        let b = bundled();
        let panic_version_of = |_s: &InstalledService| -> InstalledVersion {
            panic!("version_of must not be called for NotInstalled/Unavailable")
        };
        assert_eq!(
            plan_launch(&ServiceState::NotInstalled, panic_version_of, &b, None),
            LaunchPlan::Session
        );
        assert_eq!(
            plan_launch(
                &ServiceState::Unavailable { reason: "r".into() },
                panic_version_of,
                &b,
                None
            ),
            LaunchPlan::Session
        );
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

    // §9.1 case 16.
    #[test]
    fn t16_arg_builders_are_exact() {
        assert_eq!(status_args(), ["service", "status"]);
        assert_eq!(
            service_install_args(),
            ["service", "install", "--bind", "127.0.0.1:7823"]
        );
        assert_eq!(service_uninstall_args(), ["service", "uninstall"]);
        assert_eq!(install_args(), ["install"]);
    }
}
