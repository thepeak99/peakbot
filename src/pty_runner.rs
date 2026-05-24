//! Shared PTY-spawning core used by both the synchronous `bash` tool and
//! the long-running `bash_bg` registry.
//!
//! ## Why share?
//!
//! Foreground bash and `bash_bg` **necessarily** need the same primitive:
//! spawn a child under a PTY (so `isatty()`-checking programs behave),
//! read its output line-by-line, strip ANSI, push into a capped ring
//! buffer, and SIGHUP it on drop. Maintaining two copies of this would
//! invite drift between them — exactly the failure mode the
//! "one buffer, two views" rule in `make-term-great-again.md` exists to
//! prevent.
//!
//! ## What's shared vs. what isn't
//!
//! - **Shared (this module):** PTY allocation, command spawn, reader
//!   thread (line split + ANSI strip + ring append), buffer cap +
//!   eviction, optional notify-channel ping, child SIGHUP on drop.
//! - **Not shared:** debounce policy, multi-process registry, circuit-
//!   breaker bookkeeping, tier flags, drain-and-clear semantics — all
//!   live in `bg_processes.rs` because they're registry-level concerns,
//!   not per-process.
//!
//! ## Lock discipline
//!
//! The reader thread holds `Arc<Mutex<LineBuffer>>`. All buffer mutation
//! happens under that lock. The lock is **never** held across an
//! `.await` — this module is entirely synchronous. Async callers wrap
//! blocking waits in `tokio::task::spawn_blocking`.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};
use tokio::sync::mpsc::UnboundedSender;

/// Maximum bytes per captured line before truncation kicks in. Protects
/// the LLM from log floods carrying a single multi-MB line.
pub const MAX_LINE_BYTES: usize = 4096;

// ── Public types ─────────────────────────────────────────────────────────

/// Spawn parameters. Mirrors the union of fields needed by both
/// foreground (`bash`) and background (`bash_bg`) callers.
pub struct SpawnParams {
    /// Shell command line passed verbatim as `<shell> -c <command>` (or
    /// `-Command` on PowerShell). The model owns the quoting.
    pub command: String,
    /// Optional working directory.
    pub cwd: Option<String>,
    /// Optional env overlay applied **after** inherited OS env, so these
    /// take precedence. Source is the `bash:` config section, identical
    /// for both tools.
    pub env: Option<std::collections::HashMap<String, String>>,
    /// Shell executable. Empty ⇒ defaults to `sh` for backward compat.
    pub shell: String,
    /// Ring-buffer capacity in lines. `0` disables capture (the reader
    /// still drains the PTY so the child doesn't block, but lines are
    /// discarded — useful for fire-and-forget watchers).
    pub capture_cap: usize,
    /// Optional debounce window for the notify channel. `None` ⇒ ping
    /// on every line. `Some(d)` ⇒ ping at most once per `d`.
    pub debounce: Option<Duration>,
}

/// Liveness status, updated by the reader thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyStatus {
    Running,
    Exited(i32),
}

impl PtyStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, PtyStatus::Running)
    }
}

/// Reader-thread-side state for a single process. Cheap to lock —
/// touches only Vec/usize fields, no IO, no async.
#[derive(Debug)]
pub struct LineBuffer {
    /// Line ring. Capped at `cap`; oldest evicted on overflow.
    pub lines: VecDeque<String>,
    /// Capacity of the ring (mirrors `SpawnParams::capture_cap`).
    pub cap: usize,
    /// Live status. Reader flips to `Exited` on EOF + `wait`.
    pub status: PtyStatus,
    /// `true` ⇒ buffer changed since the last consumer-driven clear.
    /// Set by the reader on every push and on exit; cleared by the
    /// consumer (e.g. `bg_processes::drain_outputs`).
    pub dirty: bool,
}

impl LineBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(1024)),
            cap,
            status: PtyStatus::Running,
            dirty: false,
        }
    }

    /// Append a line, honouring the cap. `cap == 0` discards the
    /// payload but still flags `dirty` so consumers can observe the
    /// "process produced something" signal (e.g. used for exit pings
    /// on capture-disabled bg processes).
    pub fn push_line(&mut self, line: String) {
        if self.cap == 0 {
            self.dirty = true;
            return;
        }
        let truncated = truncate_line(&line, MAX_LINE_BYTES);
        if self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(truncated);
        self.dirty = true;
    }
}

/// Errors surfaced by `spawn`. Variants are coach-friendly — they make
/// it back to the LLM unchanged.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("openpty: {0}")]
    OpenPty(String),
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("clone_reader: {0}")]
    CloneReader(String),
    #[error("take_writer: {0}")]
    TakeWriter(String),
}

