use crate::pty_runner::{self, PtyStatus, SpawnError, SpawnParams};
use crate::state::StateManager;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 7200; // 2 hours
const TEMP_DIR_NAME: &str = "peakbot";

/// Line-buffer cap for the foreground `bash` tool. A generous ring so
/// long-running builds don't lose their preamble before we serialise the
/// final tool result. Matched to the existing ~50k-char output budget at
/// the model boundary (≈ 80 cols × 10_000 lines worst-case).
const BASH_CAPTURE_CAP: usize = 10_000;

/// Debounce for live panel updates. Mirrors `bash_bg`'s 500 ms shape
/// but tighter — foreground bash has a human watching, so we trade a
/// bit more CPU for snappier feedback. The exit ping always lands
/// regardless of debounce (see `pty_runner::spawn_reader`).
const PANEL_UPDATE_DEBOUNCE: Duration = Duration::from_millis(200);

/// Grace window after a timeout-kill, waiting for the reader to flush
/// the final exit notification. Capped so a wedged child can't pin the
/// tool call open forever.
const POST_KILL_GRACE: Duration = Duration::from_millis(500);

/// Tail rows mirrored into the live panel via
/// [`StateManager::update_bash_panel_tail`]. Sized for the **web** panel,
/// which renders a scrollable buffer (issue #121); the TUI renderer clips
/// this to its own fixed `TAIL_ROWS` (5) at draw time, so a larger value
/// here only affects how much scrollback the web UI can show — it never
/// changes the terminal panel's height.
const PANEL_TAIL_ROWS: usize = 500;

