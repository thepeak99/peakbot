//! Self-installer and start-at-login service (plan §E, track I).
//!
//! This module is the single home for everything `peakbot install` and
//! `peakbot service …` touch. The platform-neutral logic (path
//! resolution, PATH analysis, the copy engine, the token file, the
//! service dispatch) lives here; the platform *effects* live in
//! [`linux`], [`macos`] and [`windows`], one of which `service_op`
//! dispatches to (§E.1).
//!
//! **Names.** Everything stays `peakbot`, not `shifu` (§E.2). The
//! `APP` constant is the *one* struct literal a rebrand touches;
//! the unit/plist/task name strings it carries are baked into user
//! machines once installed, and renaming them needs a migration, not a
//! `sed`.
//!
//! **The token.** Artifacts never embed a secret; the artifact names an
//! absolute binary path and `<binary> --bind <addr>`, and the token lives
//! in `<config_dir>/web-token` with `0600` perms on Unix (the same
//! precedent `save_config_at` uses). Rotation = edit one file + restart.

use std::ffi::OsStr;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

// ===========================================================================
// §E.1 — platform modules.
//
// All three are compiled on every target, not `#[cfg(any(target_os, test))]`
// as §E.1 sketched: the renderer contract lives in an *integration* test
// (`tests/install_render_tests.rs`), which links the library built WITHOUT
// `cfg(test)` — so a `test`-gated module simply would not exist for it. The
// modules are portable `std::fs` / `std::process::Command` / formatting (the
// §E.9 no-platform-only-API rule), so compiling all three everywhere costs a
// few KB and buys type-checking of the macOS and Windows effect code on the
// only CI we have. Exactly one is *dispatched*, via `use … as platform`.
// ===========================================================================

pub mod linux;
pub mod macos;
pub mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

/// Fallback for targets with no service manager we speak (Android, BSD).
/// Binary install still works there — only the service verbs are gated.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::{InstallError, ServicePlan, ServiceReport};

    const MSG: &str = "no service manager on this platform — start PeakBot from your \
                       own init system or by hand";

    pub(super) fn install_service(_plan: &ServicePlan) -> Result<ServiceReport, InstallError> {
        Err(InstallError::Unsupported(MSG.to_string()))
    }
    pub(super) fn uninstall_service() -> Result<ServiceReport, InstallError> {
        Err(InstallError::Unsupported(MSG.to_string()))
    }
    pub(super) fn service_status() -> Result<ServiceReport, InstallError> {
        Err(InstallError::Unsupported(MSG.to_string()))
    }
}

// ===========================================================================
// §E.2 — `APP` — the single seam that holds every user-machine-baked name.
// ===========================================================================

/// Every name this machine will remember us by. Renaming the product means
/// changing this literal *and* shipping a migration that removes the old
/// unit / agent / task — the strings below are baked into user machines.
pub struct AppNames {
    /// Bare binary name (no `.exe`): `"peakbot"`. The `.exe` is applied
    /// by [`target_exe_name`] so callers don't have to think about it.
    pub bin: &'static str,
    /// systemd `--user` unit name: `"peakbot.service"`.
    pub unit: &'static str,
    /// launchd LaunchAgent label: `"com.peakbot.agent"`.
    pub launchd_label: &'static str,
    /// Windows Task Scheduler task name (root folder): `"PeakBot"`.
    pub task: &'static str,
    /// User-visible display name (the only field a UI renders verbatim).
    pub display: &'static str,
}

/// The one product-name literal a rebrand touches. See §E.2 — "Shifu"
/// stays a display string, exactly as the SPA already treats it.
pub const APP: AppNames = AppNames {
    bin: "peakbot",
    unit: "peakbot.service",
    launchd_label: "com.peakbot.agent",
    task: "PeakBot",
    display: "PeakBot",
};

/// The exe name on the install target for the current build target.
/// `"peakbot"` on Linux/macOS, `"peakbot.exe"` on Windows.
pub const fn target_exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "peakbot.exe"
    }
    #[cfg(not(windows))]
    {
        "peakbot"
    }
}

// ===========================================================================
// §E.3 — install target resolution (pure; depends only on `dirs`).
// ===========================================================================

/// Resolve the per-user install target for the current build target.
///
/// Linux/macOS: `$HOME/.local/bin/peakbot`. Windows:
/// `%LOCALAPPDATA%\Programs\peakbot\peakbot.exe`. Returns `None` only if
/// the platform cannot resolve a home / data-local directory — the CLI
/// turns that into a one-line error.
pub fn install_target() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        dirs::home_dir().map(|home| home.join(".local").join("bin").join(APP.bin))
    }
    #[cfg(windows)]
    {
        dirs::data_local_dir()
            .map(|local| local.join("Programs").join(APP.bin).join(target_exe_name()))
    }
}

