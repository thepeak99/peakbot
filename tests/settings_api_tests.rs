//! T17 + T18 + T19 — Settings API (RED). Spec: make-config-great-again.md §8,
//! plan: config-v2-plan.md "M4 — Settings API".
//!
//! These tests are written BEFORE the implementation, against the ASSUMED
//! API below. They will not compile until `peakbot::ui::web::settings`
//! exists; that compile failure is the expected RED state (plus the runtime
//! contract they pin: 404-JSON for unknown `/api/*`, 503 `/ws` in
//! needs_setup mode, 409 stale guard, `.pre-0.24` create-only backup, …).
//!
//! ── TEST PLAN (test → spec item) ─────────────────────────────────────────
//! T17 (settings.rs read):
//!   `requires_token`                      §8.1 — every route behind the web token (401 without, 200 with, 401 wrong token)
//!   `pin_403`                             §8.1/§8.6 — `--profile` pin: 403 + exact message on all settings routes; system routes remain
//!   `settings_shape`                      §8.2 — GET /api/settings field-for-field (needs_setup, legacy_layout, migrate_error, settings_available,
//!                                            global_path, repo_path, repo_exists flip, builtin_tools w/o delegate, tab_names, skills, builtin_persona,
//!                                            mcp_status Z3, boot{bind,token_source,tls}, facts)
//!   `config_both_scopes_missing_repo`     §8.2 — GET /api/config global (bytes+sha256) and repo (exists:false, text:"", sha256:null); 400 on missing/bogus scope
//!   `convo_never_accepts_client_path`     §8.1 — repo scope cwd never client-supplied: no `convo` / unknown `convo` → boot cwd; a third dir is never touched
//!   `migrate_route_404`                   §8.2/§2 — no migrate endpoint: GET/POST /api/migrate → 404 JSON
//!   `unknown_api_route_is_404_json`       T14 follow-up — unknown /api/* → 404 JSON envelope, not the SPA 200
//! T18 (validate + PUT):
//!   `validate_restart_required_vs_boot`   §8.2 — restart_required = boot_only_diff vs boot config; [] for repo scope; [] when unchanged
//!   `validate_422_segment_paths`          §8.1/§4.1 — 422 envelope `{error, errors[{file,path:Seg[],message}]}`; key paths, index paths, root path []
//!   `global_edit_breaks_repo_alias_reports_repo`
//!                                            §8.2 — combined validation: a global edit that breaks a repo alias fails with file:"repo"
//!   `put_409_stale`                       §8.2 step 2 — sha256 stale guard: 409 + exact copy; file untouched; base!=null with missing file → 409
//!   `put_writes_verbatim_and_bak`         §8.2 step 4/5 — bytes verbatim (comments kept); single-slot .bak; 200 shape {path,backup,sha256,restart_required,applies_on_next_new}
//!   `repo_put_creates_dir`                §8.2 step 4 — repo scope creates .peakbot/; backup null for a missing file
//!   `put_over_legacy_keeps_pre024_once`   §8.2 step 4/§7.5 — replacing a legacy file keeps its bytes create-only in .pre-0.24 (0600); never overwritten on a second PUT
//!   `w1_lan_without_token`                §4.3 W1/§8.4 — non-loopback draft bind + no effective token → 422 with exact W1 message (validate AND PUT); draft token satisfies it
//! T19 (needs_setup):
//!   `needs_setup_ws_503`                  §6.9/§8.5 — deps None: /ws → 503 {"error":"setup required"}; settings + system routes still served; needs_setup:true
//!   `ws_still_503_after_put`              §6.9 — a successful first-run PUT does NOT fill deps: /ws stays 503; "setup written, restart pending" is derived
//!   `failed_migrate_sets_error_and_legacy`§6.9 step 0/§8.5 — failed boot migration: needs_setup + legacy_layout:true (file at request time) + migrate_error verbatim
//!
//! ── ASSUMED API (does not exist yet; this file will not compile) ─────────
//! 1. `peakbot::ui::web::settings::router(SettingsState) -> axum::Router` —
//!    the settings surface, mirroring `system::router(SystemState)`:
//!    GET  /api/settings
//!    GET  /api/config?scope=global|repo[&convo=<uuid>]
//!    POST /api/config/validate   {scope, text, convo?}
//!    PUT  /api/config            {scope, text, base_sha256, convo?}
//!    The router carries its own `/api/*` catch-all: any other `/api/...`
//!    path (incl. /api/migrate — there is no migrate endpoint) answers
//!    404 with the JSON error envelope, so merging it anywhere in the
//!    main app keeps unknown /api/* out of the SPA fallback (T14 note).
//! 2. `peakbot::ui::web::settings::SettingsState` — all fields pub:
//!    deps: Option<Arc<SessionDeps>>      // None ⇔ needs_setup (§6.9), fixed at boot
//!    registry: Option<SessionRegistry>   // convo → cwd resolution; None in needs_setup mode (no sessions)
//!    config_dir: PathBuf                 // the global file is <config_dir>/config.yaml
//!    boot_cwd: PathBuf                   // repo-scope fallback cwd: <boot_cwd>/.peakbot/config.yaml
//!    migrate_error: Option<String>       // boot auto-migration failure (AutoMigrateError Display), fixed at boot
//!    boot_web: BootWeb
//! 3. `peakbot::ui::web::settings::BootWeb { bind: SocketAddr, token_source: TokenSource, tls: bool }`
//!    and `TokenSource { Config, Env, Cli, None }`, serialising lowercase
//!    ("config"|"env"|"cli"|"none") — §8.2 `boot` block.
//! 4. `peakbot::ui::web::registry::SessionRegistry` made `pub` (module +
//!    struct; constructor `SessionRegistry::new(Arc<SessionDeps>)` already
//!    pub) and a `cwd_of(&self, convo: Uuid) -> Option<PathBuf>` lookup —
//!    the ONLY way a `?convo=` becomes a cwd (§8.1). A live session's cwd
//!    is its `StateManager::session_cwd()`; unknown id → None → boot cwd.
//! 5. `peakbot::ui::web::ws_setup_guard() -> Response` (pub, beside
//!    `require_token`) — the `/ws` route mounted in place of the real
//!    WebSocket upgrade while `WebUi.deps` is None (T19): 503 with JSON
//!    body `{"error":"setup required"}`.
//! 6. `WebUi` holds `deps: Option<Arc<SessionDeps>>`, fixed at boot, never
//!    filled in later (no late build, no OnceCell — §6.9).
//!
//! Production wiring (WebUi::run): `settings::router(SettingsState)` is
//! merged BEFORE the token layer exactly like `system::router` today, so
//! every settings route is token-gated.
//!
//! ── Harness notes ─────────────────────────────────────────────────────────
//! * Same style as tests/system_api_tests.rs: real router on a random
//!   loopback port + the production `require_token` layer + reqwest.
//! * `SettingsState.config_dir` is a tempdir passed explicitly — no
//!   XDG_CONFIG_HOME juggling needed for the config paths.
//! * `w1_lan_without_token` blanks `PEAKBOT_WEB_TOKEN` (a guard restores
//!   it) because W1's effective-token chain includes the process env; the
//!   podman sandbox runs without that variable, the blanking is belt-and-
//!   braces under a file-local lock.
//! * No network: the fixture configs use an ollama provider that is never
//!   contacted; MCP servers are declared but never spawned.

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use peakbot::SkillRegistry;
use peakbot::attachments::UploadStore;
use peakbot::config::resolve::{Config, L1_MESSAGE, ParseCtx, Source};
use peakbot::config::{BUILTIN_TOOL_NAMES, TAB_NAMES, UploadsConfig};
use peakbot::session::SessionDeps;
use peakbot::ui::web::registry::SessionRegistry;
use peakbot::ui::web::require_token;
use peakbot::ui::web::settings::{BootWeb, SettingsState, TokenSource, router};
use peakbot::ui::web::system::{InstallFn, ServiceFn, SystemState, router as system_router};
use peakbot::ui::web::ws_setup_guard;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── fixture texts ─────────────────────────────────────────────────────────

