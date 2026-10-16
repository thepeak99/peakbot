//! Test doubles (DESIGN §7 T1): `FakeSys` (scripted side effects keyed
//! by (program, args), per-URL probe scripts, a virtual clock where
//! `sleep` advances `now`, in-memory files incl. 0600-write tracking,
//! and an ordered event log of every side effect), `FakeChild` (exit
//! after N `try_wait`s / on interrupt / never; shared interrupt/kill
//! counters), `FakeUi` (scripted confirm answers, recorded
//! phases/dialogs).
//!
//! These fakes are FULLY implemented; the production code behind them
//! is real, not stubs.

#![allow(dead_code)] // fakes are built for the full §9.1 list up front

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

use crate::controller::Ui;
use crate::phase::{Dialog, Phase, TrayModel};
use crate::sys::{Child, Probe, RunOutput, Signal, SpawnSpec, Sys};
use crate::token::Token;

// ---------------------------------------------------------------------------
// Effect log
// ---------------------------------------------------------------------------

/// One recorded side effect, in call order (ordering assertions).
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Run {
        program: PathBuf,
        args: Vec<String>,
    },
    Spawn {
        pid: u32,
        program: PathBuf,
        args: Vec<String>,
        extra_env: Vec<(String, String)>,
        log: PathBuf,
    },
    Signal {
        pid: u32,
        sig: Signal,
    },
    WritePrivate {
        path: PathBuf,
    },
    Remove {
        path: PathBuf,
    },
    Probe {
        url: Url,
    },
    Sleep {
        d: Duration,
    },
    FreePort,
    RandomToken,
    ChildInterrupt {
        pid: u32,
    },
    ChildKill {
        pid: u32,
    },
}

// ---------------------------------------------------------------------------
// FakeSys
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct FakeSysState {
    /// Virtual clock; lazily initialised on the first `now()`.
    pub now: Option<Instant>,
    /// Scripted `run` responses keyed by (program file name, args).
    /// Per-call sequences; the last entry repeats.
    pub run_scripts: HashMap<(String, Vec<String>), VecDeque<RunOutput>>,
    /// Per-URL probe scripts (index advances per call; last repeats).
    pub probe_scripts: HashMap<String, Vec<Probe>>,
    pub probe_idx: HashMap<String, usize>,
    /// Probe answer for URLs without an explicit script (default: empty
    /// ⇒ Unreachable).
    pub default_probe: Vec<Probe>,
    /// In-memory files.
    pub files: HashMap<PathBuf, String>,
    /// Paths `write_private` was called on (the 0600 set).
    pub private_files: HashSet<PathBuf>,
    /// Paths where `write_private` fails (design #377 t83).
    pub fail_writes: HashSet<PathBuf>,
    pub port_free: HashMap<u16, bool>,
    pub free_port_value: u16,
    pub cmdline: HashMap<u32, String>,
    /// Children `spawn` returns, in order (default: `never`).
    pub children: VecDeque<FakeChildSpec>,
    pub next_pid: u32,
    pub token_counter: u64,
    pub tokens: Vec<String>,
    pub spawned: Vec<SpawnSpec>,
    pub spawn_pids: Vec<u32>,
}

/// A scripted `Sys` with a virtual clock and in-memory files. `Clone`
/// shares all state (tests keep a clone for inspection).
#[derive(Clone)]
pub struct FakeSys {
    state: Arc<Mutex<FakeSysState>>,
    events: Arc<Mutex<Vec<Effect>>>,
}