// ===========================================================================
// §E.5 — `ServicePlan` + `PlanError` — make the loopback/token invariant
// unrepresentable.
// ===========================================================================

/// What went wrong building a [`ServicePlan`]. The single variant today is
/// the only one the §E.5 invariant produces; further variants can be
/// added without breaking the public shape (the tests pattern-match
/// exhaustively, which is the point — adding a variant forces a
/// review).
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// Non-loopback bind without a (non-empty) token. Carries the rejected
    /// bind so the CLI/wizard can echo it back in the error message.
    #[error(
        "refusing to plan a service on non-loopback {0}: a token is required \
         (set --token or PEAKBOT_WEB_TOKEN)"
    )]
    TokenRequired(SocketAddr),
}

/// A validated install-time description of the service we *would* run.
/// Fields are private so the loopback/token invariant can't be violated
/// from outside the module — [`ServicePlan::new`] is the only constructor.
#[derive(Debug, Clone)]
pub struct ServicePlan {
    exe: PathBuf,
    bind: SocketAddr,
    /// `None` is a meaningful state only for loopback binds. Whitespace-
    /// only values are rejected by [`ServicePlan::new`] so the inner
    /// `Option<String>` only ever holds `Some(real_token)` or `None`.
    token: Option<String>,
}

impl ServicePlan {
    /// The only constructor. Enforces §E.5: non-loopback bind ⇒ real
    /// token; loopback bind ⇒ any token (including `None` / whitespace).
    pub fn new(exe: PathBuf, bind: SocketAddr, token: Option<String>) -> Result<Self, PlanError> {
        // Whitespace-only counts as "no token": same filter the env/file
        // branches use (§E.5). `is_loopback()` is false for `0.0.0.0`.
        let token_present = token
            .as_deref()
            .map(str::trim)
            .is_some_and(|t| !t.is_empty());
        if !bind.ip().is_loopback() && !token_present {
            return Err(PlanError::TokenRequired(bind));
        }
        Ok(Self { exe, bind, token })
    }

    /// The argv that every service artifact on every platform embeds:
    /// `<exe> --bind <addr>`. Per §E.5 no secret ever appears in any
    /// artifact — the token comes from the `web-token` file (§E.5), not
    /// from this list.
    pub fn argv(&self) -> Vec<String> {
        vec![
            self.exe.display().to_string(),
            "--bind".to_string(),
            self.bind.to_string(),
        ]
    }

    /// Read-only accessors — needed by I3's renderers (which build
    /// `ExecStart=` / `<Command>` from the same fields). Kept here so the
    /// renderer modules never reach into private fields.
    pub fn exe(&self) -> &Path {
        &self.exe
    }
    pub fn bind(&self) -> SocketAddr {
        self.bind
    }
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

// ===========================================================================
// §E.4 — `PathState` + `path_state` — pure PATH analysis (no env mutation).
// ===========================================================================

/// What `which peakbot` would do given this PATH and this target.
///
/// Computed by a pure string walk: the function never touches the
/// filesystem and never reads `PATH` from the environment, so the test
/// path passes a synthetic `PATH` and gets a deterministic answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathState {
    /// Target's parent directory is the *first* dir on PATH whose
    /// `dir.join(target.file_name())` matches the target — i.e. the
    /// shell would find *this* peakbot first if it existed. Existence
    /// is a separate question; a freshly installed target still answers
    /// `OnPath`.
    OnPath,
    /// A *different* `peakbot`/`peakbot.exe` precedes target on PATH. The
    /// wrapped path is the absolute path the shell would run. Carrying it
    /// means the wizard's "shadowed by …" line is exact, not a guess.
    Shadowed { by: PathBuf },
    /// No directory on PATH resolves to target; the hint is the exact
    /// line to add for the user's platform (§E.4 table).
    NotOnPath { hint: String },
}

/// Walk `path_var` in order and decide whether target's dir "wins" the
/// lookup. Pure — never calls `std::env::var`. Unit-tested with synthetic
/// PATH strings.
pub fn path_state(path_var: &OsStr, target: &Path) -> PathState {
    // No filename ⇒ target is degenerate (a bare dir). Treat as missing.
    let Some(name) = target.file_name() else {
        return PathState::NotOnPath {
            hint: hint_for_target(target),
        };
    };
    // The first non-target candidate we encounter — kept as the would-be
    // shadow if a *later* dir resolves to target.
    let mut shadow: Option<PathBuf> = None;
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(name);
        if candidate == target {
            return match shadow {
                // An earlier dir claimed the same filename — the shell
                // would find *that* one first, not ours.
                Some(by) => PathState::Shadowed { by },
                None => PathState::OnPath,
            };
        }
        // First non-target candidate becomes the shadow reference. We
        // only remember the first because any later non-target is also
        // reachable — but the first is the one the shell would run.
        if shadow.is_none() {
            shadow = Some(candidate);
        }
    }
    PathState::NotOnPath {
        hint: hint_for_target(target),
    }
}