/// Owning handle to a PTY-attached child. Dropping the handle SIGHUPs
/// the child and joins the reader thread (best-effort, no block).
pub struct PtyHandle {
    pub pid: u32,
    /// Shared with the reader thread. Consumers snapshot under this
    /// lock; the reader appends under the same lock.
    pub buffer: Arc<Mutex<LineBuffer>>,

    /// Writer end of the PTY. `take_writer` is one-shot on the master,
    /// so we own it after `spawn` and reuse on `write_stdin`.
    writer: Box<dyn Write + Send>,

    /// `ChildKiller` clone — `kill` sends `SIGHUP` on Unix / terminates
    /// on Windows. Held separately from the child handle (which moved
    /// into the reader thread for `wait`) so we can signal even after
    /// the reader joins.
    killer: Box<dyn ChildKiller + Send + Sync>,

    /// Reader thread join handle. Always `Some` while the handle is
    /// live; taken out by `Drop` for the join.
    reader: Option<JoinHandle<()>>,
}

impl PtyHandle {
    /// Write a line to the child's stdin. A trailing `\n` is appended
    /// if absent. Returns the number of bytes written.
    pub fn write_stdin(&mut self, line: &str) -> std::io::Result<usize> {
        let mut owned = line.to_string();
        if !owned.ends_with('\n') {
            owned.push('\n');
        }
        let bytes = owned.as_bytes();
        self.writer.write_all(bytes)?;
        self.writer.flush().ok();
        Ok(bytes.len())
    }

    /// Send SIGHUP / terminate. Idempotent; reader thread will observe
    /// EOF and flip status to `Exited` on its own.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.killer.kill()
    }

    /// Snapshot of the current status. Cheap — takes the buffer lock.
    pub fn status(&self) -> PtyStatus {
        self.buffer
            .lock()
            .map(|b| b.status.clone())
            .unwrap_or(PtyStatus::Exited(-1))
    }

    /// Detach the reader join handle so a drop won't try to join. The
    /// caller takes responsibility for letting the thread die when the
    /// `Arc` drops. Used by registry code that wants the reader to
    /// outlive the temporary handle.
    pub fn into_parts(mut self) -> PtyHandleParts {
        let reader = self.reader.take();
        // Replace writer + killer with no-op stand-ins so Drop is safe.
        // (Drop only kills via `killer` and joins `reader`; both are now
        // either inert or moved out.)
        let writer = std::mem::replace(&mut self.writer, Box::new(std::io::sink()));
        let killer = std::mem::replace(&mut self.killer, dummy_killer());
        PtyHandleParts {
            pid: self.pid,
            buffer: self.buffer.clone(),
            writer,
            killer,
            reader,
        }
    }
}

/// Owning pieces of a `PtyHandle` after detaching. Returned by
/// [`PtyHandle::into_parts`] so the caller can re-home them inside its
/// own struct (e.g. `bg_processes::BgProcess`) without paying for a
/// 5-tuple at every call site.
pub struct PtyHandleParts {
    pub pid: u32,
    pub buffer: Arc<Mutex<LineBuffer>>,
    pub writer: Box<dyn Write + Send>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
    pub reader: Option<JoinHandle<()>>,
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        if let Some(h) = self.reader.take() {
            // Best-effort join on a side thread so Drop doesn't block.
            std::thread::spawn(move || {
                let _ = h.join();
            });
        }
    }
}

/// A no-op killer used by `into_parts` to keep the `PtyHandle`'s `Drop`
/// inert after the real killer is moved out.
fn dummy_killer() -> Box<dyn ChildKiller + Send + Sync> {
    #[derive(Debug)]
    struct Noop;
    impl ChildKiller for Noop {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Noop)
        }
    }
    Box::new(Noop)
}

// ── Spawn entry point ────────────────────────────────────────────────────

