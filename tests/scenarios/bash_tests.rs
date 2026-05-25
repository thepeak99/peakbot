//! Bash tool PTY behaviour tests.
//!
//! Slice 3 of `make-term-great-again.md` flipped the bash tool from
//! `Stdio::null()` + piped stdout/stderr to a full PTY-backed runner.
//! These tests guard the **new** contract:
//!
//! - the child sees a real TTY on stdin (so `isatty()`, `ls --color=auto`,
//!   `sudo`, `ssh` host-key prompts behave correctly);
//! - stdout and stderr are interleaved into a single OUTPUT block;
//! - commands that block reading stdin will sit until `timeout_seconds`
//!   elapses (the model is responsible for not running bare interactive
//!   programs — pipe input in instead);
//! - exit codes, head/tail truncation, and file-edit warnings still work.

use peakbot::BashTool;
use rig::tool::ToolDyn;
use serde_json::json;
use std::time::{Duration, Instant};

/// Invoke the bash tool through `ToolDyn` (takes JSON, returns String).
/// Avoids needing to expose `BashArgs` outside the crate.
async fn run_bash(cmd: &str, timeout_seconds: u64) -> String {
    let tool = BashTool::default();
    let payload = serde_json::to_string(&json!({
        "thought": "bash pty behaviour test",
        "command": cmd,
        "timeout_seconds": timeout_seconds,
    }))
    .expect("serialize bash args");
    ToolDyn::call(&tool, payload)
        .await
        .expect("bash tool call succeeded")
}

/// The PTY contract: the child DOES see a TTY on stdin. This is the
/// whole point of slice 3 — `sudo`, `ssh`, and `git push` credential
/// prompts now work because programs calling `isatty(0)` see a real
/// terminal.
///
/// `test -t 0` exits 0 iff stdin is a TTY. Under PTY, it must exit 0.
#[tokio::test]
async fn bash_child_sees_a_tty_on_stdin() {
    let out = run_bash("test -t 0; echo exit=$?", 5).await;
    assert!(
        out.contains("exit=0"),
        "PTY-backed child must see a TTY on stdin (`test -t 0` should exit 0).\n\
         Full tool output:\n{}",
        out
    );
}

/// Regression guard: PTY allocation must not break ordinary commands.
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

/// Stdout and stderr are both captured. Under PTY they're interleaved
/// into a single OUTPUT block (one tty, one byte stream) — the test
/// just checks both lines reach the result.
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

/// PTY merges stdout and stderr into a single OUTPUT block — the tool
/// result must NOT carry separate `STDOUT:` / `STDERR:` headers, since
/// that contract no longer holds.
#[tokio::test]
async fn bash_result_uses_combined_output_block() {
    let out = run_bash("echo on-stdout; echo on-stderr >&2", 5).await;
    assert!(
        out.contains("OUTPUT:"),
        "PTY result must use a single OUTPUT block; got:\n{}",
        out
    );
    assert!(
        !out.contains("STDOUT:") && !out.contains("STDERR:"),
        "PTY result must NOT carry legacy STDOUT/STDERR headers; got:\n{}",
        out
    );
}

/// Piped stdin still works inside the shell command — `echo x | cat`
/// finishes promptly because `cat` reads from the pipe, not the PTY.
/// This is the documented "use a pipe" recipe in the tool description.
#[tokio::test]
async fn bash_piped_stdin_works_under_pty() {
    let start = Instant::now();
    let out = run_bash("echo hello-pipe | cat", 5).await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "piped stdin should finish promptly; took {:?}; got:\n{}",
        elapsed,
        out
    );
    assert!(
        out.contains("hello-pipe"),
        "piped output should reach the tool result; got:\n{}",
        out
    );
}

