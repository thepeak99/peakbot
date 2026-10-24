//! `/api/settings` + `/api/config*` — the 0.24 Settings surface
//! (spec §8.1/§8.2).
//!
//! Replaces the removed setup wizard (`/api/setup*` → 404, see
//! [`super::system::setup_removed`]). The token layer is **not** wired
//! here: tests attach it themselves so the auth contract is exercised
//! independently. Production mounting merges the returned router into
//! `WebUi::run` **before** the token layer, exactly like
//! [`super::system::router`], so every settings route is gated by the
//! same `require_token` as `/ws` and `/commands`.
//!
//! The router carries its own `/api/*` catch-all: any `/api/...` path it
//! does not serve (incl. `/api/migrate` — there is no migrate endpoint,
//! migration happens only at boot) answers 404 with the JSON error
//! envelope, so merging it anywhere in the main app keeps unknown
//! `/api/*` routes out of the SPA fallback (T14 follow-up).
//!
//! `POST /api/config/validate` and `PUT /api/config` are 501 stubs for
//! now — T18 replaces them with the validating writer.

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use crate::config::migrate::is_legacy;
use crate::config::{BUILTIN_TOOL_NAMES, TAB_NAMES};
use crate::session::SessionDeps;
use crate::ui::web::registry::SessionRegistry;

// ===========================================================================
// Boot facts (spec §8.2 `boot` block, §6.9)
// ===========================================================================

/// Where the effective web token came from — the `boot.token_source`
/// wire value (spec §8.2). Serialises lowercase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    /// `general.web.token` in config.yaml.
    Config,
    /// `$PEAKBOT_WEB_TOKEN`.
    Env,
    /// `--token`.
    Cli,
    /// No token in the effective chain (loopback-only).
    None,
}

impl TokenSource {
    /// The first non-blank entry of the CLI > env > config chain — mirrors
    /// [`crate::config::resolve_token`], which collapses blanks to absent.
    pub fn resolve(cli: Option<&str>, env: Option<&str>, config: Option<&str>) -> Self {
        fn nonblank(v: Option<&str>) -> bool {
            v.is_some_and(|s| !s.trim().is_empty())
        }
        if nonblank(cli) {
            TokenSource::Cli
        } else if nonblank(env) {
            TokenSource::Env
        } else if nonblank(config) {
            TokenSource::Config
        } else {
            TokenSource::None
        }
    }
}

/// How this server was started — the `boot` block of `GET /api/settings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootWeb {
    pub bind: SocketAddr,
    pub token_source: TokenSource,
    pub tls: bool,
}

/// Boot-time facts `WebUi` threads into the settings router (spec §6.9):
/// the file locations, the boot auto-migration failure (verbatim), and the
/// token source of the running process.
#[derive(Clone, Debug)]
pub struct SettingsBoot {
    pub config_dir: PathBuf,
    pub boot_cwd: PathBuf,
    pub migrate_error: Option<String>,
    pub token_source: TokenSource,
}

/// Per-request state of the settings surface, built once at boot and
/// cloned (cheaply) into the router.
#[derive(Clone)]
pub struct SettingsState {
    /// `None` ⇔ needs_setup mode (spec §6.9) — fixed at boot, never filled
    /// in later (no late deps build).
    pub deps: Option<Arc<SessionDeps>>,
    /// Convo → cwd resolution (spec §8.1); `None` in needs_setup mode
    /// (no sessions).
    pub registry: Option<SessionRegistry>,
    /// The global file is `<config_dir>/config.yaml`.
    pub config_dir: PathBuf,
    /// The repo-scope fallback cwd: `<boot_cwd>/.peakbot/config.yaml`.
    pub boot_cwd: PathBuf,
    /// The boot auto-migration failure, verbatim (spec §6.9); `None` when
    /// step 0 succeeded.
    pub migrate_error: Option<String>,
    pub boot_web: BootWeb,
}

// ===========================================================================
// JSON / error envelope helpers
// ===========================================================================

/// Serialize `value` as a 200 JSON response. Hand-rolled (not axum's
/// `Json`) for the same reason as in [`super::system`]: axum is built
/// without the `json` feature.
fn json_ok<T: Serialize>(value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialise response: {e}"),
        )
            .into_response(),
    }
}

/// The one error envelope (spec §8.1): `{"error": "<message>"}`.
fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "error": message.into() }).to_string(),
    )
        .into_response()
}

// ===========================================================================
// Shared handler logic
// ===========================================================================

