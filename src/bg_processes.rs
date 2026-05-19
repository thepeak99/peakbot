//! Background process registry for the `bash_bg` tool.
//!
//! Long-running PTY-attached processes managed by the agent. Each process
//! has a numeric id, an optional ring-buffer of captured output lines, and
//! a tier flag (`treat_as_user_input`) that determines whether its output
//! resets the synthetic-turn circuit breaker or contributes to it.
//!
//! ## Lifecycle
//!
//! 1. `BgRegistry::start` opens a PTY, spawns the child under it, and
//!    launches a reader thread that streams line-buffered output into a
//!    ring buffer and notifies the agent via a tokio mpsc when a new
//!    line lands.
//! 2. The reader debounces notifications: after the first line, it
//!    waits up to [`DEBOUNCE_MS`] before pinging again, so chatty
//!    processes don't generate a turn per line.
//! 3. Output is drained synchronously by `BgRegistry::drain_outputs`,
//!    called between agent turns. The drain assembles a `[bg output]`
//!    block per non-empty process and clears their rings.
//! 4. `BgRegistry::stop` (or `Drop`) sends `SIGHUP` via
//!    `portable-pty`'s `ChildKiller`, joins the reader, and removes the
//!    process from the registry.
//!
//! ## Lock discipline
//!
//! All public methods take `&self` and lock the inner `Mutex` briefly.
//! Reader threads acquire the same mutex on each line append. The
//! mutex is **never** held across an `.await` — the registry is a
//! synchronous data structure that lives behind an
//! `Arc<Mutex<BgRegistry>>` on `StateManager`. See the async-lock
//! discipline rules in `memory.md`.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use portable_pty::{ChildKiller, CommandBuilder, PtySize, native_pty_system};
use tokio::sync::mpsc::UnboundedSender;

/// Maximum bytes per captured line before truncation kicks in.
/// Protects the LLM from log floods carrying a single multi-MB line.
pub const MAX_LINE_BYTES: usize = 4096;

/// Default ring-buffer capacity when `capture_output_lines` is omitted.
pub const DEFAULT_CAPTURE_LINES: usize = 200;

/// Debounce window: the reader buffers fresh-line notifications for this long
/// before pinging the agent loop. Open question Q2 was answered "OK" at 500ms.
pub const DEBOUNCE_MS: u64 = 500;

/// Capped-tier circuit breaker: after this many consecutive synthetic turns
/// driven by capped-only processes (no `treat_as_user_input` contributor),
/// suppress auto-injection until a real user message or an unlimited-tier
/// contribution resets the counter. Open question Q1 was answered "OK" at 3.
pub const MAX_CONSECUTIVE_AUTO_TURNS: usize = 3;

/// Status of a background process.
#[derive(Debug, Clone)]
pub enum BgStatus {
    Running { since: DateTime<Utc> },
    Exited { code: i32, at: DateTime<Utc> },
}

impl BgStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, BgStatus::Running { .. })
    }
}

/// A single background process entry.
///
/// Held inside `BgRegistry`. The reader thread holds a separate
/// `Arc<Mutex<ProcessShared>>` clone to drop lines into the buffer
/// without re-locking the whole registry on every line.
pub struct BgProcess {
    pub id: u32,
    pub pid: u32,
    pub command: String,
    pub label: Option<String>,
    /// `true` ⇒ output represents external input (telegram, webhooks).
    /// Synthetic turns containing any contribution from such a process
    /// reset the circuit-breaker counter, exactly like a typed user
    /// message would.
    pub treat_as_user_input: bool,
    /// Ring capacity. `0` disables capture entirely (output is still
    /// drained from the PTY so the child doesn't block, but lines are
    /// discarded — useful for fire-and-forget watchers).
    pub capture_cap: usize,
    pub started_at: DateTime<Utc>,