/// Session-unique counter for generating output filenames
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub enum BashError {
    #[error("{0}")]
    Execution(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pty spawn failed: {0}")]
    Spawn(#[from] SpawnError),
}

#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
    timeout_seconds: Option<u64>,
    /// Show first N lines of output (optional)
    head: Option<usize>,
    /// Show last N lines of output (default: 100, use 0 for all)
    tail: Option<usize>,
}

#[derive(Clone)]
pub struct BashTool {
    /// Shell executable path (e.g. "/bin/sh" or "C:\Program Files\Git\bin\bash.exe")
    shell: String,
    /// Optional environment variables to set for the command
    env: Option<HashMap<String, String>>,
    /// Optional handle for live panel updates (`start/update/finish_bash_panel`).
    /// `None` in test paths and when the agent is built without a panel —
    /// the tool still runs, just without the live UI side-effects.
    state_manager: Option<Arc<StateManager>>,
    /// The per-session working directory every call is spawned in. Set
    /// explicitly at construction (defaults to the process cwd via `new`/
    /// `Default`); an in-command `cd` dies with the child, so each call
    /// starts fresh here.
    session_cwd: PathBuf,
}

impl Default for BashTool {
    fn default() -> Self {
        Self {
            shell: "/bin/sh".to_string(),
            env: None,
            state_manager: None,
            session_cwd: std::env::current_dir().unwrap_or_default(),
        }
    }
}

impl BashTool {
    /// Create a new BashTool with the given shell path and environment variables.
    /// No panel updates — wire one in via [`Self::with_state_manager`].
    pub fn new(shell: String, env: Option<HashMap<String, String>>) -> Self {
        Self {
            shell,
            env,
            state_manager: None,
            session_cwd: std::env::current_dir().unwrap_or_default(),
        }
    }

    /// Root every spawned child at `dir` (the per-session working directory).
    /// Each call is a fresh `sh -c` rooted here; an in-command `cd` dies with
    /// the child.
    pub fn with_session_cwd(mut self, dir: PathBuf) -> Self {
        self.session_cwd = dir;
        self
    }

    /// Attach a state manager so this tool drives the live bash panel
    /// (slice 3 of `make-term-great-again.md`). When attached, every
    /// call transitions the panel `Idle → Running → Finished` and pushes
    /// debounced tail updates as the child produces output.
    pub fn with_state_manager(mut self, sm: Arc<StateManager>) -> Self {
        self.state_manager = Some(sm);
        self
    }

    /// Whether this tool drives a live bash panel. Sub-agents build their
    /// bash tool without a state manager so their shell output never bleeds
    /// into the orchestrator's panel — this accessor lets that invariant be
    /// asserted directly.
    #[cfg(test)]
    pub(crate) fn has_state_manager(&self) -> bool {
        self.state_manager.is_some()
    }

    /// Detect if the command appears to be doing file editing
    /// Returns a warning message if file-editing patterns are detected
    fn check_file_edit_patterns(&self, command: &str) -> Option<String> {
        let command_lower = command.to_lowercase();

        // Check for common file-editing bash patterns
        if command_lower.contains("sed -i") {
            return Some(
                self.file_edit_warning("sed -i for in-place file editing", "file_str_replace"),
            );
        }

        // Check for awk with output redirection (awk ... > file)
        if command_lower.contains("awk") && command.contains(">") {
            return Some(self.file_edit_warning("awk for file modification", "file_str_replace"));
        }

        if command_lower.contains("perl -pi") {
            return Some(
                self.file_edit_warning("perl for in-place file editing", "file_str_replace"),
            );
        }

        if command_lower.contains("ex +") && command.contains("%") {
            return Some(self.file_edit_warning(
                "vim/ex for file editing",
                "the file editing tools (file_create / file_str_replace / file_insert)",
            ));
        }

        if command_lower.contains("vi -c") {
            return Some(self.file_edit_warning(
                "vi for file editing",
                "the file editing tools (file_create / file_str_replace / file_insert)",
            ));
        }

        None
    }

    /// Generate a standardized warning message for file-editing bash commands
    fn file_edit_warning(&self, description: &str, tool_suggestion: &str) -> String {
        format!(
            "⚠️  Consider using {tool} instead of {description} for file modifications.\n\
            \nThe file editing tools provide:\n\
            - Safe diffs for review\n\
            - Cross-platform compatibility\n\
            - Automatic whitespace handling\n\
            \nThis command will execute, but {tool} is recommended for file content modifications.\n\
            Use bash ONLY for: file operations (mv/cp/rm), permissions, bulk operations on many files.",
            tool = tool_suggestion,
            description = description
        )
    }
}

/// Save the full PTY output to a temp file and return the path.
///
/// PTY merges stdout and stderr into a single stream (one tty, one byte
/// pipe). Mirrors the "one buffer, two views" rule from
/// `make-term-great-again.md` — same bytes the panel sees, persisted.
fn save_full_output(output: &str) -> std::io::Result<PathBuf> {
    let temp_dir = std::env::temp_dir().join(TEMP_DIR_NAME);
    std::fs::create_dir_all(&temp_dir)?;

    let counter = SESSION_COUNTER.fetch_add(1, Ordering::SeqCst);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let session_id = format!("{}_{}", timestamp, counter);
    let path = temp_dir.join(format!("bash_{}.output.txt", session_id));
    std::fs::write(&path, output)?;
    Ok(path)
}

/// Apply head/tail line truncation to output
/// Returns (displayed_output, was_modified)
fn apply_head_tail(s: &str, head: Option<usize>, tail: Option<usize>) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    let total_lines = lines.len();

    // No truncation needed
    if head.is_none() && tail.is_none() {
        return (s.to_string(), false);
    }

    // Only head specified
    if let Some(h) = head
        && tail.is_none()
    {
        if total_lines <= h {
            return (s.to_string(), false);
        }
        return (lines[..h].join("\n"), true);
    }

    // Only tail specified (default 100)
    if head.is_none() {
        let t = tail.unwrap_or(100);
        if t == 0 || total_lines <= t {
            return (s.to_string(), false);
        }
        return (lines[total_lines - t..].join("\n"), true);
    }

    // Both head and tail specified
    let h = head.unwrap_or(usize::MAX);
    let t = tail.unwrap_or(100);

    // If lines fit in head + tail, show all
    if total_lines <= h + t {
        return (s.to_string(), false);
    }

    // Need to truncate: head at top, tail at bottom
    let head_lines = &lines[..h];
    let tail_lines = &lines[total_lines - t..];
    let middle_count = total_lines - h - t;

    (
        format!(
            "{}\n... {} lines in between ...\n{}",
            head_lines.join("\n"),
            middle_count,
            tail_lines.join("\n")
        ),
        true,
    )
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";
    type Error = BashError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let shell_name = std::path::Path::new(&self.shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sh");
        ToolDefinition {
            name: "bash".to_string(),
            description: format!(
                "Run a shell command under a pseudo-terminal and return its output. \
                stdout and stderr are interleaved into a single OUTPUT stream (PTY semantics); \
                the child sees a real TTY so programs that check `isatty()` behave normally \
                (`ls --color=auto`, `sudo`, `ssh`, `git push` credential prompts). \
                Live output is mirrored to the on-screen bash panel while the command runs. \
                Use `head` to show first N lines, `tail` to show last N lines (default: 100). \
                Full output is always saved to /tmp/peakbot/ and accessible via file_read. \
                Commands run in {}. Default timeout is 30 seconds. \
                Note: commands that block reading stdin (e.g. bare `cat`) will hang until \
                timeout — pipe input in (`echo x | cat`) or redirect from a file.",
                shell_name
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Optional timeout in seconds (default: 30, max: 7200 = 2 hours)"
                    },
                    "head": {
                        "type": "integer",
                        "description": "Show first N lines of output (optional)"
                    },
                    "tail": {
                        "type": "integer",
                        "description": "Show last N lines of output (default: 100, use 0 for all)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Check for file-editing patterns and add warning if detected
        let warning = self.check_file_edit_patterns(&args.command);

        let timeout_secs = args
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        tracing::info!(
            target: "peakbot",
            tool_type = "bash",
            command = %args.command,
            timeout_secs = timeout_secs,
            env_vars = ?self.env.as_ref().map(|e| e.keys().collect::<Vec<_>>()),
            "Starting bash tool execution (PTY)"
        );

        let start_time = Instant::now();

        // Spawn under a PTY. The reader thread streams output into the
        // shared buffer, ANSI-stripped, line by line. `notify_tx` pings
        // (debounced) as fresh output lands and once unconditionally on
        // exit — see `pty_runner::spawn_reader`.
        let (notify_tx, mut notify_rx) = unbounded_channel::<()>();
        let mut handle = pty_runner::spawn(
            SpawnParams {
                command: args.command.clone(),
                cwd: (!self.session_cwd.as_os_str().is_empty())
                    .then(|| self.session_cwd.to_string_lossy().into_owned()),
                env: self.env.clone(),
                shell: self.shell.clone(),
                capture_cap: BASH_CAPTURE_CAP,
                debounce: Some(PANEL_UPDATE_DEBOUNCE),
            },
            Some(notify_tx),
        )?;
        let pid = handle.pid;
        let buffer = handle.buffer.clone();

        // Panel goes Idle → Running. Transitions are no-ops without a
        // state manager — the test path (`BashTool::default()`) takes
        // this branch silently.
        if let Some(sm) = &self.state_manager {
            sm.start_bash_panel(args.command.clone(), pid);
        }

        // Stdin forwarding channel (slice 4). Per-call: UI registers
        // here, pushes typed lines, the select! arm below drains and
        // writes to the PTY master. Receiver lives on the stack; the
        // sender goes into the state manager so the REPL can find it
        // via `try_forward_bash_stdin`. Cleared **before**
        // `finish_bash_panel` on the loop exit path so a late UI send
        // during the Running → Finished window can't land on a dropped
        // receiver.
        let mut stdin_rx = if self.state_manager.is_some() {
            let (stdin_tx, stdin_rx) = unbounded_channel::<String>();
            if let Some(sm) = &self.state_manager {
                sm.set_bash_stdin_tx(stdin_tx);
            }
            Some(stdin_rx)
        } else {
            None
        };

        // Wait loop: pump notify pings into panel tail updates; break
        // on the exit notification or hit the timeout. The buffer lock
        // is only held inside scoped blocks — never across `.await`.
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let mut killed = false;
        let exit_code: i32 = loop {
            let now = Instant::now();
            let wait = if killed {
                POST_KILL_GRACE
            } else if now >= deadline {
                Duration::ZERO
            } else {
                deadline - now
            };

            // `tokio::select!` needs a concrete future per arm. When
            // there's no state manager (test path), the receiver is
            // absent — substitute a future that never resolves so the
            // arm is structurally present but never wins. Two-arm and
            // three-arm select! diverge in macro shape; keeping the
            // arm always-present is cheaper than duplicating the loop.
            let stdin_recv = async {
                match &mut stdin_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<String>>().await,
                }
            };

            tokio::select! {
                biased;
                ping = notify_rx.recv() => {
                    if ping.is_none() {
                        // Reader thread vanished without delivering an exit
                        // ping — treat as failure.
                        tracing::warn!(target: "peakbot", "pty notify channel closed unexpectedly");
                        break -1;
                    }
                    let (status, tail) = snapshot_for_panel(&buffer);
                    if let Some(sm) = &self.state_manager {
                        sm.update_bash_panel_tail(tail);
                    }
                    if let PtyStatus::Exited(code) = status {
                        break code;
                    }
                }
                line = stdin_recv => {
                    // `None` ⇒ UI dropped its sender (impossible while
                    // the slot is held in the state manager, but cheap
                    // to handle). The loop continues either way.
                    if let Some(line) = line {
                        // `write_stdin` appends `\n` if missing — see
                        // `pty_runner::PtyHandle::write_stdin`. Errors
                        // here mean the child closed stdin (e.g. exited
                        // mid-prompt). We swallow + log; the next loop
                        // iteration hits the exit ping and breaks
                        // cleanly with the real exit code.
                        if let Err(e) = handle.write_stdin(&line) {
                            tracing::warn!(
                                target: "peakbot",
                                error = %e,
                                "bash stdin forward failed (child likely closed stdin)"
                            );
                        }
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    if killed {
                        // Grace period exhausted; child is wedged. Give up
                        // and let `Drop` clean up.
                        break -1;
                    }
                    // First timeout — SIGHUP and wait for the exit ping.
                    let _ = handle.kill();
                    killed = true;
                }
            }
        };

        // Deregister the stdin sender BEFORE flipping the panel to
        // Finished. Ordering matters: any UI send arriving during the
        // tiny window between these two state changes will see a
        // panel still nominally Running but get `Err(StdinNotActive)`
        // back, which the UI handles by preserving the buffer (the
        // user retries on the next prompt). The inverse order would
        // let a send land on a dropped receiver after the panel
        // already says "Finished" — strictly worse.
        if let Some(sm) = &self.state_manager {
            sm.clear_bash_stdin_tx();
        }
        // Drop the receiver explicitly so any in-flight send observes
        // the channel closure deterministically.
        drop(stdin_rx);

        // Drain the final buffer for the tool result. Same bytes the
        // panel just saw — the "one buffer, two views" rule.
        let final_output = {
            let buf = buffer.lock().expect("pty buffer poisoned");
            buf.lines.iter().cloned().collect::<Vec<_>>().join("\n")
        };
        let final_tail_for_panel = {
            let buf = buffer.lock().expect("pty buffer poisoned");
            buf.lines
                .iter()
                .rev()
                .take(PANEL_TAIL_ROWS)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
        };

        // Panel goes Running → Finished. Carries the exit code and final
        // tail; the renderer freezes the strip until the next bash call.
        if let Some(sm) = &self.state_manager {
            sm.finish_bash_panel(exit_code, final_tail_for_panel);
        }

        // Explicitly drop the handle now to SIGHUP any lingering child
        // and join the reader thread. (Drop runs on return anyway, but
        // doing it here keeps the OS resource lifecycle obvious.)
        drop(handle);

        if killed {
            tracing::warn!(
                target: "peakbot",
                tool_type = "bash",
                timeout_secs = timeout_secs,
                "Bash tool timed out"
            );
            return Err(BashError::Execution(format!(
                "Command timed out after {} seconds. Consider increasing timeout_seconds.",
                timeout_secs
            )));
        }

        // Save the full PTY output to a temp file (always saved).
        let output_path = match save_full_output(&final_output) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    target: "peakbot",
                    tool_type = "bash",
                    error = %e,
                    "Failed to save full output to temp file"
                );
                PathBuf::new()
            }
        };

        // Apply head/tail truncation (defaults to tail: 100).
        let default_tail = Some(100);
        let (displayed, modified) =
            apply_head_tail(&final_output, args.head, args.tail.or(default_tail));

        let mut result = format!("Exit code: {}\n", exit_code);
        if !final_output.is_empty() {
            result.push_str(&format!("\nOUTPUT:\n{}\n", displayed));
        }

        if !output_path.as_os_str().is_empty() {
            let divider = "\n─────────────────────────────────────────────────\n";
            result.push_str(divider);
            result.push_str("Full output saved to:\n");
            result.push_str(&format!("  {}\n", output_path.display()));
            if !modified {
                result.push_str("(output was not truncated)\n");
            } else {
                result.push_str("Use file_read tool to access the complete output.\n");
            }
            result.push_str(divider);
        }

        if let Some(warning_msg) = warning {
            result.push_str(&format!("\n\n{}", warning_msg));
        }

        tracing::info!(
            target: "peakbot",
            tool_type = "bash",
            exit_code = exit_code,
            duration_ms = start_time.elapsed().as_millis(),
            output_modified = modified,
            output_path = %output_path.display(),
            "Bash tool completed successfully"
        );

        Ok(result)
    }
}

