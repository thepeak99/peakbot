//! C8 — `RealSys` integration tests (DESIGN §7, unix only).
//!
//! A fake sidecar is a shell script in a tempdir.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use peakbot_desktop_core::consts::STOP_GRACE;
use peakbot_desktop_core::sys::{RealSys, SpawnSpec, Sys};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Every test that **binds a port** or **forks a child** in this binary
/// must hold this lock. Two test-harness races (not production bugs) make
/// port assertions flaky under the default parallel execution:
/// 1. A forked child inherits a copy of any open fd (fork → exec/exec-
///    exit), keeping a just-dropped listener socket in LISTEN for a few
///    ms — the sibling's fork can land inside a test's bind→drop window.
/// 2. A `bind(127.0.0.1, 0)` may hand out a port another test just
///    released, stealing it between that test's drop and rebind.
///
/// The affected tests are short, so serializing them is cheap.
static PORT_RACE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn real_sys_run_captures_stdout_stderr_and_exit_code() {
    let _guard = PORT_RACE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        dir.path(),
        "sidecar.sh",
        "#!/bin/sh\necho out-line\necho err-line >&2\nexit 3\n",
    );
    let out = RealSys::new().run(&script, &[]).expect("the script runs");
    assert_eq!(out.code, Some(3));
    assert!(out.stdout.contains("out-line"), "stdout: {:?}", out.stdout);
    assert!(out.stderr.contains("err-line"), "stderr: {:?}", out.stderr);
    assert!(!out.ok(), "exit 3 is not ok");
}

#[test]
fn real_sys_run_missing_program_is_notfound() {
    let dir = tempfile::tempdir().unwrap();
    let res = RealSys::new().run(&dir.path().join("no-such-program"), &[]);
    let is_not_found = res
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    assert!(is_not_found, "got {res:?}");
}

#[test]
fn real_sys_run_timeout_kills_the_child() {
    let _guard = PORT_RACE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(dir.path(), "sleepy.sh", "#!/bin/sh\nsleep 30\n");
    // Test-only short-timeout constructor — a REQUIRED addition for C8
    // (see the T1 report): without it this test would wait the full 60 s
    // RUN_TIMEOUT.
    let sys = RealSys::with_run_timeout(Duration::from_millis(500));
    let start = std::time::Instant::now();
    let res = sys.run(&script, &[]);
    let elapsed = start.elapsed();
    let is_timed_out = res
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::TimedOut);
    assert!(is_timed_out, "got {res:?}");
    assert!(
        elapsed < Duration::from_secs(10),
        "the timeout must be ~500 ms, not 60 s; took {elapsed:?}"
    );
}

#[test]
fn real_sys_spawn_interrupt_exits_within_grace() {
    let _guard = PORT_RACE_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        dir.path(),
        "graceful.sh",
        "#!/bin/sh\ntrap 'exit 0' INT\nwhile :; do sleep 0.1; done\n",
    );
    let log = dir.path().join("session.log");
    let mut child = RealSys::new()
        .spawn(SpawnSpec {
            program: script,
            args: vec![],
            extra_env: vec![],
            log,
        })
        .expect("spawn");
    // Give the shell a moment to install the trap.
    std::thread::sleep(Duration::from_millis(200));
    child.interrupt();
    let start = std::time::Instant::now();
    let mut exit = None;
    while start.elapsed() < STOP_GRACE {
        if let Some(e) = child.try_wait().unwrap() {
            exit = e;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        exit,
        Some(0),
        "the trap must turn SIGINT into a clean exit 0 within STOP_GRACE"
    );
}

#[test]
fn real_sys_write_private_is_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("secret");
    RealSys::new()
        .write_private(&path, "top secret")
        .expect("write");
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "write_private must produce mode 0600 (incl. mkdir -p)"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "top secret");
}

// Design #377 t88 (unix): the https probe is a plain TCP connect — no TLS
// handshake (core ureq has no TLS; #374 replaces this branch with a
// CA-verified GET).
#[test]
fn t88_real_sys_probe_https_listening_and_unreachable() {
    use peakbot_desktop_core::consts::PROBE_TIMEOUT;
    use peakbot_desktop_core::sys::Probe;
    use url::Url;

    let _guard = PORT_RACE_LOCK.lock().unwrap();
    let sys = RealSys::new();

    // A bound listener ⇒ Listening (the socket accepts, no HTTP is spoken).
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let probe_url = Url::parse(&format!("https://127.0.0.1:{port}/commands")).expect("url");
    assert_eq!(
        sys.probe(&probe_url),
        Probe::Listening,
        "a live TCP listener must answer the https probe as Listening"
    );

    // A closed port ⇒ Unreachable, and it must fail fast.
    drop(listener);
    // Sibling tests in this binary fork child processes; the forked child
    // holds a copy of our listener fd (fork → exec/exec-exit), which can
    // keep the socket in LISTEN for a few ms after the drop. Wait (bounded)
    // until the port is actually refused so the probe result is
    // deterministic — a test-harness race, not a production one. The
    // sibling children here live at most ~500 ms, so 3 s is generous.
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("a loopback addr is valid");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "port {port} stayed in LISTEN past 3 s after the drop"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let start = std::time::Instant::now();
    assert_eq!(
        sys.probe(&probe_url),
        Probe::Unreachable,
        "a closed port must be Unreachable"
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed <= PROBE_TIMEOUT + Duration::from_secs(2),
        "the failed connect must fail fast (≤ PROBE_TIMEOUT + slack); took {elapsed:?}"
    );
}