    /// Shared with the reader thread: the line buffer + status + exit
    /// code land here when the reader observes them. The registry's
    /// `drain_outputs` snapshots this slot under the lock.
    shared: Arc<Mutex<ProcessShared>>,

    /// Writer end of the PTY — used by `send_line`.
    /// `take_writer` is one-shot on the master, so we own it after
    /// `start` and reuse on subsequent `send_line` calls.
    writer: Box<dyn Write + Send>,

    /// `ChildKiller` clone — calling `kill` sends `SIGHUP` on Unix /
    /// terminates the process on Windows. Held separately from the
    /// child handle (which moved into the reader thread for `wait`)
    /// so we can signal even after the reader joins.
    killer: Box<dyn ChildKiller + Send + Sync>,

    /// Reader thread join handle. Always `Some` while the process is
    /// in the registry; taken out by `Drop` for the join.
    reader: Option<JoinHandle<()>>,
}

/// Reader-thread-side state for a single process. Cheap to lock because
/// it only touches Vec/usize fields — no IO, no async.
#[derive(Debug)]
struct ProcessShared {
    /// Line ring. Capped at `capture_cap`; oldest evicted on overflow.
    buffer: VecDeque<String>,
    /// Capacity for the ring (mirrors `BgProcess::capture_cap`).
    capture_cap: usize,
    /// Live status. Reader flips to `Exited` on EOF + `wait`.
    status: BgStatus,
    /// True ⇒ buffer changed since last drain. Set by the reader,
    /// cleared by `drain_outputs`.
    dirty: bool,
}

impl ProcessShared {
    fn push_line(&mut self, line: String) {
        if self.capture_cap == 0 {
            // Capture disabled — still mark dirty so the agent gets
            // *some* signal (e.g. "process exited"), but no payload.
            self.dirty = true;
            return;
        }
        let truncated = truncate_line(&line, MAX_LINE_BYTES);
        if self.buffer.len() >= self.capture_cap {
            self.buffer.pop_front();
        }
        self.buffer.push_back(truncated);
        self.dirty = true;
    }
}

fn truncate_line(s: &str, max_bytes: usize) -> String {
    // Be UTF-8 boundary safe — see memory.md regression-pin arithmetic.
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let suffix = " … (truncated)";
    // Walk back from `max_bytes - suffix.len()` to the nearest char boundary.
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &s[..cut])
}

/// Strip ANSI escape sequences from a captured line so the LLM doesn't
/// see SGR garbage. Conservative — only removes CSI sequences
/// (`ESC [ … final-byte`). OSC and other less common sequences are
/// rare in practice for the kinds of commands the agent runs.
fn strip_ansi(s: &str) -> String {
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
    // Reader splits on `\n`; strip trailing `\r` left by PTY line discipline.
    let mut s = String::from_utf8_lossy(&out).into_owned();
    if s.ends_with('\r') {
        s.pop();
    }
    s
}

/// Arguments for `BgRegistry::start`.
pub struct StartParams {
    pub command: String,
    pub capture_cap: usize,
    pub cwd: Option<String>,
    pub label: Option<String>,
    pub treat_as_user_input: bool,
    /// Optional environment variables to set for the spawned process,
    /// inherited from the `bash:` config section (same source as `bash`).
    pub env: Option<std::collections::HashMap<String, String>>,
}

/// A drained chunk of output, one per contributing process.
#[derive(Debug, Clone)]
pub struct DrainedBlock {
    pub id: u32,
    pub command: String,
    pub label: Option<String>,
    pub treat_as_user_input: bool,
    pub status_after: BgStatus,
    pub lines: Vec<String>,
}

impl DrainedBlock {
    /// Whether this block represents an exited-since-last-drain process.
    #[allow(dead_code)] // public API hook for future renderers / tests
    pub fn exited(&self) -> bool {
        matches!(self.status_after, BgStatus::Exited { .. })
    }
}

