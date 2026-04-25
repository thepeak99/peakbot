//! Bash tool TTY isolation tests.
//!
//! Guards the invariant that commands run via `BashTool` do NOT inherit the
//! parent's controlling TTY. This prevents `sudo`, `ssh`, `$EDITOR`, and
//! anything else that opens `/dev/tty` or calls `isatty(0)` from racing with
//! ratatui for stdin and corrupting termios state.
//!
//! See `better-tty.md` for the full rationale.

use peakbot::BashTool;
use rig::tool::ToolDyn;
use serde_json::json;
use std::time::{Duration, Instant};

/// Invoke the bash tool through `ToolDyn` (takes JSON, returns String).
/// Avoids needing to expose `BashArgs` outside the crate.
async fn run_bash(cmd: &str, timeout_seconds: u64) -> String {
    let tool = BashTool::default();
    let payload = serde_json::to_string(&json!({
        "thought": "bash tty isolation test",
        "command": cmd,
        "timeout_seconds": timeout_seconds,
    }))
    .expect("serialize bash args");
    ToolDyn::call(&tool, payload)
        .await
        .expect("bash tool call succeeded")
}

/// The core invariant: the child must not see a TTY on stdin.
///
/// `test -t 0` exits 0 iff stdin is a TTY. We want it to exit non-zero,
/// proving stdin was detached (null/pipe), not inherited from the parent.
#[tokio::test]
async fn bash_child_does_not_inherit_a_tty_on_stdin() {
    let out = run_bash("test -t 0; echo exit=$?", 5).await;
    assert!(
        out.contains("exit=1"),
        "child's stdin should not be a TTY, but `test -t 0` reported it is.\n\
         Full tool output:\n{}",
        out
    );
}

/// A command that reads stdin must not hang when there's no input to give.
///
/// `cat` with no args reads stdin until EOF. With a detached stdin, EOF is
/// immediate and `cat` exits 0 promptly. With a blocking inherited stdin it
/// would hang until the timeout.
#[tokio::test]
async fn bash_child_reading_stdin_returns_promptly() {
    let start = Instant::now();
    let out = run_bash("cat", 5).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "`cat` should return promptly when stdin is detached; took {:?}.\n\
         Full tool output:\n{}",
        elapsed,
        out
    );
    assert!(
        out.contains("Exit code: 0"),
        "`cat` should exit 0 on immediate EOF, got:\n{}",
        out
    );
}

/// Regression guard: detaching stdin must not break ordinary commands.
#[tokio::test]
async fn bash_echo_still_works() {
    let out = run_bash("echo hello", 5).await;
    assert!(
        out.contains("Exit code: 0"),
        "expected exit 0, got:\n{}",
        out
    );
    assert!(
        out.contains("hello"),
        "expected `hello` in output, got:\n{}",
        out
    );
}