/// Slice 4 cardinal pin — stdin forwarding reaches the child under
/// the PTY. We spawn a `read x; echo "got: $x"` script, register a
/// stdin sender via `StateManager::set_bash_stdin_tx` (done by the
/// tool internally — we just verify it's active), then push a line
/// from outside the call() future and assert the child consumed it.
#[tokio::test]
async fn bash_stdin_forward_reaches_child_under_pty() {
    use peakbot::StateManager;
    use std::sync::Arc;

    let sm = Arc::new(StateManager::new());
    let tool = BashTool::default().with_state_manager(sm.clone());

    let payload = serde_json::to_string(&json!({
        "thought": "slice 4 cardinal: stdin reaches the child",
        "command": "read x; echo \"got: $x\"",
        "timeout_seconds": 5,
        "tail": 0,
    }))
    .expect("serialize");

    // Spawn the tool call so we can interact from the test task.
    let sm_for_call = sm.clone();
    let tool_handle = tokio::spawn(async move {
        let _ = sm_for_call; // keep the Arc alive on the call task
        ToolDyn::call(&tool, payload).await.expect("call")
    });

    // Wait until the tool registers its stdin tx (race-tight: the
    // child is spawned and the wait loop is entered). 1s should be
    // overkill on any sane runner.
    let started = Instant::now();
    while !sm.has_active_bash_stdin() {
        if started.elapsed() > Duration::from_secs(2) {
            panic!("tool never registered stdin tx within 2s");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Forward the line. write_stdin appends '\n' so we don't need to.
    sm.try_forward_bash_stdin("hello".to_string())
        .expect("forward should succeed while child is reading");

    let out = tool_handle.await.expect("tool task panicked");
    assert!(
        out.contains("got: hello"),
        "tool result should contain `got: hello`; got:\n{}",
        out
    );
}

/// After the tool call returns, the stdin tx slot must be empty.
/// Proves the `clear_bash_stdin_tx()` call on the loop exit path
/// actually runs.
#[tokio::test]
async fn bash_stdin_tx_cleared_after_exit() {
    use peakbot::StateManager;
    use std::sync::Arc;

    let sm = Arc::new(StateManager::new());
    let tool = BashTool::default().with_state_manager(sm.clone());

    assert!(
        !sm.has_active_bash_stdin(),
        "slot should start empty before any call"
    );

    let payload = serde_json::to_string(&json!({
        "thought": "stdin tx cleanup after exit",
        "command": "echo hello",
        "timeout_seconds": 5,
    }))
    .expect("serialize");
    let _ = ToolDyn::call(&tool, payload).await.expect("call");

    assert!(
        !sm.has_active_bash_stdin(),
        "slot should be cleared after the call returns"
    );
}

/// No-echo suppression: the PTY honours `termios ECHO off` set by
/// `read -s`, so a forwarded password never echoes back into the
/// output buffer / tool result. This pin makes "no masked-input mode
/// needed" a hard contract, not a future-verify claim.
#[tokio::test]
async fn bash_stdin_no_echo_suppressed_under_pty() {
    use peakbot::StateManager;
    use std::sync::Arc;

    let sm = Arc::new(StateManager::new());
    let tool = BashTool::default().with_state_manager(sm.clone());

    let payload = serde_json::to_string(&json!({
        "thought": "slice 4 no-echo invariant under PTY",
        // POSIX-portable equivalent of `read -s`: disable echo on the
        // tty before reading, restore after. Plain `read -s` doesn't
        // work under dash (CI image's /bin/sh), but `stty -echo` is
        // honoured the same way by the PTY layer and proves the same
        // invariant: the password bytes never echo into the output
        // stream. The trailing `echo got: $pw` confirms the bytes
        // *did* reach the shell.
        "command": "stty -echo; read pw; stty echo; echo \"got: $pw\"",
        "timeout_seconds": 5,
        "tail": 0,
    }))
    .expect("serialize");

    let sm_for_call = sm.clone();
    let tool_handle = tokio::spawn(async move {
        let _ = sm_for_call;
        ToolDyn::call(&tool, payload).await.expect("call")
    });

    let started = Instant::now();
    while !sm.has_active_bash_stdin() {
        if started.elapsed() > Duration::from_secs(2) {
            panic!("tool never registered stdin tx within 2s");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    sm.try_forward_bash_stdin("s3cret".to_string())
        .expect("forward should succeed while child is reading");

    let out = tool_handle.await.expect("tool task panicked");

    // The echo from `echo "got: $pw"` must contain the password —
    // that proves the byte stream reached the shell.
    assert!(
        out.contains("got: s3cret"),
        "tool result should contain `got: s3cret`; got:\n{}",
        out
    );

    // But the typed characters themselves must NOT appear on a line
    // of their own (or as a prefix that wasn't part of "got: …").
    // Strip the only legitimate occurrence and assert no other
    // `s3cret` survives in the output.
    let scrubbed = out.replace("got: s3cret", "got: [REDACTED]");
    assert!(
        !scrubbed.contains("s3cret"),
        "echo leaked the typed password into the output stream; \
         the PTY did not honour ECHO off. Output:\n{}",
        out
    );
}

/// The cardinal rule of `make-term-great-again.md`: one buffer, two
/// views. When wired to a `StateManager`, every line the panel saw
/// must also appear in the tool result — same bytes, presented twice.
#[tokio::test]
async fn bash_panel_and_tool_result_share_the_same_bytes() {
    use peakbot::StateManager;
    use std::sync::Arc;

    let sm = Arc::new(StateManager::new());
    let tool = BashTool::default().with_state_manager(sm.clone());

    let payload = serde_json::to_string(&json!({
        "thought": "one-buffer two-views invariant",
        "command": "for i in 1 2 3; do echo invariant-line-$i; done",
        "timeout_seconds": 5,
        // Disable tail truncation so the result mirrors the buffer exactly.
        "tail": 0,
    }))
    .expect("serialize");
    let out = ToolDyn::call(&tool, payload).await.expect("call");

    // Tool result must contain every produced line.
    for i in 1..=3 {
        assert!(
            out.contains(&format!("invariant-line-{}", i)),
            "tool result missing line {}; got:\n{}",
            i,
            out
        );
    }

    // Panel snapshot must be in Finished state with the tail mirroring
    // the same lines (last 5 — we produced 3, so all three).
    let snap = sm.get_state();
    match snap.bash_panel {
        peakbot::ui::app_state::BashPanelState::Finished {
            exit_code, tail, ..
        } => {
            assert_eq!(
                exit_code, 0,
                "panel should record exit 0; got {}",
                exit_code
            );
            for i in 1..=3 {
                let needle = format!("invariant-line-{}", i);
                assert!(
                    tail.iter().any(|l| l.contains(&needle)),
                    "panel tail missing line {} (panel saw {:?})",
                    i,
                    tail
                );
            }
        }
        other => panic!(
            "panel should have transitioned to Finished after exec; saw {:?}",
            other
        ),
    }
}