/// Valid 0.24 global: offline ollama provider, two MCPs (one enabled, one
/// disabled — the Z3 mcp_status fixture), one profile.
const GLOBAL_SHAPE: &str = r#"
providers:
  - name: local
    type: ollama
    models:
      - { name: llama3, alias: local }
mcps:
  - name: playwright
    command: npx
    args: ["@playwright/mcp@latest"]
  - name: linear
    type: streamable-http
    url: https://mcp.linear.app/mcp
    enabled: false
general:
  web:
    bind: 127.0.0.1:7823
profiles:
  dev:
    agents:
      team:
        model: local
"#;

/// Minimal valid 0.24 global (first-run saves, PUT fixtures).
const GLOBAL_024: &str = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: local }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: local\n";

/// Valid 0.24 global for the boot baseline: loopback web, 50 max turns,
/// no mcps — every variation below changes exactly one block.
const GLOBAL_BOOT: &str = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: local }\ngeneral:\n  web:\n    bind: 127.0.0.1:7823\n  agent_max_turns: 50\nprofiles:\n  dev:\n    agents:\n      team:\n        model: local\n";

/// Boot global with two model aliases (the repo profile below uses
/// `sonnet`); the "break the alias" edit keeps only `qwen`.
const GLOBAL_TWO_ALIASES: &str = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: sonnet }\n      - { name: llama3.1, alias: qwen }\nprofiles:\n  dev:\n    agents:\n      team:\n        model: sonnet\n";
const GLOBAL_ONLY_QWEN: &str = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3.1, alias: qwen }\nprofiles:\n  dev:\n    agents:\n      team:\n        model: qwen\n";

/// A pre-0.24 global (the L1 / legacy fixture — same shape as the
/// config_v2_boot LEGACY_GLOBAL).
const LEGACY_GLOBAL: &str = "providers:\n  - name: openrouter\n    type: openrouter\n    api_key: fake-key\n    models:\n      - name: anthropic/claude-3.7-sonnet\n        alias: default\ndefault_model: default\npersona: You are a careful engineer.\n";

/// Valid 0.24 repo file (references the global alias `local`).
const REPO_024: &str =
    "profiles:\n  peakbot-dev:\n    agents:\n      team:\n        model: local\n";

/// Repo profile that uses the global alias `sonnet` (for the
/// combined-validation test).
const REPO_SONNET: &str =
    "profiles:\n  peakbot-dev:\n    agents:\n      team:\n        model: sonnet\n";

// ── pure helpers ──────────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn parse_global(text: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture global must parse")
}

fn parse_combined(global: &str, repo: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: global,
        },
        Some(Source {
            path: Path::new("/repo/.peakbot/config.yaml"),
            text: repo,
        }),
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture global+repo must parse")
}

/// The only way tests build deps: every field pub, offline fixture config.
fn deps_with_config(cfg: Config, pin: Option<&str>) -> Arc<SessionDeps> {
    Arc::new(SessionDeps {
        config: Arc::new(cfg),
        skills: SkillRegistry::new(),
        mcp_handles: Arc::new(Vec::new()),
        boot_notices: std::sync::Mutex::new(Vec::new()),
        shell_kind: None,
        storage: None,
        profile_pin: pin.map(str::to_string),
        uploads: UploadStore::new(UploadStore::default_root(), UploadsConfig::default()),
    })
}

/// A W1 draft: valid 0.24 global with the given `general.web.bind` and
/// optional `general.web.token`.
fn w1_draft(bind: &str, token: Option<&str>) -> String {
    let token_line = token
        .map(|t| format!("    token: {t}\n"))
        .unwrap_or_default();
    format!(
        "providers:\n  - name: local\n    type: ollama\n    models:\n      - {{ name: llama3, alias: local }}\ngeneral:\n  web:\n    bind: {bind}\n{token_line}profiles:\n  dev:\n    agents:\n      team:\n        model: local\n"
    )
}

/// Blanks `PEAKBOT_WEB_TOKEN` for the test's duration (W1's effective-token
/// chain includes the process env; resolve_token collapses blank to absent).
/// The podman sandbox never sets the variable; this is belt-and-braces.
static ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

struct BlankTokenGuard(Option<String>);

impl BlankTokenGuard {
    fn new() -> Self {
        let _lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var("PEAKBOT_WEB_TOKEN").ok();
        unsafe { std::env::set_var("PEAKBOT_WEB_TOKEN", "") };
        BlankTokenGuard(original)
    }
}

impl Drop for BlankTokenGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => unsafe { std::env::set_var("PEAKBOT_WEB_TOKEN", value) },
            None => unsafe { std::env::remove_var("PEAKBOT_WEB_TOKEN") },
        }
    }
}

// ── sandbox + router spawn ────────────────────────────────────────────────

/// One isolated file system: a tempdir config dir (passed to the state
/// explicitly — no XDG env needed) plus a boot-cwd tempdir.
struct Sandbox {
    xdg: tempfile::TempDir,
    cwd: tempfile::TempDir,
    state: SettingsState,
}

