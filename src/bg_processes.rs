//! Background process registry for the `bash_bg` tool.
//!
//! Long-running PTY-attached processes managed by the agent. Each process
//! has a numeric id, an optional ring-buffer of captured output lines, and
//! a per-process `cooldown` that throttles how often its output is injected
//! into the conversation as a synthetic turn.
//!
//! The PTY plumbing (spawn, reader thread, ANSI strip, ring buffer, kill)
//! is delegated to [`crate::pty_runner`]. This module owns the
//! *registry-level* concerns: multi-process bookkeeping, the per-process
//! cooldown gate, and the drain-into-synthetic-turn semantics.
//!
//! ## Cooldown
//!
//! Each process coalesces its output: after an injection, further output
//! accumulates in the ring buffer until the process's `cooldown` elapses,
//! then the whole batch is flushed in one synthetic turn. `cooldown == 0`
//! disables coalescing (real-time — every drained batch injects). Process
//! exits always bypass the cooldown so the model learns the process is
//! gone immediately.
//!
//! ## Lifecycle
//!
//! 1. `BgRegistry::start` calls [`pty_runner::spawn`] with a 500 ms
//!    debounce window, then wraps the returned `PtyHandle` parts in a
//!    `BgProcess` entry.
//! 2. The reader thread (owned by `pty_runner`) streams output into the
//!    shared `LineBuffer` and pings the agent loop on dirty.
//! 3. Output is drained synchronously by `BgRegistry::drain_outputs`,
//!    called between agent turns. The drain assembles one `[bg output]`
//!    block per non-empty process whose cooldown has elapsed and clears
//!    their rings. Exit notifications bypass the cooldown gate.
//! 4. `BgRegistry::stop` (or `Drop`) calls the killer (SIGHUP on Unix),
//!    joins the reader, and removes the process from the registry.
//!
//! ## Lock discipline
//!
//! All public methods take `&self` and lock the inner `Mutex` briefly.
//! Reader threads acquire the per-process `LineBuffer` mutex on each
//! line append. The registry mutex is **never** held across an `.await`
//! — the registry is a synchronous data structure that lives behind an
//! `Arc<Mutex<BgRegistry>>` on `StateManager`. See the async-lock
//! discipline rules in `memory.md`.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use portable_pty::ChildKiller;
use tokio::sync::mpsc::UnboundedSender;

use crate::pty_runner::{self, LineBuffer, PtyStatus, SpawnError, SpawnParams as PtySpawnParams};

/// Default ring-buffer capacity when `capture_output_lines` is omitted.
pub const DEFAULT_CAPTURE_LINES: usize = 200;

/// Debounce window: the reader buffers fresh-line notifications for this long
/// before pinging the agent loop. Open question Q2 was answered "OK" at 500ms.
pub const DEBOUNCE_MS: u64 = 500;

/// Default per-process cooldown (seconds) when the model omits `cooldown_secs`.
/// Output is coalesced into at most one synthetic turn per this interval; `0`
/// means real-time (every drained batch injects).
pub const DEFAULT_COOLDOWN_SECS: u64 = 60;

/// Status of a background process. Mirrors [`pty_runner::PtyStatus`] but
/// adds a timestamp so the TUI / drain blocks can show *when* the
/// transition happened.
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
/// Held inside `BgRegistry`. The reader thread (owned by `pty_runner`)
/// holds an `Arc<Mutex<LineBuffer>>` clone to drop lines into the buffer
/// without re-locking the whole registry on every line.
pub struct BgProcess {
    pub id: u32,
    pub pid: u32,
    pub command: String,
    pub label: Option<String>,
    /// How long to coalesce this process's output before injecting a
    /// synthetic turn. `Duration::ZERO` ⇒ real-time (no coalescing).
    pub cooldown: Duration,
    /// When this process last had output injected. `None` until the first
    /// injection — the cooldown gate treats `None` as "eligible now".
    last_inject: Option<Instant>,
    /// Ring capacity. `0` disables capture entirely (output is still
    /// drained from the PTY so the child doesn't block, but lines are
    /// discarded — useful for fire-and-forget watchers).
    pub capture_cap: usize,
    pub started_at: DateTime<Utc>,
    /// Wall-clock timestamp the reader thread observed exit. `None`
    /// while running; set on the first drain that sees `PtyStatus::Exited`.
    exited_at: Option<DateTime<Utc>>,

    /// Shared with the reader thread (via `pty_runner`): the line ring
    /// + liveness status. The registry's `drain_outputs` snapshots this
    ///   slot under the lock.
    buffer: Arc<Mutex<LineBuffer>>,

