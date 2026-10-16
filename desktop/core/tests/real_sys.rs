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

#[test]
fn real_sys_run_captures_stdout_stderr_and_exit_code() {
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

#[test]
fn real_sys_free_port_is_bindable() {
    let port = RealSys::new().free_port().expect("free_port");
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("the returned port must be bindable");
    drop(listener);
}

// §9.1 case 3.
#[test]
fn real_sys_random_token_is_64_lowercase_hex_and_distinct() {
    let sys = RealSys::new();
    let a = sys.random_token();
    let b = sys.random_token();
    let (ta, tb) = (a.as_str().to_string(), b.as_str().to_string());
    assert_eq!(ta.len(), 64, "32 random bytes = 64 hex chars; got {ta:?}");
    assert!(
        ta.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "lowercase hex only; got {ta:?}"
    );
    assert_ne!(ta, tb, "two calls must differ");
}