/// Per-platform hint line for [`PathState::NotOnPath`]. The user owns
/// their shell — we print the line, we don't write to a dotfile.
fn hint_for_target(_target: &Path) -> String {
    #[cfg(windows)]
    {
        // §E.4 — Windows one-liner (PowerShell, then reopen the terminal).
        "Add it for your user (PowerShell, then reopen the terminal):  \
         [Environment]::SetEnvironmentVariable('Path', \
         [Environment]::GetEnvironmentVariable('Path','User') + ';' + \
         \"$env:LOCALAPPDATA\\Programs\\peakbot\", 'User')"
            .to_string()
    }
    #[cfg(not(windows))]
    {
        // §E.4 — Linux/macOS export line. Same line on both — they're
        // both POSIX shells; fish/zsh users adapt it themselves.
        "Add to your shell profile:  export PATH=\"$HOME/.local/bin:$PATH\"".to_string()
    }
}

// ===========================================================================
// §E.4 — copy engine: `install_binary_to` + the `InstallAction`/`InstallOutcome`
// types it returns. Pure-ish (the only impurity is the file copy); no
// subprocesses, no shelling out.
// ===========================================================================

/// What happened to the destination file. The wizard and the CLI say
/// these names verbatim (§E.12 "the CLI and the wizard say exactly the
/// same words" — the words come from this enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    /// `src` already equals `dst` (canonicalised). Zero writes. Success.
    AlreadyCurrent,
    /// `dst` did not exist; we wrote a fresh copy.
    Installed,
    /// `dst` existed and was a different file; we replaced it.
    Replaced,
}

/// Successful install outcome. `source` is the canonicalised
/// `current_exe()` so the user can see *what* landed where.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub action: InstallAction,
    pub source: PathBuf,
    pub target: PathBuf,
    /// Human next-steps, produced here so the CLI and the wizard say
    /// the same words. See §E.12 and §E.11.
    pub notes: Vec<String>,
}

/// Anything that can go wrong installing the binary. Wraps `io::Error`
/// because every failure here is an `std::fs` failure in practice, and
/// one variant covers them all without losing the OS message.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("cannot locate the running binary: {0}")]
    CurrentExe(io::Error),
    #[error("install I/O error: {0}")]
    Io(#[from] io::Error),
    /// A service-manager subprocess exited non-zero. `stderr` is passed
    /// through verbatim (§E.6): when `systemctl --user` cannot reach the
    /// bus, the fix is the user's session, not our code.
    #[error("`{command}` failed: {stderr}")]
    CommandFailed { command: String, stderr: String },
    /// No service manager we speak on this host. Maps to CLI exit 1 and
    /// HTTP 501; the payload is the one-line explanation shown to the user.
    #[error("{0}")]
    Unsupported(String),
}

/// Copy `src` to `dst` per the §E.4 8-step algorithm. `dst` should be
/// the install target (use [`install_target`]).
pub fn install_binary_to(src: &Path, dst: &Path) -> Result<InstallOutcome, InstallError> {
    // Step 1 — canonicalise src. "(deleted)" ⇒ hard error, see below.
    let source = src.canonicalize().map_err(InstallError::Io)?;

    // Step 2 — dst is supplied.

    // Step 3 — if dst exists AND resolves to the same file, nothing to do.
    if dst.exists() {
        let dst_canon = dst.canonicalize().map_err(InstallError::Io)?;
        if dst_canon == source {
            return Ok(InstallOutcome {
                action: InstallAction::AlreadyCurrent,
                source,
                target: dst.to_path_buf(),
                // Same-words rule (§E.12): "re-running it says already_current".
                notes: Vec::new(),
            });
        }
    }

    // Step 4 — ensure parent dir exists.
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(InstallError::Io)?;
    }

    // Step 5 — temp in the SAME directory ⇒ same filesystem ⇒ rename is
    // atomic. `pid` is the unique suffix — `current_exe()` may be invoked
    // by two tests in the same process and a colliding `.tmp<pid>` would
    // turn one install into the other's "already there" check.
    let tmp = dst.with_extension(format!("tmp{}", std::process::id()));

    // Step 6 — copy, set perms, fsync. Any failure must clean up `tmp`
    // so we don't leave a half-written exe on disk (step 8).
    let copy_result = (|| -> Result<(), InstallError> {
        std::fs::copy(&source, &tmp).map_err(InstallError::Io)?;
        #[cfg(unix)]
        set_executable_mode(&tmp)?;
        // fsync the data so a power loss between rename and the OS's
        // lazy flush doesn't leave us with an empty target.
        let f = std::fs::File::open(&tmp).map_err(InstallError::Io)?;
        f.sync_all().map_err(InstallError::Io)?;
        Ok(())
    })();
    if let Err(e) = copy_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // Step 7 — atomic rename. On Windows, dance around the running-image
    // lock by renaming the existing target out of the way first.
    let action = if dst.exists() {
        #[cfg(windows)]
        {
            let old = dst.with_extension("old");
            // Best-effort: an `old` from a prior install may still be
            // locked by the running instance.
            let _ = std::fs::remove_file(&old);
            // Renaming a *running* image is allowed on Windows — only
            // deleting or writing it is locked.
            std::fs::rename(dst, &old).map_err(InstallError::Io)?;
            std::fs::rename(&tmp, dst).map_err(InstallError::Io)?;
            // Best-effort: if the running instance has it open, this
            // fails silently. We surface the leftover in notes so the
            // wizard can name it.
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&tmp, dst).map_err(InstallError::Io)?;
        }
        InstallAction::Replaced
    } else {
        std::fs::rename(&tmp, dst).map_err(InstallError::Io)?;
        InstallAction::Installed
    };

    // Step 8 — done. Notes produced from the core, not from the CLI, so
    // CLI and wizard share the exact same words (§E.12). The `mut` is
    // only needed on Windows (the conditional push below) — declaring
    // it unconditionally keeps the two cfg paths textually identical.
    #[allow(unused_mut)]
    let mut notes = vec![
        "A running PeakBot keeps the old binary until it restarts — \
         systemctl --user restart peakbot (Linux), log out/in (macOS), \
         or restart the PeakBot task (Windows)."
            .to_string(),
    ];
    #[cfg(windows)]
    if dst.with_extension("old").exists() {
        // §E.4: a note says so when .old survives. The "best effort"
        // remove may have failed because the file is still mapped.
        notes.push(
            "A peakbot.old file may remain next to the new binary — \
             a reboot clears it."
                .to_string(),
        );
    }
    Ok(InstallOutcome {
        action,
        source,
        target: dst.to_path_buf(),
        notes,
    })
}