impl Sandbox {
    fn new(deps: Option<Arc<SessionDeps>>, migrate_error: Option<&str>) -> Self {
        let xdg = tempfile::tempdir().expect("config tempdir");
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let state = SettingsState {
            deps: deps.clone(),
            registry: deps.map(SessionRegistry::new),
            config_dir: xdg.path().to_path_buf(),
            boot_cwd: cwd.path().to_path_buf(),
            migrate_error: migrate_error.map(str::to_string),
            boot_web: BootWeb {
                bind: "127.0.0.1:7823".parse().expect("bind"),
                token_source: TokenSource::Config,
                tls: false,
            },
        };
        Self { xdg, cwd, state }
    }

    fn global_path(&self) -> PathBuf {
        self.xdg.path().join("config.yaml")
    }
    fn repo_path(&self) -> PathBuf {
        self.cwd.path().join(".peakbot").join("config.yaml")
    }
    fn write_global(&self, text: &str) {
        std::fs::create_dir_all(self.xdg.path()).expect("config dir");
        std::fs::write(self.global_path(), text).expect("write global");
    }
    fn write_repo(&self, text: &str) {
        std::fs::create_dir_all(self.cwd.path().join(".peakbot")).expect(".peakbot");
        std::fs::write(self.repo_path(), text).expect("write repo");
    }
}

/// Spawn the settings router (plus, when asked, the needs_setup `/ws`
/// guard and the system routes) behind the production token layer on a
/// random loopback port — the same shape as `WebUi::run` and the
/// system_api_tests harness.
async fn spawn(
    state: SettingsState,
    token: Option<&str>,
    ws_guard: bool,
    system_routes: bool,
) -> SocketAddr {
    let mut app: Router = router(state);
    if ws_guard {
        // Production mounts this in place of the real /ws upgrade while
        // WebUi.deps is None (assumed API item 5).
        app = app.merge(Router::new().route("/ws", get(ws_setup_guard)));
    }
    if system_routes {
        // §8.6: system endpoints remain (also under a pin).
        app = app.merge(system_router(SystemState {
            install: InstallFn::default_for_tests(),
            service: ServiceFn::default_for_tests(),
        }));
    }
    if let Some(t) = token {
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn url(addr: SocketAddr, path: &str, token: Option<&str>) -> String {
    let sep = if path.contains('?') { "&" } else { "?" };
    match token {
        Some(t) => format!("http://{addr}{path}{sep}token={t}"),
        None => format!("http://{addr}{path}"),
    }
}

/// GET and parse the JSON body (asserted separately per test).
async fn get_json(addr: SocketAddr, path: &str, token: Option<&str>) -> reqwest::Response {
    client().get(url(addr, path, token)).send().await.unwrap()
}

async fn post_json(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::Response {
    client()
        .post(url(addr, path, token))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

async fn put_json(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::Response {
    client()
        .put(url(addr, path, token))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

fn is_json(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .is_some_and(|v| v.to_str().is_ok_and(|v| v.starts_with("application/json")))
}

fn validate_body(scope: &str, text: &str, convo: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "scope": scope, "text": text });
    if let Some(c) = convo {
        body["convo"] = serde_json::json!(c);
    }
    body
}

fn put_body(scope: &str, text: &str, base_sha256: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "scope": scope, "text": text, "base_sha256": base_sha256 })
}

/// The exact §8.1 pin refusal.
const PIN_403_MSG: &str = "Settings are disabled while PeakBot runs with --profile dev.";

// ===========================================================================
// T17 — settings.rs read
// ===========================================================================

/// §8.1: the whole settings surface is behind the web token.
#[tokio::test]
async fn requires_token() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    let addr = spawn(sb.state.clone(), Some("s3cret"), false, false).await;

    // Every route 401s without the token.
    let r = get_json(addr, "/api/settings", None).await;
    assert_eq!(r.status(), 401, "GET /api/settings without token");
    let r = get_json(addr, "/api/config?scope=global", None).await;
    assert_eq!(r.status(), 401, "GET /api/config without token");
    let r = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", GLOBAL_024, None),
    )
    .await;
    assert_eq!(r.status(), 401, "POST /api/config/validate without token");
    let r = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", GLOBAL_024, None),
    )
    .await;
    assert_eq!(r.status(), 401, "PUT /api/config without token");

    // A wrong token is refused too.
    let r = get_json(addr, "/api/settings?token=wrong", None).await;
    assert_eq!(r.status(), 401, "a wrong token must not authenticate");

    // With the token, all four routes answer.
    let r = get_json(addr, "/api/settings", Some("s3cret")).await;
    assert_eq!(r.status(), 200, "GET /api/settings with token");
    let r = get_json(addr, "/api/config?scope=global", Some("s3cret")).await;
    assert_eq!(r.status(), 200, "GET /api/config with token");
    let r = post_json(
        addr,
        "/api/config/validate",
        Some("s3cret"),
        &validate_body("global", GLOBAL_024, None),
    )
    .await;
    assert_eq!(r.status(), 200, "validate of a valid config with token");
    let r = put_json(
        addr,
        "/api/config",
        Some("s3cret"),
        &put_body("global", GLOBAL_024, None),
    )
    .await;
    assert_eq!(
        r.status(),
        200,
        "PUT of a valid config (missing file, base null) with token"
    );
}

/// §8.1/§8.6: under a `--profile` pin every settings route refuses with the
/// exact 403 message; the system endpoints remain.
async fn assert_pin_refusal(resp: reqwest::Response) {
    let status = resp.status();
    assert!(is_json(&resp), "the 403 envelope must be JSON");
    let value: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 403, "a settings route under a pin must 403");
    assert_eq!(
        value["error"].as_str(),
        Some(PIN_403_MSG),
        "the pin refusal must use the exact §8.1 message"
    );
}

#[tokio::test]
async fn pin_403() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), Some("dev"))),
        None,
    );
    let addr = spawn(sb.state.clone(), None, false, true).await;

    assert_pin_refusal(get_json(addr, "/api/settings", None).await).await;
    assert_pin_refusal(get_json(addr, "/api/config?scope=global", None).await).await;
    assert_pin_refusal(
        post_json(
            addr,
            "/api/config/validate",
            None,
            &validate_body("global", GLOBAL_024, None),
        )
        .await,
    )
    .await;
    assert_pin_refusal(
        put_json(
            addr,
            "/api/config",
            None,
            &put_body("global", GLOBAL_024, None),
        )
        .await,
    )
    .await;

    // System endpoints are NOT refused under a pin (§8.6).
    let r = get_json(addr, "/api/system/service", None).await;
    assert_eq!(
        r.status(),
        200,
        "GET /api/system/service must remain served under a pin"
    );
}