/// Spawn a child under a PTY. Returns a [`PtyHandle`] that owns the
/// child, its writer, and the buffer the reader thread writes to.
///
/// If `notify_tx` is provided, the reader pings it whenever fresh
/// lines land (debounced per `SpawnParams::debounce` if set). Always
/// pings once on exit so consumers observe the terminal transition.
pub fn spawn(
    params: SpawnParams,
    notify_tx: Option<UnboundedSender<()>>,
) -> Result<PtyHandle, SpawnError> {
    let SpawnParams {
        command,
        cwd,
        env,
        shell,
        capture_cap,
        debounce,
    } = params;

    // Shell + arg flag — same logic that `bg_processes` had inline.
    let shell = if shell.is_empty() { "sh" } else { &shell };
    let lower = shell.to_lowercase();
    let is_powershell = lower.contains("pwsh") || lower.contains("powershell");
    let cmd_arg = if is_powershell { "-Command" } else { "-c" };

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| SpawnError::OpenPty(e.to_string()))?;

    let mut cmd = CommandBuilder::new(shell);
    cmd.arg(cmd_arg);
    cmd.arg(&command);
    if let Some(d) = cwd.as_ref() {
        cmd.cwd(d);
    }
    // Inherit env explicitly — portable-pty strips by default on some
    // platforms, which breaks `$PATH` and friends.
    for (k, v) in std::env::vars_os() {
        cmd.env(k, v);
    }
    if let Some(env_vars) = env {
        for (k, v) in env_vars {
            cmd.env(k, v);
        }
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| SpawnError::Spawn(e.to_string()))?;
    let pid = child.process_id().unwrap_or(0);
    let killer = child.clone_killer();

    // Drop the slave so EOF reaches the reader once the child exits.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| SpawnError::CloneReader(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| SpawnError::TakeWriter(e.to_string()))?;

    let buffer = Arc::new(Mutex::new(LineBuffer::new(capture_cap)));
    let reader_handle = spawn_reader(reader, buffer.clone(), child, notify_tx, debounce);

    Ok(PtyHandle {
        pid,
        buffer,
        writer,
        killer,
        reader: Some(reader_handle),
    })
}

/// Per-process reader thread. Streams the PTY master into the line
/// buffer, splits on `\n`, strips ANSI, and pings `notify_tx` on
/// dirty (debounced if requested). On EOF: waits the child, flips
/// `status` to `Exited`, sets `dirty`, and pings once unconditionally.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    buffer: Arc<Mutex<LineBuffer>>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    notify_tx: Option<UnboundedSender<()>>,
    debounce: Option<Duration>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut leftover: Vec<u8> = Vec::new();
        let mut last_notify: Option<Instant> = None;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    leftover.extend_from_slice(&buf[..n]);
                    let mut any_new_line = false;
                    while let Some(pos) = leftover.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = leftover.drain(..=pos).collect();
                        let raw = String::from_utf8_lossy(&line_bytes);
                        let clean = strip_ansi(raw.trim_end_matches('\n'));
                        {
                            let mut b = buffer.lock().expect("pty buffer mutex poisoned");
                            b.push_line(clean);
                        }
                        any_new_line = true;
                    }
                    if any_new_line {
                        ping(&notify_tx, &mut last_notify, debounce);
                    }
                }
                Err(e) => {
                    tracing::debug!("pty reader read error: {e}");
                    break;
                }
            }
        }

        // Flush trailing leftover bytes as one final line (no newline).
        if !leftover.is_empty() {
            let raw = String::from_utf8_lossy(&leftover);
            let clean = strip_ansi(raw.trim_end_matches('\n'));
            if !clean.is_empty()
                && let Ok(mut b) = buffer.lock()
            {
                b.push_line(clean);
            }
        }

        // Reap exit code.
        let code = match child.wait() {
            Ok(status) => status.exit_code() as i32,
            Err(_) => -1,
        };
        if let Ok(mut b) = buffer.lock() {
            b.status = PtyStatus::Exited(code);
            b.dirty = true;
        }
        // Always ping once on exit so consumers observe the transition,
        // even if debounce is suppressing other pings.
        if let Some(tx) = &notify_tx {
            let _ = tx.send(());
        }
        let _ = last_notify; // silence "unused" when no debounce
    })
}

fn ping(
    notify_tx: &Option<UnboundedSender<()>>,
    last: &mut Option<Instant>,
    debounce: Option<Duration>,
) {
    let Some(tx) = notify_tx else { return };
    let now = Instant::now();
    let should = match (debounce, *last) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(d), Some(t)) => now.duration_since(t) >= d,
    };
    if should {
        let _ = tx.send(());
        *last = Some(now);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Strip ANSI escape sequences from a captured line. Conservative —
/// removes CSI (`ESC [ … final-byte`) and OSC (`ESC ] … BEL` / `ESC \`)
/// sequences, which together cover almost every colour / cursor-movement
/// escape an agent-driven shell command will emit.
pub fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI: ESC [ ... final byte in 0x40..=0x7E
                    let mut j = i + 2;
                    while j < bytes.len() && !(0x40..=0x7E).contains(&bytes[j]) {
                        j += 1;
                    }
                    i = j.saturating_add(1).min(bytes.len());
                    continue;
                }
                b']' => {
                    // OSC: ESC ] ... BEL or ESC \
                    let mut j = i + 2;
                    while j < bytes.len() && bytes[j] != 0x07 {
                        if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                            j += 1;
                            break;
                        }
                        j += 1;
                    }
                    i = j.saturating_add(1).min(bytes.len());
                    continue;
                }
                _ => {
                    // Bare ESC + single byte — skip the pair.
                    i += 2;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // PTY line discipline leaves a `\r` before `\n`; readers split on
    // `\n` and trim it themselves, but the trailing `\r` survives.
    let mut s = String::from_utf8_lossy(&out).into_owned();
    if s.ends_with('\r') {
        s.pop();
    }
    s
}

