//! The `Sys` seam (DESIGN §5.1) — the only place that spawns processes,
//! opens sockets, sends signals, and touches files. Tests inject the
//! `FakeSys` from `crate::testing`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use url::Url;

use crate::consts::{PROBE_TIMEOUT, RUN_TIMEOUT};
use crate::env::sanitize_env;
use crate::token::Token;

/// THE only process constructor in this crate: every child goes through
/// it, and on Windows it is created with the `CREATE_NO_WINDOW` flag, so
/// the GUI-subsystem desktop app never spawns a console (a bare
/// `std::process::Command` would, for EVERY child — `wmic` and
/// `taskkill` flash consoles too).
fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)] // mut is only used in the (windows) cfg block
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

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
        self.code == Some(0)
    }

    /// stdout + stderr in one string (for diagnostics).
    pub fn combined(&self) -> String {
        if self.stderr.is_empty() {
            self.stdout.clone()
        } else if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            format!("{}\n{}", self.stdout, self.stderr)
        }
    }
}

/// Outcome of a readiness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Status(u16),
    /// TCP connect accepted, no HTTP spoken (https only, until #374
    /// swaps in a CA-verified GET — design #377 §6 T3).
    Listening,
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
    /// Byte-for-byte equality of two files. Lengths are compared first
    /// (different length ⇒ Ok(false) without reading). Err if either file
    /// cannot be opened or read (NotFound included).
    fn same_contents(&self, a: &Path, b: &Path) -> io::Result<bool>;
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
    base_env: Vec<(OsString, OsString)>,
    run_timeout: Duration,
}

impl RealSys {
    pub fn new() -> RealSys {
        Self {
            base_env: sanitize_env(std::env::vars_os()),
            run_timeout: RUN_TIMEOUT,
        }
    }

    /// **Test-only addition, REQUIRED for §7 C8** (flagged in the T1
    /// report): overrides the `run` timeout so the "run timeout kills"
    /// integration test does not wait the full 60 s `RUN_TIMEOUT`. Not
    /// part of the locked §5.1 surface.
    pub fn with_run_timeout(timeout: Duration) -> RealSys {
        Self {
            base_env: sanitize_env(std::env::vars_os()),
            run_timeout: timeout,
        }
    }
}

impl Default for RealSys {
    fn default() -> Self {
        Self::new()
    }
}

impl Sys for RealSys {
    /// `env_clear` + base env + `PEAKBOT_NO_OPEN=1`, stdin null,
    /// stdout/stderr piped and drained on two reader threads, `try_wait`
    /// polled until the timeout then kill → `Err(TimedOut)`. `NotFound`
    /// if the program is missing.
    fn run(&self, program: &Path, args: &[&str]) -> io::Result<RunOutput> {
        let mut child = command(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .envs(self.base_env.iter().cloned())
            .env("PEAKBOT_NO_OPEN", "1")
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let out_thread = thread::spawn(move || drain(stdout));
        let err_thread = thread::spawn(move || drain(stderr));
        let deadline = Instant::now() + self.run_timeout;
        loop {
            match child.try_wait()? {
                Some(status) => {
                    let stdout = out_thread.join().unwrap_or_default();
                    let stderr = err_thread.join().unwrap_or_default();
                    return Ok(RunOutput {
                        code: status.code(),
                        stdout,
                        stderr,
                    });
                }
                None => {
                    if Instant::now() >= deadline {
                        child.kill().ok();
                        let _ = child.wait();
                        // Detach the readers on timeout: a grandchild may
                        // have inherited the pipes (no process group by
                        // design, §2), and joining would block past the
                        // timeout. The result is discarded anyway.
                        drop(out_thread);
                        drop(err_thread);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "sidecar command timed out",
                        ));
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// `env_clear` + base env + `extra_env`, stdin null, stdout+stderr
    /// → `spec.log` (truncated, 0600 on unix).
    fn spawn(&self, spec: SpawnSpec) -> io::Result<Box<dyn Child>> {
        if let Some(parent) = spec.log.parent() {
            fs::create_dir_all(parent)?;
        }
        let log_file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&spec.log)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&spec.log, fs::Permissions::from_mode(0o600))?;
        }
        let child = command(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file))
            .env_clear()
            .envs(self.base_env.iter().cloned())
            .envs(spec.extra_env.iter().cloned())
            .spawn()?;
        Ok(Box::new(RealChild(child)))
    }