impl FakeSys {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeSysState {
                now: None,
                free_port_value: 7999,
                next_pid: 1001,
                ..Default::default()
            })),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Script the output of `<program file name> <args…>` (the key uses
    /// the program's file name, so the absolute path is irrelevant).
    pub fn set_run(&self, program: &str, args: &[&str], out: RunOutput) {
        self.set_run_seq(program, args, vec![out]);
    }

    /// Per-call response sequence; the last entry repeats.
    pub fn set_run_seq(&self, program: &str, args: &[&str], outs: Vec<RunOutput>) {
        let key = (
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        );
        self.state
            .lock()
            .unwrap()
            .run_scripts
            .insert(key, outs.into());
    }

    pub fn set_probe(&self, url: &str, probes: Vec<Probe>) {
        self.state
            .lock()
            .unwrap()
            .probe_scripts
            .insert(url.to_string(), probes);
    }

    pub fn set_default_probe(&self, probes: Vec<Probe>) {
        self.state.lock().unwrap().default_probe = probes;
    }

    pub fn set_port_free(&self, port: u16, free: bool) {
        self.state.lock().unwrap().port_free.insert(port, free);
    }

    pub fn set_free_port(&self, port: u16) {
        self.state.lock().unwrap().free_port_value = port;
    }

    pub fn set_cmdline(&self, pid: u32, cmd: impl Into<String>) {
        self.state.lock().unwrap().cmdline.insert(pid, cmd.into());
    }

    pub fn queue_child(&self, spec: FakeChildSpec) {
        self.state.lock().unwrap().children.push_back(spec);
    }

    /// Seed an in-memory file (web-token, config.yaml, pidfile, …).
    pub fn seed_file(&self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.state
            .lock()
            .unwrap()
            .files
            .insert(path.into(), contents.into());
    }

    /// Make `write_private` fail for this path (design #377 t83).
    pub fn fail_write(&self, path: impl Into<PathBuf>) {
        self.state.lock().unwrap().fail_writes.insert(path.into());
    }

    pub fn file(&self, path: &Path) -> Option<String> {
        self.state.lock().unwrap().files.get(path).cloned()
    }

    pub fn has_file(&self, path: &Path) -> bool {
        self.state.lock().unwrap().files.contains_key(path)
    }

    /// Paths `write_private` was called on.
    pub fn private_written(&self) -> HashSet<PathBuf> {
        self.state.lock().unwrap().private_files.clone()
    }

    /// The ordered log of all side effects.
    pub fn events(&self) -> Vec<Effect> {
        self.events.lock().unwrap().clone()
    }

    /// The args of every `run` call, in order.
    pub fn run_args(&self) -> Vec<Vec<String>> {
        self.events()
            .iter()
            .filter_map(|e| match e {
                Effect::Run { args, .. } => Some(args.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn spawned_specs(&self) -> Vec<SpawnSpec> {
        self.state.lock().unwrap().spawned.clone()
    }

    pub fn spawned_pids(&self) -> Vec<u32> {
        self.state.lock().unwrap().spawn_pids.clone()
    }

    /// The raw token strings `random_token` produced, in order.
    pub fn tokens_generated(&self) -> Vec<String> {
        self.state.lock().unwrap().tokens.clone()
    }

    /// Advance the virtual clock (what `sleep` does).
    pub fn advance(&self, d: Duration) {
        let mut st = self.state.lock().unwrap();
        let now = st.now.get_or_insert_with(Instant::now);
        let next = *now + d;
        *now = next;
    }
}

impl Default for FakeSys {
    fn default() -> Self {
        Self::new()
    }
}

impl Sys for FakeSys {
    fn run(&self, program: &Path, args: &[&str]) -> io::Result<RunOutput> {
        let name = program
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        let key = (
            name.clone(),
            args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        self.events.lock().unwrap().push(Effect::Run {
            program: program.to_path_buf(),
            args: key.1.clone(),
        });
        let out = {
            let mut st = self.state.lock().unwrap();
            match st.run_scripts.get_mut(&key) {
                None => None,
                Some(q) if q.len() > 1 => q.pop_front(),
                Some(q) => q.front().cloned(),
            }
        };
        out.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "FakeSys: no script for `{name} {}` (program missing)",
                    key.1.join(" ")
                ),
            )
        })
    }

    fn spawn(&self, spec: SpawnSpec) -> io::Result<Box<dyn Child>> {
        let (pid, child_spec) = {
            let mut st = self.state.lock().unwrap();
            // RealSys truncates the session log at spawn.
            st.files.remove(&spec.log);
            let child_spec = st.children.pop_front().unwrap_or(FakeChildSpec::never());
            let pid = st.next_pid;
            st.next_pid += 1;
            st.spawned.push(spec.clone());
            st.spawn_pids.push(pid);
            (pid, child_spec)
        };
        self.events.lock().unwrap().push(Effect::Spawn {
            pid,
            program: spec.program.clone(),
            args: spec.args.clone(),
            extra_env: spec.extra_env.clone(),
            log: spec.log.clone(),
        });
        let mut cs = child_spec;
        cs.pid = pid;
        cs.log_path = Some(spec.log.clone());
        Ok(Box::new(FakeChild::with_shared(
            cs,
            self.state.clone(),
            self.events.clone(),
        )))
    }

    fn probe(&self, url: &Url) -> Probe {
        let key = url.to_string();
        self.events
            .lock()
            .unwrap()
            .push(Effect::Probe { url: url.clone() });
        let mut st = self.state.lock().unwrap();
        let script = st
            .probe_scripts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| st.default_probe.clone());
        if script.is_empty() {
            return Probe::Unreachable;
        }
        let i = *st.probe_idx.entry(key.clone()).or_insert(0);
        let probe = script[i.min(script.len() - 1)];
        st.probe_idx.insert(key, i + 1);
        probe
    }

    fn free_port(&self) -> io::Result<u16> {
        self.events.lock().unwrap().push(Effect::FreePort);
        Ok(self.state.lock().unwrap().free_port_value)
    }

    fn port_is_free(&self, port: u16) -> bool {
        self.state
            .lock()
            .unwrap()
            .port_free
            .get(&port)
            .copied()
            .unwrap_or(true)
    }

    fn random_token(&self) -> Token {
        let n = {
            let mut st = self.state.lock().unwrap();
            st.token_counter += 1;
            st.token_counter
        };
        self.events.lock().unwrap().push(Effect::RandomToken);
        let raw = format!("{n:064x}");
        self.state.lock().unwrap().tokens.push(raw.clone());
        Token::parse(&raw).expect("a fake token must parse (no whitespace)")
    }

    fn read(&self, path: &Path) -> io::Result<Option<String>> {
        Ok(self.state.lock().unwrap().files.get(path).cloned())
    }

    fn write_private(&self, path: &Path, contents: &str) -> io::Result<()> {
        if self.state.lock().unwrap().fail_writes.contains(path) {
            return Err(io::Error::other("fake write failure"));
        }
        {
            let mut st = self.state.lock().unwrap();
            st.files.insert(path.to_path_buf(), contents.to_string());
            st.private_files.insert(path.to_path_buf());
        }
        self.events.lock().unwrap().push(Effect::WritePrivate {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        self.state.lock().unwrap().files.remove(path);
        self.events.lock().unwrap().push(Effect::Remove {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.state.lock().unwrap().files.contains_key(path)
    }

    fn cmdline_of(&self, pid: u32) -> Option<String> {
        self.state.lock().unwrap().cmdline.get(&pid).cloned()
    }

    fn signal(&self, pid: u32, sig: Signal) {
        self.events
            .lock()
            .unwrap()
            .push(Effect::Signal { pid, sig });
    }

    fn sleep(&self, d: Duration) {
        self.advance(d);
        self.events.lock().unwrap().push(Effect::Sleep { d });
    }

    fn now(&self) -> Instant {
        *self
            .state
            .lock()
            .unwrap()
            .now
            .get_or_insert_with(Instant::now)
    }
}

// ---------------------------------------------------------------------------
// FakeChild
// ---------------------------------------------------------------------------

/// How a fake child exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildMode {
    /// `Some(exit)` on the Nth `try_wait`.
    ExitsAfter(u32),
    /// `Some(exit)` on the first `try_wait` after `interrupt()`.
    ExitsOnInterrupt,
    /// Never exits on its own; `kill()` makes the next `try_wait` report
    /// `Some(None)` (killed by signal).
    Never,
}

#[derive(Debug, Clone)]
pub struct FakeChildSpec {
    pub mode: ChildMode,
    /// Exit code when exiting on its own (default `Some(0)`).
    pub exit: Option<i32>,
    /// Written to the session log when the child exits (models the
    /// sidecar's stdout+stderr landing in the log).
    pub log_write: Option<String>,
    pub pid: u32,
    /// Set by `FakeSys::spawn`; `None` for directly-constructed children.
    pub log_path: Option<PathBuf>,
}

impl Default for FakeChildSpec {
    fn default() -> Self {
        Self {
            mode: ChildMode::Never,
            exit: Some(0),
            log_write: None,
            pid: 0,
            log_path: None,
        }
    }
}

impl FakeChildSpec {
    /// Stays alive until interrupted/killed (happy-path session child).
    pub fn alive() -> Self {
        Self {
            mode: ChildMode::ExitsAfter(u32::MAX),
            ..Default::default()
        }
    }

    /// Never responds to signals (stubborn child).
    pub fn never() -> Self {
        Self::default()
    }
}

struct FakeChildInner {
    spec: FakeChildSpec,
    try_waits: u32,
    interrupts: u32,
    kills: u32,
    /// `None` = alive; `Some(Some(code))` = exited; `Some(None)` =
    /// killed by signal.
    exit: Option<Option<i32>>,
    logged: bool,
}

/// A snapshot of the child's state for assertions (shared handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeChildStats {
    pub try_waits: u32,
    pub interrupts: u32,
    pub kills: u32,
    pub exit: Option<Option<i32>>,
}

/// A `Child` double with shared, inspectable state.
#[derive(Clone)]
pub struct FakeChild {
    inner: Arc<Mutex<FakeChildInner>>,
    state: Arc<Mutex<FakeSysState>>,
    events: Arc<Mutex<Vec<Effect>>>,
}

impl FakeChild {
    /// Standalone (its own empty event log).
    pub fn new(spec: FakeChildSpec) -> Self {
        Self::with_shared(
            spec,
            Arc::new(Mutex::new(FakeSysState::default())),
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    pub(crate) fn with_shared(
        spec: FakeChildSpec,
        state: Arc<Mutex<FakeSysState>>,
        events: Arc<Mutex<Vec<Effect>>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeChildInner {
                spec,
                try_waits: 0,
                interrupts: 0,
                kills: 0,
                exit: None,
                logged: false,
            })),
            state,
            events,
        }
    }

    pub fn stats(&self) -> FakeChildStats {
        let st = self.inner.lock().unwrap();
        FakeChildStats {
            try_waits: st.try_waits,
            interrupts: st.interrupts,
            kills: st.kills,
            exit: st.exit,
        }
    }
}

impl Child for FakeChild {
    fn pid(&self) -> u32 {
        self.inner.lock().unwrap().spec.pid
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        let mut st = self.inner.lock().unwrap();
        st.try_waits += 1;
        if st.exit.is_none() {
            match st.spec.mode {
                ChildMode::ExitsAfter(n) if st.try_waits >= n => st.exit = Some(st.spec.exit),
                ChildMode::ExitsOnInterrupt if st.interrupts > 0 => st.exit = Some(st.spec.exit),
                _ => {}
            }
        }
        // Once exited, model the sidecar's output landing in the log.
        if st.exit.is_some() && !st.logged {
            st.logged = true;
            if let (Some(text), Some(log)) = (st.spec.log_write.clone(), st.spec.log_path.clone()) {
                self.state.lock().unwrap().files.insert(log, text);
            }
        }
        Ok(st.exit)
    }

    fn interrupt(&mut self) {
        let pid = {
            let mut st = self.inner.lock().unwrap();
            st.interrupts += 1;
            st.spec.pid
        };
        self.events
            .lock()
            .unwrap()
            .push(Effect::ChildInterrupt { pid });
    }

    fn kill(&mut self) {
        let pid = {
            let mut st = self.inner.lock().unwrap();
            st.kills += 1;
            if st.exit.is_none() {
                st.exit = Some(None); // SIGKILL ⇒ killed by a signal
            }
            st.spec.pid
        };
        self.events.lock().unwrap().push(Effect::ChildKill { pid });
    }
}

// ---------------------------------------------------------------------------
// FakeUi
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct FakeUiState {
    /// (phase, tray) pairs from `phase_changed`, in order.
    pub phases: Vec<(Phase, TrayModel)>,
    pub confirms: Vec<Dialog>,
    pub messages: Vec<Dialog>,
    /// Scripted confirm answers (FIFO); falls back to `confirm_default`.
    pub confirm_answers: VecDeque<bool>,
    pub confirm_default: bool,
}

/// A `Ui` double: scripted confirm answers, recorded phases/dialogs.
#[derive(Clone, Default)]
pub struct FakeUi {
    inner: Arc<Mutex<FakeUiState>>,
}

impl FakeUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an answer for the next `confirm` (FIFO).
    pub fn answer_confirm(&self, answer: bool) {
        self.inner.lock().unwrap().confirm_answers.push_back(answer);
    }

    pub fn phases(&self) -> Vec<(Phase, TrayModel)> {
        self.inner.lock().unwrap().phases.clone()
    }

    pub fn steps(&self) -> Vec<Phase> {
        self.inner
            .lock()
            .unwrap()
            .phases
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }

    pub fn last_tray(&self) -> TrayModel {
        self.inner
            .lock()
            .unwrap()
            .phases
            .last()
            .map(|(_, t)| t.clone())
            .expect("no phases recorded")
    }

    pub fn confirms(&self) -> Vec<Dialog> {
        self.inner.lock().unwrap().confirms.clone()
    }

    pub fn messages(&self) -> Vec<Dialog> {
        self.inner.lock().unwrap().messages.clone()
    }
}

impl Ui for FakeUi {
    fn phase_changed(&self, phase: &Phase, tray: &TrayModel) {
        self.inner
            .lock()
            .unwrap()
            .phases
            .push((phase.clone(), tray.clone()));
    }

    fn confirm(&self, d: &Dialog) -> bool {
        let mut st = self.inner.lock().unwrap();
        st.confirms.push(d.clone());
        st.confirm_answers.pop_front().unwrap_or(st.confirm_default)
    }

    fn message(&self, d: &Dialog) {
        self.inner.lock().unwrap().messages.push(d.clone());
    }
}
