//! The old background service: detection and removal (DESIGN §3.4/§4.3).
//! The desktop no longer installs one; it only offers to remove it.

use crate::phase::Dialog;
use crate::sys::RunOutput;

pub fn status_args() -> [&'static str; 2] {
    ["service", "status"]
}

pub fn service_uninstall_args() -> [&'static str; 2] {
    ["service", "uninstall"]
}

/// `peakbot service status` (the B4-pinned text contract) says a service
/// is installed, running or stopped. A failed or unrecognised status is
/// "not installed": the session then reports any real conflict itself.
pub fn is_service_installed(out: &RunOutput) -> bool {
    out.ok()
        && out.stdout.lines().any(|line| {
            line.trim()
                .strip_prefix("Installed:")
                .is_some_and(|v| v.trim() == "true")
        })
}

pub fn remove_old_service_dialog() -> Dialog {
    Dialog {
        title: "Remove the old background service?".into(),
        body: "PeakBot Desktop now runs PeakBot itself and keeps it running while the app is \
               open. The old background service would conflict with it.\n\n\
               Your settings, conversations and the peakbot command are kept."
            .into(),
        ok: "Remove service".into(),
        cancel: Some("Not now".into()),
    }
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

    // Close-to-tray plan §C/§D: `is_service_installed` — exit 0 AND a
    // trimmed line `Installed: true` (any `State:` — running OR stopped),
    // nothing else.
    #[test]
    fn is_service_installed_true_on_exit_zero_and_installed_true_any_state() {
        let running = run_out(
            Some(0),
            "Installed: true\nExec:      /home/u/.local/bin/peakbot\nState:     running\n",
            "",
        );
        assert!(
            is_service_installed(&running),
            "exit 0 + Installed: true + running ⇒ installed"
        );

        let stopped = run_out(Some(0), "Installed: true\nState:     stopped\n", "");
        assert!(
            is_service_installed(&stopped),
            "exit 0 + Installed: true + stopped ⇒ installed (a stopped service still conflicts)"
        );

        let unknown = run_out(Some(0), "Installed: true\nState:     unknown\n", "");
        assert!(
            is_service_installed(&unknown),
            "exit 0 + Installed: true + unknown state ⇒ installed"
        );

        // The line may be padded, and other lines (Exec:, TLS:) coexist.
        let padded = run_out(
            Some(0),
            "Manager:   systemd-user\nInstalled:    true\nTLS:       on\nState:     running\n",
            "",
        );
        assert!(
            is_service_installed(&padded),
            "padded `Installed:    true` among other lines ⇒ installed"
        );
    }

    #[test]
    fn is_service_installed_false_on_negation_nonzero_exit_or_missing_line() {
        assert!(
            !is_service_installed(&run_out(Some(0), "Installed: false\n", "")),
            "`Installed: false` ⇒ not installed"
        );
        assert!(
            !is_service_installed(&run_out(Some(1), "Installed: true\n", "boom\n",)),
            "non-zero exit ⇒ NOT installed, even with `Installed: true` in stdout"
        );
        assert!(
            !is_service_installed(&run_out(Some(1), "", "no bus\n")),
            "non-zero exit, no stdout ⇒ not installed (status unavailable ⇒ launch session, no prompt)"
        );
        assert!(
            !is_service_installed(&run_out(Some(0), "Manager:   systemd-user\n", "")),
            "exit 0 without an `Installed:` line ⇒ not installed (unrecognised output)"
        );
        assert!(
            !is_service_installed(&run_out(Some(0), "Installed: yes\n", "")),
            "`Installed: yes` is not the B4-pinned format ⇒ not installed"
        );
        assert!(
            !is_service_installed(&run_out(Some(0), "installed: true\n", "")),
            "case matters: `installed: true` is not the line"
        );
        assert!(
            !is_service_installed(&run_out(Some(0), "", "")),
            "empty output ⇒ not installed"
        );
    }

    // §B: the migration dialog's locked strings.
    #[test]
    fn remove_old_service_dialog_locked_strings() {
        let d = remove_old_service_dialog();
        assert_eq!(d.title, "Remove the old background service?");
        assert_eq!(
            d.body,
            "PeakBot Desktop now runs PeakBot itself and keeps it running while the app is open. The old background service would conflict with it.\n\nYour settings, conversations and the peakbot command are kept."
        );
        assert_eq!(d.ok, "Remove service");
        assert_eq!(d.cancel.as_deref(), Some("Not now"));
    }

    // §C: the arg builders that survive the deletion.
    #[test]
    fn status_and_uninstall_args_are_exact() {
        assert_eq!(status_args(), ["service", "status"]);
        assert_eq!(service_uninstall_args(), ["service", "uninstall"]);
    }
}
