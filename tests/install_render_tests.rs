//! T3 — Installer / service renderer tests (plan §E, track I, tasks I1-I4).
//!
//! **Status: compile-fail until Track I lands.** This file targets the
//! planned public API in `peakbot::install`:
//!
//! - `peakbot::install::linux::render_unit(ServicePlan) -> String`
//! - `peakbot::install::macos::render_plist(ServicePlan) -> String`
//! - `peakbot::install::windows::render_task_xml(ServicePlan) -> Vec<u8>`
//!   (UTF-16LE + BOM — byte test lives here)
//! - `peakbot::install::path_state(path_var: &OsStr, target: &Path) -> PathState`
//! - `peakbot::install::ServicePlan::new(exe, bind, token) -> Result<Self, PlanError>`
//!
//! **B2 (desktop design §7):** services must never open a browser — the
//! Linux unit carries exactly one `Environment=` line,
//! `Environment=PEAKBOT_NO_OPEN=1`, under `[Service]`, and the launchd
//! plist carries an `EnvironmentVariables` dict with `PEAKBOT_NO_OPEN =
//! 1`. The snapshots below pin that contract; the secret-free invariant
//! (§E.5) is preserved: the token itself must never appear in any
//! artifact. Until B2 lands in the renderers, the snapshot/Environment
//! tests below fail — that is the RED state.
//!
//! The §E.1 `#[cfg(any(target_os = "X", test))]` gate means **all three
//! platform modules compile on Linux CI** and their pure renderers are
//! unit-testable there. The tests in this file are the contract — exact
//! byte-level assertions on the three artifacts, plus a UTF-16 BOM byte
//! test for the Windows task XML.
//!
//! Until Track I lands, this file fails to compile and `cargo test` stops
//! at this integration target. That is the RED state we want.

use peakbot::install::linux::render_unit;
use peakbot::install::macos::render_plist;
use peakbot::install::path_state;
use peakbot::install::windows::render_task_xml;
use peakbot::install::{PathState, PlanError, ServicePlan};
use std::ffi::OsStr;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

// ===========================================================================
// Helper builders — keep the test bodies declarative.
// ===========================================================================

fn bind_loopback() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7823)
}

fn bind_lan() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 7823)
}

fn plan_loopback(exe: &str) -> ServicePlan {
    ServicePlan::new(PathBuf::from(exe), bind_loopback(), None).unwrap()
}

fn plan_lan_with_token(exe: &str) -> ServicePlan {
    ServicePlan::new(PathBuf::from(exe), bind_lan(), Some("s3cret".to_string())).unwrap()
}

// ===========================================================================
// I4 — ServicePlan::new loopback/token invariant (plan §E.5).
// ===========================================================================

#[test]
fn service_plan_loopback_without_token_is_ok() {
    let plan = ServicePlan::new(PathBuf::from("/usr/bin/peakbot"), bind_loopback(), None).unwrap();
    // `argv()` is the production seam that emits `<exe> --bind <addr>`.
    let argv = plan.argv();
    assert_eq!(argv[0], "/usr/bin/peakbot");
    assert_eq!(argv[1], "--bind");
    assert_eq!(argv[2], "127.0.0.1:7823");
}

// Defensive `other => panic!` against a single-variant enum (the assertion
// is the point even if a future variant makes the arm reachable) — see the
// repo's clippy escape-hatch rule.
#[allow(unreachable_patterns)]
#[test]
fn service_plan_lan_without_token_is_token_required_error() {
    let err = ServicePlan::new(PathBuf::from("/usr/bin/peakbot"), bind_lan(), None).unwrap_err();
    match err {
        PlanError::TokenRequired(addr) => {
            assert_eq!(
                addr,
                bind_lan(),
                "the bind that was rejected must be reported"
            );
        }
        other => panic!("expected PlanError::TokenRequired, got {other:?}"),
    }
}

#[test]
fn service_plan_lan_with_whitespace_only_token_is_token_required() {
    // Whitespace-only token must NOT satisfy the invariant (§E.5).
    let err = ServicePlan::new(
        PathBuf::from("/usr/bin/peakbot"),
        bind_lan(),
        Some("   ".to_string()),
    )
    .unwrap_err();
    assert!(
        matches!(err, PlanError::TokenRequired(_)),
        "whitespace-only token must be treated as no token"
    );
}