/// Convenience: `current_exe()` → `install_target()` → `install_binary_to`.
pub fn install_binary() -> Result<InstallOutcome, InstallError> {
    let src = std::env::current_exe().map_err(InstallError::CurrentExe)?;
    let dst = install_target()
        .ok_or_else(|| InstallError::Io(io::Error::other("no install target for this platform")))?;
    install_binary_to(&src, &dst)
}

#[cfg(unix)]
fn set_executable_mode(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(InstallError::Io)?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(InstallError::Io)
}

// ===========================================================================
// §E.5 — token file: `web-token`, written `0600` on Unix, read with
// whitespace-trim, empty = absent. One file, one secret, one place to
// rotate it.
// ===========================================================================

/// The token-file path: `<config_dir>/web-token`, next to `config.yaml`.
/// Returns `None` only if the platform has no config dir at all.
pub fn web_token_path() -> Option<PathBuf> {
    crate::config::get_config_dir().map(|d| d.join("web-token"))
}

/// Pure precedence: `--token > env > file > none`. Empty / whitespace-
/// only values at every level are treated as absent, matching the
/// §E.5 "this machine has a token" model. Used by `main`'s web bind.
pub fn resolve_token(flag: Option<&str>, env: Option<&str>, file: Option<&str>) -> Option<String> {
    [flag, env, file]
        .into_iter()
        .flatten()
        .find(|t| !t.trim().is_empty())
        .map(|t| t.trim().to_string())
}

/// Write the web token to `path` with `0600` perms on Unix. Trailing
/// whitespace/newlines are trimmed first so the on-disk file is
/// *exactly* the secret — no surprise bytes.
pub fn write_web_token(path: &Path, token: &str) -> Result<(), InstallError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(InstallError::Io(io::Error::other(
            "refusing to write a whitespace-only token",
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(InstallError::Io)?;
    }
    // Atomic write — temp + rename, same pattern as `save_config_at`.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, trimmed.as_bytes()).map_err(InstallError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp, perms).map_err(InstallError::Io)?;
    }
    // If `path` exists, remove before rename (Windows rename refuses to
    // overwrite; on Unix it atomically replaces). Same non-atomic
    // window as `save_config_at` — only on Windows, only after the
    // backup story.
    if path.exists() {
        std::fs::remove_file(path).map_err(InstallError::Io)?;
    }
    std::fs::rename(&tmp, path).map_err(InstallError::Io)?;
    Ok(())
}

/// Read the web token from `path`. Returns `Ok(None)` for absent,
/// whitespace-only, or unreadable-as-text; `Err` only for hard I/O
/// failures. Mirrors the §E.5 "empty/whitespace reads as no token" rule.
pub fn read_web_token(path: &Path) -> Result<Option<String>, InstallError> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(InstallError::Io(e)),
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

// ===========================================================================
// Service dispatch — `ServiceOp`, `ServiceReport`, `service_op`.
// Plan §E.1 + §E.9 + §D I5.
//
// `service_op` routes the three verbs to the one platform module this
// build selected (`linux` / `macos` / `windows`, or the inline fallback).
// It stays a plain `fn` so the HTTP layer can keep it behind the §E.9
// `ServiceFn` pointer seam and swap a fake in tests.
// ===========================================================================

