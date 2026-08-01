//! macOS — launchd LaunchAgent (plan §E.7, task I3b).
//!
//! One artifact (`~/Library/LaunchAgents/com.peakbot.agent.plist`) and the
//! modern `launchctl bootstrap`/`bootout` verbs, which report real exit
//! codes (the legacy `load -w`/`unload -w` lie about failure).
//!
//! Hand-written XML, no `plist` crate: the only interpolated values are
//! paths and a socket address, so a five-character escape helper covers it.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::{
    APP, InstallError, RunState, ServiceManager, ServicePlan, ServiceReport, run, xml_escape,
};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Renderer (pure — the §E.9 snapshot surface).
// ---------------------------------------------------------------------------

/// Render the LaunchAgent plist locked in §E.7.
///
/// `RunAtLoad` + `KeepAlive{SuccessfulExit:false}` is the analogue of
/// systemd's `Restart=on-failure`: start at login, restart on crash, stay
/// down after a clean exit. Plain `KeepAlive: true` would resurrect an
/// agent the user deliberately quit. No token — see §E.5.
pub fn render_plist(plan: &ServicePlan) -> String {
    let args = plan
        .argv()
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml_escape(a)))
        .collect::<String>();
    let log = xml_escape(&log_path(plan.exe()).display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n\
         \x20 <string>{label}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         {args}\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n\
         \x20 <true/>\n\
         \x20 <key>KeepAlive</key>\n\
         \x20 <dict>\n\
         \x20   <key>SuccessfulExit</key>\n\
         \x20   <false/>\n\
         \x20 </dict>\n\
         \x20 <key>StandardOutPath</key>\n\
         \x20 <string>{log}</string>\n\
         \x20 <key>StandardErrorPath</key>\n\
         \x20 <string>{log}</string>\n\
         </dict>\n\
         </plist>\n",
        label = APP.launchd_label,
    )
}

/// `~/Library/Logs/peakbot.log` — launchd has no journal, so without this
/// the agent's output is unrecoverable.
///
/// The home directory is derived from the *plan*, not from `$HOME`: the
/// renderer must be a pure function of its input so Linux CI can snapshot
/// it (§E.9). On macOS a home directory is by definition a child of
/// `/Users`, which makes the derivation exact for every real install.
fn log_path(exe: &Path) -> PathBuf {
    let home = exe
        .ancestors()
        .find(|a| a.parent() == Some(Path::new("/Users")))
        .map(Path::to_path_buf)
        // Network/`/var/root` homes are not under /Users; fall back to the
        // live home, then to the binary's own directory.
        .or_else(dirs::home_dir)
        .or_else(|| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    home.join("Library")
        .join("Logs")
        .join(format!("{}.log", APP.bin))
}

// ---------------------------------------------------------------------------
// Pure parsers — the effect paths' decision logic, testable on any host.
// ---------------------------------------------------------------------------

/// `id -u` stdout → the uid string used in `gui/<uid>` domain targets.
/// Rejects anything non-numeric rather than pasting junk into a
/// `launchctl` argument.
fn parse_uid(stdout: &str) -> Option<String> {
    let t = stdout.trim();
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        Some(t.to_string())
    } else {
        None
    }
}

/// `launchctl print gui/<uid>/<label>` stdout → run state. The `state = `
/// line is the only field we read; anything else is honestly `Unknown`
/// (the agent is loaded but launchd did not tell us what it is doing).
fn parse_launchctl_state(stdout: &str) -> RunState {
    let Some(state) = stdout
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("state = "))
    else {
        return RunState::Unknown;
    };
    match state.trim() {
        "running" => RunState::Running,
        "not running" | "waiting" => RunState::Stopped,
        _ => RunState::Unknown,
    }
}

/// Pull `<string>` argv[0] back out of an installed plist so `status` can
/// show a stale exe path.
fn parse_program_path(plist: &str) -> Option<PathBuf> {
    let array = plist
        .split("<key>ProgramArguments</key>")
        .nth(1)?
        .split("</array>")
        .next()?;
    let raw = array.split("<string>").nth(1)?.split("</string>").next()?;
    Some(PathBuf::from(unescape_xml(raw)))
}