/// Snapshot the buffer for a live panel push: returns the current
/// status plus the last `PANEL_TAIL_ROWS` lines. Scoped so the buffer
/// lock is never held across `.await` (see `pty_runner` lock discipline).
fn snapshot_for_panel(
    buffer: &Arc<std::sync::Mutex<pty_runner::LineBuffer>>,
) -> (PtyStatus, Vec<String>) {
    let buf = buffer.lock().expect("pty buffer poisoned");
    let status = buf.status.clone();
    let tail: Vec<String> = buf
        .lines
        .iter()
        .rev()
        .take(PANEL_TAIL_ROWS)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (status, tail)
}

#[cfg(test)]
mod tests {
    //! Tests for issue #183 — "Stop button stops *everything*".
    //!
    //! The keystone assertion is `dropping_the_bash_call_future_kills_the_child`
    //! (T7): the design's entire `Stop = drop the turn's future` strategy rests
    //! on the fact that a `PtyHandle` owned by an `async fn` body lives in the
    //! future's state machine. Dropping the future therefore runs
    //! `PtyHandle::drop` → `killer.kill()` → SIGHUP to the PTY session leader.
    //! If T7 is false on the current code, the whole design is wrong (see
    //! design §9 R1 — fallback is a `Drop`-guard, not a token-in-every-tool).
    //!
    //! `cancelling_the_turn_kills_the_bash_child` (T6) is the production mirror:
    //! the test wraps the bash call in a `select!` against the turn-cancel token
    //! and asserts that `stop_turn_processes()` causes the future to be dropped
    //! (and therefore the child killed) within the design's 500 ms budget.

    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::time::sleep;