#[test]
fn service_plan_lan_with_real_token_is_ok() {
    let plan = plan_lan_with_token("/usr/bin/peakbot");
    let argv = plan.argv();
    assert_eq!(argv.len(), 3);
    assert_eq!(argv[2], "0.0.0.0:7823");
}

// ===========================================================================
// I1 — path_state over a synthetic PATH (plan §E.4).
// ===========================================================================

#[test]
fn path_state_on_path_when_target_dir_is_first_and_file_present() {
    let path = OsStr::new("/home/u/.local/bin:/usr/bin");
    let target = Path::new("/home/u/.local/bin/peakbot");
    match path_state(path, target) {
        PathState::OnPath => {}
        other => panic!("expected OnPath, got {other:?}"),
    }
}

#[test]
fn path_state_on_path_when_dir_present_even_if_file_not_yet() {
    // §E.4: `OnPath` when the dir is on PATH and "wins the lookup" — the
    // existence of the target file is a separate question. The install
    // command returns OnPath so the wizard says "you're good".
    let path = OsStr::new("/home/u/.local/bin:/usr/bin");
    let target = Path::new("/home/u/.local/bin/peakbot");
    match path_state(path, target) {
        PathState::OnPath => {}
        other => panic!("expected OnPath, got {other:?}"),
    }
}

#[test]
fn path_state_shadowed_when_a_different_peakbot_precedes_target() {
    // §E.4: a *different* `peakbot` earlier on PATH shadows the target.
    // This is the "I installed it but peakbot --version is still the old
    // one" case the wizard must surface.
    let path = OsStr::new("/opt/old/bin:/home/u/.local/bin");
    let target = Path::new("/home/u/.local/bin/peakbot");
    let s = path_state(path, target);
    match s {
        PathState::Shadowed { by } => {
            assert_eq!(by, PathBuf::from("/opt/old/bin/peakbot"));
        }
        other => panic!("expected Shadowed, got {other:?}"),
    }
}

#[test]
fn path_state_not_on_path_when_dir_is_missing_from_path() {
    let path = OsStr::new("/usr/bin:/bin");
    let target = Path::new("/home/u/.local/bin/peakbot");
    match path_state(path, target) {
        PathState::NotOnPath { hint } => {
            assert!(
                hint.contains(".local/bin"),
                "hint must name the exact path to add; got: {hint}"
            );
        }
        other => panic!("expected NotOnPath, got {other:?}"),
    }
}

#[test]
fn path_state_hint_is_per_platform_text() {
    let path = OsStr::new("/usr/bin");
    let target = Path::new("/home/u/.local/bin/peakbot");
    let hint = match path_state(path, target) {
        PathState::NotOnPath { hint } => hint,
        other => panic!("expected NotOnPath, got {other:?}"),
    };
    // Linux/macOS hint from §E.4.
    assert!(
        hint.contains("export PATH"),
        "Unix hint must be the export-PATH line; got: {hint}"
    );
}

// ===========================================================================
// I3a — Linux systemd --user unit renderer (plan §E.6).
// ===========================================================================

#[test]
fn linux_unit_text_matches_the_locked_artifact() {
    // The exact artifact text from §E.6 + B2: one secret-free
    // `Environment=PEAKBOT_NO_OPEN=1` line under [Service] (services
    // must never open a browser). Snapshot test: any drift in the
    // renderer fails this test.
    let plan = plan_loopback("/home/you/.local/bin/peakbot");
    let unit = render_unit(&plan);
    let expected = "\
[Unit]
Description=PeakBot agent (web UI)
After=network-online.target

[Service]
Type=simple
Environment=PEAKBOT_NO_OPEN=1
ExecStart=\"/home/you/.local/bin/peakbot\" --bind 127.0.0.1:7823
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
";
    assert_eq!(unit, expected);
}

#[test]
fn linux_unit_quoting_handles_home_directory_with_space() {
    // §E.6: "The path is double-quoted so a home directory with a space
    // cannot split the argv — systemd's own quoting rules."
    let plan = plan_loopback("/home/user with space/.local/bin/peakbot");
    let unit = render_unit(&plan);
    assert!(
        unit.contains("ExecStart=\"/home/user with space/.local/bin/peakbot\" --bind "),
        "ExecStart must be double-quoted; got: {unit}"
    );
}