    /// Writer end of the PTY — used by `send_line`.
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

/// Arguments for `BgRegistry::start`.
pub struct StartParams {
    pub command: String,
    pub capture_cap: usize,
    pub cwd: Option<String>,
    pub label: Option<String>,
    /// Output-coalescing window. `Duration::ZERO` ⇒ real-time.
    pub cooldown: Duration,
    /// Optional environment variables to set for the spawned process,
    /// inherited from the `bash:` config section (same source as `bash`).
    pub env: Option<std::collections::HashMap<String, String>>,
    /// Shell executable to use (e.g. "sh", "bash", "pwsh", "powershell").
    /// If empty, defaults to "sh" for backward compatibility.
    pub shell: String,
}

/// A drained chunk of output, one per contributing process.
#[derive(Debug, Clone)]
pub struct DrainedBlock {
    pub id: u32,
    pub command: String,
    pub label: Option<String>,
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
    pub cooldown: Duration,
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

impl From<SpawnError> for BgError {
    fn from(e: SpawnError) -> Self {
        BgError::Spawn(e.to_string())
    }
}

/// The registry. Held as `Arc<Mutex<BgRegistry>>` on `StateManager`.
pub struct BgRegistry {
    procs: HashMap<u32, BgProcess>,
    next_id: u32,
}

impl BgRegistry {
    pub fn new() -> Self {
        Self {
            procs: HashMap::new(),
            next_id: 1,
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
                p.buffer
                    .lock()
                    .map(|b| b.status.is_running())
                    .unwrap_or(false)
            })
            .count()
    }

    /// Earliest instant at which any running process with buffered output
    /// becomes eligible to inject (its cooldown elapses). `None` when no
    /// process is currently waiting out a cooldown. Drives the agent
    /// loop's flush wakeup so a buffer that goes quiet mid-cooldown still
    /// flushes when its window expires.
    pub fn next_poke_deadline(&self, now: Instant) -> Option<Instant> {
        self.procs
            .values()
            .filter_map(|p| {
                let buf = p.buffer.lock().ok()?;
                // Only running, dirty processes that are still inside their
                // cooldown window are waiting on a future poke. Eligible-now
                // and clean processes don't need one.
                if !buf.dirty || !buf.status.is_running() {
                    return None;
                }
                let deadline = p.last_inject? + p.cooldown;
                (deadline > now).then_some(deadline)
            })
            .min()
    }

