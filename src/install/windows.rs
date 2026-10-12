//! Windows — Task Scheduler logon task via `schtasks.exe` (plan §E.8, I3c).
//!
//! A per-user logon task, not a service: a service would run in session 0
//! with no desktop and a different profile. `schtasks /Create /XML` rather
//! than `/TR`, because quoting an exe path *and* its arguments through
//! `/TR` is the classic Windows escaping minefield.
//!
//! **The artifact must be UTF-16LE with a BOM.** `schtasks /Create /XML`
//! rejects UTF-8 with a useless "The task XML is malformed" — this is the
//! single most likely bug in the module, so it has its own byte test.
#![cfg_attr(not(windows), allow(dead_code))]

use super::{
    APP, InstallError, RunState, ServiceManager, ServicePlan, ServiceReport, run, xml_escape,
};
use std::path::PathBuf;

/// The `<UserId>` used when the environment cannot name the real user —
/// which is every non-Windows build, including CI. Keeping it a constant
/// (rather than an `Option` threaded through the renderer) is what makes
/// the artifact snapshot-testable on Linux (§E.9).
const PLACEHOLDER_USER: &str = "DOMAIN\\user";

// ---------------------------------------------------------------------------
// Renderer (pure — the §E.9 snapshot surface).
// ---------------------------------------------------------------------------

/// Render the task XML locked in §E.8, encoded UTF-16LE with a BOM.
pub fn render_task_xml(plan: &ServicePlan) -> Vec<u8> {
    to_utf16le_with_bom(&render_task_xml_for(plan, &task_user_id()))
}

/// The XML body, with the `<UserId>` supplied by the caller. Split out so
/// the encoding and the user lookup are each testable on their own.
fn render_task_xml_for(plan: &ServicePlan, user_id: &str) -> String {
    let argv = plan.argv();
    let command = xml_escape(&argv[0]);
    let arguments = xml_escape(&argv[1..].join(" "));
    let user = xml_escape(user_id);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" \
         xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n\
         \x20   <Description>{display} agent (web UI)</Description>\n\
         \x20 </RegistrationInfo>\n\
         \x20 <Triggers>\n\
         \x20   <LogonTrigger>\n\
         \x20     <Enabled>true</Enabled>\n\
         \x20     <UserId>{user}</UserId>\n\
         \x20   </LogonTrigger>\n\
         \x20 </Triggers>\n\
         \x20 <Principals>\n\
         \x20   <Principal id=\"Author\">\n\
         \x20     <UserId>{user}</UserId>\n\
         \x20     <LogonType>InteractiveToken</LogonType>\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\n\
         \x20   </Principal>\n\
         \x20 </Principals>\n\
         \x20 <Settings>\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <StartWhenAvailable>true</StartWhenAvailable>\n\
         \x20   <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
         \x20   <Enabled>true</Enabled>\n\
         \x20 </Settings>\n\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>{command}</Command>\n\
         \x20     <Arguments>{arguments}</Arguments>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
        display = APP.display,
    )
}

/// UTF-16LE + `FF FE` BOM — see the module docs. Eight lines, no crate.
fn to_utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + s.len() * 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// `whoami` output is authoritative — under Windows OpenSSH, `USERDOMAIN`
/// is `WORKGROUP` (Win32-OpenSSH #2407), so env vars alone can't name the
/// account. Off Windows there are no such variables, so the renderer emits
/// the placeholder and stays a pure function of its input — that is what
/// lets Linux CI pin the whole artifact byte-for-byte (§E.9).
///
/// `cfg!` rather than `#[cfg]` on purpose: both branches are portable, and
/// a runtime `if` keeps them both type-checked on every target.
fn task_user_id() -> String {
    if !cfg!(windows) {
        return PLACEHOLDER_USER.to_string();
    }
    if let Ok(r) = run("whoami", &[])
        && let Some(id) = parse_whoami(&r.stdout)
    {
        return id;
    }
    fallback_user_id(
        env_var("USERDOMAIN").as_deref(),
        env_var("USERNAME").as_deref(),
        env_var("COMPUTERNAME").as_deref(),
    )
}