#[test]
fn linux_unit_exec_start_can_be_parsed_back_into_argv() {
    // Round-trip: parse ExecStart back out → first component equals exe.
    let plan = plan_loopback("/home/you/.local/bin/peakbot");
    let unit = render_unit(&plan);
    let exec = unit
        .lines()
        .find(|l| l.starts_with("ExecStart="))
        .expect("ExecStart must be present");
    let argv_str = exec.trim_start_matches("ExecStart=");
    // Systemd quoting: split on whitespace; quoted segments are preserved.
    // We just check the exe is the first whitespace-separated token.
    let first = argv_str.split_whitespace().next().unwrap();
    assert_eq!(first, "\"/home/you/.local/bin/peakbot\"");
}

#[test]
fn linux_unit_embeds_only_the_secret_free_environment_line() {
    // B2: a background service must never open a browser, so the unit
    // carries EXACTLY ONE `Environment=` line — `PEAKBOT_NO_OPEN=1`.
    // The secret-free invariant (§E.5) is preserved: that line is not a
    // secret, and the token must still never appear in the unit.
    let plan = plan_lan_with_token("/home/you/.local/bin/peakbot");
    let unit = render_unit(&plan);
    let env_lines: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("Environment="))
        .collect();
    assert_eq!(
        env_lines,
        vec!["Environment=PEAKBOT_NO_OPEN=1"],
        "the only Environment= line must be exactly Environment=PEAKBOT_NO_OPEN=1; got: {unit}"
    );
    assert!(
        !unit.contains("s3cret"),
        "the literal token must not appear in the unit"
    );
}

// ===========================================================================
// I3b — macOS LaunchAgent plist renderer (plan §E.7).
// ===========================================================================

#[test]
fn macos_plist_matches_the_locked_artifact() {
    // §E.7 + B2: the plist carries an `EnvironmentVariables` dict with
    // `PEAKBOT_NO_OPEN = 1` (services must never open a browser).
    let plan = plan_loopback("/Users/you/.local/bin/peakbot");
    let plist = render_plist(&plan);
    let expected = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.peakbot.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/.local/bin/peakbot</string>
    <string>--bind</string>
    <string>127.0.0.1:7823</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PEAKBOT_NO_OPEN</key>
    <string>1</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>/Users/you/Library/Logs/peakbot.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/you/Library/Logs/peakbot.log</string>
</dict>
</plist>
";
    assert_eq!(plist, expected);
}

#[test]
fn macos_plist_xml_escaping_handles_ampersand_and_quote_in_path() {
    // §E.7: "the only interpolated values are paths and a socket address;
    // a 5-character escape helper (`& < > " '`) covers them."
    let plan = ServicePlan::new(
        PathBuf::from("/Users/has&quote'/bin/peakbot"),
        bind_loopback(),
        None,
    )
    .unwrap();
    let plist = render_plist(&plan);
    assert!(plist.contains("&amp;"), "ampersand must be XML-escaped");
    // Quote and apostrophe are escaped as &quot; / &apos; in XML.
    assert!(
        plist.contains("&quot;") || plist.contains("&apos;"),
        "quote/apostrophe must be XML-escaped; got: {plist}"
    );
    // And the raw byte sequence `/Users/has&quote'/bin/peakbot` must NOT
    // appear unescaped inside the ProgramArguments array.
    let in_prog_args = plist
        .split("<key>ProgramArguments</key>")
        .nth(1)
        .and_then(|s| s.split("</array>").next())
        .unwrap();
    assert!(
        !in_prog_args.contains("/Users/has&quote'/bin/peakbot"),
        "unescaped path with & and ' must not appear in ProgramArguments"
    );
}

#[test]
fn macos_plist_does_not_embed_a_token() {
    let plan = plan_lan_with_token("/Users/you/.local/bin/peakbot");
    let plist = render_plist(&plan);
    assert!(
        !plist.contains("s3cret"),
        "the token must not appear in the plist"
    );
}

// ===========================================================================
// I3c — Windows Task Scheduler XML renderer (plan §E.8).
// ===========================================================================

