//! The `Sys` seam (DESIGN §5.1) — the only place that spawns processes,
//! opens sockets, sends signals, and touches files. Tests inject the
//! `FakeSys` from `crate::testing`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

use crate::token::Token;

/// Output of one short-lived command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RunOutput {
    /// `code == Some(0)`.
    pub fn ok(&self) -> bool {
        todo!()
    }

    /// stdout + stderr in one string (for diagnostics).
    pub fn combined(&self) -> String {
        todo!()
    }
}

/// Outcome of a readiness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Status(u16),
    Unreachable,
}

/// Signals the reaper / stop path can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Kill,
}

/// How to spawn the session sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added on top of the sanitized base env.
    pub extra_env: Vec<(String, String)>,
    /// Truncated, 0600, receives stdout+stderr.
    pub log: PathBuf,
}

/// A spawned sidecar child.
pub trait Child: Send {
    fn pid(&self) -> u32;
    /// `Some(exit)` once exited; `exit == None` = killed by signal.
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>>;
    /// unix: `kill(pid, SIGINT)`; windows: `kill()`.
    fn interrupt(&mut self);
    /// SIGKILL / TerminateProcess; errors ignored (already dead).
    fn kill(&mut self);
}

/// The side-effect seam.
pub trait Sys: Send {
    /// RUN_TIMEOUT; `NotFound` if the program is missing.
    fn run(&self, program: &Path, args: &[&str]) -> io::Result<RunOutput>;
    fn spawn(&self, spec: SpawnSpec) -> io::Result<Box<dyn Child>>;
    /// GET, PROBE_TIMEOUT, no redirects followed.
    fn probe(&self, url: &Url) -> Probe;
    /// Bind 127.0.0.1:0, read the port, drop.
    fn free_port(&self) -> io::Result<u16>;
    /// Bind of 127.0.0.1:port succeeds.
    fn port_is_free(&self, port: u16) -> bool;
    /// 32 random bytes, lowercase hex (64 chars).
    fn random_token(&self) -> Token;
    /// `Ok(None)` if NotFound.
    fn read(&self, path: &Path) -> io::Result<Option<String>>;
    /// mkdir -p, tmp+rename, 0600.
    fn write_private(&self, path: &Path, contents: &str) -> io::Result<()>;
    /// NotFound = Ok.
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
    /// `ps -o command= -p <pid>`; None if not alive.
    fn cmdline_of(&self, pid: u32) -> Option<String>;
    fn signal(&self, pid: u32, sig: Signal);
    fn sleep(&self, d: Duration);
    fn now(&self) -> std::time::Instant;
}

/// The real implementation. `base_env` =
/// `sanitize_env(std::env::vars_os())` at construction.
pub struct RealSys {
    #[allow(dead_code)] // read by the (not-yet-written) C8 implementation
    base_env: Vec<(OsString, OsString)>,
}

impl RealSys {
    pub fn new() -> RealSys {
        todo!()
    }

    /// **Test-only addition, REQUIRED for §7 C8** (flagged in the T1
    /// report): overrides the `run` timeout so the "run timeout kills"
    /// integration test does not wait the full 60 s `RUN_TIMEOUT`. Not
    /// part of the locked §5.1 surface.
    pub fn with_run_timeout(timeout: Duration) -> RealSys {
        todo!()
    }
}

impl Sys for RealSys {
    fn run(&self, program: &Path, args: &[&str]) -> io::Result<RunOutput> {
        todo!()
    }

    fn spawn(&self, spec: SpawnSpec) -> io::Result<Box<dyn Child>> {
        todo!()
    }

    fn probe(&self, url: &Url) -> Probe {
        todo!()
    }

    fn free_port(&self) -> io::Result<u16> {
        todo!()
    }

    fn port_is_free(&self, port: u16) -> bool {
        todo!()
    }

    fn random_token(&self) -> Token {
        todo!()
    }

    fn read(&self, path: &Path) -> io::Result<Option<String>> {
        todo!()
    }

    fn write_private(&self, path: &Path, contents: &str) -> io::Result<()> {
        todo!()
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        todo!()
    }

    fn exists(&self, path: &Path) -> bool {
        todo!()
    }

    fn cmdline_of(&self, pid: u32) -> Option<String> {
        todo!()
    }

    fn signal(&self, pid: u32, sig: Signal) {
        todo!()
    }

    fn sleep(&self, d: Duration) {
        todo!()
    }

    fn now(&self) -> std::time::Instant {
        todo!()
    }
}
