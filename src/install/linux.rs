//! Linux — systemd `--user` unit (plan §E.6, task I3a).
//!
//! One artifact (`~/.config/systemd/user/peakbot.service`) and three
//! `systemctl --user` calls. No D-Bus crate: `systemctl` is what the user
//! will type when diagnosing, so it is what we run.
//!
//! **Linger is reported, never attempted** — `loginctl enable-linger` goes
//! through polkit and can prompt for a password, which is impossible from a
//! background HTTP handler. We check the marker file and print the command.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::{APP, InstallError, RunState, ServiceManager, ServicePlan, ServiceReport, run};
use std::path::{Path, PathBuf};

/// The systemd marker that says "there is a user manager here". Absent in
/// WSL1, most containers, and on runit/s6/OpenRC boxes.
const SYSTEMD_USER_RUNTIME: &str = "/run/systemd/user";

/// Root-owned, 0755 — so an unprivileged existence check works, and we do
/// not have to parse `loginctl show-user` output for the same one bit.
const LINGER_DIR: &str = "/var/lib/systemd/linger";

// ---------------------------------------------------------------------------
// Renderer (pure — the §E.9 snapshot surface).
// ---------------------------------------------------------------------------

/// Render the unit text locked in §E.6.
///
/// `ExecStart` is absolute and double-quoted: systemd splits the value on
/// whitespace, so a home directory with a space would otherwise become two
/// argv entries. No `Environment=` — the token lives in the `web-token`
/// file (§E.5), never in an artifact.
pub fn render_unit(plan: &ServicePlan) -> String {
    let argv = plan.argv();
    let exe = &argv[0];
    let args = argv[1..].join(" ");
    format!(
        "[Unit]\n\
         Description={display} agent (web UI)\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{exe}\" {args}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        display = APP.display,
    )
}

// ---------------------------------------------------------------------------
// Pure parsers — the effect paths' decision logic, testable on any host.
// ---------------------------------------------------------------------------

/// Pull argv[0] back out of a unit's `ExecStart=` line so a stale path is
/// visible in `service status` (§E.6). Handles the quoted form we write,
/// including paths containing spaces.
fn parse_exec_start(unit: &str) -> Option<PathBuf> {
    let value = unit
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("ExecStart="))?
        .trim();
    let exe = match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => value.split_whitespace().next()?,
    };
    if exe.is_empty() {
        return None;
    }
    Some(PathBuf::from(exe))
}

/// Map `systemctl --user is-active` stdout to a run state. The words are
/// stable ASCII (not localised); anything mid-transition or unrecognised
/// is reported as `Unknown` rather than guessed at.
fn parse_is_active(stdout: &str) -> RunState {
    match stdout.trim() {
        "active" => RunState::Running,
        "inactive" | "failed" | "deactivating" => RunState::Stopped,
        _ => RunState::Unknown,
    }
}

/// `~/.config/systemd/user/peakbot.service`.
fn unit_path() -> Result<PathBuf, InstallError> {
    let config = dirs::config_dir().ok_or_else(|| {
        InstallError::Io(std::io::Error::other(
            "cannot resolve a config directory for the systemd user unit",
        ))
    })?;
    Ok(config.join("systemd").join("user").join(APP.unit))
}

/// `survives_logout`: linger is on when systemd's per-user marker exists.
fn linger_enabled() -> bool {
    match current_user() {
        Some(user) => Path::new(LINGER_DIR).join(user).exists(),
        None => false,
    }
}

fn current_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|u| !u.trim().is_empty())
}

/// §E.6 — bail before running `systemctl` at all when there is no user
/// manager: the resulting `systemctl` error is confusing, ours is not.
fn preflight() -> Result<(), InstallError> {
    if Path::new(SYSTEMD_USER_RUNTIME).exists() {
        return Ok(());
    }
    Err(InstallError::Unsupported(format!(
        "no systemd user session here (WSL1, container, runit/s6/OpenRC) — \
         start PeakBot from your own init system, or write the unit by hand to {}",
        unit_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "~/.config/systemd/user/peakbot.service".to_string())
    )))
}

/// The linger note, verbatim from §E.6. Emitted whenever linger is off —
/// it is the honest headline of what "start at login" does and does not do.
fn linger_note() -> String {
    "The service starts when you log in. To keep it running after logout / at boot, \
     run once: loginctl enable-linger $USER (asks for admin)."
        .to_string()
}

fn write_unit(path: &Path, text: &str) -> Result<(), InstallError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Effects — the three functions every platform module exposes (§E.1).
// ---------------------------------------------------------------------------

pub(super) fn install_service(plan: &ServicePlan) -> Result<ServiceReport, InstallError> {
    preflight()?;
    let path = unit_path()?;
    write_unit(&path, &render_unit(plan))?;

    let mut commands = Vec::new();
    let reload = run("systemctl", &["--user", "daemon-reload"])?.require_ok()?;
    commands.push(reload.display);
    // `enable --now` both wires up default.target and starts it. Failure
    // here is usually a missing session bus over SSH — we surface stderr
    // verbatim rather than paper over it (§E.6).
    let enable = run("systemctl", &["--user", "enable", "--now", APP.unit])?.require_ok()?;
    commands.push(enable.display);

    let survives_logout = linger_enabled();
    let mut notes = vec![format!("Unit written to {}", path.display())];
    if !survives_logout {
        notes.push(linger_note());
    }
    Ok(ServiceReport {
        manager: ServiceManager::SystemdUser,
        name: APP.unit.to_string(),
        artifact: Some(path),
        installed: true,
        exe: Some(plan.exe().to_path_buf()),
        run_state: current_run_state(),
        survives_logout,
        commands,
        notes,
    })
}

