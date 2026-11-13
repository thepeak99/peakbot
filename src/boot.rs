//! The boot sequence every front end shares once its own policy has
//! produced a `Config` (or decided there is none): the CLI (`main`) and the
//! in-process phone core (`embed::LocalCore`) both build their web UI
//! through [`web_boot`], so the two can never wire sessions differently.
//!
//! What stays with the caller is policy: where the config comes from
//! (repo files, a `--profile` pin), which skills / MCP servers / shell the
//! process gets, how bind, token and TLS resolve, and what a broken config
//! means (the CLI exits; the phone shows setup).

use crate::config::Config;
use crate::config::resolve::Home;
use crate::ui::web::settings::{SettingsBoot, TokenSource};
use crate::ui::{WebUi, build_models_snapshot};
use crate::{ConversationStorage, FileStorage, McpServerHandle, SessionDeps, ShellKind};
use crate::{SkillRegistry, attachments::UploadStore};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Build the deps every session is created from: the loaded config plus
/// the process-wide pieces the caller resolved, with the conversation
/// storage and the upload store derived from the config's boot-only
/// `general` keys. One instance per process (or per embedded core).
pub fn session_deps(
    config: Config,
    skills: SkillRegistry,
    mcp_handles: Vec<McpServerHandle>,
    boot_notices: Vec<String>,
    shell_kind: Option<ShellKind>,
    profile_pin: Option<String>,
) -> Arc<SessionDeps> {
    session_deps_in(
        config,
        skills,
        mcp_handles,
        boot_notices,
        shell_kind,
        profile_pin,
        UploadStore::default_root(),
    )
}

/// [`session_deps`] with the upload root injected (PR-C e2e seam):
/// `paths::init_roots` is a process-global OnceLock, so two cores in one
/// process must not both derive their upload dir from it.
pub fn session_deps_in(
    config: Config,
    skills: SkillRegistry,
    mcp_handles: Vec<McpServerHandle>,
    boot_notices: Vec<String>,
    shell_kind: Option<ShellKind>,
    profile_pin: Option<String>,
    uploads_root: PathBuf,
) -> Arc<SessionDeps> {
    let uploads = UploadStore::new(uploads_root, config.general.uploads);
    // Boot-only: the turn-end channel is process-wide and built once
    // (notify PR-B §7.6) — `None` iff `general.notifications.enabled`
    // is false, so "disabled but still emitting" cannot exist. Derived
    // before `config` moves into the Arc.
    let turn_ends = config
        .general
        .notifications
        .enabled
        .then(crate::notify::turn_end_channel);
    Arc::new(SessionDeps {
        storage: conversation_storage(&config),
        config: Arc::new(config),
        skills,
        mcp_handles: Arc::new(mcp_handles),
        boot_notices: Mutex::new(boot_notices),
        shell_kind,
        profile_pin,
        uploads,
        turn_ends,
    })
}

/// One storage shared by every session (it writes a file per conversation
/// id), or `None` when auto-save is off. A storage dir that can't be
/// created degrades to no persistence rather than refusing to boot.
fn conversation_storage(config: &Config) -> Option<Arc<dyn ConversationStorage>> {
    let conversation = &config.general.conversation;
    if !conversation.auto_save {
        return None;
    }
    let storage_dir = conversation.storage_dir_or_default();
    match FileStorage::new(storage_dir.clone()) {
        Ok(storage) => {
            tracing::info!("Conversation storage enabled at: {:?}", storage_dir);
            Some(Arc::new(storage))
        }
        Err(e) => {
            tracing::warn!(
                "Failed to create conversation storage at {:?}: {}. \
                 Continuing without persistence.",
                storage_dir,
                e
            );
            None
        }
    }
}

/// The web-server facts the caller resolved by its own policy (spec §8.4):
/// where it listens, the token guarding every route and where that token
/// came from, TLS, and the file locations the settings surface edits.
pub struct WebBoot {
    pub addr: SocketAddr,
    pub token: Option<String>,
    pub token_source: TokenSource,
    pub tls: bool,
    /// Extra TLS SANs (`--tls-name`); only meaningful with `tls`.
    pub tls_names: Vec<String>,
    pub config_dir: PathBuf,
    pub boot_cwd: PathBuf,
}

/// What the web UI serves: live chat sessions, or the needs_setup server
/// (spec §6.9) — the SPA and settings with `/ws` the 503 setup guard. A
/// boot migration failure only exists without a config, so it lives here.
pub enum WebSession {
    Ready(Arc<SessionDeps>),
    NeedsSetup { migrate_error: Option<String> },
}