#[test]
fn windows_task_xml_is_utf16_le_with_bom() {
    let plan = ServicePlan::new(
        PathBuf::from("C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe"),
        bind_loopback(),
        None,
    )
    .unwrap();
    let bytes = render_task_xml(&plan);

    // §E.8 — load-bearing: schtasks /Create /XML rejects UTF-8 with a
    // useless "The task XML is malformed" error. The artifact must be
    // UTF-16LE with a `FF FE` BOM. This is the single most likely bug.
    assert!(
        bytes.len() >= 2,
        "task XML must have at least a BOM; got {} bytes",
        bytes.len()
    );
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xFE],
        "task XML must start with the UTF-16LE BOM (FF FE)"
    );

    // Every subsequent byte pair must be a valid UTF-16LE code unit, and
    // the result, decoded back to UTF-8, must start with `<?xml`.
    let mut u16_units: Vec<u16> = Vec::with_capacity((bytes.len() - 2) / 2);
    for chunk in bytes[2..].as_chunks::<2>().0 {
        u16_units.push(u16::from_le_bytes(*chunk));
    }
    let decoded = String::from_utf16(&u16_units).expect("body must be valid UTF-16LE");
    assert!(
        decoded.starts_with("<?xml"),
        "decoded XML must start with <?xml; got: {decoded:?}"
    );
}

#[test]
fn windows_task_xml_decoded_matches_the_locked_artifact() {
    let plan = ServicePlan::new(
        PathBuf::from("C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe"),
        bind_loopback(),
        None,
    )
    .unwrap();
    let bytes = render_task_xml(&plan);
    let (units, _trailing) = bytes[2..].as_chunks::<2>();
    let u16_units: Vec<u16> = units.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let decoded = String::from_utf16(&u16_units).unwrap();
    let expected = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo>\n    <Description>PeakBot agent (web UI)</Description>\n  </RegistrationInfo>\n  <Triggers>\n    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>DOMAIN\\user</UserId>\n    </LogonTrigger>\n  </Triggers>\n  <Principals>\n    <Principal id=\"Author\">\n      <UserId>DOMAIN\\user</UserId>\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <Enabled>true</Enabled>\n  </Settings>\n  <Actions Context=\"Author\">\n    <Exec>\n      <Command>C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe</Command>\n      <Arguments>--bind 127.0.0.1:7823</Arguments>\n    </Exec>\n  </Actions>\n</Task>\n";
    assert_eq!(decoded, expected);
}

#[test]
fn windows_task_xml_does_not_embed_a_token() {
    let plan = ServicePlan::new(
        PathBuf::from("C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe"),
        bind_lan(),
        Some("s3cret".to_string()),
    )
    .unwrap();
    let bytes = render_task_xml(&plan);
    let (units, _trailing) = bytes[2..].as_chunks::<2>();
    let u16_units: Vec<u16> = units.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let decoded = String::from_utf16(&u16_units).unwrap();
    assert!(
        !decoded.contains("s3cret"),
        "the token must not appear in the task XML"
    );
}

// ===========================================================================
// #375 — `service install --tls`: the three renderers emit the TLS flags.
//
// All three renderers run on Linux CI (the §E.1 all-platforms-compile rule);
// the non-tls artifacts stay byte-identical to the locked snapshots above
// (criterion 7 — those existing tests are the guard and are unchanged).
// ===========================================================================

fn plan_loopback_tls(exe: &str) -> ServicePlan {
    plan_loopback(exe).with_tls(true)
}

fn plan_loopback_tls_named(exe: &str, names: &[&str]) -> ServicePlan {
    plan_loopback(exe)
        .with_tls(true)
        .with_tls_names(names.iter().map(|s| s.to_string()).collect())
}

/// Decode a rendered Windows task artifact (UTF-16LE + BOM) back to text.
fn decode_task_xml(bytes: &[u8]) -> String {
    let (units, _trailing) = bytes[2..].as_chunks::<2>();
    let units: Vec<u16> = units.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16(&units).expect("body must be valid UTF-16LE")
}

#[test]
fn linux_unit_tls_matches_the_locked_artifact() {
    // #375 criterion 1: ExecStart is exactly
    // `"<exe>" --bind 127.0.0.1:7823 --tls` — nothing else in the unit
    // changes.
    let plan = plan_loopback_tls("/home/you/.local/bin/peakbot");
    let unit = render_unit(&plan);
    let expected = "\
[Unit]
Description=PeakBot agent (web UI)
After=network-online.target

[Service]
Type=simple
Environment=PEAKBOT_NO_OPEN=1
ExecStart=\"/home/you/.local/bin/peakbot\" --bind 127.0.0.1:7823 --tls
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
";
    assert_eq!(unit, expected);
}