pub(super) fn uninstall_service() -> Result<ServiceReport, InstallError> {
    preflight()?;
    let path = unit_path()?;
    let mut commands = Vec::new();

    // Tolerated: the unit may already be disabled, or never have existed.
    // Uninstall must be re-runnable.
    let disable = run("systemctl", &["--user", "disable", "--now", APP.unit])?;
    commands.push(disable.display);

    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(InstallError::Io(e)),
    }

    let reload = run("systemctl", &["--user", "daemon-reload"])?.require_ok()?;
    commands.push(reload.display);

    Ok(ServiceReport {
        manager: ServiceManager::SystemdUser,
        name: APP.unit.to_string(),
        artifact: Some(path),
        installed: false,
        exe: None,
        run_state: RunState::Stopped,
        survives_logout: false,
        commands,
        notes: Vec::new(),
    })
}

pub(super) fn service_status() -> Result<ServiceReport, InstallError> {
    preflight()?;
    let path = unit_path()?;
    let installed = path.exists();
    let exe = std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .and_then(parse_exec_start);

    let mut commands = Vec::new();
    let run_state = if installed {
        // `is-active` exits non-zero for a stopped unit — that is an
        // answer, not a failure, so we read stdout either way.
        let active = run("systemctl", &["--user", "is-active", APP.unit])?;
        commands.push(active.display.clone());
        parse_is_active(&active.stdout)
    } else {
        RunState::Stopped
    };

    let survives_logout = linger_enabled();
    let mut notes = Vec::new();
    if installed && !survives_logout {
        notes.push(linger_note());
    }
    Ok(ServiceReport {
        manager: ServiceManager::SystemdUser,
        name: APP.unit.to_string(),
        artifact: Some(path),
        installed,
        exe,
        run_state,
        survives_logout,
        commands,
        notes,
    })
}

/// Post-install run state: `enable --now` succeeded, so ask rather than
/// assume — a unit can start and immediately fail.
fn current_run_state() -> RunState {
    match run("systemctl", &["--user", "is-active", APP.unit]) {
        Ok(r) => parse_is_active(&r.stdout),
        Err(_) => RunState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn plan(exe: &str) -> ServicePlan {
        ServicePlan::new(
            PathBuf::from(exe),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7823),
            None,
        )
        .unwrap()
    }

    #[test]
    fn exec_start_round_trips_through_the_parser() {
        let unit = render_unit(&plan("/home/you/.local/bin/peakbot"));
        assert_eq!(
            parse_exec_start(&unit),
            Some(PathBuf::from("/home/you/.local/bin/peakbot"))
        );
    }

    #[test]
    fn exec_start_round_trips_with_a_space_in_the_home_directory() {
        // The reason the path is quoted at all — an unquoted parse would
        // hand back "/home/user".
        let unit = render_unit(&plan("/home/user with space/.local/bin/peakbot"));
        assert_eq!(
            parse_exec_start(&unit),
            Some(PathBuf::from("/home/user with space/.local/bin/peakbot"))
        );
    }

    #[test]
    fn parse_exec_start_accepts_a_hand_edited_unquoted_unit() {
        // Users edit units. An unquoted ExecStart is legal systemd.
        let unit = "[Service]\nExecStart=/usr/bin/peakbot --bind 127.0.0.1:7823\n";
        assert_eq!(
            parse_exec_start(unit),
            Some(PathBuf::from("/usr/bin/peakbot"))
        );
    }

    #[test]
    fn parse_exec_start_returns_none_without_the_key() {
        assert_eq!(parse_exec_start("[Service]\nType=simple\n"), None);
        assert_eq!(parse_exec_start("ExecStart=\n"), None);
    }

    #[test]
    fn parse_is_active_maps_the_systemctl_vocabulary() {
        // Captured from `systemctl --user is-active peakbot.service`.
        assert_eq!(parse_is_active("active\n"), RunState::Running);
        assert_eq!(parse_is_active("inactive\n"), RunState::Stopped);
        assert_eq!(parse_is_active("failed\n"), RunState::Stopped);
        // Mid-transition and "unit not found" are honestly unknown.
        assert_eq!(parse_is_active("activating\n"), RunState::Unknown);
        assert_eq!(parse_is_active("unknown\n"), RunState::Unknown);
        assert_eq!(parse_is_active(""), RunState::Unknown);
    }

    #[test]
    fn unit_has_no_environment_directive() {
        let unit = render_unit(&plan("/home/you/.local/bin/peakbot"));
        assert!(!unit.contains("Environment"));
        assert!(unit.ends_with("WantedBy=default.target\n"));
    }

    #[test]
    fn linger_note_names_the_exact_command() {
        assert!(linger_note().contains("loginctl enable-linger $USER"));
    }

    #[test]
    #[ignore = "writes ~/.config/systemd/user and runs systemctl; CI has no user bus"]
    fn live_install_then_status_then_uninstall() {
        // `/usr/bin/true` rather than our own binary: this test exercises
        // the *effects* (write → daemon-reload → enable --now → is-active
        // → disable → remove), and a unit that starts and exits cleanly
        // leaves nothing running on the developer's desktop.
        let p = plan("/usr/bin/true");
        let installed = install_service(&p).expect("install");
        assert!(installed.installed);
        assert!(unit_path().unwrap().exists());

        let status = service_status().expect("status");
        assert!(status.installed);
        assert_eq!(status.exe, Some(PathBuf::from("/usr/bin/true")));

        let gone = uninstall_service().expect("uninstall");
        assert!(!gone.installed);
        assert!(!unit_path().unwrap().exists());
    }
}