/// §8.2: GET /api/settings — field-for-field shape check in normal (deps)
/// mode.
#[tokio::test]
async fn settings_shape() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    sb.write_global(GLOBAL_SHAPE);
    // A local skill for the scope cwd: <cwd>/.agents/skills/<name>/SKILL.md.
    let skill_dir = sb
        .cwd
        .path()
        .join(".agents")
        .join("skills")
        .join("m4-test-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: m4-test-skill\ndescription: A fixture skill for the settings API tests.\n---\n# Body\n",
    )
    .unwrap();

    let addr = spawn(sb.state.clone(), None, false, false).await;
    let resp = get_json(addr, "/api/settings", None).await;
    assert_eq!(resp.status(), 200);
    assert!(is_json(&resp), "settings must be JSON");
    let v: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(v["needs_setup"], false, "deps present → not needs_setup");
    assert_eq!(
        v["legacy_layout"], false,
        "current-layout file → not legacy"
    );
    assert_eq!(
        v["migrate_error"],
        serde_json::Value::Null,
        "no boot migration failure"
    );
    assert_eq!(
        v["settings_available"], true,
        "not pinned → settings available"
    );
    assert_eq!(
        v["global_path"],
        sb.global_path().to_string_lossy().to_string()
    );
    assert_eq!(v["repo_path"], sb.repo_path().to_string_lossy().to_string());
    assert_eq!(v["repo_exists"], false, "no repo file yet");

    // builtin_tools: the filterable list — BUILTIN_TOOL_NAMES minus `delegate`.
    let expected_tools: Vec<&str> = BUILTIN_TOOL_NAMES
        .iter()
        .copied()
        .filter(|t| *t != "delegate")
        .collect();
    let tools = v["builtin_tools"].as_array().expect("builtin_tools[]");
    assert_eq!(
        tools
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect::<Vec<_>>(),
        expected_tools,
        "builtin_tools must be the filterable list"
    );
    assert!(
        !tools.iter().any(|t| t.as_str() == Some("delegate")),
        "`delegate` is not filterable (§2)"
    );

    // tab_names: the 0.24 set (no `profile`, with `settings`).
    assert_eq!(
        v["tab_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect::<Vec<_>>(),
        TAB_NAMES.to_vec(),
        "tab_names must be TAB_NAMES"
    );

    // skills: discovered for the scope cwd.
    let skills = v["skills"].as_array().expect("skills[]");
    assert!(
        skills.iter().any(|s| s.as_str() == Some("m4-test-skill")),
        "the cwd's local skill must be listed; got: {skills:?}"
    );

    // builtin_persona: the built-in persona text, verbatim.
    assert_eq!(
        v["builtin_persona"].as_str(),
        Some(include_str!("../src/system_prompt_persona.txt")),
        "builtin_persona must be the persona file's text"
    );

    // mcp_status: Z3 — disabled = enabled:false, failed = enabled & no
    // handle, and NO `detail` field. The test deps have no handles.
    let mcp = v["mcp_status"].as_array().expect("mcp_status[]");
    assert_eq!(mcp.len(), 2, "one entry per configured MCP server");
    let by_name = |n: &str| {
        mcp.iter()
            .find(|e| e["name"] == n)
            .unwrap_or_else(|| panic!("mcp_status missing {n}: {mcp:?}"))
    };
    assert_eq!(
        by_name("playwright")["state"],
        "failed",
        "enabled with no boot handle → failed (Z3)"
    );
    assert_eq!(
        by_name("linear")["state"],
        "disabled",
        "enabled:false → disabled (Z3)"
    );
    for e in mcp {
        assert!(
            e.get("detail").is_none(),
            "Z3: mcp_status entries carry no `detail`"
        );
    }

    // boot block: from the BootWeb the server was started with.
    assert_eq!(v["boot"]["bind"], "127.0.0.1:7823");
    assert_eq!(
        v["boot"]["token_source"], "config",
        "TokenSource::Config serialises lowercase"
    );
    assert_eq!(v["boot"]["tls"], false);

    // facts: machine facts + the existing InstallInfo.
    assert_eq!(v["facts"]["os"], std::env::consts::OS);
    assert_eq!(v["facts"]["arch"], std::env::consts::ARCH);
    assert_eq!(
        v["facts"]["version"],
        env!("CARGO_PKG_VERSION"),
        "no version bump until release (plan)"
    );
    assert!(
        v["facts"]["exe_path"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "facts.exe_path"
    );
    assert!(
        v["facts"]["data_dir"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "facts.data_dir"
    );
    assert!(
        v["facts"]["install"].is_object(),
        "facts.install = the existing InstallInfo"
    );

    // repo_exists flips at request time when a repo file appears.
    sb.write_repo(REPO_024);
    let v2: serde_json::Value = get_json(addr, "/api/settings", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        v2["repo_exists"], true,
        "a repo file that now exists must flip repo_exists"
    );
}

/// §8.2: GET /api/config for both scopes when the repo file is missing.
#[tokio::test]
async fn config_both_scopes_missing_repo() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    sb.write_global(GLOBAL_SHAPE);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // Global: exists, exact bytes, sha256 of those bytes.
    let resp = get_json(addr, "/api/config?scope=global", None).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["scope"], "global");
    assert_eq!(v["path"], sb.global_path().to_string_lossy().to_string());
    assert_eq!(v["exists"], true);
    assert_eq!(
        v["text"], GLOBAL_SHAPE,
        "the file's bytes, unmasked and unrendered"
    );
    assert_eq!(
        v["sha256"],
        sha256_hex(GLOBAL_SHAPE.as_bytes()),
        "sha256 of the bytes"
    );

    // Repo (missing): exists:false, empty text, null sha256 — but the
    // would-be path is reported.
    let resp = get_json(addr, "/api/config?scope=repo", None).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["scope"], "repo");
    assert_eq!(v["path"], sb.repo_path().to_string_lossy().to_string());
    assert_eq!(v["exists"], false);
    assert_eq!(v["text"], "");
    assert_eq!(v["sha256"], serde_json::Value::Null);

    // Missing / bogus scope → 400 with the error envelope.
    for path in ["/api/config", "/api/config?scope=bogus"] {
        let resp = get_json(addr, path, None).await;
        assert_eq!(resp.status(), 400, "{path} must be a 400");
        let v: serde_json::Value = resp.json().await.unwrap();
        assert!(
            v.get("error").is_some(),
            "{path} must use the error envelope"
        );
    }
}

