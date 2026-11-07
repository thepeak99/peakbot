//! Platform path roots — the ONLY module that may call the platform-dir
//! helpers (`dirs` crate, `ProjectDirs`). `init_roots` (boot-only, like
//! `http::init_timeouts`) lets an embedder relocate every PeakBot path
//! under its own sandbox before any core call; unset = today's platform
//! behaviour, byte-for-byte.
//!
//! Set-roots layout: config → `<config>/config.yaml` (+ `<config>/systemd/…`),
//! data → `<data>/{conversations,uploads,skills}`, cache → `<cache>/{tls,mcp-auth}`.

use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::Context;
use directories_next::ProjectDirs;

static ROOTS: OnceLock<Roots> = OnceLock::new();

/// The three roots every PeakBot path is resolved under when set.
#[derive(Debug, Clone)]
pub struct Roots {
    /// `config.yaml`, the systemd unit — everything config-shaped.
    pub config: PathBuf,
    /// conversations, uploads, global skills, install data.
    pub data: PathBuf,
    /// TLS CA, MCP OAuth cache — everything cache-shaped.
    pub cache: PathBuf,
}

/// Boot-only: later calls are ignored (like `http::init_timeouts`), so the
/// roots are fixed before any core call and never mid-run.
pub fn init_roots(r: Roots) {
    let _ = ROOTS.set(r);
}

fn roots() -> Option<&'static Roots> {
    ROOTS.get()
}

// ── Raw roots: what today's code gets when unset, the root when set. ──────

/// PeakBot's config dir: the platform `…/peakbot` dir when unset, the
/// `config` root when set.
pub fn config_dir() -> Option<PathBuf> {
    match roots() {
        Some(r) => Some(r.config.clone()),
        // The exact dir `get_config_dir` produced before the seam.
        None => {
            ProjectDirs::from("com", "peakbot", "peakbot").map(|d| d.config_dir().to_path_buf())
        }
    }
}

/// The bare platform config dir (no `peakbot` subdir) — the systemd unit's
/// home; the `config` root when set.
pub fn platform_config_dir() -> Option<PathBuf> {
    roots().map(|r| r.config.clone()).or_else(dirs::config_dir)
}

/// PeakBot's data dir shown in Settings: `<data_dir>/peakbot` (unset) /
/// the `data` root itself (set).
pub fn app_data_dir() -> Option<PathBuf> {
    match roots() {
        Some(r) => Some(r.data.clone()),
        None => dirs::data_dir().map(|p| p.join("peakbot")),
    }
}

/// The platform data-local dir (differs from `data_dir` on Windows); the
/// `data` root when set.
pub fn data_local_dir() -> Option<PathBuf> {
    roots()
        .map(|r| r.data.clone())
        .or_else(dirs::data_local_dir)
}

/// The platform cache dir; the `cache` root when set.
pub fn cache_dir() -> Option<PathBuf> {
    roots().map(|r| r.cache.clone()).or_else(dirs::cache_dir)
}

/// The home dir. `None` when roots are set — Android has no HOME — so every
/// `~` expander keeps its existing `None` fallback instead of panicking.
pub fn home_dir() -> Option<PathBuf> {
    match roots() {
        Some(_) => None,
        None => dirs::home_dir(),
    }
}

// ── Layout: where PeakBot keeps each thing under the roots. ───────────────

/// `<data_local>/peakbot/uploads` (unset) / `<data>/uploads` (set).
pub fn uploads_root() -> PathBuf {
    match roots() {
        Some(r) => r.data.join("uploads"),
        None => dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("peakbot")
            .join("uploads"),
    }
}

/// `<data_local>/peakbot/conversations` (unset) / `<data>/conversations` (set).
pub fn conversations_dir() -> PathBuf {
    match roots() {
        Some(r) => r.data.join("conversations"),
        None => dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("peakbot")
            .join("conversations"),
    }
}

/// `<home>/.agents/skills` (unset) / `Some(<data>/skills)` (set); `None`
/// unset only when there is no home dir, exactly as today.
pub fn global_skills_dir() -> Option<PathBuf> {
    match roots() {
        Some(r) => Some(r.data.join("skills")),
        None => dirs::home_dir().map(|h| h.join(".agents").join("skills")),
    }
}

/// `<cache>/peakbot/tls` (unset) / `<cache>/tls` (set).
pub fn tls_dir() -> anyhow::Result<PathBuf> {
    match roots() {
        Some(r) => Ok(r.cache.join("tls")),
        None => Ok(dirs::cache_dir()
            .context("no cache directory available to store the PeakBot CA")?
            .join("peakbot")
            .join("tls")),
    }
}

/// `<cache>/peakbot/mcp-auth` (unset) / `<cache>/mcp-auth` (set).
pub fn mcp_auth_dir() -> Option<PathBuf> {
    match roots() {
        Some(r) => Some(r.cache.join("mcp-auth")),
        None => Some(dirs::cache_dir()?.join("peakbot").join("mcp-auth")),
    }
}