    /// Build a `BashArgs` whose child appends one timestamp line to `path` every
    /// 100 ms forever. Used by both T6 and T7 as a live-process probe: the
    /// child is alive iff the file is still growing.
    fn long_running_writer_args(path: &std::path::Path) -> BashArgs {
        BashArgs {
            command: format!(
                // `date` output is ~24-30 bytes depending on locale; 100 ms
                // cadence gives us a few hundred lines in the first 500 ms
                // window so a no-growth check has plenty of margin.
                "while true; do date >> \"{}\"; sleep 0.1; done",
                path.display()
            ),
            timeout_seconds: Some(300),
            head: None,
            tail: None,
        }
    }

    /// Cheap file-size probe used as the "is the child still alive?" signal.
    /// Avoids `libc::kill(pid, 0)` (no `libc` dev-dep — task says prefer
    /// `/proc` or file growth), and works on every Unix without a `/proc` mount.
    fn file_len(path: &std::path::Path) -> u64 {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }

    /// T7 — the keystone. Pins that dropping the `BashTool::call` future kills
    /// the spawned PTY child. Run this first, before any other #183 work, and
    /// report its result verbatim — the entire design rests on it.
    ///
    /// Mechanism (from design §0.1 + §9 R1): `BashTool::call` is an `async fn`
    /// whose body-local `handle: PtyHandle` lives in the generator state. The
    /// `PtyHandle::drop` impl (`pty_runner.rs:224-234`) calls
    /// `self.killer.kill()` (SIGHUP on Unix). So dropping the future at any
    /// await point should terminate the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_bash_call_future_kills_the_child() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("t7_bash_child.log");
        let args = long_running_writer_args(&log);