/// The ONE web boot: the models snapshot and starting alias for the
/// picker, the settings surface's boot facts, and the `WebUi` itself.
/// Fails only when the deps' `--profile` pin names no local profile — the
/// CLI validates the pin first, and the embedded core never pins. A
/// relay-only config boots Ready with no models and an empty alias: the
/// remote catalog and list come from the relay on attach.
pub fn web_boot(web: WebBoot, session: WebSession) -> anyhow::Result<WebUi> {
    let (deps, models, active_alias, migrate_error) = match session {
        WebSession::Ready(deps) => {
            let (models, active_alias) = match deps.config.catalog.home() {
                Home::Local(_) => {
                    let start = deps
                        .config
                        .catalog
                        .start_bound(deps.profile_pin.as_deref())
                        .map_err(anyhow::Error::msg)?;
                    let active_alias = start.agent().main.model.alias.clone();
                    let models = build_models_snapshot(&deps.config.registry);
                    (models, active_alias)
                }
                Home::Remote(_) => (Vec::new(), String::new()),
            };
            (Some(deps), models, active_alias, None)
        }
        WebSession::NeedsSetup { migrate_error } => {
            (None, Vec::new(), String::new(), migrate_error)
        }
    };
    let settings = SettingsBoot {
        config_dir: web.config_dir,
        boot_cwd: web.boot_cwd,
        migrate_error,
        token_source: web.token_source,
    };
    Ok(WebUi::new(
        web.addr,
        deps,
        models,
        active_alias,
        web.token,
        web.tls,
        web.tls_names,
        settings,
    ))
}

/// Boot wiring (p3-design §B, PR-C): `general.cloud` present ⇒ spawn the
/// process-wide CloudLink (and publish it via `cloud::set_boot_handle` so
/// the remote `Directory` can resolve `profile@device`); absent ⇒ `None`
/// (the web API answers `cloud_disabled`). `state_dir` and `store` are
/// injected: `paths::init_roots` is process-global, so per-core state is
/// never derived from roots in tests. `inbound` = `WebUi::cloud_inbound()`
/// (host) or `None` (Local edition).
pub fn boot_cloud(
    config: &Config,
    state_dir: PathBuf,
    device_name: &str,
    store: Arc<dyn crate::cloud::SeedStore>,
    inbound: Option<crate::cloud::InboundHook>,
) -> Option<crate::cloud::CloudHandle> {
    let Some(def) = &config.general.cloud else {
        return None;
    };
    // The resolve layer already validated this block; a re-parse failure is
    // a logic bug — stay Disabled rather than boot a broken link.
    let Ok(Some(mut cloud)) = crate::cloud::config::parse_cloud(Some(def)) else {
        tracing::error!("general.cloud failed to re-parse; Cloud Mode stays off");
        return None;
    };
    cloud.state_dir = state_dir;
    cloud.device_name = device_name.to_string();
    // Card platform byte (spec §4.1): the host edition is a computer, the
    // Local edition is the phone.
    #[cfg(feature = "host")]
    {
        cloud.platform = 1;
    }
    #[cfg(not(feature = "host"))]
    {
        cloud.platform = 4;
    }
    let handle = crate::cloud::spawn(cloud, store, inbound);
    crate::cloud::set_boot_handle(handle.clone());
    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve::{Config, ParseCtx, Source};
    use crate::session::test_support::test_deps;
    use crate::storage::InMemoryStorage;
    use crate::ui::web::settings::TokenSource;
    use std::sync::Arc;

    /// Mobile relay: a relay-only config (no providers, no local profile)
    /// boots the web UI as Ready — no models, empty active alias — instead
    /// of panicking on the CE1-era `default_bound` expect.
    #[test]
    fn web_boot_relay_only_config_is_ready_not_a_panic() {
        let yaml = "profiles:\n  dev on nas:\n    remote:\n      url: https://nas.lan:7823\n      token: tok123\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n";
        let config = Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: yaml,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("a relay-only config must parse");
        let mut deps = test_deps(Some(Arc::new(InMemoryStorage::default())));
        deps.config = Arc::new(config);
        let dir = tempfile::tempdir().expect("boot dir");
        let web = WebBoot {
            addr: "127.0.0.1:0".parse().expect("loopback addr"),
            token: None,
            token_source: TokenSource::None,
            tls: false,
            tls_names: Vec::new(),
            config_dir: dir.path().to_path_buf(),
            boot_cwd: dir.path().to_path_buf(),
        };
        // A relay-only config must boot Ready (no models, empty alias).
        let ui = web_boot(web, WebSession::Ready(Arc::new(deps)))
            .expect("a relay-only config boots the web UI as Ready");
        let _ = ui; // the WebUi is unrunnable in a unit test; Ready-ness is the assertion
    }

    /// A global config with the `general.notifications` switch as asked.
    /// `auto_save: false` keeps the test off the disk (no storage dir).
    fn config_with_notifications(enabled: Option<bool>) -> Config {
        let line = enabled
            .map(|e| format!("  notifications:\n    enabled: {e}\n"))
            .unwrap_or_default();
        let text = format!(
            "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\ngeneral:\n  conversation:\n    auto_save: false\n{line}"
        );
        Config::parse(
            Source {
                path: std::path::Path::new("/global/config.yaml"),
                text: &text,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture config must parse")
    }

    /// T-B1 — the config key is the boot switch for the process-wide
    /// channel: off ⇒ no channel at all; default ⇒ channel present.
    #[test]
    fn session_deps_has_no_channel_when_disabled() {
        let base = |enabled: Option<bool>| {
            session_deps(
                config_with_notifications(enabled),
                SkillRegistry::default(),
                Vec::new(),
                Vec::new(),
                None,
                None,
            )
        };

        assert!(
            base(None).turn_ends.is_some(),
            "the default config (notifications enabled: true) must build the channel"
        );
        assert!(
            base(Some(true)).turn_ends.is_some(),
            "enabled: true must build the channel"
        );
        assert!(
            base(Some(false)).turn_ends.is_none(),
            "enabled: false must mean no channel (disabled ⇒ not emittable)"
        );
    }
}