/// Snapshot row returned by `list`. Cheap to compute under the registry lock.
#[derive(Debug, Clone)]
pub struct BgListEntry {
    pub id: u32,
    pub pid: u32,
    pub command: String,
    pub label: Option<String>,
    pub status: BgStatus,
    pub buffer_len: usize,
    pub capture_cap: usize,
    pub treat_as_user_input: bool,
}

/// Errors surfaced by registry operations. Each variant maps to a coach
/// message returned to the LLM (per the tool-design rules in `memory.md`).
#[derive(Debug, thiserror::Error)]
pub enum BgError {
    #[error("no background process with id {0}")]
    NotFound(u32),
    #[error("process #{0} has exited; start a new one")]
    Exited(u32),
    #[error("failed to spawn process: {0}")]
    Spawn(String),
    #[error("failed to write to process #{id}: {source}")]
    Write {
        id: u32,
        #[source]
        source: std::io::Error,
    },
}

/// The registry. Held as `Arc<Mutex<BgRegistry>>` on `StateManager`.
pub struct BgRegistry {
    procs: HashMap<u32, BgProcess>,
    next_id: u32,
    /// Counter for the capped-tier circuit breaker. Incremented when a
    /// synthetic turn drains capped-only contributions; reset by a real
    /// user message OR any unlimited-tier contribution.
    consecutive_capped_turns: usize,
}

impl BgRegistry {
    pub fn new() -> Self {
        Self {
            procs: HashMap::new(),
            next_id: 1,
            consecutive_capped_turns: 0,
        }
    }

    /// Number of currently-tracked processes (running + exited-but-not-drained).
    #[allow(dead_code)] // sibling-of-is_empty; future call sites
    pub fn len(&self) -> usize {
        self.procs.len()
    }

    /// `true` iff `len() == 0`.
    #[allow(dead_code)] // sibling-of-len; future call sites
    pub fn is_empty(&self) -> bool {
        self.procs.is_empty()
    }

    /// Running-only count (excludes exited rows waiting for the next drain
    /// to remove them). Used by the TUI status counter `🛰 N bg`.
    pub fn running_count(&self) -> usize {
        self.procs
            .values()
            .filter(|p| {
                p.shared
                    .lock()
                    .map(|s| s.status.is_running())
                    .unwrap_or(false)
            })
            .count()
    }

    /// Whether the capped-tier circuit breaker is currently suppressing
    /// auto-injection. Reader threads keep filling buffers regardless;
    /// the next reset flushes everything accumulated.
    pub fn suppress_capped(&self) -> bool {
        self.consecutive_capped_turns >= MAX_CONSECUTIVE_AUTO_TURNS
    }