/// Parse the output of `whoami` (no args) into `DOMAIN\\user`. Pure.
/// `Some` only for a single, non-blank line with a backslash and non-empty
/// parts on both sides; everything else is `None`.
fn parse_whoami(output: &str) -> Option<String> {
    let line = output.trim();
    // Exactly one line left after trim.
    if line.is_empty() || line.contains('\n') || line.contains('\r') {
        return None;
    }
    let (domain, user) = line.split_once('\\')?;
    if domain.is_empty() || user.is_empty() {
        return None;
    }
    Some(line.to_string())
}

/// Pure env-var fallback for [`task_user_id`] when `whoami` is unusable.
/// Returns `String` (`PLACEHOLDER_USER` when there is no user name); a
/// missing or `WORKGROUP` (case-insensitive) `userdomain` is replaced by
/// `computername` (Win32-OpenSSH sets `USERDOMAIN=WORKGROUP`, issue #2407).
fn fallback_user_id(
    userdomain: Option<&str>,
    username: Option<&str>,
    computername: Option<&str>,
) -> String {
    let user = match username {
        Some(u) => u,
        None => return PLACEHOLDER_USER.to_string(),
    };
    // WORKGROUP (any case) is OpenSSH's lie, not a domain; no domain at all
    // means "the machine itself" → the computer name.
    let domain = match userdomain {
        Some(d) if !d.eq_ignore_ascii_case("workgroup") => Some(d),
        _ => computername,
    };
    match domain {
        Some(d) => format!("{d}\\{user}"),
        None => user.to_string(),
    }
}

/// An environment variable that is present and not blank.
fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Pure parsers — the effect paths' decision logic, testable on any host.
// ---------------------------------------------------------------------------

/// Pull `<Command>` out of `schtasks /Query /TN PeakBot /XML` output so
/// `status` can show a stale exe path. The XML form is culture-invariant;
/// `/FO LIST` is localised and must never be parsed (§E.8).
fn parse_task_command(xml: &str) -> Option<PathBuf> {
    let raw = xml.split("<Command>").nth(1)?.split("</Command>").next()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(unescape_xml(trimmed)))
}

/// Inverse of [`super::xml_escape`] for the five entities we emit.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // `&amp;` last: doing it first would re-expand `&amp;lt;`.
        .replace("&amp;", "&")
}

/// Where the temporary XML goes. The registered task is the single source
/// of truth, so this file is deleted straight after `/Create`; a leftover
/// on disk would just be a staler second copy (§E.8).
fn temp_xml_path() -> PathBuf {
    std::env::temp_dir().join(format!("peakbot-task-{}.xml", std::process::id()))
}

/// Shipped honestly on install: PeakBot is a console-subsystem binary, and
/// `<Hidden>` hides the *task* in the UI, not the process's window (§E.8).
fn console_note() -> String {
    "A console window opens at sign-in and stays open — that is PeakBot running. \
     Closing it stops the agent; minimise it instead."
        .to_string()
}

/// Why `run_state` is `unknown` here, in the user's words.
fn run_state_note() -> String {
    "Windows cannot tell us whether the task is running without parsing localised \
     output, so we do not guess: open Task Scheduler, or just open the URL — if it \
     answers, it is running."
        .to_string()
}

// ---------------------------------------------------------------------------
// Effects — the three functions every platform module exposes (§E.1).
// ---------------------------------------------------------------------------

pub(super) fn install_service(plan: &ServicePlan) -> Result<ServiceReport, InstallError> {
    let tmp = temp_xml_path();
    std::fs::write(&tmp, render_task_xml(plan))?;
    let tmp_arg = tmp.display().to_string();

    // `/F` overwrites an existing task ⇒ install is idempotent.
    let created = run(
        "schtasks",
        &["/Create", "/TN", APP.task, "/XML", &tmp_arg, "/F"],
    );
    // Delete the temp file whatever happened — it carries no secret, but
    // it is still litter.
    let _ = std::fs::remove_file(&tmp);
    let created = created?.require_ok()?;

    Ok(ServiceReport {
        manager: ServiceManager::WindowsTask,
        name: APP.task.to_string(),
        // The registered task IS the artifact (§B: null on Windows).
        artifact: None,
        installed: true,
        exe: Some(plan.exe().to_path_buf()),
        run_state: RunState::Unknown,
        survives_logout: false,
        commands: vec![created.display],
        notes: vec![console_note(), run_state_note()],
    })
}

