//! The in-process core a phone app embeds (Local edition). The app picks a loopback listener and a per-launch token; the
//! core boots from `<config root>/config.yaml` and serves the full web UI
//! on whatever listener it is handed — again after iOS reclaims one.
//!
//! Local limits: global config only (`RepoMode::Ignore`), no skills,
//! no MCP, no shell. A missing or invalid config boots the needs_setup
//! server, so the first-run wizard can write one; only IO fails the boot.

use crate::SkillRegistry;
use crate::boot::{WebBoot, WebSession, session_deps, web_boot};
use crate::config::resolve::{LoadError, ParseCtx};
use crate::config::{Config, RepoMode, auto_migrate};
use crate::ui::web::settings::TokenSource;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

/// A booted core: the router (its state is shared behind `Arc`s, so every
/// serve reaches the same sessions) and the idle-session reaper.
pub struct LocalCore {
    router: axum::Router,
    reaper: Option<tokio::task::JoinHandle<()>>,
}

impl LocalCore {
    /// Boot from `<config root>/config.yaml`; `paths::init_roots` must have
    /// run. `token` guards every route — other apps can reach loopback.
    pub async fn boot(token: String) -> anyhow::Result<LocalCore> {
        // A blank token would match a bare `?token=`: the core would be open.
        anyhow::ensure!(!token.trim().is_empty(), "the Local core needs a token");
        let config_dir = crate::paths::config_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config root — call paths::init_roots before booting",
            )
        })?;
        let boot_cwd = std::env::current_dir().unwrap_or_default();
        let session = load_session(&config_dir, &boot_cwd)?;
        let ui = web_boot(
            WebBoot {
                // The port is the embedder's, chosen per serve; this is only
                // the loopback fact the settings surface reports.
                addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                token: Some(token),
                // Handed in by the launcher, like `--token`.
                token_source: TokenSource::Cli,
                tls: false,
                tls_names: Vec::new(),
                config_dir,
                boot_cwd,
            },
            session,
        )?;
        Ok(LocalCore {
            router: ui.router(),
            reaper: ui.spawn_reaper(),
        })
    }

    /// Serve the full router on `listener` until the returned future is
    /// dropped. Callable again on a new listener (iOS re-serve) — every
    /// serve shares this core's sessions. The future owns a router clone
    /// rather than borrowing `self`, so the caller can `tokio::spawn` it.
    pub fn serve(
        &self,
        listener: tokio::net::TcpListener,
    ) -> impl Future<Output = std::io::Result<()>> + Send + use<> {
        let router = self.router.clone();
        async move { axum::serve(listener, router).await }
    }
}

impl Drop for LocalCore {
    /// A restart drops the core and boots a new one in the same process;
    /// a detached reaper would keep the old sessions alive forever.
    fn drop(&mut self) {
        if let Some(reaper) = &self.reaper {
            reaper.abort();
        }
    }
}

/// Migrate, then load the global config. Any failure but IO means "set up
/// again": the reason is logged and the SPA shows first-run.
fn load_session(config_dir: &Path, boot_cwd: &Path) -> anyhow::Result<WebSession> {
    let boot_notices = match auto_migrate(config_dir, None) {
        Ok(outcome) => outcome.notices(),
        Err(e) => {
            return Ok(WebSession::NeedsSetup {
                migrate_error: Some(e.to_string()),
            });
        }
    };
    let ctx = ParseCtx { known_skills: &[] };
    let config = match Config::load(boot_cwd, RepoMode::Ignore, &ctx) {
        Ok(config) => config,
        Err(LoadError::Io(path, e)) => {
            return Err(anyhow::Error::new(e).context(format!("cannot read {}", path.display())));
        }
        Err(LoadError::Missing(_)) => {
            return Ok(WebSession::NeedsSetup {
                migrate_error: None,
            });
        }
        Err(e) => {
            tracing::warn!("config.yaml is not usable, booting setup: {e:?}");
            return Ok(WebSession::NeedsSetup {
                migrate_error: None,
            });
        }
    };
    // Boot-only, process-wide: a restart in the same process keeps the
    // first boot's timeouts.
    crate::http::init_timeouts(config.general.http.clone());
    Ok(WebSession::Ready(session_deps(
        config,
        SkillRegistry::new(),
        Vec::new(),
        boot_notices,
        None,
        None,
    )))
}