/// UTF-8-boundary-safe truncation with a `" … (truncated)"` suffix.
/// See the regression-pin notes in `memory.md`.
pub fn truncate_line(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let suffix = " … (truncated)";
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &s[..cut])
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn strip_ansi_removes_csi() {
        let s = "\x1b[31merror.log\x1b[0m";
        assert_eq!(strip_ansi(s), "error.log");
    }

    #[test]
    fn strip_ansi_passes_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn strip_ansi_drops_trailing_cr() {
        assert_eq!(strip_ansi("line\r"), "line");
    }

    #[test]
    fn truncate_line_short_input_passes_through() {
        assert_eq!(truncate_line("hi", MAX_LINE_BYTES), "hi");
    }

    #[test]
    fn truncate_line_long_input_gets_truncated_marker() {
        let s = "a".repeat(MAX_LINE_BYTES + 100);
        let t = truncate_line(&s, MAX_LINE_BYTES);
        assert!(t.ends_with(" … (truncated)"));
        assert!(t.len() <= MAX_LINE_BYTES);
    }

    #[test]
    fn truncate_line_respects_utf8_boundaries() {
        // 4-byte emoji + 1-byte prefix shifts boundaries by 1 so a
        // naive byte cut would land mid-codepoint.
        let s = format!("a{}", "🦀".repeat(2000));
        let t = truncate_line(&s, MAX_LINE_BYTES);
        assert!(t.is_char_boundary(t.len() - " … (truncated)".len()));
    }

    #[test]
    fn line_buffer_evicts_oldest_on_overflow() {
        let mut b = LineBuffer::new(3);
        for i in 0..5 {
            b.push_line(format!("line {i}"));
        }
        assert_eq!(b.lines.len(), 3);
        assert_eq!(b.lines[0], "line 2");
        assert_eq!(b.lines[2], "line 4");
        assert!(b.dirty);
    }

    #[test]
    fn line_buffer_capacity_zero_drops_payload_but_marks_dirty() {
        let mut b = LineBuffer::new(0);
        b.push_line("anything".into());
        assert!(b.lines.is_empty());
        assert!(b.dirty);
    }

    /// End-to-end: spawn a trivial child, collect output, verify exit.
    /// Skipped on platforms without a usable PTY (extremely rare on
    /// hosted CI, but defensive).
    #[test]
    fn spawn_echo_collects_output_and_exits_zero() {
        let handle = spawn(
            SpawnParams {
                command: "echo hello-pty".into(),
                cwd: None,
                env: None,
                shell: String::new(),
                capture_cap: 10,
                debounce: None,
            },
            None,
        );
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                eprintln!("skipping pty test: {e}");
                return;
            }
        };

        // Wait up to 2s for the child to exit.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if !handle.status().is_running() {
                break;
            }
            if Instant::now() >= deadline {
                panic!("echo did not exit within 2s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let buf = handle.buffer.lock().unwrap();
        assert_eq!(buf.status, PtyStatus::Exited(0));
        let joined: String = buf.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains("hello-pty"),
            "buffer was: {joined:?}; lines: {:?}",
            buf.lines
        );
    }

    #[test]
    fn notify_channel_pings_on_exit_even_without_lines() {
        // capture_cap: 0 ⇒ no lines retained. We must still get the
        // exit ping so consumers learn the process is gone.
        let (tx, mut rx) = unbounded_channel();
        let handle = match spawn(
            SpawnParams {
                command: "true".into(),
                cwd: None,
                env: None,
                shell: String::new(),
                capture_cap: 0,
                debounce: None,
            },
            Some(tx),
        ) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("skipping pty test: {e}");
                return;
            }
        };

        let deadline = Instant::now() + Duration::from_secs(2);
        while handle.status().is_running() {
            if Instant::now() >= deadline {
                panic!("true did not exit within 2s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Drain the channel — at least one ping must have landed.
        let mut got_any = false;
        while rx.try_recv().is_ok() {
            got_any = true;
        }
        assert!(got_any, "expected at least one notify ping on exit");
    }
}