/// §8.1: the repo-scope cwd is NEVER client-supplied. The client can only
/// pass a conversation uuid; an absent or unknown uuid falls back to the
/// boot cwd. A third directory holding a repo file must never be read
/// from or written to.
#[tokio::test]
async fn convo_never_accepts_client_path() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    sb.write_global(GLOBAL_SHAPE);
    // A victim directory with its own repo file: the API has no way to
    // point at it, and it must stay byte-identical.
    let victim = tempfile::tempdir().expect("victim tempdir");
    let victim_dir = victim.path().join(".peakbot");
    std::fs::create_dir_all(&victim_dir).unwrap();
    let canary = "profiles:\n  canary:\n    agents:\n      main:\n        model: local\n";
    std::fs::write(victim_dir.join("config.yaml"), canary).unwrap();

    let addr = spawn(sb.state.clone(), None, false, false).await;

    // No convo → boot cwd.
    let v: serde_json::Value = get_json(addr, "/api/config?scope=repo", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["path"],
        sb.repo_path().to_string_lossy().to_string(),
        "no convo → boot cwd"
    );
    assert_eq!(v["exists"], false);

    // PUT with no convo writes into the boot cwd's .peakbot/.
    let text1 = "profiles:\n  peakbot-dev:\n    agents:\n      team:\n        model: local\n        prompt: first save\n";
    let resp = put_json(addr, "/api/config", None, &put_body("repo", text1, None)).await;
    assert_eq!(resp.status(), 200, "PUT repo scope without convo");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["path"], sb.repo_path().to_string_lossy().to_string());
    assert_eq!(std::fs::read_to_string(sb.repo_path()).unwrap(), text1);

    // PUT with an UNKNOWN convo also resolves to the boot cwd (never a
    // client path).
    let unknown = uuid::Uuid::new_v4().to_string();
    let text2 = format!("{text1}# second save\n");
    let base = sha256_hex(text1.as_bytes());
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("repo", &text2, Some(&unknown)),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "validate repo with an unknown convo falls back to the boot cwd"
    );
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &serde_json::json!({ "scope": "repo", "text": text2, "base_sha256": base, "convo": unknown }),
    )
    .await;
    assert_eq!(resp.status(), 200, "PUT repo with an unknown convo");
    assert_eq!(
        std::fs::read_to_string(sb.repo_path()).unwrap(),
        text2,
        "the unknown-convo PUT must land in the boot cwd"
    );

    // The victim directory is untouched — the API can't reach it at all.
    assert_eq!(
        std::fs::read_to_string(victim_dir.join("config.yaml")).unwrap(),
        canary,
        "a client-supplied path can never steer the repo scope"
    );
}

/// §2/§8.2: there is no migrate endpoint — migration happens only at boot.
/// GET/POST /api/migrate must be 404 JSON, not the SPA 200.
#[tokio::test]
async fn migrate_route_404() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    let addr = spawn(sb.state.clone(), None, false, false).await;

    let resp = get_json(addr, "/api/migrate", None).await;
    assert_eq!(
        resp.status(),
        404,
        "GET /api/migrate must be a 404 tombstone"
    );
    assert!(
        is_json(&resp),
        "the 404 must be the JSON envelope, not the SPA fallback"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v.get("error").is_some(), "404 uses the error envelope");

    let resp = post_json(addr, "/api/migrate", None, &serde_json::json!({})).await;
    assert_eq!(
        resp.status(),
        404,
        "POST /api/migrate must be a 404 tombstone"
    );
    assert!(is_json(&resp), "the POST 404 must be JSON");
}

/// T14 follow-up: unknown `/api/*` routes answer 404 JSON (one envelope),
/// never the SPA catch-all's 200 index.html.
#[tokio::test]
async fn unknown_api_route_is_404_json() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_SHAPE), None)),
        None,
    );
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // Unknown paths under /api/.
    for path in [
        "/api/definitely/not/a/route",
        "/api/settings/extra",
        "/api/config/x/y",
    ] {
        let resp = get_json(addr, path, None).await;
        assert_eq!(
            resp.status(),
            404,
            "GET {path} must be 404, not the SPA 200"
        );
        assert!(
            is_json(&resp),
            "GET {path} must be the JSON envelope, not index.html"
        );
        let v: serde_json::Value = resp.json().await.unwrap();
        assert!(
            v.get("error").is_some(),
            "GET {path} uses the error envelope"
        );
    }

    // A method that no /api route serves.
    let resp = post_json(addr, "/api/config", None, &serde_json::json!({})).await;
    assert_eq!(
        resp.status(),
        404,
        "POST /api/config (no such method) must be 404 JSON, not the SPA 200"
    );
    assert!(is_json(&resp));
}

// ===========================================================================
// T18 — validate + PUT
// ===========================================================================

/// §8.2: `restart_required` is `boot_only_diff(&boot_config, &parsed)` —
/// the ONE boot-only list — and is always [] for the repo scope.
#[tokio::test]
async fn validate_restart_required_vs_boot() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(GLOBAL_BOOT);
    sb.write_repo(REPO_024);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // Unchanged → no restart keys.
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", GLOBAL_BOOT, None),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(
        v["restart_required"],
        serde_json::json!([]),
        "identical config → no restart keys"
    );
    assert!(v["warnings"].is_array(), "warnings is always an array");

    // general.web is boot-only → changing the bind requires a restart.
    let text = GLOBAL_BOOT.replace("127.0.0.1:7823", "127.0.0.1:7999");
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &text, None),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["restart_required"],
        serde_json::json!(["general.web"]),
        "a general.web change is exactly one boot-only key"
    );

    // agent_max_turns is reload-safe → no restart keys.
    let text = GLOBAL_BOOT.replace("agent_max_turns: 50", "agent_max_turns: 42");
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &text, None),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["restart_required"],
        serde_json::json!([]),
        "agent_max_turns is reload-safe"
    );

    // mcps is boot-only.
    let text = GLOBAL_BOOT.replace(
        "general:",
        "mcps:\n  - name: fake\n    command: /bin/true\ngeneral:",
    );
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &text, None),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(
        v["restart_required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "mcps"),
        "adding an MCP server is boot-only; got: {v}"
    );

    // Repo scope: always [] (repo files carry no boot-only blocks).
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("repo", REPO_024, None),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(
        v["restart_required"],
        serde_json::json!([]),
        "repo scope is always []"
    );
}