    /// GET with PROBE_TIMEOUT, no redirects. Any HTTP status (incl.
    /// 4xx/5xx) → `Status(code)`; transport-level failure →
    /// `Unreachable`. https URLs get a plain TCP connect (core ureq has
    /// no TLS — design #377 §6 T3; #374 replaces this branch).
    fn probe(&self, url: &Url) -> Probe {
        if url.scheme() == "https" {
            let addr: std::net::SocketAddr = format!("127.0.0.1:{}", url.port().unwrap_or(443))
                .parse()
                .expect("a loopback addr is valid");
            return match std::net::TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
                Ok(_) => Probe::Listening,
                Err(_) => Probe::Unreachable,
            };
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(PROBE_TIMEOUT))
            .max_redirects(0)
            // 4xx/5xx must surface as Probe::Status, not as an error we
            // would misreport as Unreachable.
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        match agent.get(url.as_str()).call() {
            Ok(resp) => Probe::Status(resp.status().as_u16()),
            // Belt and braces: even if the flag is ever flipped, a
            // status code is a status code, not "unreachable".
            Err(ureq::Error::StatusCode(code)) => Probe::Status(code),
            Err(_) => Probe::Unreachable,
        }
    }

    fn free_port(&self) -> io::Result<u16> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        Ok(listener.local_addr()?.port())
    }

    fn port_is_free(&self, port: u16) -> bool {
        TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    fn random_token(&self) -> Token {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("getrandom: system RNG unavailable");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Token::parse(&hex).expect("lowercase hex always parses as a token")
    }

    fn read(&self, path: &Path) -> io::Result<Option<String>> {
        match fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Design auto-replace §5.1: length first (a different length is the
    /// common mismatch and costs no reads), then 64 KiB chunks until a
    /// difference or both EOFs. Short reads are legal, so every chunk is
    /// filled with `read_full`.
    fn same_contents(&self, a: &Path, b: &Path) -> io::Result<bool> {
        let mut fa = fs::File::open(a)?;
        let mut fb = fs::File::open(b)?;
        if fa.metadata()?.len() != fb.metadata()?.len() {
            return Ok(false);
        }
        let mut chunk_a = [0u8; 64 * 1024];
        let mut chunk_b = [0u8; 64 * 1024];
        loop {
            let n_a = read_full(&mut fa, &mut chunk_a)?;
            let n_b = read_full(&mut fb, &mut chunk_b)?;
            if n_a != n_b {
                return Ok(false); // same length up front; defensive
            }
            if n_a == 0 {
                return Ok(true); // both reached EOF together
            }
            if chunk_a[..n_a] != chunk_b[..n_b] {
                return Ok(false);
            }
        }
    }

    fn write_private(&self, path: &Path, contents: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(contents.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn cmdline_of(&self, pid: u32) -> Option<String> {
        #[cfg(unix)]
        {
            let out = command("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!cmd.is_empty()).then_some(cmd)
        }
        #[cfg(windows)]
        {
            // Not exercised (unix is the only tested target); best
            // effort via wmic.
            let out = command("wmic")
                .args([
                    "process",
                    "where",
                    &format!("ProcessId={pid}"),
                    "get",
                    "CommandLine",
                ])
                .output()
                .ok()?;
            let cmd = String::from_utf8_lossy(&out.stdout)
                .lines()
                .nth(1)?
                .trim()
                .to_string();
            (!cmd.is_empty() && cmd != "No Instance(s) Available.").then_some(cmd)
        }
    }

    fn signal(&self, pid: u32, sig: Signal) {
        #[cfg(unix)]
        {
            let code = match sig {
                Signal::Interrupt => libc::SIGINT,
                Signal::Kill => libc::SIGKILL,
            };
            let _ = unsafe { libc::kill(pid as libc::pid_t, code) };
        }
        #[cfg(windows)]
        {
            // No signal delivery on Windows; best effort via taskkill.
            let pid_s = pid.to_string();
            let args: Vec<String> = match sig {
                Signal::Interrupt => vec!["/pid".into(), pid_s],
                Signal::Kill => vec!["/pid".into(), pid_s, "/f".into()],
            };
            let _ = command("taskkill")
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A spawned sidecar child (§5.1 `Child`).
struct RealChild(std::process::Child);

impl Child for RealChild {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        Ok(self.0.try_wait()?.map(|status| status.code()))
    }

    fn interrupt(&mut self) {
        #[cfg(unix)]
        {
            // kill(pid, SIGINT); errors ignored (the child may be gone).
            let _ = unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGINT) };
        }
        #[cfg(windows)]
        {
            // Windows has no SIGINT; terminate (DESIGN §2 keeps the
            // Windows differences in RealSys).
            self.0.kill().ok();
        }
    }

    fn kill(&mut self) {
        // SIGKILL / TerminateProcess; errors ignored (already dead).
        self.0.kill().ok();
    }
}

/// Fill `buf` by repeated reads until it is full or EOF (short reads
/// are legal) — used by `RealSys::same_contents`.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut off = 0;
    while off < buf.len() {
        match r.read(&mut buf[off..])? {
            0 => return Ok(off),
            n => off += n,
        }
    }
    Ok(off)
}

/// Read a pipe to EOF on a helper thread (so `run` never deadlocks on a
/// full pipe buffer).
fn drain(mut reader: impl Read + Send + 'static) -> String {
    let mut s = String::new();
    let _ = reader.read_to_string(&mut s);
    s
}