/// The §8.1 pin refusal, checked FIRST in every settings handler: under a
/// `--profile` pin the whole settings surface is disabled (the SPA hides
/// the gear via `settings_available`). System routes are not affected.
fn pin_refusal(state: &SettingsState) -> Option<Response> {
    let pin = state.deps.as_ref()?.profile_pin.as_deref()?;
    Some(json_error(
        StatusCode::FORBIDDEN,
        format!("Settings are disabled while PeakBot runs with --profile {pin}."),
    ))
}

/// The only way a `?convo=` becomes a cwd (spec §8.1): the live session's
/// cwd through the registry. Absent or unknown convo → the boot cwd. A
/// client can never steer the repo scope at a third directory.
fn repo_cwd(state: &SettingsState, convo: Option<&str>) -> PathBuf {
    convo
        .and_then(|c| c.parse::<Uuid>().ok())
        .and_then(|id| {
            state
                .registry
                .as_ref()
                .and_then(|registry| registry.cwd_of(id))
        })
        .unwrap_or_else(|| state.boot_cwd.clone())
}

/// Request-time legacy check (spec §6.9): the file is legacy iff it reads
/// and parses as YAML and [`is_legacy`] holds. A missing or unparsable
/// file is not legacy (it gets the normal parse errors elsewhere).
fn is_legacy_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return false;
    };
    is_legacy(&doc)
}

/// Plan Z3: `mcp_status` is derived, never stored — `disabled` =
/// `enabled: false`, `connected` = a boot handle exists, `failed` =
/// enabled with no handle. No `detail` field. needs_setup mode (no
/// config) → empty.
fn mcp_status(deps: Option<&Arc<SessionDeps>>) -> Vec<JsonValue> {
    let Some(deps) = deps else {
        return Vec::new();
    };
    deps.config
        .mcps
        .iter()
        .map(|m| {
            let state = if !m.enabled {
                "disabled"
            } else if deps.mcp_handles.iter().any(|h| h.name() == m.name) {
                "connected"
            } else {
                "failed"
            };
            json!({ "name": m.name, "state": state })
        })
        .collect()
}

/// `sha256` of the file's bytes, hex.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// The `facts` block (spec §8.2): machine facts + the install info the
/// 0.23 setup wizard surfaced (target / state / PATH verdict).
fn facts() -> JsonValue {
    let exe_path: Option<String> = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let data_dir = dirs::data_dir()
        .map(|p| p.join("peakbot").to_string_lossy().into_owned())
        .unwrap_or_default();
    let install = install_info(&exe_path);
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "exe_path": exe_path.unwrap_or_default(),
        "data_dir": data_dir,
        "version": env!("CARGO_PKG_VERSION"),
        "install": install,
    })
}

/// The `facts.install` object — the 0.23 `build_install_info` shape
/// (`target`, `state`, `path`) built from `install::install_target()` +
/// `current_exe()` + `path_state` over the process PATH.
fn install_info(exe_path: &Option<String>) -> JsonValue {
    let target = crate::install::install_target();
    let target_display = target
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // "current" when the running binary IS the install target, "other"
    // when a different file sits in the target slot, "absent" when the
    // slot (or the platform target) does not exist.
    let install_state = match (&target, exe_path.as_deref()) {
        (Some(t), Some(exe)) if t.to_string_lossy() == exe => "current",
        (Some(t), _) if t.exists() => match (t.canonicalize(), std::fs::canonicalize(t)) {
            (Ok(a), Ok(b)) if a == b => "current",
            _ => "other",
        },
        _ => "absent",
    };
    let path = match target.as_deref() {
        Some(t) => {
            let path_var: std::ffi::OsString = std::env::var_os("PATH").unwrap_or_default();
            super::system::InstallPath::from_core(&crate::install::path_state(&path_var, t))
        }
        None => super::system::InstallPath::Absent {
            hint: "no install target on this platform".to_string(),
        },
    };
    json!({
        "target": target_display,
        "state": install_state,
        "path": path,
    })
}

// ===========================================================================
// Handlers
// ===========================================================================

#[derive(Deserialize)]
struct SettingsQuery {
    convo: Option<String>,
}