    /// Snapshot list for the `list` verb / `/bg` slash command.
    pub fn list(&self) -> Vec<BgListEntry> {
        let mut rows: Vec<BgListEntry> = self
            .procs
            .values()
            .map(|p| {
                let buf = p.buffer.lock().expect("pty buffer mutex poisoned");
                BgListEntry {
                    id: p.id,
                    pid: p.pid,
                    command: p.command.clone(),
                    label: p.label.clone(),
                    status: bg_status_from(p, &buf),
                    buffer_len: buf.lines.len(),
                    capture_cap: p.capture_cap,
                    cooldown: p.cooldown,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.id);
        rows
    }

    /// Spawn a new background process. The `notify_tx` channel is pinged
    /// (debounced at [`DEBOUNCE_MS`]) whenever fresh output lands so the
    /// agent loop can wake up and drain. The notification payload is
    /// intentionally empty — see `QueueMessage::BackgroundOutputReady`.
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
            cooldown,
            env,
            shell,
        } = params;

        let handle = pty_runner::spawn(
            PtySpawnParams {
                command: command.clone(),
                cwd,
                env,
                shell,
                capture_cap,
                debounce: Some(Duration::from_millis(DEBOUNCE_MS)),
            },
            Some(notify_tx),
        )?;
        let parts = handle.into_parts();

        let id = self.next_id;
        self.next_id += 1;
        let started_at = Utc::now();

        let proc = BgProcess {
            id,
            pid: parts.pid,
            command,
            label,
            cooldown,
            last_inject: None,
            capture_cap,
            started_at,
            exited_at: None,
            buffer: parts.buffer,
            writer: parts.writer,
            killer: parts.killer,
            reader: parts.reader,
        };

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
            cooldown: proc.cooldown,
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
            let buf = proc.buffer.lock().expect("pty buffer mutex poisoned");
            let code = match &buf.status {
                PtyStatus::Exited(code) => *code,
                PtyStatus::Running => -1,
            };
            (code, buf.lines.iter().cloned().collect::<Vec<_>>())
        };
        if let Some(h) = proc.reader.take() {
            // Best-effort join on a side thread so stop doesn't block.
            std::thread::spawn(move || {
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
            let buf = proc.buffer.lock().expect("pty buffer mutex poisoned");
            if !buf.status.is_running() {
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

    /// Drain every eligible non-empty buffer into one `DrainedBlock` per
    /// process. `now` is the reference instant for the cooldown gate
    /// (injected so the decision is a pure, testable function of time).
    ///
    /// A dirty process contributes iff it has **exited** OR its cooldown
    /// has elapsed since its last injection (`last_inject + cooldown <=
    /// now`; a never-injected process is always eligible). Processes still
    /// inside their cooldown window are skipped — their buffers keep
    /// accumulating and flush on a later drain once the window expires.
    ///
    /// Side effects:
    /// - Each contributing ring is cleared.
    /// - Contributing running processes have `last_inject` stamped to `now`.
    /// - Exited processes are removed from the registry **after** their
    ///   block is captured (so the model sees the final tail once).
    ///
    /// Returns `None` when nothing was drained.
    pub fn drain_outputs(&mut self, now: Instant) -> Option<Vec<DrainedBlock>> {
        let mut blocks: Vec<DrainedBlock> = Vec::new();
        let mut to_remove: Vec<u32> = Vec::new();

        for proc in self.procs.values_mut() {
            let mut buf = proc.buffer.lock().expect("pty buffer mutex poisoned");
            if !buf.dirty {
                continue;
            }
            let exited = matches!(buf.status, PtyStatus::Exited(_));
            // Cooldown gate: a running process inside its window is held
            // back (the reader keeps filling the buffer; a later drain
            // flushes it). Exits are exempt — a one-shot terminal
            // transition the model must learn immediately.
            let elapsed = proc.last_inject.is_none_or(|t| now >= t + proc.cooldown);
            if !exited && !elapsed {
                continue;
            }
            let lines: Vec<String> = buf.lines.drain(..).collect();
            buf.dirty = false;
            // Stamp the exit timestamp on first observation so the
            // surfaced `BgStatus::Exited::at` is stable across drains.
            if exited && proc.exited_at.is_none() {
                proc.exited_at = Some(Utc::now());
            }
            let status_after = bg_status_from(proc, &buf);
            drop(buf);

            if lines.is_empty() && !exited {
                continue;
            }

            if exited {
                to_remove.push(proc.id);
            } else {
                proc.last_inject = Some(now);
            }
            blocks.push(DrainedBlock {
                id: proc.id,
                command: proc.command.clone(),
                label: proc.label.clone(),
                status_after,
                lines,
            });
        }

        // Remove exited processes after capturing their final tail.
        for id in to_remove {
            if let Some(mut p) = self.procs.remove(&id)
                && let Some(h) = p.reader.take()
            {
                std::thread::spawn(move || {
                    let _ = h.join();
                });
            }
        }

        (!blocks.is_empty()).then_some(blocks)
    }

    /// Clear every process's `last_inject` so the next drain flushes all
    /// buffered output regardless of cooldown. Called by `StateManager`
    /// whenever a *real* user message is dequeued — engaging the agent is
    /// a natural point to surface everything accumulated.
    pub fn reset_cooldowns(&mut self) {
        for proc in self.procs.values_mut() {
            proc.last_inject = None;
        }
    }

    /// Kill every process. Called on `/new`, `/model`, `/load` rebuild,
    /// and from `Drop`.
    pub fn clear(&mut self) {
        let ids: Vec<u32> = self.procs.keys().copied().collect();
        for id in ids {
            let _ = self.stop(id);
        }
    }

    /// Process count accessor for tests.
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

/// Convert a `pty_runner::PtyStatus` plus the registry's bookkeeping
/// timestamps into the timestamped `BgStatus` consumed by the rest of
/// the codebase.
fn bg_status_from(proc: &BgProcess, buf: &LineBuffer) -> BgStatus {
    match &buf.status {
        PtyStatus::Running => BgStatus::Running {
            since: proc.started_at,
        },
        PtyStatus::Exited(code) => BgStatus::Exited {
            code: *code,
            at: proc.exited_at.unwrap_or_else(Utc::now),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tokio::sync::mpsc::unbounded_channel;

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
                    cooldown: Duration::ZERO,
                    env: None,
                    shell: String::new(),
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
                    cooldown: Duration::ZERO,
                    env: None,
                    shell: String::new(),
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

    /// Insert a hand-built process (bypassing real PTY spawn) so the
    /// cooldown gate can be exercised synchronously. Returns the shared
    /// buffer so the test can push more lines later.
    fn insert_fake(
        r: &mut BgRegistry,
        id: u32,
        cooldown: Duration,
        status: PtyStatus,
        first_line: &str,
    ) -> Arc<Mutex<LineBuffer>> {
        let buf = Arc::new(Mutex::new(LineBuffer {
            lines: VecDeque::from(vec![first_line.to_string()]),
            cap: 10,
            status,
            dirty: true,
        }));
        r.procs.insert(
            id,
            BgProcess {
                id,
                pid: 0,
                command: "fake".into(),
                label: None,
                cooldown,
                last_inject: None,
                capture_cap: 10,
                started_at: Utc::now(),
                exited_at: None,
                buffer: buf.clone(),
                writer: Box::new(std::io::sink()),
                killer: dummy_killer(),
                reader: None,
            },
        );
        buf
    }

    #[test]
    fn cooldown_withholds_capped_until_window_elapses() {
        let mut r = BgRegistry::new();
        let cooldown = Duration::from_secs(60);
        let buf = insert_fake(&mut r, 1, cooldown, PtyStatus::Running, "first");
        let t0 = Instant::now();

        // First drain: never injected → eligible → flushes.
        let d = r.drain_outputs(t0).expect("first batch flushes");
        assert_eq!(d[0].lines, vec!["first".to_string()]);

        // More output arrives inside the window → withheld.
        buf.lock().unwrap().push_line("second".into());
        assert!(
            r.drain_outputs(t0 + Duration::from_secs(10)).is_none(),
            "output inside cooldown must be withheld"
        );

        // Still buffered (not lost).
        assert_eq!(buf.lock().unwrap().lines.len(), 1);

        // Window elapses → the accumulated batch flushes.
        let d = r
            .drain_outputs(t0 + cooldown)
            .expect("batch flushes once window elapses");
        assert_eq!(d[0].lines, vec!["second".to_string()]);
    }

    #[test]
    fn cooldown_zero_is_realtime() {
        let mut r = BgRegistry::new();
        let buf = insert_fake(&mut r, 1, Duration::ZERO, PtyStatus::Running, "a");
        let t0 = Instant::now();

        assert!(r.drain_outputs(t0).is_some(), "first batch flushes");
        buf.lock().unwrap().push_line("b".into());
        // Same instant, zero cooldown → still eligible (last_inject + 0 <= now).
        assert!(
            r.drain_outputs(t0).is_some(),
            "zero cooldown injects every batch (real-time)"
        );
    }

    #[test]
    fn exit_notification_bypasses_cooldown() {
        // Exits must surface immediately even inside a long cooldown
        // window — a one-shot terminal transition the model must learn.
        let mut r = BgRegistry::new();
        let buf = insert_fake(
            &mut r,
            42,
            Duration::from_secs(3600),
            PtyStatus::Running,
            "warmup",
        );
        let t0 = Instant::now();
        r.drain_outputs(t0)
            .expect("warmup flush stamps last_inject");

        // Process exits 1s later — well inside the 1h cooldown.
        {
            let mut b = buf.lock().unwrap();
            b.push_line("final tail line".into());
            b.status = PtyStatus::Exited(0);
        }
        let drained = r
            .drain_outputs(t0 + Duration::from_secs(1))
            .expect("exit must surface even inside the cooldown window");
        assert_eq!(drained.len(), 1, "exit block must drain");
        assert!(drained[0].exited(), "block must reflect Exited status");
        assert_eq!(drained[0].lines, vec!["final tail line".to_string()]);
        assert_eq!(r.proc_count(), 0, "exited process reaped after final drain");
    }

    #[test]
    fn next_poke_deadline_tracks_soonest_capped() {
        let mut r = BgRegistry::new();
        let short = insert_fake(&mut r, 1, Duration::from_secs(10), PtyStatus::Running, "x");
        let _long = insert_fake(&mut r, 2, Duration::from_secs(300), PtyStatus::Running, "y");
        let t0 = Instant::now();

        // Nothing injected yet → both eligible-now → no future deadline.
        assert!(r.next_poke_deadline(t0).is_none());

        // Flush both (stamps last_inject = t0), then refill so they're
        // dirty again and waiting out their windows.
        r.drain_outputs(t0).expect("both flush");
        short.lock().unwrap().push_line("x2".into());
        r.procs
            .get(&2)
            .unwrap()
            .buffer
            .lock()
            .unwrap()
            .push_line("y2".into());

        // Soonest deadline is the 10s process.
        let deadline = r.next_poke_deadline(t0).expect("a window is pending");
        assert_eq!(deadline, t0 + Duration::from_secs(10));
    }

    #[test]
    fn reset_cooldowns_flushes_everything_next_drain() {
        let mut r = BgRegistry::new();
        let buf = insert_fake(
            &mut r,
            1,
            Duration::from_secs(60),
            PtyStatus::Running,
            "first",
        );
        let t0 = Instant::now();
        r.drain_outputs(t0).expect("first flush");

        buf.lock().unwrap().push_line("second".into());
        // Inside the window normally → withheld; reset clears the stamp.
        r.reset_cooldowns();
        let d = r
            .drain_outputs(t0 + Duration::from_secs(1))
            .expect("reset_cooldowns makes the next drain flush immediately");
        assert_eq!(d[0].lines, vec!["second".to_string()]);
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