/// §8.1/§4.1: the 422 envelope is `{error, errors:[{file, path: Seg[],
/// message}]}` — paths are SEGMENT ARRAYS (keys as strings, indices as
/// numbers; [] = document root).
#[tokio::test]
async fn validate_422_segment_paths() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(GLOBAL_BOOT);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // Key segments: an unknown model alias on an agent.
    let text = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: local }\nprofiles:\n  dev:\n    agents:\n      team:\n        model: gpt5\n";
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", text, None),
    )
    .await;
    assert_eq!(resp.status(), 422, "an unparseable config is a 422");
    assert!(is_json(&resp));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["error"], "config is not valid",
        "the §8.1 envelope header"
    );
    let errs = v["errors"].as_array().expect("errors[]");
    assert_eq!(errs[0]["file"], "global");
    assert_eq!(
        errs[0]["path"],
        serde_json::json!(["profiles", "dev", "agents", "team", "model"]),
        "key paths are an array of strings"
    );
    assert!(
        errs[0]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown model alias gpt5"),
        "the M2 message names the alias; got: {}",
        errs[0]["message"]
    );

    // Index segments: an unknown field inside a model entry →
    // ["providers", 0, "models", 0] with NUMBERS for the indices.
    let text = "providers:\n  - name: local\n    type: ollama\n    models:\n      - { name: llama3, alias: local, bogus: 1 }\nprofiles:\n  dev:\n    agents:\n      team:\n        model: local\n";
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", text, None),
    )
    .await;
    assert_eq!(resp.status(), 422);
    let v: serde_json::Value = resp.json().await.unwrap();
    let errs = v["errors"].as_array().expect("errors[]");
    assert_eq!(errs[0]["file"], "global");
    let path = errs[0]["path"].as_array().expect("path[]");
    assert_eq!(path[0], "providers");
    assert!(
        path[1].is_u64() && path[1].as_u64() == Some(0),
        "list indices are JSON numbers; got: {path:?}"
    );
    assert_eq!(path[2], "models");
    assert!(
        path[3].is_u64() && path[3].as_u64() == Some(0),
        "list indices are JSON numbers; got: {path:?}"
    );
    assert_eq!(
        path.len(),
        4,
        "P2: the unknown-field error sits at the model entry (the parent \
         mapping), not at the rejected key; got: {path:?}"
    );
    assert!(
        errs[0]["message"].as_str().unwrap().contains("bogus"),
        "serde wording names the field"
    );

    // Root path: a legacy global is L1 at the document root (path []).
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", LEGACY_GLOBAL, None),
    )
    .await;
    assert_eq!(resp.status(), 422, "validating legacy text hits L1");
    let v: serde_json::Value = resp.json().await.unwrap();
    let errs = v["errors"].as_array().expect("errors[]");
    assert_eq!(errs[0]["file"], "global");
    assert_eq!(
        errs[0]["path"],
        serde_json::json!([]),
        "L1 is a root-level error (path [])"
    );
    assert_eq!(errs[0]["message"], L1_MESSAGE, "the exact L1 message");
}

/// §8.2: validation is COMBINED (global text + the repo file on disk, and
/// vice versa) — a global edit that removes an alias a repo profile uses
/// fails with `file: "repo"` errors.
#[tokio::test]
async fn global_edit_breaks_repo_alias_reports_repo() {
    // Boot: the global declares sonnet+qwen, the repo profile uses sonnet.
    let cfg = parse_combined(GLOBAL_TWO_ALIASES, REPO_SONNET);
    let sb = Sandbox::new(Some(deps_with_config(cfg, None)), None);
    sb.write_global(GLOBAL_TWO_ALIASES);
    sb.write_repo(REPO_SONNET);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // The repo file alone is valid — the failure must come from the
    // combined parse of the edited global + the repo file on disk.
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", GLOBAL_ONLY_QWEN, None),
    )
    .await;
    assert_eq!(
        resp.status(),
        422,
        "a global edit that breaks a repo alias must fail"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "config is not valid");
    let errs = v["errors"].as_array().expect("errors[]");
    let repo_err = errs
        .iter()
        .find(|e| e["file"] == "repo")
        .unwrap_or_else(|| panic!("the error must be attributed to the repo file; got: {errs:?}"));
    assert_eq!(
        repo_err["path"],
        serde_json::json!(["profiles", "peakbot-dev", "agents", "team", "model"]),
        "the repo profile's model path"
    );
    assert!(
        repo_err["message"]
            .as_str()
            .unwrap()
            .contains("Unknown model alias sonnet"),
        "the M2 message names the now-missing alias; got: {}",
        repo_err["message"]
    );
    assert!(
        !errs.iter().any(|e| e["file"] == "global"),
        "the edited global text is valid on its own — no global errors; got: {errs:?}"
    );
}

/// §8.2 step 2: the sha256 stale guard. A PUT whose `base_sha256` no longer
/// matches the file on disk is a 409 with the exact copy; the file is
/// untouched.
#[tokio::test]
async fn put_409_stale() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(GLOBAL_BOOT);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // The client opens the file: sha256 S0.
    let v: serde_json::Value = get_json(addr, "/api/config?scope=global", None)
        .await
        .json()
        .await
        .unwrap();
    let s0 = v["sha256"].as_str().unwrap().to_string();
    assert_eq!(s0, sha256_hex(GLOBAL_BOOT.as_bytes()));

    // Someone else edits the file on disk (S1).
    let edited = GLOBAL_BOOT.replace("agent_max_turns: 50", "agent_max_turns: 51");
    std::fs::write(sb.global_path(), &edited).unwrap();

    // Stale PUT → 409, exact copy, file untouched.
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &edited, Some(&s0)),
    )
    .await;
    assert_eq!(resp.status(), 409, "a stale base_sha256 must 409");
    assert!(is_json(&resp));
    let v: serde_json::Value = resp.json().await.unwrap();
    let msg = v["error"].as_str().expect("409 envelope");
    assert!(
        msg.contains("changed on disk since you opened Settings — reload to see the new version."),
        "the §8.2 409 copy; got: {msg}"
    );
    assert_eq!(
        std::fs::read_to_string(sb.global_path()).unwrap(),
        edited,
        "a refused PUT must not touch the file"
    );

    // Control: the same PUT with the CURRENT hash succeeds.
    let s1 = sha256_hex(edited.as_bytes());
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &edited, Some(&s1)),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "a fresh base_sha256 must pass the guard"
    );

    // Boundary: a non-null base against a MISSING file is stale too.
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("repo", REPO_024, Some("deadbeef")),
    )
    .await;
    assert_eq!(
        resp.status(),
        409,
        "base_sha256 set against a missing file is 409"
    );
}