        let tool = BashTool::default();
        // `Box::pin` (not `tokio::pin!`) so that `drop(...)` actually drops
        // the future state. `tokio::pin!` produces a `Pin<&mut F>` over a
        // hidden variable — dropping that Pin only drops the reference, NOT
        // the underlying future. The whole design hinges on a *real* drop,
        // so we test the real one.
        let mut fut: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> =
            Box::pin(tool.call(args));

        // Let the child run for 500 ms — well past startup, so the PTY
        // session leader is established and the reader thread is draining.
        let timeout_outcome = tokio::time::timeout(Duration::from_millis(500), fut.as_mut()).await;
        assert!(
            timeout_outcome.is_err(),
            "the 500 ms timeout must fire (child never exits on its own); \
             if this fires, the test is racy or the child died prematurely"
        );

        // Real drop: drops the Box, which drops the future state, which
        // drops the `handle: PtyHandle` captured in the async-fn body, which
        // runs `PtyHandle::drop` → `killer.kill()` → SIGHUP.
        drop(fut);

        // Sanity: 500 ms of `date >> log; sleep 0.1` ⇒ file must be non-empty.
        let len_after_drop = file_len(&log);
        assert!(
            len_after_drop > 0,
            "the bash child should have written at least one line during \
             the 500 ms activity window (got {len_after_drop} bytes)"
        );

