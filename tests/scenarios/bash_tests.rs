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

// ── Execution behaviour tests ───────────────────────────────────────────────

/// Stdout and stderr are both captured and labelled.
#[tokio::test]
async fn bash_captures_stdout_and_stderr() {
    let out = run_bash("echo stdout-line; echo stderr-line >&2", 5).await;
    assert!(
        out.contains("stdout-line"),
        "stdout must be captured; got:\n{}",
        out
    );
    assert!(
        out.contains("stderr-line"),
        "stderr must be captured; got:\n{}",
        out
    );
    assert!(
        out.contains("Exit code: 0"),
        "expected exit 0; got:\n{}",
        out
    );
}

/// Non-zero exit codes are reported to the model.
#[tokio::test]
async fn bash_reports_nonzero_exit_code() {
    let out = run_bash("exit 42", 5).await;
    assert!(
        out.contains("Exit code: 42"),
        "expected exit 42; got:\n{}",
        out
    );
}

/// The `head` parameter truncates output to the first N lines.
/// `tail: 0` disables the default tail truncation so head works in isolation.
#[tokio::test]
async fn bash_head_truncates_to_first_n_lines() {
    let tool = BashTool::default();
    let payload = serde_json::to_string(&json!({
        "thought": "test head truncation",
        "command": "for i in 1 2 3 4 5; do echo line$i; done",
        "head": 2,
        "tail": 0,
    }))
    .expect("serialize");
    let out = ToolDyn::call(&tool, payload).await.expect("call");
    assert!(
        out.contains("line1"),
        "head=2 must include line1; got:\n{}",
        out
    );
    assert!(
        out.contains("line2"),
        "head=2 must include line2; got:\n{}",
        out
    );
    assert!(
        !out.contains("line3"),
        "head=2 must exclude line3; got:\n{}",
        out
    );
}

/// The `tail` parameter truncates output to the last N lines.
#[tokio::test]
async fn bash_tail_truncates_to_last_n_lines() {
    let tool = BashTool::default();
    let payload = serde_json::to_string(&json!({
        "thought": "test tail truncation",
        "command": "for i in 1 2 3 4 5; do echo line$i; done",
        "tail": 2,
    }))
    .expect("serialize");
    let out = ToolDyn::call(&tool, payload).await.expect("call");
    assert!(
        out.contains("line5"),
        "tail=2 must include line5; got:\n{}",
        out
    );
    assert!(
        out.contains("line4"),
        "tail=2 must include line4; got:\n{}",
        out
    );
    assert!(
        !out.contains("line1"),
        "tail=2 must exclude line1; got:\n{}",
        out
    );
}

/// File-editing pattern: `sed -i` triggers a warning.
#[tokio::test]
async fn bash_warns_on_sed_in_place() {
    let out = run_bash("sed -i 's/foo/bar/' /tmp/fake.txt || true", 5).await;
    assert!(
        out.contains("Consider using file_str_replace"),
        "sed -i should trigger a file-edit warning; got:\n{}",
        out
    );
}

/// File-editing pattern: `awk ... > file` triggers a warning.
#[tokio::test]
async fn bash_warns_on_awk_redirection() {
    let out = run_bash("awk '{print}' /etc/hosts > /tmp/out.txt || true", 5).await;
    assert!(
        out.contains("Consider using file_str_replace"),
        "awk redirection should trigger a file-edit warning; got:\n{}",
        out
    );
}

/// Commands that exceed the timeout are killed and a timeout message is returned.
#[tokio::test]
async fn bash_timeout_kills_long_running_command() {
    let tool = BashTool::default();
    let payload = serde_json::to_string(&json!({
        "thought": "test timeout",
        "command": "sleep 10",
        "timeout_seconds": 1,
    }))
    .expect("serialize");

    let start = Instant::now();
    let result = ToolDyn::call(&tool, payload).await;
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(3),
        "timed-out command should return quickly; took {:?}",
        elapsed
    );

    // Timeout returns an error (not a success string).
    let err = result.expect_err("timeout should return an error");
    let msg = format!("{err}");
    assert!(
        msg.contains("timed out"),
        "expected timeout message; got: {}",
        msg
    );
}