/// §8.2 step 4/5: the server writes the client's bytes VERBATIM (comments
/// survive), keeps the single-slot .bak, and answers with the full 200
/// shape.
#[tokio::test]
async fn put_writes_verbatim_and_bak() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    let t0 = "# original comment — must survive in .bak\n".to_string() + GLOBAL_BOOT;
    sb.write_global(&t0);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    let v: serde_json::Value = get_json(addr, "/api/config?scope=global", None)
        .await
        .json()
        .await
        .unwrap();
    let s0 = v["sha256"].as_str().unwrap().to_string();

    // The new bytes: a different comment + a reload-safe edit.
    let t1 = "# the new comment\n".to_string()
        + &GLOBAL_BOOT.replace("agent_max_turns: 50", "agent_max_turns: 42");
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &t1, Some(&s0)),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(is_json(&resp));
    let v: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(v["path"], sb.global_path().to_string_lossy().to_string());
    assert_eq!(
        v["sha256"],
        sha256_hex(t1.as_bytes()),
        "sha256 of the NEW bytes"
    );
    assert_eq!(
        v["backup"],
        sb.global_path()
            .with_extension("yaml.bak")
            .to_string_lossy()
            .to_string(),
        "backup = the single-slot .bak path"
    );
    assert_eq!(
        v["restart_required"],
        serde_json::json!([]),
        "only a reload-safe key changed"
    );
    assert_eq!(v["applies_on_next_new"], true, "the §8.2 200 shape");

    // Verbatim: the disk bytes are exactly what the client sent.
    assert_eq!(
        std::fs::read_to_string(sb.global_path()).unwrap(),
        t1,
        "the server must write the client's bytes verbatim (no re-rendering)"
    );
    // .bak: the previous bytes, single slot.
    let bak = sb.global_path().with_extension("yaml.bak");
    assert!(
        bak.exists(),
        ".bak must exist after replacing an existing file"
    );
    assert_eq!(
        std::fs::read_to_string(&bak).unwrap(),
        t0,
        ".bak holds the previous bytes"
    );
}

/// §8.2 step 4: a repo-scope PUT creates `.peakbot/` when missing; a
/// missing target file has no backup (`backup: null`).
#[tokio::test]
async fn repo_put_creates_dir() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(GLOBAL_BOOT);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    assert!(!sb.repo_path().exists(), "no repo file at start");
    let resp = put_json(addr, "/api/config", None, &put_body("repo", REPO_024, None)).await;
    assert_eq!(
        resp.status(),
        200,
        "PUT repo scope with a missing file (base null)"
    );
    let v: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(v["path"], sb.repo_path().to_string_lossy().to_string());
    assert_eq!(
        v["backup"],
        serde_json::Value::Null,
        "no previous file → no backup"
    );
    assert_eq!(v["sha256"], sha256_hex(REPO_024.as_bytes()));
    assert_eq!(v["applies_on_next_new"], true);
    assert!(
        sb.repo_path().parent().unwrap().is_dir(),
        ".peakbot/ must be created"
    );
    assert_eq!(
        std::fs::read_to_string(sb.repo_path()).unwrap(),
        REPO_024,
        "the repo file's bytes are verbatim"
    );
}

/// §8.2 step 4 + §7.5: replacing a LEGACY file first keeps its exact bytes
/// create-only in `<file>.pre-0.24` (0600). A second PUT must NOT
/// overwrite the backup.
#[tokio::test]
async fn put_over_legacy_keeps_pre024_once() {
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(LEGACY_GLOBAL);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // The legacy file is served for reading (exists, bytes, sha256).
    let v: serde_json::Value = get_json(addr, "/api/config?scope=global", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["exists"], true);
    assert_eq!(v["text"], LEGACY_GLOBAL);
    let s0 = v["sha256"].as_str().unwrap().to_string();

    // First PUT: legacy → 0.24. The .pre-0.24 backup is created.
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", GLOBAL_024, Some(&s0)),
    )
    .await;
    assert_eq!(resp.status(), 200, "PUT over a legacy file");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["backup"],
        sb.global_path()
            .with_extension("yaml.bak")
            .to_string_lossy()
            .to_string(),
        "save_config_at's .bak is still reported"
    );

    let pre024 = sb.global_path().with_extension("yaml.pre-0.24");
    assert!(
        pre024.exists(),
        "replacing a legacy file must keep .pre-0.24 first"
    );
    let backup_bytes = std::fs::read(&pre024).unwrap();
    assert_eq!(
        backup_bytes,
        LEGACY_GLOBAL.as_bytes(),
        ".pre-0.24 holds the EXACT legacy bytes"
    );
    assert_eq!(
        std::fs::metadata(&pre024).unwrap().permissions().mode() & 0o777,
        0o600,
        ".pre-0.24 is 0600 (§7.5)"
    );

    // Second PUT (now over a CURRENT file): the backup must be unchanged.
    let v: serde_json::Value = get_json(addr, "/api/config?scope=global", None)
        .await
        .json()
        .await
        .unwrap();
    let s1 = v["sha256"].as_str().unwrap().to_string();
    let t2 = GLOBAL_024.replace(
        "model: local",
        "model: local\n        prompt: second save\n",
    );
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &t2, Some(&s1)),
    )
    .await;
    assert_eq!(resp.status(), 200, "the second PUT");

    assert_eq!(
        std::fs::read(&pre024).unwrap(),
        LEGACY_GLOBAL.as_bytes(),
        ".pre-0.24 is CREATE-ONLY — the second PUT must not overwrite it"
    );
    // The single-slot .bak now holds the first 0.24 bytes.
    let bak = sb.global_path().with_extension("yaml.bak");
    assert_eq!(
        std::fs::read_to_string(&bak).unwrap(),
        GLOBAL_024,
        "the single-slot .bak rolls to the most recent previous bytes"
    );
}