/// `GET /api/settings?convo=` — meta for the Settings screen (spec §8.2).
/// `legacy_layout` / `repo_exists` are computed at request time;
/// `settings_available` is derived as `!profile_pinned`, never stored.
async fn get_settings(
    State(state): State<Arc<SettingsState>>,
    Query(query): Query<SettingsQuery>,
) -> Response {
    if let Some(refused) = pin_refusal(&state) {
        return refused;
    }
    let cwd = repo_cwd(&state, query.convo.as_deref());
    let global_path = state.config_dir.join("config.yaml");
    let repo_path = cwd.join(".peakbot").join("config.yaml");
    let value = json!({
        "needs_setup": state.deps.is_none(),
        "legacy_layout": is_legacy_file(&global_path),
        "migrate_error": state.migrate_error,
        "settings_available": state.deps.as_ref().is_none_or(|d| d.profile_pin.is_none()),
        "global_path": global_path.to_string_lossy().to_string(),
        "repo_path": repo_path.to_string_lossy().to_string(),
        "repo_exists": repo_path.exists(),
        // The filterable list — `delegate` is not filterable (§2).
        "builtin_tools": BUILTIN_TOOL_NAMES
            .iter()
            .filter(|t| **t != "delegate")
            .map(|t| t.to_string())
            .collect::<Vec<_>>(),
        "tab_names": TAB_NAMES.to_vec(),
        "skills": crate::load_default_skills(&cwd).0.names(),
        "builtin_persona": include_str!("../../system_prompt_persona.txt"),
        "mcp_status": mcp_status(state.deps.as_ref()),
        "boot": {
            "bind": state.boot_web.bind.to_string(),
            "token_source": state.boot_web.token_source,
            "tls": state.boot_web.tls,
        },
        "facts": facts(),
    });
    json_ok(&value)
}

#[derive(Deserialize)]
struct ConfigQuery {
    scope: Option<String>,
    convo: Option<String>,
}

/// `GET /api/config?scope=global|repo&convo=` — one file's text
/// (spec §8.2). Bytes travel unmasked behind the token (§8.3);
/// `exists: false` gives `text: ""` and `sha256: null`.
async fn get_config(
    State(state): State<Arc<SettingsState>>,
    Query(query): Query<ConfigQuery>,
) -> Response {
    if let Some(refused) = pin_refusal(&state) {
        return refused;
    }
    let Some(scope) = query.scope.as_deref() else {
        return json_error(StatusCode::BAD_REQUEST, "scope must be `global` or `repo`");
    };
    let path = match scope {
        "global" => state.config_dir.join("config.yaml"),
        "repo" => repo_cwd(&state, query.convo.as_deref())
            .join(".peakbot")
            .join("config.yaml"),
        _ => return json_error(StatusCode::BAD_REQUEST, "scope must be `global` or `repo`"),
    };
    match std::fs::read(&path) {
        Ok(bytes) => json_ok(&json!({
            "scope": scope,
            "path": path.to_string_lossy().to_string(),
            "exists": true,
            "text": String::from_utf8_lossy(&bytes),
            "sha256": sha256_hex(&bytes),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json_ok(&json!({
            "scope": scope,
            "path": path.to_string_lossy().to_string(),
            "exists": false,
            "text": "",
            "sha256": null,
        })),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot read {}: {e}", path.display()),
        ),
    }
}

/// `POST /api/config/validate` — T18 implements the dry run; this stub
/// carries the §8.1 pin refusal until then.
async fn validate_config_stub(State(state): State<Arc<SettingsState>>) -> Response {
    if let Some(refused) = pin_refusal(&state) {
        return refused;
    }
    not_implemented("validate")
}

/// `PUT /api/config` — T18 implements the validating writer; this stub
/// carries the §8.1 pin refusal until then.
async fn put_config_stub(State(state): State<Arc<SettingsState>>) -> Response {
    if let Some(refused) = pin_refusal(&state) {
        return refused;
    }
    not_implemented("config write")
}

fn not_implemented(what: &str) -> Response {
    json_error(
        StatusCode::NOT_IMPLEMENTED,
        format!("{what} is not implemented yet (T18)"),
    )
}

/// Unknown `/api/*` → 404 JSON envelope (T14 follow-up): never the SPA's
/// 200 `index.html`. Also answers methods no `/api` route provides (e.g.
/// `POST /api/config`), where axum would otherwise 405.
async fn api_not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "not found")
}

// ===========================================================================
// Router (spec §8.2).
// ===========================================================================