        // Give a wedged child plenty of time to keep writing. If the kill
        // mechanism (PtyHandle::drop → killer.kill() → SIGHUP) didn't fire,
        // the file will grow noticeably.
        sleep(Duration::from_secs(1)).await;
        let len_after_sleep = file_len(&log);
        assert_eq!(
            len_after_drop, len_after_sleep,
            "the bash child must NOT write after the future was dropped \
             (file grew from {len_after_drop} to {len_after_sleep} bytes \
             during the 1 s observation window — the child survived)"
        );
    }

    /// T6 — `cancelling_the_turn_kills_the_bash_child` (design §8, T6;
    /// ticket-named #2: "a synthetic bash child is killed when the
    /// cancellation token fires").
    ///
    /// Mirrors production: a `tokio::select!` races the bash-call future
    /// against the per-turn cancellation token. `stop_turn_processes()` must
    /// fire the cancel (design §4 step 5.2), the select's cancelled arm must
    /// win, the bash future must be dropped (T7's keystone property), and the
    /// underlying PTY child must die. The test asserts (a) the racing task
    /// joins within the design's 500 ms budget, and (b) the file the bash
    /// child was writing to stops growing.
    ///
    /// Against the current code's stubs: `stop_turn_processes` is a no-op,
    /// so the token never fires, the `cancelled()` future never resolves, the
    /// bash-call future keeps the child alive, the task never joins — the
    /// test goes RED on the `join.is_ok()` assertion. Against the
    /// implementation: the join resolves inside 500 ms, the file stops
    /// growing, GREEN.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_the_turn_kills_the_bash_child() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("t6_bash_child.log");
        let args = long_running_writer_args(&log);

        // Set up a real `StateManager` so we exercise the public cancel API
        // the design names (`StateManager::turn_cancel_token` /
        // `StateManager::stop_turn_processes`).
        let sm = std::sync::Arc::new(crate::state::StateManager::new_arc());
        // Production: mints the per-turn token. Stub: no-op.
        sm.set_running(true);
        let cancel = sm.turn_cancel_token();

        let tool = BashTool::default();
        // The future of the racing task.
        let task = tokio::spawn(async move {
            tokio::select! {
                biased;
                // Arm 1: the cancel token wins → returns None.
                _ = cancel.cancelled() => None,
                // Arm 2: the bash call future completes (it never does
                // here — the child runs forever) → returns Some(result).
                r = tool.call(args) => Some(r),
            }
        });

        // Let the bash child run for 500 ms so the file is provably alive
        // and growing when the stop lands.
        sleep(Duration::from_millis(500)).await;
        let len_before_stop = file_len(&log);
        assert!(
            len_before_stop > 0,
            "the bash child should have written at least one line \
             during the 500 ms activity window (got {len_before_stop} bytes)"
        );

        // Act — production: cancels the token → the racing task's
        // `cancelled()` arm wins → `tool.call` future is dropped → its
        // `PtyHandle::drop` fires → SIGHUP. Stub: no-op, the cancel
        // never fires, the task keeps running.
        sm.stop_turn_processes();

        // (a) The racing task must join within 500 ms.
        let join_outcome = tokio::time::timeout(Duration::from_millis(500), task).await;
        assert!(
            join_outcome.is_ok(),
            "the racing task must join within 500 ms after \
             stop_turn_processes() — the cancel token did not fire, so the \
             bash call future was never dropped, so the child kept running"
        );
        let join_outcome = join_outcome.unwrap();
        assert!(
            join_outcome.is_ok(),
            "the racing task itself must not panic; got: {:?}",
            join_outcome.err()
        );
        // Production: the select! returned None (cancel arm won).
        let _selected = join_outcome.unwrap();
        // (b) The file must stop growing.
        let len_after = file_len(&log);
        assert_eq!(
            len_before_stop, len_after,
            "the bash child must NOT write after the cancel; \
             file grew from {len_before_stop} to {len_after} bytes during \
             the join window — the child survived SIGHUP or SIGHUP wasn't sent"
        );
    }
}