/// Inverse of [`super::xml_escape`] for the five entities we emit. Only
/// used to read back our own artifact.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // `&amp;` last: doing it first would re-expand `&amp;lt;`.
        .replace("&amp;", "&")
}

fn plist_path() -> Result<PathBuf, InstallError> {
    let home = dirs::home_dir().ok_or_else(|| {
        InstallError::Io(std::io::Error::other(
            "cannot resolve a home directory for the LaunchAgent plist",
        ))
    })?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", APP.launchd_label)))
}

/// `id -u`, not `libc::getuid()`: one dep for one number would also cost
/// this module its CI coverage (§E.1/§E.7).
fn uid() -> Result<String, InstallError> {
    let out = run("id", &["-u"])?.require_ok()?;
    parse_uid(&out.stdout).ok_or_else(|| {
        InstallError::Io(std::io::Error::other("id -u did not return a numeric uid"))
    })
}

fn write_plist(path: &Path, text: &str) -> Result<(), InstallError> {
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

/// Emitted on install: an unsigned binary carrying `com.apple.quarantine`
/// can be blocked when *launchd* starts it, with no dialog at all.
fn gatekeeper_note(exe: &Path) -> String {
    format!(
        "If the agent never starts, macOS may be quarantining the binary. \
         Clear it once: xattr -d com.apple.quarantine {}",
        exe.display()
    )
}

/// A LaunchAgent lives in the user's GUI session, full stop. The
/// LaunchDaemon alternative needs root and resolves a different config
/// dir — the macOS version of the Windows session-0 mistake (§E.7).
fn session_note() -> String {
    "A LaunchAgent runs in your login session: it starts when you log in and stops \
     when you log out. There is no per-user equivalent of systemd linger."
        .to_string()
}

// ---------------------------------------------------------------------------
// Effects — the three functions every platform module exposes (§E.1).
// ---------------------------------------------------------------------------

pub(super) fn install_service(plan: &ServicePlan) -> Result<ServiceReport, InstallError> {
    let uid = uid()?;
    let path = plist_path()?;
    let mut commands = Vec::new();

    // Unconditional bootout first — `bootstrap` on an already-loaded label
    // fails with "Bootstrap failed: 5: Input/output error". Ignoring this
    // one failure is exactly what makes install idempotent (§E.7).
    let target = format!("gui/{uid}/{}", APP.launchd_label);
    let bootout = run("launchctl", &["bootout", &target])?;
    commands.push(bootout.display);

    write_plist(&path, &render_plist(plan))?;

    let domain = format!("gui/{uid}");
    let plist_arg = path.display().to_string();
    let bootstrap = run("launchctl", &["bootstrap", &domain, &plist_arg])?.require_ok()?;
    commands.push(bootstrap.display);

    Ok(ServiceReport {
        manager: ServiceManager::LaunchdAgent,
        name: APP.launchd_label.to_string(),
        artifact: Some(path),
        installed: true,
        exe: Some(plan.exe().to_path_buf()),
        run_state: print_state(&uid),
        survives_logout: false,
        commands,
        notes: vec![session_note(), gatekeeper_note(plan.exe())],
    })
}

pub(super) fn uninstall_service() -> Result<ServiceReport, InstallError> {
    let uid = uid()?;
    let path = plist_path()?;
    let mut commands = Vec::new();

    // Tolerated: the label may not be loaded. Uninstall must re-run clean.
    let target = format!("gui/{uid}/{}", APP.launchd_label);
    let bootout = run("launchctl", &["bootout", &target])?;
    commands.push(bootout.display);

    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(InstallError::Io(e)),
    }

    Ok(ServiceReport {
        manager: ServiceManager::LaunchdAgent,
        name: APP.launchd_label.to_string(),
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
    let uid = uid()?;
    let path = plist_path()?;
    let installed = path.exists();
    let exe = std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .and_then(parse_program_path);

    let mut commands = Vec::new();
    let run_state = if installed {
        let target = format!("gui/{uid}/{}", APP.launchd_label);
        let printed = run("launchctl", &["print", &target])?;
        commands.push(printed.display.clone());
        // Non-zero exit means the label is not loaded — a stopped agent,
        // not a broken query.
        if printed.ok {
            parse_launchctl_state(&printed.stdout)
        } else {
            RunState::Stopped
        }
    } else {
        RunState::Stopped
    };

    let notes = if installed {
        vec![session_note()]
    } else {
        Vec::new()
    };
    Ok(ServiceReport {
        manager: ServiceManager::LaunchdAgent,
        name: APP.launchd_label.to_string(),
        artifact: Some(path),
        installed,
        exe,
        run_state,
        survives_logout: false,
        commands,
        notes,
    })
}

/// Post-install state: ask launchd rather than assume `bootstrap` means
/// "running" — the process can bootstrap and immediately exit.
fn print_state(uid: &str) -> RunState {
    let target = format!("gui/{uid}/{}", APP.launchd_label);
    match run("launchctl", &["print", &target]) {
        Ok(r) if r.ok => parse_launchctl_state(&r.stdout),
        Ok(_) => RunState::Stopped,
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
    fn log_path_is_derived_from_the_users_home_in_the_plan() {
        assert_eq!(
            log_path(Path::new("/Users/you/.local/bin/peakbot")),
            PathBuf::from("/Users/you/Library/Logs/peakbot.log")
        );
        // Deeper nesting still resolves to the /Users child.
        assert_eq!(
            log_path(Path::new("/Users/you/dev/target/debug/peakbot")),
            PathBuf::from("/Users/you/Library/Logs/peakbot.log")
        );
    }

    #[test]
    fn program_arguments_round_trip_through_the_parser() {
        let p = plan("/Users/you/.local/bin/peakbot");
        let plist = render_plist(&p);
        assert_eq!(
            parse_program_path(&plist),
            Some(PathBuf::from("/Users/you/.local/bin/peakbot"))
        );
    }

    #[test]
    fn escaped_program_arguments_round_trip() {
        // The escaping is only useful if we can read our own artifact back.
        let p = plan("/Users/has&quote'/bin/peakbot");
        let plist = render_plist(&p);
        assert!(plist.contains("&amp;"));
        assert_eq!(
            parse_program_path(&plist),
            Some(PathBuf::from("/Users/has&quote'/bin/peakbot"))
        );
    }

    #[test]
    fn keep_alive_is_the_successful_exit_dict_not_a_bare_true() {
        let plist = render_plist(&plan("/Users/you/.local/bin/peakbot"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <dict>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(!plist.contains("<key>KeepAlive</key>\n  <true/>"));
    }

    #[test]
    fn parse_uid_accepts_only_digits() {
        assert_eq!(parse_uid("501\n"), Some("501".to_string()));
        assert_eq!(parse_uid(" 0 "), Some("0".to_string()));
        assert_eq!(parse_uid(""), None);
        assert_eq!(parse_uid("uid=501(you)"), None);
    }

    #[test]
    fn parse_launchctl_state_reads_the_state_line() {
        // Trimmed from real `launchctl print gui/501/com.peakbot.agent`.
        let running = "com.peakbot.agent = {\n\tactive count = 1\n\tstate = running\n\tprogram = /Users/you/.local/bin/peakbot\n}";
        assert_eq!(parse_launchctl_state(running), RunState::Running);
        let waiting = "com.peakbot.agent = {\n\tstate = not running\n}";
        assert_eq!(parse_launchctl_state(waiting), RunState::Stopped);
        assert_eq!(
            parse_launchctl_state("state = spawn scheduled"),
            RunState::Unknown
        );
        assert_eq!(
            parse_launchctl_state("Could not find service"),
            RunState::Unknown
        );
    }

    #[test]
    fn notes_name_the_quarantine_fix() {
        let note = gatekeeper_note(Path::new("/Users/you/.local/bin/peakbot"));
        assert!(note.contains("xattr -d com.apple.quarantine /Users/you/.local/bin/peakbot"));
    }

    #[test]
    #[ignore = "requires macOS launchd; no macOS CI runner exists"]
    fn live_install_then_status_then_uninstall() {
        // `/usr/bin/true`, not our own binary — see the Linux twin: the
        // point is the bootout/bootstrap effects, not a running agent.
        let p = plan("/usr/bin/true");
        assert!(install_service(&p).expect("install").installed);
        assert!(service_status().expect("status").installed);
        assert!(!uninstall_service().expect("uninstall").installed);
        assert!(!plist_path().unwrap().exists());
    }
}