/// Build the settings router. Mount this in `WebUi::run` **before** the
/// token layer so every settings route is gated by the same
/// `require_token` as `/ws` and `/commands`. The returned router carries
/// no auth — the caller adds it — and its own `/api/*` catch-all.
pub fn router(state: SettingsState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/api/settings", get(get_settings))
        .route(
            "/api/config",
            get(get_config)
                .put(put_config_stub)
                // POST is not a method the API provides: answer it like any
                // unknown /api route (404 JSON), not axum's default 405.
                .post(api_not_found),
        )
        .route("/api/config/validate", post(validate_config_stub))
        // The /api/* catch-all: any other /api path (incl. /api/migrate —
        // there is no migrate endpoint) is a 404 JSON envelope.
        .route("/api/{*rest}", any(api_not_found))
        .with_state(state)
}

// ===========================================================================
// Unit tests for the pure helpers
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillRegistry;
    use crate::attachments::UploadStore;
    use crate::config::UploadsConfig;
    use crate::config::resolve::{Config, ParseCtx, Source};

    fn parse(text: &str) -> Arc<SessionDeps> {
        let config = Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text,
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect("fixture parses");
        Arc::new(SessionDeps {
            config: Arc::new(config),
            skills: SkillRegistry::new(),
            mcp_handles: Arc::new(Vec::new()),
            boot_notices: std::sync::Mutex::new(Vec::new()),
            shell_kind: None,
            storage: None,
            profile_pin: None,
            uploads: UploadStore::new(UploadStore::default_root(), UploadsConfig::default()),
        })
    }

    #[test]
    fn token_source_resolve_follows_the_cli_env_config_chain() {
        assert_eq!(
            TokenSource::resolve(Some("a"), Some("b"), Some("c")),
            TokenSource::Cli
        );
        assert_eq!(
            TokenSource::resolve(None, Some("b"), Some("c")),
            TokenSource::Env
        );
        assert_eq!(
            TokenSource::resolve(None, None, Some("c")),
            TokenSource::Config
        );
        assert_eq!(TokenSource::resolve(None, None, None), TokenSource::None);
        // Blanks collapse to absent, exactly like resolve_token.
        assert_eq!(
            TokenSource::resolve(Some("  "), Some("b"), None),
            TokenSource::Env
        );
        assert_eq!(
            TokenSource::resolve(Some(""), None, Some("c")),
            TokenSource::Config
        );
    }

    #[test]
    fn token_source_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&TokenSource::Config).unwrap(),
            "\"config\""
        );
        assert_eq!(serde_json::to_string(&TokenSource::Env).unwrap(), "\"env\"");
        assert_eq!(serde_json::to_string(&TokenSource::Cli).unwrap(), "\"cli\"");
        assert_eq!(
            serde_json::to_string(&TokenSource::None).unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn mcp_status_is_derived_per_z3() {
        let deps = parse(
            "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: m, alias: a }\n\
             mcps:\n  - name: on\n    command: npx\n  - name: off\n    command: npx\n    enabled: false\n\
             profiles:\n  p:\n    agents:\n      x:\n        model: a\n",
        );
        let status = mcp_status(Some(&deps));
        assert_eq!(status.len(), 2);
        assert_eq!(status[0]["name"], "on");
        assert_eq!(status[0]["state"], "failed", "enabled, no handle → failed");
        assert_eq!(status[1]["name"], "off");
        assert_eq!(status[1]["state"], "disabled", "enabled:false → disabled");
        for e in &status {
            assert!(e.get("detail").is_none(), "no `detail` field (Z3)");
        }
        assert!(mcp_status(None).is_empty(), "needs_setup → no status");
    }

    #[test]
    fn repo_cwd_falls_back_to_boot_cwd_for_absent_or_unknown_convo() {
        let deps = parse(
            "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: m, alias: a }\n\
             profiles:\n  p:\n    agents:\n      x:\n        model: a\n",
        );
        let state = SettingsState {
            deps: Some(deps),
            registry: None,
            config_dir: PathBuf::from("/cfg"),
            boot_cwd: PathBuf::from("/boot"),
            migrate_error: None,
            boot_web: BootWeb {
                bind: "127.0.0.1:7823".parse().unwrap(),
                token_source: TokenSource::None,
                tls: false,
            },
        };
        assert_eq!(repo_cwd(&state, None), PathBuf::from("/boot"));
        assert_eq!(repo_cwd(&state, Some("not-a-uuid")), PathBuf::from("/boot"));
        assert_eq!(
            repo_cwd(&state, Some(&Uuid::new_v4().to_string())),
            PathBuf::from("/boot"),
            "an unknown convo id never leaves the boot cwd"
        );
    }
}