    /// Snapshot list for the `list` verb / `/bg` slash command.
    pub fn list(&self) -> Vec<BgListEntry> {
        let mut rows: Vec<BgListEntry> = self
            .procs
            .values()
            .map(|p| {
                let shared = p.shared.lock().expect("bg shared mutex poisoned");
                BgListEntry {
                    id: p.id,
                    pid: p.pid,
                    command: p.command.clone(),
                    label: p.label.clone(),
                    status: shared.status.clone(),
                    buffer_len: shared.buffer.len(),
                    capture_cap: p.capture_cap,
                    treat_as_user_input: p.treat_as_user_input,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.id);
        rows
    }

    /// Spawn a new background process. The `notify_tx` channel is pinged
    /// (debounced) whenever fresh output lands so the agent loop can
    /// wake up and drain. The notification payload is intentionally
    /// empty — see `QueueMessage::BackgroundOutputReady`.
    pub fn start(
        &mut self,
        params: StartParams,
        notify_tx: UnboundedSender<()>,
    ) -> Result<BgListEntry, BgError> {
        let StartParams {
            command,
            capture_cap,
            cwd,
            label,
            treat_as_user_input,
            env,
        } = params;

        // Spawn via `sh -c` so the model can pass a shell line verbatim,
        // identical to the existing `bash` tool's contract.
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| BgError::Spawn(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new("sh");
        cmd.arg("-c");
        cmd.arg(&command);
        if let Some(d) = cwd.as_ref() {
            cmd.cwd(d);
        }
        // Inherit env explicitly so $PATH and friends work; portable-pty
        // strips by default on some platforms.
        for (k, v) in std::env::vars_os() {
            cmd.env(k, v);
        }
        // Apply configured env vars from `bash:` config section (same source
        // as the synchronous `bash` tool). These override inherited OS vars.
        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| BgError::Spawn(format!("spawn: {e}")))?;
        let pid = child.process_id().unwrap_or(0);
        let killer = child.clone_killer();

        // Drop the slave so EOF reaches the reader once the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| BgError::Spawn(format!("clone_reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| BgError::Spawn(format!("take_writer: {e}")))?;

        let id = self.next_id;
        self.next_id += 1;

        let shared = Arc::new(Mutex::new(ProcessShared {
            buffer: VecDeque::with_capacity(capture_cap.min(1024)),
            capture_cap,
            status: BgStatus::Running { since: Utc::now() },
            dirty: false,
        }));

        let reader_handle = spawn_reader(reader, shared.clone(), child, notify_tx);

        let proc = BgProcess {
            id,
            pid,
            command,
            label,
            treat_as_user_input,
            capture_cap,
            started_at: Utc::now(),
            shared,
            writer,
            killer,
            reader: Some(reader_handle),
        };

        // Build the list entry before moving proc into the map.
        let entry = BgListEntry {
            id: proc.id,
            pid: proc.pid,
            command: proc.command.clone(),
            label: proc.label.clone(),
            status: BgStatus::Running {
                since: proc.started_at,
            },
            buffer_len: 0,
            capture_cap: proc.capture_cap,
            treat_as_user_input: proc.treat_as_user_input,
        };
        self.procs.insert(id, proc);
        Ok(entry)
    }

    /// Kill a process and remove it from the registry. Returns the
    /// exit code (best-effort — `-1` if we never observed one) and a
    /// final snapshot of the ring buffer at the moment of stop.
    pub fn stop(&mut self, id: u32) -> Result<(i32, Vec<String>), BgError> {
        let Some(mut proc) = self.procs.remove(&id) else {
            return Err(BgError::NotFound(id));
        };
        // Kill (SIGHUP on Unix). Reader will see EOF and exit on its own;
        // we join it below.
        let _ = proc.killer.kill();
        let (exit_code, final_lines) = {
            let shared = proc.shared.lock().expect("bg shared mutex poisoned");
            let code = match &shared.status {
                BgStatus::Exited { code, .. } => *code,
                BgStatus::Running { .. } => -1,
            };
            (code, shared.buffer.iter().cloned().collect::<Vec<_>>())
        };
        if let Some(h) = proc.reader.take() {
            // Best-effort join with a short timeout — if the reader is
            // wedged on `read()` for an exotic process, we don't want
            // to block the agent.
            let _ = std::thread::spawn(move || {
                let _ = h.join();
            });
        }
        Ok((exit_code, final_lines))
    }

    /// Write `line` (newline-terminated) to a running process's stdin.
    pub fn send_line(&mut self, id: u32, mut line: String) -> Result<usize, BgError> {
        let Some(proc) = self.procs.get_mut(&id) else {
            return Err(BgError::NotFound(id));
        };
        {
            let shared = proc.shared.lock().expect("bg shared mutex poisoned");
            if !shared.status.is_running() {
                return Err(BgError::Exited(id));
            }
        }
        if !line.ends_with('\n') {
            line.push('\n');
        }
        let bytes = line.as_bytes();
        proc.writer
            .write_all(bytes)
            .map_err(|source| BgError::Write { id, source })?;
        let _ = proc.writer.flush();
        Ok(bytes.len())
    }

    /// Drain every non-empty buffer into one `DrainedBlock` per process.
    ///
    /// Side effects:
    /// - Each touched ring is cleared.
    /// - Exited processes are removed from the registry **after** their
    ///   block is captured (so the model sees the final tail once).
    /// - The circuit-breaker counter is updated based on the tiers of
    ///   contributing processes — capped-only ⇒ `+= 1`, any unlimited
    ///   contribution ⇒ reset to 0.
    ///
    /// Returns `None` when nothing was drained (all buffers clean OR
    /// the capped-tier circuit breaker is suppressing).
    pub fn drain_outputs(&mut self) -> Option<Vec<DrainedBlock>> {
        // Pre-filter under a single pass: collect (id, treat_as_user_input,
        // is_dirty, exited) for every process, then decide whether to flush.
        let suppress = self.suppress_capped();
        let mut blocks: Vec<DrainedBlock> = Vec::new();
        let mut had_unlimited = false;
        let mut had_capped = false;
        let mut to_remove: Vec<u32> = Vec::new();

        for proc in self.procs.values() {
            let mut shared = proc.shared.lock().expect("bg shared mutex poisoned");
            if !shared.dirty {
                continue;
            }
            // Honour the suppression gate for capped-only contributors:
            // their notifications still fired (reader is unaware of the
            // breaker), but we don't drain them. The buffer keeps
            // accumulating; a future drain after a reset will flush it.
            if suppress && !proc.treat_as_user_input {
                continue;
            }
            let lines: Vec<String> = shared.buffer.drain(..).collect();
            shared.dirty = false;
            let exited = matches!(shared.status, BgStatus::Exited { .. });
            let status_after = shared.status.clone();
            drop(shared);

            if lines.is_empty() && !exited {
                continue;
            }

            if proc.treat_as_user_input {
                had_unlimited = true;
            } else {
                had_capped = true;
            }
            if exited {
                to_remove.push(proc.id);
            }
            blocks.push(DrainedBlock {
                id: proc.id,
                command: proc.command.clone(),
                label: proc.label.clone(),
                treat_as_user_input: proc.treat_as_user_input,
                status_after,
                lines,
            });
        }

        // Remove exited processes after capturing their final tail.
        for id in to_remove {
            if let Some(mut p) = self.procs.remove(&id)
                && let Some(h) = p.reader.take()
            {
                let _ = std::thread::spawn(move || {
                    let _ = h.join();
                });
            }
        }

        if blocks.is_empty() {
            return None;
        }

        // Counter update.
        if had_unlimited {
            self.consecutive_capped_turns = 0;
        } else if had_capped {
            self.consecutive_capped_turns += 1;
        }

        Some(blocks)
    }

    /// Reset the consecutive-capped-turns counter. Called by `StateManager`
    /// whenever a *real* user message is dequeued. (Synthetic turns
    /// driven by unlimited contributors reset it via `drain_outputs`.)
    pub fn reset_counter(&mut self) {
        self.consecutive_capped_turns = 0;
    }

    /// Kill every process. Called on `/new`, `/model`, `/load` rebuild,
    /// and from `Drop`.
    pub fn clear(&mut self) {
        let ids: Vec<u32> = self.procs.keys().copied().collect();
        for id in ids {
            let _ = self.stop(id);
        }
        self.consecutive_capped_turns = 0;
    }

    /// Process id ⇒ contributing-tier accessor for tests.
    #[cfg(test)]
    fn proc_count(&self) -> usize {
        self.procs.len()
    }
}

impl Default for BgRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for BgRegistry {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Spawn the per-process reader thread. Owns the `Box<dyn Read>` and the
/// `Box<dyn Child>`. The thread terminates when `read` returns EOF (i.e.
/// the child closed its side of the PTY); it then calls `wait` to harvest
/// the exit status and flips `shared.status` to `Exited`.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    shared: Arc<Mutex<ProcessShared>>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    notify_tx: UnboundedSender<()>,
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
                    // Pull complete lines.
                    let mut any_new_line = false;
                    while let Some(pos) = leftover.iter().position(|&b| b == b'\n') {
                        let line_bytes: Vec<u8> = leftover.drain(..=pos).collect();
                        let raw = String::from_utf8_lossy(&line_bytes);
                        let clean = strip_ansi(raw.trim_end_matches('\n'));
                        {
                            let mut s = shared.lock().expect("bg shared mutex poisoned");
                            s.push_line(clean);
                        }
                        any_new_line = true;
                    }
                    if any_new_line {
                        // Debounce: only ping if it's been DEBOUNCE_MS
                        // since the last successful ping, OR this is
                        // the first one.
                        let now = Instant::now();
                        let should_ping = match last_notify {
                            None => true,
                            Some(t) => now.duration_since(t) >= Duration::from_millis(DEBOUNCE_MS),
                        };
                        if should_ping {
                            let _ = notify_tx.send(());
                            last_notify = Some(now);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("bg reader read error: {e}");
                    break;
                }
            }
        }

        // Flush trailing leftover bytes as one final line (no newline).
        if !leftover.is_empty() {
            let raw = String::from_utf8_lossy(&leftover);
            let clean = strip_ansi(raw.trim_end_matches('\n'));
            if !clean.is_empty()
                && let Ok(mut s) = shared.lock()
            {
                s.push_line(clean);
            }
        }

        // Reap exit code.
        let code = match child.wait() {
            Ok(status) => status.exit_code() as i32,
            Err(_) => -1,
        };
        if let Ok(mut s) = shared.lock() {
            s.status = BgStatus::Exited {
                code,
                at: Utc::now(),
            };
            s.dirty = true;
        }
        // Always ping once on exit so the agent observes the transition.
        let _ = notify_tx.send(());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn strip_ansi_removes_csi() {
        // `ls --color=auto`-style red coloured filename.
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
        // 4-byte emoji at a boundary that would land mid-codepoint
        // under naive byte slicing.
        let s = format!("a{}", "🦀".repeat(2000));
        let t = truncate_line(&s, MAX_LINE_BYTES);
        // Must still be valid UTF-8 and not panic.
        assert!(t.is_char_boundary(t.len() - " … (truncated)".len()));
    }

    #[test]
    fn ring_buffer_evicts_oldest_on_overflow() {
        let mut p = ProcessShared {
            buffer: VecDeque::new(),
            capture_cap: 3,
            status: BgStatus::Running { since: Utc::now() },
            dirty: false,
        };
        for i in 0..5 {
            p.push_line(format!("line {i}"));
        }
        assert_eq!(p.buffer.len(), 3);
        assert_eq!(p.buffer[0], "line 2");
        assert_eq!(p.buffer[2], "line 4");
    }

    #[test]
    fn ring_buffer_capacity_zero_drops_everything() {
        let mut p = ProcessShared {
            buffer: VecDeque::new(),
            capture_cap: 0,
            status: BgStatus::Running { since: Utc::now() },
            dirty: false,
        };
        p.push_line("anything".into());
        assert!(p.buffer.is_empty());
        // …but `dirty` is still set so the agent learns the process
        // produced something (e.g. for the "exited" signal).
        assert!(p.dirty);
    }

    #[test]
    fn registry_assigns_monotonic_ids() {
        let mut r = BgRegistry::new();
        let (tx, _rx) = unbounded_channel();
        let id1 = r
            .start(
                StartParams {
                    command: "true".into(),
                    capture_cap: 0,
                    cwd: None,
                    label: None,
                    treat_as_user_input: false,
                    env: None,
                },
                tx.clone(),
            )
            .expect("start id1")
            .id;
        let id2 = r
            .start(
                StartParams {
                    command: "true".into(),
                    capture_cap: 0,
                    cwd: None,
                    label: None,
                    treat_as_user_input: false,
                    env: None,
                },
                tx,
            )
            .expect("start id2")
            .id;
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        // Reap children so the test doesn't leak processes.
        let _ = r.stop(id1);
        let _ = r.stop(id2);
    }

    #[test]
    fn stop_unknown_id_returns_not_found() {
        let mut r = BgRegistry::new();
        match r.stop(999) {
            Err(BgError::NotFound(999)) => {}
            other => panic!("expected NotFound(999), got {other:?}"),
        }
    }

    #[test]
    fn circuit_breaker_increments_on_capped_only_and_resets_on_unlimited() {
        let mut r = BgRegistry::new();
        // Simulate the contributing-tier bookkeeping directly — full
        // PTY-driven integration sits in tests/scenarios/bg_tests.rs.
        // Manually push a fake exited process.
        let shared_cap = Arc::new(Mutex::new(ProcessShared {
            buffer: VecDeque::from(vec!["hi".to_string()]),
            capture_cap: 10,
            status: BgStatus::Running { since: Utc::now() },
            dirty: true,
        }));
        let shared_un = Arc::new(Mutex::new(ProcessShared {
            buffer: VecDeque::from(vec!["from-tg".to_string()]),
            capture_cap: 10,
            status: BgStatus::Running { since: Utc::now() },
            dirty: true,
        }));
        // Insert capped process by hand.
        r.procs.insert(
            10,
            BgProcess {
                id: 10,
                pid: 0,
                command: "tail".into(),
                label: None,
                treat_as_user_input: false,
                capture_cap: 10,
                started_at: Utc::now(),
                shared: shared_cap.clone(),
                writer: Box::new(std::io::sink()),
                killer: dummy_killer(),
                reader: None,
            },
        );
        r.procs.insert(
            11,
            BgProcess {
                id: 11,
                pid: 0,
                command: "telegram".into(),
                label: None,
                treat_as_user_input: true,
                capture_cap: 10,
                started_at: Utc::now(),
                shared: shared_un.clone(),
                writer: Box::new(std::io::sink()),
                killer: dummy_killer(),
                reader: None,
            },
        );

        // First drain: both contribute → unlimited wins → counter = 0.
        let _ = r.drain_outputs();
        assert_eq!(r.consecutive_capped_turns, 0);

        // Now refill *only* the capped one.
        shared_cap.lock().unwrap().push_line("hi again".into());
        let _ = r.drain_outputs();
        assert_eq!(r.consecutive_capped_turns, 1);

        shared_cap.lock().unwrap().push_line("hi 3".into());
        let _ = r.drain_outputs();
        assert_eq!(r.consecutive_capped_turns, 2);

        shared_cap.lock().unwrap().push_line("hi 4".into());
        let _ = r.drain_outputs();
        assert_eq!(r.consecutive_capped_turns, 3);
        assert!(r.suppress_capped());

        // Even when capped fires again, suppression skips the drain
        // entirely → counter stays at 3.
        shared_cap.lock().unwrap().push_line("hi 5".into());
        let drained = r.drain_outputs();
        assert!(drained.is_none(), "capped-only drain must suppress");
        assert_eq!(r.consecutive_capped_turns, 3);

        // Unlimited fires → suppression bypassed → counter resets.
        shared_un
            .lock()
            .unwrap()
            .push_line("hi from tg again".into());
        let _ = r.drain_outputs();
        assert_eq!(r.consecutive_capped_turns, 0);

        // Sanity: registry still owns both procs.
        assert_eq!(r.proc_count(), 2);
    }

    /// Dummy killer for tests that bypass real PTY spawning.
    fn dummy_killer() -> Box<dyn ChildKiller + Send + Sync> {
        #[derive(Debug)]
        struct Dummy;
        impl ChildKiller for Dummy {
            fn kill(&mut self) -> std::io::Result<()> {
                Ok(())
            }
            fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
                Box::new(Dummy)
            }
        }
        Box::new(Dummy)
    }
}