/// §4.3 W1 / §8.4: a draft whose `general.web.bind` is non-loopback needs a
/// token in the effective chain (CLI/env — fixed for the running process —
/// or the draft's own `general.web.token`). Runs in validate AND PUT.
#[tokio::test]
async fn w1_lan_without_token() {
    let _blank = BlankTokenGuard::new(); // no env token in the chain
    let sb = Sandbox::new(
        Some(deps_with_config(parse_global(GLOBAL_BOOT), None)),
        None,
    );
    sb.write_global(GLOBAL_BOOT);
    let addr = spawn(sb.state.clone(), None, false, false).await;

    // LAN bind, no token anywhere → 422 with the exact W1 message at
    // general.web.token.
    let draft = w1_draft("0.0.0.0:7823", None);
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &draft, None),
    )
    .await;
    assert_eq!(
        resp.status(),
        422,
        "W1: LAN bind without a token must fail validation"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "config is not valid");
    let errs = v["errors"].as_array().expect("errors[]");
    assert_eq!(errs[0]["file"], "global");
    assert_eq!(
        errs[0]["path"],
        serde_json::json!(["general", "web", "token"]),
        "W1 is reported at general.web.token"
    );
    assert_eq!(
        errs[0]["message"],
        "\"0.0.0.0:7823\" is reachable from other devices and requires an access token.",
        "the exact W1 message"
    );

    // The draft's own token satisfies W1.
    let draft = w1_draft("0.0.0.0:7823", Some("t0ken"));
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &draft, None),
    )
    .await;
    assert_eq!(resp.status(), 200, "a draft token clears W1");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], true);

    // Loopback without a token is fine.
    let draft = w1_draft("127.0.0.1:7823", None);
    let resp = post_json(
        addr,
        "/api/config/validate",
        None,
        &validate_body("global", &draft, None),
    )
    .await;
    assert_eq!(resp.status(), 200, "loopback needs no token");

    // W1 applies to PUT as well. The global file exists, so the PUT must
    // carry its sha256 (a null base over an existing file is the §8.2
    // step-2 409, which would mask W1). A refused PUT leaves the file
    // untouched, so the same base is still current for the second PUT.
    let base = sha256_hex(GLOBAL_BOOT.as_bytes());
    let draft = w1_draft("0.0.0.0:7823", None);
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &draft, Some(&base)),
    )
    .await;
    assert_eq!(resp.status(), 422, "W1 is enforced on PUT too");
    let draft = w1_draft("0.0.0.0:7823", Some("t0ken"));
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", &draft, Some(&base)),
    )
    .await;
    assert_eq!(resp.status(), 200, "a token clears W1 on PUT");
}

// ===========================================================================
// T19 — needs_setup
// ===========================================================================

/// §6.9/§8.5: while `WebUi.deps` is None the server is in needs_setup mode:
/// `/ws` answers 503 `{"error":"setup required"}`, while the settings and
/// system routes are still served and `GET /api/settings` reports
/// `needs_setup: true`.
#[tokio::test]
async fn needs_setup_ws_503() {
    let sb = Sandbox::new(None, None); // no global file, no boot config
    let addr = spawn(sb.state.clone(), None, true, true).await;

    // /ws → 503 with the exact JSON body.
    let resp = client().get(url(addr, "/ws", None)).send().await.unwrap();
    assert_eq!(resp.status(), 503, "/ws in needs_setup mode must be 503");
    assert!(is_json(&resp), "the 503 body must be JSON");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "error": "setup required" }),
        "the exact §6.9 body"
    );

    // Settings are still served and report needs_setup.
    let resp = get_json(addr, "/api/settings", None).await;
    assert_eq!(
        resp.status(),
        200,
        "GET /api/settings is served in needs_setup mode"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["needs_setup"], true);
    assert_eq!(
        v["legacy_layout"], false,
        "no file → not legacy (computed at request time)"
    );
    assert_eq!(v["migrate_error"], serde_json::Value::Null);
    assert_eq!(v["settings_available"], true);
    assert_eq!(
        v["global_path"],
        sb.global_path().to_string_lossy().to_string()
    );
    assert_eq!(v["boot"]["bind"], "127.0.0.1:7823");

    // System routes are still served too.
    let resp = get_json(addr, "/api/system/service", None).await;
    assert_eq!(
        resp.status(),
        200,
        "GET /api/system/service is served in needs_setup mode"
    );
}

/// §6.9: a successful first-run `PUT /api/config` only writes the file —
/// the deps are never filled in later, so `/ws` stays 503 until the server
/// restarts. "Setup written, restart pending" is DERIVED: needs_setup &&
/// global file exists && !legacy_layout.
#[tokio::test]
async fn ws_still_503_after_put() {
    let sb = Sandbox::new(None, None);
    let addr = spawn(sb.state.clone(), None, true, false).await;

    // First-run save: writes the file.
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &put_body("global", GLOBAL_024, None),
    )
    .await;
    assert_eq!(resp.status(), 200, "the first-run PUT writes the file");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["path"], sb.global_path().to_string_lossy().to_string());
    assert_eq!(
        v["backup"],
        serde_json::Value::Null,
        "no previous file → no backup"
    );
    assert_eq!(v["applies_on_next_new"], true);
    assert!(sb.global_path().exists(), "the file must be on disk");

    // /ws is STILL 503 — no late deps build.
    let resp = client().get(url(addr, "/ws", None)).send().await.unwrap();
    assert_eq!(
        resp.status(),
        503,
        "/ws must stay 503 after a first-run save"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v, serde_json::json!({ "error": "setup required" }));

    // The derived "restart pending" state: needs_setup && file exists &&
    // !legacy_layout.
    let v: serde_json::Value = get_json(addr, "/api/settings", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["needs_setup"], true, "needs_setup is fixed at boot");
    assert_eq!(v["legacy_layout"], false, "the file is now current layout");
    assert_eq!(
        v["global_path"],
        sb.global_path().to_string_lossy().to_string()
    );
}

/// §6.9 step 0 / §8.5: a FAILED boot auto-migration of the global file puts
/// the server in needs_setup mode with `legacy_layout: true` (the file is
/// still legacy, computed at request time) and `migrate_error` carrying the
/// failure verbatim — the SPA can only show the reason (no migrate
/// endpoint).
#[tokio::test]
async fn failed_migrate_sets_error_and_legacy() {
    let reason = format!(
        "could not migrate {}: pipeline 0 has no name",
        "config.yaml"
    );
    let sb = Sandbox::new(None, Some(&reason));
    sb.write_global(LEGACY_GLOBAL); // the migration failed → the file is still legacy
    let addr = spawn(sb.state.clone(), None, true, false).await;

    let resp = get_json(addr, "/api/settings", None).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        v["needs_setup"], true,
        "a failed global migration is needs_setup (windowed web)"
    );
    assert_eq!(
        v["legacy_layout"], true,
        "the file on disk is still legacy (request-time check)"
    );
    assert_eq!(
        v["migrate_error"].as_str(),
        Some(reason.as_str()),
        "migrate_error carries the boot failure verbatim"
    );
    assert_eq!(v["settings_available"], true);

    // And /ws is 503 in this mode as well.
    let resp = client().get(url(addr, "/ws", None)).send().await.unwrap();
    assert_eq!(resp.status(), 503);
}