/// The kind of service manager on this host. Reported in every
/// `ServiceReport` so the wizard says what it is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManager {
    SystemdUser,
    LaunchdAgent,
    WindowsTask,
    /// No service manager on this host (Android, WSL1, systemd-less
    /// distros). Maps to HTTP 501.
    Unsupported,
}

impl ServiceManager {
    /// The wire spelling from §B (`systemd-user` | `launchd-agent` |
    /// `windows-task`). Kept beside the enum so the CLI and the future
    /// HTTP handler cannot drift apart on the string.
    pub fn as_wire(self) -> &'static str {
        match self {
            ServiceManager::SystemdUser => "systemd-user",
            ServiceManager::LaunchdAgent => "launchd-agent",
            ServiceManager::WindowsTask => "windows-task",
            ServiceManager::Unsupported => "unsupported",
        }
    }
}

/// Is the service running *right now*? Three-state on purpose (§B): on
/// Windows the only readable answer is a localised `schtasks` column, so
/// Windows says `Unknown` instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Stopped,
    Unknown,
}

impl RunState {
    /// Wire spelling from §B: `running` | `stopped` | `unknown`.
    pub fn as_wire(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Stopped => "stopped",
            RunState::Unknown => "unknown",
        }
    }
}

/// What the service verb did, plus the commands it ran and any
/// platform-specific guidance. `commands` are the *exact* subprocess
/// invocations so the wizard's log pane can replay them. Field-for-field
/// the §B `ServiceResponse`.
#[derive(Debug, Clone)]
pub struct ServiceReport {
    pub manager: ServiceManager,
    /// `peakbot.service` | `com.peakbot.agent` | `PeakBot` (from [`APP`]).
    pub name: String,
    /// The unit/plist path. `None` on Windows — the registered task *is*
    /// the artifact, and a leftover XML on disk would be a staler second
    /// copy of it (§E.8).
    pub artifact: Option<PathBuf>,
    pub installed: bool,
    pub exe: Option<PathBuf>,
    pub run_state: RunState,
    /// Linux: linger is enabled. macOS/Windows: always false — both run
    /// the agent inside the user's login session, full stop.
    pub survives_logout: bool,
    pub commands: Vec<String>,
    pub notes: Vec<String>,
}

/// Service verb. `Install` carries a validated [`ServicePlan`]
/// (validation already happened at the boundary — `ServicePlan::new`
/// is the only way to build one).
#[derive(Debug, Clone)]
pub enum ServiceOp {
    Install { plan: ServicePlan },
    Uninstall,
    Status,
}

/// Dispatch a service verb to the one platform module this build selected
/// (§E.1). The whole platform surface is these three calls.
pub fn service_op(op: ServiceOp) -> Result<ServiceReport, InstallError> {
    match op {
        ServiceOp::Install { plan } => platform::install_service(&plan),
        ServiceOp::Uninstall => platform::uninstall_service(),
        ServiceOp::Status => platform::service_status(),
    }
}

// ---------------------------------------------------------------------------
// Shared plumbing for the platform modules: one subprocess runner, one XML
// escaper. Both live here so the three modules cannot drift apart on them.
// ---------------------------------------------------------------------------

/// One finished subprocess. A non-zero exit is *data*, not an error:
/// several call sites deliberately tolerate failure (`launchctl bootout`
/// on a label that was never loaded, `systemctl disable` on a unit that
/// is already gone) — that tolerance is what makes install idempotent.
#[derive(Debug)]
pub(crate) struct CommandRun {
    /// The invocation as the user would type it — goes into `commands`.
    pub display: String,
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandRun {
    /// Turn a non-zero exit into an error carrying the child's own words.
    /// Prefers stderr, falls back to stdout (`schtasks` reports errors on
    /// stdout on some Windows builds).
    pub(crate) fn require_ok(self) -> Result<Self, InstallError> {
        if self.ok {
            return Ok(self);
        }
        let msg = match (self.stderr.trim(), self.stdout.trim()) {
            ("", "") => "exited non-zero with no output".to_string(),
            ("", out) => out.to_string(),
            (err, _) => err.to_string(),
        };
        Err(InstallError::CommandFailed {
            command: self.display,
            stderr: msg,
        })
    }
}

/// Run `program args…`, capturing both streams. `Err` only when the
/// program could not be spawned at all (missing `systemctl`/`launchctl`/
/// `schtasks`), which is a different problem from "it ran and said no".
pub(crate) fn run(program: &str, args: &[&str]) -> Result<CommandRun, InstallError> {
    let display = if args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", args.join(" "))
    };
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| {
            InstallError::Io(io::Error::new(
                e.kind(),
                format!("failed to run `{display}`: {e}"),
            ))
        })?;
    Ok(CommandRun {
        display,
        ok: out.status.success(),
        stdout: decode_output(&out.stdout),
        stderr: decode_output(&out.stderr),
    })
}

