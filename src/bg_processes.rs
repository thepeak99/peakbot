//! Background process registry for the `bash_bg` tool.
//!
//! Long-running PTY-attached processes managed by the agent. Each process
//! has a numeric id, an optional ring-buffer of captured output lines, and
//! a tier flag (`treat_as_user_input`) that determines whether its output
//! resets the synthetic-turn circuit breaker or contributes to it.
//!
//! The PTY plumbing (spawn, reader thread, ANSI strip, ring buffer, kill)
//! is delegated to [`crate::pty_runner`]. This module owns the
//! *registry-level* concerns: multi-process bookkeeping, the
//! consecutive-auto-turns circuit breaker, two-tier classification
//! (capped vs. unlimited), and the drain-into-synthetic-turn semantics.
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
//!    block per non-empty process and clears their rings. Exit
//!    notifications bypass the circuit-breaker gate.
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
use std::time::Duration;

use chrono::{DateTime, Utc};
use portable_pty::ChildKiller;
use tokio::sync::mpsc::UnboundedSender;

use crate::pty_runner::{self, LineBuffer, PtyStatus, SpawnError, SpawnParams as PtySpawnParams};

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
    pub treat_as_user_input: bool,
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

impl From<SpawnError> for BgError {
    fn from(e: SpawnError) -> Self {
        BgError::Spawn(e.to_string())
    }
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
                p.buffer
                    .lock()
                    .map(|b| b.status.is_running())
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
                let buf = p.buffer.lock().expect("pty buffer mutex poisoned");
                BgListEntry {
                    id: p.id,
                    pid: p.pid,
                    command: p.command.clone(),
                    label: p.label.clone(),
                    status: bg_status_from(p, &buf),
                    buffer_len: buf.lines.len(),
                    capture_cap: p.capture_cap,
                    treat_as_user_input: p.treat_as_user_input,
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
            treat_as_user_input,
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
            treat_as_user_input,
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
        let suppress = self.suppress_capped();
        let mut blocks: Vec<DrainedBlock> = Vec::new();
        let mut had_unlimited = false;
        let mut had_capped = false;
        let mut to_remove: Vec<u32> = Vec::new();

        for proc in self.procs.values_mut() {
            let mut buf = proc.buffer.lock().expect("pty buffer mutex poisoned");
            if !buf.dirty {
                continue;
            }
            let exited = matches!(buf.status, PtyStatus::Exited(_));
            // Honour the suppression gate for capped-only *running*
            // contributors: their notifications still fired (reader is
            // unaware of the breaker), but we don't drain them. The
            // buffer keeps accumulating; a future drain after a reset
            // will flush it.
            //
            // Exit notifications are exempt — a process exit is a
            // one-shot terminal transition that cannot loop, so the
            // breaker has no protective value here, and the model needs
            // to learn the process is gone regardless of breaker state.
            if suppress && !proc.treat_as_user_input && !exited {
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
                std::thread::spawn(move || {
                    let _ = h.join();
                });
            }
        }

        if blocks.is_empty() {
            return None;
        }

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
                    treat_as_user_input: false,
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
                    treat_as_user_input: false,
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

    #[test]
    fn circuit_breaker_increments_on_capped_only_and_resets_on_unlimited() {
        let mut r = BgRegistry::new();
        // Simulate the contributing-tier bookkeeping directly — full
        // PTY-driven integration sits in tests/scenarios/bg_tests.rs.
        // Manually push a fake exited process.
        let shared_cap = Arc::new(Mutex::new(LineBuffer {
            lines: VecDeque::from(vec!["hi".to_string()]),
            cap: 10,
            status: PtyStatus::Running,
            dirty: true,
        }));
        let shared_un = Arc::new(Mutex::new(LineBuffer {
            lines: VecDeque::from(vec!["from-tg".to_string()]),
            cap: 10,
            status: PtyStatus::Running,
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
                exited_at: None,
                buffer: shared_cap.clone(),
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
                exited_at: None,
                buffer: shared_un.clone(),
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

    #[test]
    fn exit_notification_bypasses_capped_suppression() {
        // Regression pin for the "process exit must always notify"
        // invariant: if the capped-tier circuit breaker is saturated and
        // a capped process exits, the exit block must still drain so the
        // model learns the process is gone. Exits are one-shot terminal
        // transitions and cannot loop, so the breaker has no protective
        // value here.
        let mut r = BgRegistry::new();
        r.consecutive_capped_turns = MAX_CONSECUTIVE_AUTO_TURNS;
        assert!(r.suppress_capped(), "test precondition: breaker saturated");

        let buf = Arc::new(Mutex::new(LineBuffer {
            lines: VecDeque::from(vec!["final tail line".to_string()]),
            cap: 10,
            status: PtyStatus::Exited(0),
            dirty: true,
        }));
        r.procs.insert(
            42,
            BgProcess {
                id: 42,
                pid: 0,
                command: "echo bye".into(),
                label: None,
                treat_as_user_input: false,
                capture_cap: 10,
                started_at: Utc::now(),
                exited_at: None,
                buffer: buf,
                writer: Box::new(std::io::sink()),
                killer: dummy_killer(),
                reader: None,
            },
        );

        let drained = r
            .drain_outputs()
            .expect("exit must surface even when capped suppression is active");
        assert_eq!(drained.len(), 1, "exit block must drain");
        let block = &drained[0];
        assert!(block.exited(), "drained block must reflect Exited status");
        assert_eq!(block.lines, vec!["final tail line".to_string()]);

        // The exited process must be removed from the registry on drain
        // (per `drain_outputs` contract — exited rows are reaped after
        // their final tail is captured).
        assert_eq!(
            r.proc_count(),
            0,
            "exited process must be reaped after final drain"
        );
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
