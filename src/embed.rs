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
    /// This core's own Cloud handle (the process-wide one is set once and
    /// would go stale across a restart in the same process).
    cloud: Option<crate::cloud::CloudHandle>,
}

impl LocalCore {
    /// Boot from `<config root>/config.yaml`; `paths::init_roots` must have
    /// run. `token` guards every route — other apps can reach loopback.
    pub async fn boot(token: String) -> anyhow::Result<LocalCore> {
        Self::boot_inner(token, None).await
    }

    async fn boot_inner(
        token: String,
        keystore: Option<std::sync::Arc<dyn crate::cloud::SeedStore>>,
    ) -> anyhow::Result<LocalCore> {
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
        // PR-C §B: the phone is controller-only — CloudLink yes (with its
        // own identity), CloudServe never (`inbound = None`, B8).
        let cloud_config = match &session {
            WebSession::Ready(deps) => Some(deps.config.clone()),
            WebSession::NeedsSetup { .. } => None,
        };
        let mut cloud = None;
        let mut ui = web_boot(
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
        if let Some(config) = cloud_config
            && let Some(handle) = {
                let (store, callback) = match keystore {
                    Some(ks) => (
                        migrated_store(ks, cloud_state_dir()?),
                        crate::cloud::login::CallbackSource::DeepLink(
                            crate::cloud::login::AppRedirect::mobile(),
                        ),
                    ),
                    None => (
                        cloud_store(&config)?,
                        crate::cloud::login::CallbackSource::Loopback,
                    ),
                };
                crate::boot::boot_cloud_with_callback(
                    &config,
                    cloud_state_dir()?,
                    &crate::boot::device_name(),
                    store,
                    None,
                    callback,
                )
            }
        {
            cloud = Some(handle.clone());
            ui = ui.with_cloud(Some(handle));
        }
        Ok(LocalCore {
            router: ui.router(),
            reaper: ui.spawn_reaper(),
            cloud,
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

    /// P5 C5: boot with an injected keystore (the phone's OS keystore). The
    /// given `Arc<dyn SeedStore>` is wrapped in a MigratingSeedStore that
    /// moves a legacy file seed into it once. A migration error pauses
    /// Cloud Mode in a visible error state — it never boots a fresh genesis.
    pub async fn boot_with_store(
        token: String,
        keystore: std::sync::Arc<dyn crate::cloud::SeedStore>,
    ) -> anyhow::Result<LocalCore> {
        Self::boot_inner(token, Some(keystore)).await
    }

    /// P5 C2: hand an inbound deep-link URL to the pending DeepLink login.
    /// Never panics; a failure is stored as the login error.
    pub async fn deliver_login_callback(&self, url: &str) {
        let Some(handle) = self.cloud.clone() else {
            tracing::warn!("login callback with Cloud Mode off");
            return;
        };
        if let Err(e) = handle.deliver_callback(url).await {
            tracing::warn!(error = %e, "login callback rejected");
            handle.set_login_error(e.to_string());
        }
    }

    /// P5 C3: app foreground signal.
    pub fn set_foreground(&self, fg: bool) {
        if let Some(handle) = self.cloud.clone() {
            handle.set_foreground(fg);
        }
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

/// `<data dir>/cloud` — the public chain/pin state (p3-design §C).
fn cloud_state_dir() -> anyhow::Result<std::path::PathBuf> {
    let dir = crate::paths::app_data_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data dir"))?;
    Ok(dir.join("cloud"))
}

/// The phone's store: the 0600 file (no OS keyring in the Local edition).
fn cloud_store(config: &Config) -> anyhow::Result<std::sync::Arc<dyn crate::cloud::SeedStore>> {
    if config.general.cloud.is_some() {
        return Ok(std::sync::Arc::new(
            crate::cloud::keyring::FileSeedStore::new(cloud_state_dir()?),
        ));
    }
    // boot_cloud returns None before the store is touched; never used.
    Ok(std::sync::Arc::new(
        crate::cloud::keyring::FileSeedStore::new(std::env::temp_dir()),
    ))
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

/// Wrap the keystore in the one-shot migration. On a migration error the
/// store handed to Cloud fails every call, so the actor lands in a visible
/// `Trust(SeedStoreError)` state and never generates a fresh identity.
fn migrated_store(
    keystore: std::sync::Arc<dyn crate::cloud::SeedStore>,
    dir: std::path::PathBuf,
) -> std::sync::Arc<dyn crate::cloud::SeedStore> {
    let legacy = crate::cloud::keyring::FileSeedStore::new(dir);
    match crate::cloud::migrate::MigratingSeedStore::open(keystore, legacy) {
        Ok(m) => std::sync::Arc::new(m),
        Err(e) => {
            tracing::error!(error = %e, "keystore migration failed; Cloud Mode paused");
            std::sync::Arc::new(BrokenStore(e.to_string()))
        }
    }
}

/// Every call errors; see [`migrated_store`].
struct BrokenStore(String);

impl BrokenStore {
    fn err<T>(&self) -> std::io::Result<T> {
        Err(std::io::Error::other(self.0.clone()))
    }
}

impl crate::cloud::SeedStore for BrokenStore {
    fn load_seed(&self) -> std::io::Result<Option<crate::cloud::Seed>> {
        self.err()
    }
    fn store_seed(&self, _: &crate::cloud::Seed) -> std::io::Result<()> {
        self.err()
    }
    fn load_token(&self) -> std::io::Result<Option<crate::cloud::SessionToken>> {
        self.err()
    }
    fn store_token(&self, _: &crate::cloud::SessionToken) -> std::io::Result<()> {
        self.err()
    }
    fn clear_token(&self) -> std::io::Result<()> {
        self.err()
    }
    fn wipe(&self) -> std::io::Result<()> {
        self.err()
    }
}