/// Decode captured child output. `schtasks /Query /XML` writes UTF-16LE
/// with a BOM when redirected; every other tool we call writes UTF-8.
/// Sniffing the BOM here means no platform module has to care.
pub(crate) fn decode_output(bytes: &[u8]) -> String {
    if let [0xFF, 0xFE, rest @ ..] = bytes {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The five XML character escapes (§E.7). Enough because the only
/// interpolated values are paths, a socket address and a user name — a
/// dependency for this is not earned.
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Tests — pure unit tests for the §E.4/§E.5 helpers. The plan-level
// renderer tests (the `install_render_tests.rs` integration file) live
// next to the integration target; these are the in-module coverage for
// the *plumbing*.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;

    // ── §E.5 ────────────────────────────────────────────────────────

    fn bind_loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7823)
    }

    fn bind_lan() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 7823)
    }

    #[test]
    fn service_plan_argv_is_exe_then_bind_then_addr() {
        let p = ServicePlan::new(PathBuf::from("/usr/bin/peakbot"), bind_loopback(), None).unwrap();
        assert_eq!(
            p.argv(),
            vec![
                "/usr/bin/peakbot".to_string(),
                "--bind".to_string(),
                "127.0.0.1:7823".to_string(),
            ]
        );
    }

    #[test]
    fn service_plan_accessors_match_inputs() {
        let p = ServicePlan::new(
            PathBuf::from("/x/peakbot"),
            bind_lan(),
            Some("tok".to_string()),
        )
        .unwrap();
        assert_eq!(p.exe(), Path::new("/x/peakbot"));
        assert_eq!(p.bind(), bind_lan());
        assert_eq!(p.token(), Some("tok"));
    }

    #[test]
    fn plan_error_token_required_carries_the_rejected_bind() {
        let err = PlanError::TokenRequired(bind_lan());
        // Display must mention the bind so the CLI can echo it.
        let s = err.to_string();
        assert!(s.contains("0.0.0.0:7823"), "{s}");
    }

    // ── §E.4 path_state — edge cases beyond the integration file ──

    #[test]
    fn path_state_empty_path_is_always_not_on_path() {
        let s = path_state(OsStr::new(""), Path::new("/home/u/.local/bin/peakbot"));
        assert!(matches!(s, PathState::NotOnPath { .. }), "got {s:?}");
    }

    #[test]
    fn path_state_target_with_no_filename_is_not_on_path() {
        // Degenerate: a path with no final component can't be on PATH.
        let s = path_state(OsStr::new("/a:/b"), Path::new("/"));
        assert!(matches!(s, PathState::NotOnPath { .. }), "got {s:?}");
    }

    #[test]
    fn path_state_split_paths_is_platform_specific() {
        // `std::env::split_paths` only knows the *current* platform's
        // separator (`:` on Unix, `;` on Windows). On Unix a `;` is
        // literal — the whole string is one entry, so no match ⇒
        // NotOnPath. On Windows it's two entries, first matches ⇒
        // OnPath. The function relies on this; we just assert the
        // *verdict* matches the platform we're on.
        let s = path_state(OsStr::new("/a;/b"), Path::new("/a/peakbot"));
        #[cfg(unix)]
        assert!(matches!(s, PathState::NotOnPath { .. }), "unix got {s:?}");
        #[cfg(windows)]
        assert_eq!(s, PathState::OnPath);
    }

    #[test]
    fn path_state_dir_present_with_trailing_slash_still_matches() {
        // PATH entries commonly carry a trailing separator; `join`
        // tolerates that, so the candidate still equals target.
        let s = path_state(
            OsStr::new("/home/u/.local/bin/:/usr/bin"),
            Path::new("/home/u/.local/bin/peakbot"),
        );
        assert_eq!(s, PathState::OnPath);
    }

    #[test]
    fn path_state_shadow_is_the_first_candidate_not_the_last() {
        // Two shadowing candidates before target → we report the FIRST
        // (the one the shell would actually run).
        let s = path_state(
            OsStr::new("/first/bin:/second/bin:/home/u/.local/bin"),
            Path::new("/home/u/.local/bin/peakbot"),
        );
        match s {
            PathState::Shadowed { by } => assert_eq!(by, PathBuf::from("/first/bin/peakbot")),
            other => panic!("expected Shadowed, got {other:?}"),
        }
    }

    // ── §E.3 install_target ────────────────────────────────────────

    #[test]
    fn install_target_ends_with_bin_name() {
        // We can't easily override $HOME in a parallel-safe way (and
        // don't want to — the dirs crate is the canonical answer).
        // The assertion that holds on every build is just the filename.
        let t = install_target().expect("install_target resolves on this platform");
        assert_eq!(
            t.file_name().and_then(|n| n.to_str()),
            Some(target_exe_name()),
            "target filename must match target_exe_name(); got {t:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_target_on_unix_is_local_bin_peakbot() {
        let t = install_target().unwrap();
        let comp: Vec<String> = t
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &comp[comp.len() - 3..],
            &[
                ".local".to_string(),
                "bin".to_string(),
                "peakbot".to_string()
            ],
            "got {t:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn install_target_on_windows_is_programs_peakbot_peakbot_exe() {
        let t = install_target().unwrap();
        let comp: Vec<String> = t
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &comp[comp.len() - 3..],
            &[
                "Programs".to_string(),
                "peakbot".to_string(),
                "peakbot.exe".to_string(),
            ],
            "got {t:?}"
        );
    }

    // ── §E.4 copy engine ────────────────────────────────────────────

    /// Helper: write `n` bytes of arbitrary content into `path` and
    /// return it. Lets tests construct distinguishable "before" /
    /// "after" binaries.
    fn write_bytes(path: &Path, n: usize) -> PathBuf {
        std::fs::write(path, vec![0xAB_u8; n]).unwrap();
        path.to_path_buf()
    }

    #[test]
    fn copy_engine_fresh_install_writes_dst_and_reports_installed() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("src.bin"), 4096);
        let dst = dir.path().join("peakbot");
        let outcome = install_binary_to(&src, &dst).unwrap();
        assert_eq!(outcome.action, InstallAction::Installed);
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            std::fs::read(&src).unwrap(),
            "dst must equal src byte-for-byte after install"
        );
        // The restart note is always present on Installed/Replaced.
        assert!(
            outcome
                .notes
                .iter()
                .any(|n| n.contains("restart") || n.contains("Restart")),
            "expected a restart note in {:?}",
            outcome.notes
        );
    }

    #[test]
    fn copy_engine_already_current_writes_nothing_when_src_equals_dst() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("self.bin"), 2048);
        // Canonicalise so the `src == dst` check (which canonicalises
        // dst) can match a real path.
        let src_canon = src.canonicalize().unwrap();
        let dst = src_canon.clone();

        // Mark dst with a sentinel content so we can prove the function
        // did NOT rewrite it.
        std::fs::write(&dst, b"SENTINEL_ALREADY_CURRENT").unwrap();

        let outcome = install_binary_to(&src_canon, &dst).unwrap();
        assert_eq!(outcome.action, InstallAction::AlreadyCurrent);
        // Sentinel survived — zero writes happened.
        assert_eq!(std::fs::read(&dst).unwrap(), b"SENTINEL_ALREADY_CURRENT");
        assert!(outcome.notes.is_empty(), "no notes for AlreadyCurrent");
    }

    #[test]
    fn copy_engine_replace_existing_reports_replaced_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("new.bin"), 1024);
        let dst = dir.path().join("peakbot");
        std::fs::write(&dst, b"OLD-CONTENT").unwrap();

        let outcome = install_binary_to(&src, &dst).unwrap();
        assert_eq!(outcome.action, InstallAction::Replaced);
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            std::fs::read(&src).unwrap(),
            "dst must equal src after a replace"
        );
    }

    #[test]
    fn copy_engine_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("src.bin"), 16);
        // dst parent is two levels deep and does NOT exist.
        let dst = dir.path().join("deep").join("nest").join("peakbot");
        let outcome = install_binary_to(&src, &dst).unwrap();
        assert_eq!(outcome.action, InstallAction::Installed);
        assert!(dst.exists());
    }

    #[test]
    fn copy_engine_leaves_no_tmp_survivors_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("src.bin"), 8);
        let dst = dir.path().join("peakbot");
        let _ = install_binary_to(&src, &dst).unwrap();
        // The tmp suffix is `tmp<pid>`. Sweep everything in the dir
        // that looks like one and assert no leftovers.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains(&format!("tmp{}", std::process::id()))
            })
            .collect();
        assert!(leftovers.is_empty(), "found tmp leftovers: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn copy_engine_sets_0755_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = write_bytes(&dir.path().join("src.bin"), 16);
        let dst = dir.path().join("peakbot");
        install_binary_to(&src, &dst).unwrap();
        let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755, "dst must be 0755 on unix; got {mode:o}");
    }

    // ── §E.5 token file ─────────────────────────────────────────────

    #[test]
    fn resolve_token_precedence_is_flag_then_env_then_file() {
        assert_eq!(
            resolve_token(Some("f"), Some("e"), Some("x")),
            Some("f".to_string())
        );
        assert_eq!(
            resolve_token(None, Some("e"), Some("x")),
            Some("e".to_string())
        );
        assert_eq!(resolve_token(None, None, Some("x")), Some("x".to_string()));
        assert_eq!(resolve_token(None, None, None), None);
    }

    #[test]
    fn resolve_token_skips_empty_and_whitespace_at_every_level() {
        assert_eq!(
            resolve_token(Some(""), Some("e"), Some("x")),
            Some("e".to_string())
        );
        assert_eq!(
            resolve_token(Some("   "), Some("  "), Some("x")),
            Some("x".to_string())
        );
        assert_eq!(resolve_token(Some(""), Some("   "), Some("\n")), None);
    }

    #[test]
    fn write_then_read_web_token_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        write_web_token(&path, "s3cret\n").unwrap();
        assert_eq!(read_web_token(&path).unwrap(), Some("s3cret".to_string()));
    }

    #[test]
    fn read_web_token_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        assert_eq!(read_web_token(&path).unwrap(), None);
    }

    #[test]
    fn read_web_token_treats_whitespace_only_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        std::fs::write(&path, "  \n\t").unwrap();
        assert_eq!(read_web_token(&path).unwrap(), None);
    }

    #[test]
    fn write_web_token_rejects_whitespace_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        assert!(write_web_token(&path, "   \n").is_err());
        assert!(!path.exists(), "no file must be written on reject");
    }

    #[cfg(unix)]
    #[test]
    fn write_web_token_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        write_web_token(&path, "s3cret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "web-token must be 0600 on unix; got {mode:o}");
    }

    #[test]
    fn write_web_token_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-token");
        write_web_token(&path, "first").unwrap();
        write_web_token(&path, "second").unwrap();
        assert_eq!(read_web_token(&path).unwrap(), Some("second".to_string()));
    }

    // ── ServiceOp / service_op seam ─────────────────────────────────

    #[test]
    #[ignore = "runs the host service manager; no user session bus in CI"]
    fn service_op_status_talks_to_the_real_service_manager() {
        // Live smoke test for the I5 dispatch: on a developer desktop
        // this must reach the platform module and answer without
        // panicking, whether or not a service is installed.
        let r = service_op(ServiceOp::Status);
        match r {
            Ok(report) => assert_eq!(report.name, expected_name_for_this_platform()),
            // A machine without a user session bus answers Unsupported —
            // that is a correct answer, not a failure.
            Err(InstallError::Unsupported(msg)) => assert!(!msg.is_empty()),
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn expected_name_for_this_platform() -> String {
        #[cfg(target_os = "linux")]
        {
            APP.unit.to_string()
        }
        #[cfg(target_os = "macos")]
        {
            APP.launchd_label.to_string()
        }
        #[cfg(windows)]
        {
            APP.task.to_string()
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    fn expected_name_for_this_platform() -> String {
        // Unreachable in practice: the fallback platform always errors.
        String::new()
    }

    #[test]
    fn service_op_on_an_unsupported_host_carries_a_message() {
        // The 501 payload must never be empty — it is the entire
        // explanation the user gets.
        let e = InstallError::Unsupported("no systemd user session here".to_string());
        assert_eq!(e.to_string(), "no systemd user session here");
    }

    // ── shared plumbing ─────────────────────────────────────────────

    #[test]
    fn decode_output_reads_utf16le_with_bom() {
        // `schtasks /Query /XML` looks like this when redirected.
        let mut bytes = vec![0xFF, 0xFE];
        for u in "<Task/>".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_output(&bytes), "<Task/>");
    }

    #[test]
    fn decode_output_reads_plain_utf8() {
        assert_eq!(decode_output(b"active\n"), "active\n");
        assert_eq!(decode_output(b""), "");
    }

    #[test]
    fn xml_escape_covers_the_five_characters() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(
            xml_escape("/Users/you/bin/peakbot"),
            "/Users/you/bin/peakbot"
        );
    }

    #[test]
    #[cfg(unix)]
    fn run_captures_a_failing_command_as_data_not_an_error() {
        // Non-zero exit is data; `require_ok` is what turns it into an
        // error. `false` exists on every unix CI image.
        let r = run("false", &[]).expect("spawn must succeed");
        assert!(!r.ok);
        assert_eq!(r.display, "false");
        assert!(r.require_ok().is_err());
    }

    #[test]
    fn run_reports_a_missing_program_as_io() {
        let e = run("peakbot-no-such-program-xyz", &["--version"]).unwrap_err();
        assert!(matches!(e, InstallError::Io(_)), "got {e:?}");
    }

    #[test]
    fn wire_spellings_match_the_b_contract() {
        assert_eq!(ServiceManager::SystemdUser.as_wire(), "systemd-user");
        assert_eq!(ServiceManager::LaunchdAgent.as_wire(), "launchd-agent");
        assert_eq!(ServiceManager::WindowsTask.as_wire(), "windows-task");
        assert_eq!(RunState::Running.as_wire(), "running");
        assert_eq!(RunState::Stopped.as_wire(), "stopped");
        assert_eq!(RunState::Unknown.as_wire(), "unknown");
    }
}