pub(super) fn uninstall_service() -> Result<ServiceReport, InstallError> {
    let mut commands = Vec::new();

    // Ask first: "already absent" must not be an error, and the only
    // culture-invariant way to tell is a failing /Query (the /Delete
    // failure text is localised).
    let query = run("schtasks", &["/Query", "/TN", APP.task, "/XML"])?;
    commands.push(query.display.clone());
    if query.ok {
        let deleted = run("schtasks", &["/Delete", "/TN", APP.task, "/F"])?.require_ok()?;
        commands.push(deleted.display);
    }

    Ok(ServiceReport {
        manager: ServiceManager::WindowsTask,
        name: APP.task.to_string(),
        artifact: None,
        installed: false,
        exe: None,
        run_state: RunState::Stopped,
        survives_logout: false,
        commands,
        notes: Vec::new(),
    })
}

pub(super) fn service_status() -> Result<ServiceReport, InstallError> {
    let query = run("schtasks", &["/Query", "/TN", APP.task, "/XML"])?;
    let installed = query.ok;
    let exe = if installed {
        parse_task_command(&query.stdout)
    } else {
        None
    };
    let notes = if installed {
        vec![console_note(), run_state_note()]
    } else {
        Vec::new()
    };
    Ok(ServiceReport {
        manager: ServiceManager::WindowsTask,
        name: APP.task.to_string(),
        artifact: None,
        installed,
        exe,
        // Honest, not lazy: see `run_state_note`.
        run_state: if installed {
            RunState::Unknown
        } else {
            RunState::Stopped
        },
        survives_logout: false,
        commands: vec![query.display],
        notes,
    })
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

    fn decode(bytes: &[u8]) -> String {
        let (units, _trailing) = bytes[2..].as_chunks::<2>();
        let units: Vec<u16> = units.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn utf16le_encoding_round_trips_through_the_bom() {
        let bytes = to_utf16le_with_bom("<?xml?>ü");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(decode(&bytes), "<?xml?>ü");
        // Two bytes per BMP code unit, plus the BOM.
        assert_eq!(bytes.len(), 2 + "<?xml?>ü".encode_utf16().count() * 2);
    }

    #[test]
    fn command_round_trips_through_the_parser() {
        let xml = render_task_xml_for(
            &plan("C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe"),
            "DOMAIN\\user",
        );
        assert_eq!(
            parse_task_command(&xml),
            Some(PathBuf::from(
                "C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe"
            ))
        );
    }

    #[test]
    fn a_path_needing_escaping_round_trips() {
        let xml = render_task_xml_for(&plan("C:\\Users\\a&b\\peakbot.exe"), "DOM\\a&b");
        assert!(xml.contains("<Command>C:\\Users\\a&amp;b\\peakbot.exe</Command>"));
        assert!(xml.contains("<UserId>DOM\\a&amp;b</UserId>"));
        assert_eq!(
            parse_task_command(&xml),
            Some(PathBuf::from("C:\\Users\\a&b\\peakbot.exe"))
        );
    }

    #[test]
    fn parse_task_command_returns_none_without_the_element() {
        assert_eq!(parse_task_command("<Task/>"), None);
        assert_eq!(parse_task_command("<Command></Command>"), None);
    }

    #[test]
    fn settings_carry_the_four_load_bearing_values() {
        let xml = render_task_xml_for(&plan("C:\\peakbot.exe"), "DOMAIN\\user");
        // PT0S: the 72-hour default would silently kill the agent.
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        // The default `true` is why "my task never runs on my laptop".
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        // A second logon must not start a second agent on port 7823.
        assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(xml.contains("<LogonTrigger>"));
    }

    #[test]
    #[cfg(not(windows))]
    fn task_user_id_is_the_placeholder_off_windows() {
        // The property the whole Linux-CI snapshot rests on. Asserted
        // without touching the environment — process-global mutation in a
        // parallel test run is exactly what §E.4 avoids elsewhere.
        assert_eq!(task_user_id(), PLACEHOLDER_USER);
    }

    #[test]
    #[ignore = "requires Windows Task Scheduler; no Windows CI runner exists"]
    fn live_install_then_status_then_uninstall() {
        let p = plan(&std::env::current_exe().unwrap().to_string_lossy());
        assert!(install_service(&p).expect("install").installed);
        assert!(service_status().expect("status").installed);
        assert!(!uninstall_service().expect("uninstall").installed);
        assert!(!temp_xml_path().exists(), "temp XML must not survive");
    }

    // ── parse_whoami ────────────────────────────────────────────────

    #[test]
    fn parse_whoami_accepts_and_trims_a_domain_user() {
        let cases: &[(&str, &str)] = &[
            ("host\\user\r\n", "host\\user"),
            ("host\\user\n", "host\\user"),
            ("host\\user", "host\\user"),
            ("  DOMAIN\\Jane Doe \n", "DOMAIN\\Jane Doe"),
            ("win-cbtf92k3hq0\\docker\r\n", "win-cbtf92k3hq0\\docker"),
            ("\t CORP\\you \r\n\r\n", "CORP\\you"),
            ("MÜNCHEN\\José\r\n", "MÜNCHEN\\José"),
        ];
        for (input, want) in cases {
            assert_eq!(
                parse_whoami(input).as_deref(),
                Some(*want),
                "input={input:?}"
            );
        }
    }

    #[test]
    fn parse_whoami_rejects_unusable_output() {
        let cases: &[&str] = &[
            "",
            "   \r\n",
            "\n\n",
            "nobackslash",
            "nobackslash\r\n",
            "\\user",
            "host\\",
            "host\\\r\n",
            "\\",
            " \\ ",
            "host\\user\r\nother\\user2\r\n",
            "ERROR: something\r\nhost\\user",
        ];
        for input in cases {
            assert_eq!(parse_whoami(input), None, "input={input:?}");
        }
    }

    // ── fallback_user_id ────────────────────────────────────────────
    // Choice: returns `String` (not `Option`), consistent with `task_user_id`:
    // no user name => PLACEHOLDER_USER; no usable domain
    // and no computer name => bare user name.

    #[test]
    fn fallback_user_id_uses_computername_for_workgroup_domain_any_case() {
        for d in ["WORKGROUP", "workgroup", "WorkGroup"] {
            assert_eq!(
                fallback_user_id(Some(d), Some("docker"), Some("WIN-HOST")),
                "WIN-HOST\\docker",
                "domain={d}"
            );
        }
    }

    #[test]
    fn fallback_user_id_uses_computername_when_domain_missing() {
        assert_eq!(
            fallback_user_id(None, Some("docker"), Some("WIN-HOST")),
            "WIN-HOST\\docker"
        );
    }

    #[test]
    fn fallback_user_id_keeps_a_real_domain() {
        assert_eq!(
            fallback_user_id(Some("CORP"), Some("you"), Some("WIN-HOST")),
            "CORP\\you"
        );
        assert_eq!(
            fallback_user_id(Some("CORP"), Some("you"), None),
            "CORP\\you"
        );
        // Only the exact word WORKGROUP is special, not a prefix of it.
        assert_eq!(
            fallback_user_id(Some("WORKGROUP2"), Some("you"), Some("WIN-HOST")),
            "WORKGROUP2\\you"
        );
    }

    #[test]
    fn fallback_user_id_without_username_is_the_placeholder() {
        assert_eq!(
            fallback_user_id(Some("CORP"), None, Some("WIN-HOST")),
            PLACEHOLDER_USER
        );
        assert_eq!(
            fallback_user_id(Some("WORKGROUP"), None, Some("WIN-HOST")),
            PLACEHOLDER_USER
        );
        assert_eq!(fallback_user_id(None, None, None), PLACEHOLDER_USER);
    }

    #[test]
    fn fallback_user_id_without_any_domain_source_is_the_bare_username() {
        // Mirrors `format_user_id(None, Some(u)) == u`.
        assert_eq!(fallback_user_id(None, Some("you"), None), "you");
        assert_eq!(
            fallback_user_id(Some("WORKGROUP"), Some("you"), None),
            "you"
        );
    }
}