#[test]
fn macos_plist_tls_matches_the_locked_artifact() {
    // #375 criterion 1: `--tls` joins ProgramArguments after the bind
    // address — nothing else in the plist changes.
    let plan = plan_loopback_tls("/Users/you/.local/bin/peakbot");
    let plist = render_plist(&plan);
    let expected = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.peakbot.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/you/.local/bin/peakbot</string>
    <string>--bind</string>
    <string>127.0.0.1:7823</string>
    <string>--tls</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PEAKBOT_NO_OPEN</key>
    <string>1</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>/Users/you/Library/Logs/peakbot.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/you/Library/Logs/peakbot.log</string>
</dict>
</plist>
";
    assert_eq!(plist, expected);
}

#[test]
fn windows_task_xml_tls_decoded_matches_the_locked_artifact() {
    // #375 criterion 1: the task's `<Arguments>` is
    // `--bind 127.0.0.1:7823 --tls` — nothing else in the XML changes.
    let plan = plan_loopback_tls("C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe");
    let bytes = render_task_xml(&plan);
    let decoded = decode_task_xml(&bytes);
    let expected = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo>\n    <Description>PeakBot agent (web UI)</Description>\n  </RegistrationInfo>\n  <Triggers>\n    <LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>DOMAIN\\user</UserId>\n    </LogonTrigger>\n  </Triggers>\n  <Principals>\n    <Principal id=\"Author\">\n      <UserId>DOMAIN\\user</UserId>\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <Enabled>true</Enabled>\n  </Settings>\n  <Actions Context=\"Author\">\n    <Exec>\n      <Command>C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe</Command>\n      <Arguments>--bind 127.0.0.1:7823 --tls</Arguments>\n    </Exec>\n  </Actions>\n</Task>\n";
    assert_eq!(decoded, expected);
}

#[test]
fn tls_names_emit_after_tls_in_given_order_on_all_three_renderers() {
    // #375 criterion 2: `--tls-name a.lan --tls-name 10.0.0.9` — both
    // names, after `--tls`, in the given order, on every platform.
    let names = ["a.lan", "10.0.0.9"];

    let unit = render_unit(&plan_loopback_tls_named(
        "/home/you/.local/bin/peakbot",
        &names,
    ));
    assert!(
        unit.contains("--bind 127.0.0.1:7823 --tls --tls-name a.lan --tls-name 10.0.0.9"),
        "unit: {unit}"
    );

    let plist = render_plist(&plan_loopback_tls_named(
        "/Users/you/.local/bin/peakbot",
        &names,
    ));
    assert!(
        plist.contains(
            "<string>--tls</string>\n    <string>--tls-name</string>\n    <string>a.lan</string>\n    <string>--tls-name</string>\n    <string>10.0.0.9</string>"
        ),
        "plist: {plist}"
    );

    let xml = render_task_xml(&plan_loopback_tls_named(
        "C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe",
        &names,
    ));
    let decoded = decode_task_xml(&xml);
    assert!(
        decoded.contains(
            "<Arguments>--bind 127.0.0.1:7823 --tls --tls-name a.lan --tls-name 10.0.0.9</Arguments>"
        ),
        "xml: {decoded}"
    );
}

#[test]
fn no_artifact_embeds_a_token_when_tls_is_on() {
    // #375 criterion 5: the secret-free invariant (§E.5) with TLS on —
    // the token never appears in the unit, plist, or task XML, and the
    // Linux unit keeps exactly one `Environment=PEAKBOT_NO_OPEN=1` line.
    let mk = |exe: &str| {
        ServicePlan::new(PathBuf::from(exe), bind_lan(), Some("s3cret".to_string()))
            .unwrap()
            .with_tls(true)
    };

    let unit = render_unit(&mk("/home/you/.local/bin/peakbot"));
    assert!(unit.contains("--tls"), "unit: {unit}");
    assert!(!unit.contains("s3cret"), "unit: {unit}");
    let env_lines: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("Environment="))
        .collect();
    assert_eq!(
        env_lines,
        vec!["Environment=PEAKBOT_NO_OPEN=1"],
        "unit: {unit}"
    );

    let plist = render_plist(&mk("/Users/you/.local/bin/peakbot"));
    assert!(plist.contains("<string>--tls</string>"), "plist: {plist}");
    assert!(!plist.contains("s3cret"), "plist: {plist}");

    let bytes = render_task_xml(&mk(
        "C:\\Users\\you\\AppData\\Local\\Programs\\peakbot\\peakbot.exe",
    ));
    let decoded = decode_task_xml(&bytes);
    assert!(decoded.contains("--tls"), "xml: {decoded}");
    assert!(!decoded.contains("s3cret"), "xml: {decoded}");
}
