//! T10 (RED) — Settings support for remote profiles + pairing link
//! (remote-profiles design §9 T10, §8.4, §7).
//!
//! Written BEFORE the implementation, against the contract below. The
//! settings API + `Config::parse` remote kind already exist (T3, T7), so
//! the round-trip tests may already be green — they are regression guards.
//! The `/api/pairing` surface does NOT exist yet; the compile failure on
//! `pairing_links` / `PairingWire` / the extended `BootWeb` is the expected
//! RED state.
//!
//! ── TEST PLAN (test → spec item) ─────────────────────────────────────────
//! `remote_profile_draft_round_trips_unmasked`
//!   §9 T10 / §7 — a global draft with a remote profile: GET /api/config
//!   returns the bytes (token in plaintext — remote tokens are the same
//!   class as API keys, §7 "What a local-token holder can do"), PUT
//!   unchanged with the sha256 base → 200, the file is byte-identical.
//! `invalid_remote_fields_rejected_with_t3_paths`
//!   §9 T10 / §4.4 — a draft with invalid remote fields is rejected with
//!   the T3 field-path errors: 422 envelope, path
//!   `["profiles","<n>","remote","<field>"]`, exact §4.4 messages; the
//!   file is not written.
//! `pairing_links_build_entry_links_with_ca`
//!   §8.4 / §9 T10 — the pairing link is the server's entry link plus
//!   `&ca=sha256:…`: `https://<host>/?token=<t>&ca=sha256:<fp>`; first
//!   link = `tls::primary_lan_host(bind)`, the rest = the other
//!   `tls::local_sans` minus loopback, deduped; `fp` =
//!   `tls::ca_fingerprint(tls_dir)`.
//! `pairing_links_loopback_bind_unavailable`
//!   MP §4.4 — a loopback bind is `Unavailable(LoopbackBind)`, checked
//!   BEFORE NoTls (the test passes a live TLS dir to prove the order).
//! `pairing_links_no_tls_unavailable`
//!   MP §4.4 — a non-loopback bind without TLS is `Unavailable(NoTls)`.
//! `pairing_endpoint_serves_token_and_ca_links`
//!   §8.4 / §9 T10 — `GET /api/pairing` (token-gated, merged with the
//!   settings routes before the token layer) answers 200 with
//!   `{"Available": {"links": […]}}`; links[0] equals
//!   `https://<primary host>/?token=<t>&ca=sha256:<fp>` with
//!   `<fp> == tls::ca_fingerprint`.
//! `pairing_endpoint_is_token_gated`
//!   §6.2 / MP §6.4 — 401 without the token, 401 with a wrong token,
//!   200 with the right one.
//! `pairing_endpoint_unavailable_reasons`
//!   MP §4.4 — loopback bind → `{"Unavailable":{"reason":"loopback_bind"}}`
//!   (even with TLS on); non-loopback without TLS →
//!   `{"Unavailable":{"reason":"no_tls"}}`.
//!
//! ── ASSUMED API (does not exist yet; this file will not compile) ────────
//! 1. `peakbot::ui::web::settings::PairingWire` — MP §4.4, amended by
//!    §8.4 (the links carry `&ca=sha256:<fp>`):
//!    ```
//!    #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
//!    pub enum PairingWire {
//!        Available { links: Vec<String> },   // non-empty by construction
//!        Unavailable { reason: PairingUnavailable },
//!    }
//!    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
//!    #[serde(rename_all = "snake_case")]
//!    pub enum PairingUnavailable { LoopbackBind, NoTls }
//!    ```
//!    (default serde external tagging: `{"Available":{…}}` /
//!    `{"Unavailable":{"reason":"loopback_bind"|"no_tls"}}`)
//! 2. `peakbot::ui::web::settings::pairing_links(
//!        bind: SocketAddr, extra_sans: &[String], token: &str,
//!        tls_dir: Option<&Path>,
//!    ) -> PairingWire` — the pure link builder (MP task 2's
//!    `pairing_links`, amended: the CA fingerprint is computed from
//!    `tls_dir` via `tls::ca_fingerprint`). `Some(dir)` ⇔ TLS on.
//!    Link shape: `https://<host>/?token=<token>&ca=sha256:<fp>` —
//!    `&profile=<name>` is appended CLIENT-side by the panel dropdown
//!    (§8.4), never by the server.
//! 3. `peakbot::ui::web::settings::BootWeb` gains the facts the handler
//!    needs (the server knows its effective token and cert dir at boot):
//!    `token: Option<String>` (None = loopback-only, no token),
//!    `extra_sans: Vec<String>` (the `--tls-name` extras),
//!    `tls_dir: Option<PathBuf>` (None = TLS off).
//! 4. `GET /api/pairing` on the settings `router(SettingsState)` — merged
//!    before the token layer like every other settings route (MP §6.4),
//!    answering `PairingWire` as JSON.
//!
//! ── Harness notes ─────────────────────────────────────────────────────────
//! * Same style as tests/settings_api_tests.rs: real settings router on a
//!   random loopback port + the production `require_token` layer + reqwest.
//! * `tempfile` for the config dir and the TLS dir; the CA is generated
//!   with the production `tls::ca_fingerprint` (which creates it on demand).
//! * No network: the fixture's ollama provider is never contacted; the
//!   remote profile in the YAML is never dialed.

use axum::Router;
use axum::middleware::from_fn_with_state;
use peakbot::SkillRegistry;
use peakbot::attachments::UploadStore;
use peakbot::config::UploadsConfig;
use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::session::SessionDeps;
use peakbot::ui::web::registry::SessionRegistry;
use peakbot::ui::web::require_token;
use peakbot::ui::web::settings::{
    BootWeb, PairingUnavailable, PairingWire, SettingsState, TokenSource, pairing_links, router,
};
use peakbot::ui::web::tls;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ── fixtures ──────────────────────────────────────────────────────────────

/// Valid 0.24 global with ONE local and ONE remote profile (design §4.1).
const REMOTE_GLOBAL: &str = "\
providers:
  - name: local
    type: ollama
    models:
      - { name: llama3, alias: local }
general:
  web:
    bind: 127.0.0.1:7823
profiles:
  dev:
    agents:
      team:
        model: local
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
      profile: dev
    ui:
      color: purple
";

/// The §4.4 messages (locked by T3; re-asserted here through the API).
const URL_LOOPBACK_MESSAGE: &str =
    "remote profiles connect over HTTPS — http is allowed only for 127.0.0.1 / localhost";
const CA_MISSING_MESSAGE: &str = "pin the server's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate";

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

fn deps_with_config(cfg: Config) -> Arc<SessionDeps> {
    Arc::new(SessionDeps {
        config: Arc::new(cfg),
        skills: SkillRegistry::new(),
        mcp_handles: Arc::new(Vec::new()),
        boot_notices: std::sync::Mutex::new(Vec::new()),
        shell_kind: None,
        storage: None,
        profile_pin: None,
        uploads: UploadStore::new(UploadStore::default_root(), UploadsConfig::default()),
    })
}

// ── sandbox + router spawn ────────────────────────────────────────────────

/// One isolated file system: tempdir config dir + boot cwd, and (when TLS
/// is on) a tempdir TLS dir whose CA is generated on demand.
struct Sandbox {
    xdg: tempfile::TempDir,
    /// Keeps the boot-cwd tempdir alive; the path itself is never read
    /// (the router takes it by value in `SettingsState`).
    #[allow(dead_code)]
    cwd: tempfile::TempDir,
    tls_dir: Option<tempfile::TempDir>,
    state: SettingsState,
}

impl Sandbox {
    fn new(cfg: Config, bind: &str, token: Option<&str>, tls: bool) -> Self {
        let xdg = tempfile::tempdir().expect("config tempdir");
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        let tls_dir = tls
            .then(tempfile::tempdir)
            .transpose()
            .expect("tls tempdir");
        let deps = deps_with_config(cfg);
        let state = SettingsState {
            deps: Some(deps.clone()),
            registry: Some(SessionRegistry::new(deps)),
            config_dir: xdg.path().to_path_buf(),
            boot_cwd: cwd.path().to_path_buf(),
            migrate_error: None,
            boot_web: BootWeb {
                bind: bind.parse().expect("bind"),
                token_source: TokenSource::Config,
                tls,
                // T10 contract (assumed API item 3): the pairing handler
                // needs the effective token, the --tls-name extras and the
                // cert dir.
                token: token.map(str::to_string),
                extra_sans: Vec::new(),
                tls_dir: tls_dir.as_ref().map(|d| d.path().to_path_buf()),
            },
        };
        Self {
            xdg,
            cwd,
            tls_dir,
            state,
        }
    }

    fn global_path(&self) -> PathBuf {
        self.xdg.path().join("config.yaml")
    }
    fn write_global(&self, text: &str) {
        std::fs::write(self.global_path(), text).expect("write global");
    }
    fn tls_dir_path(&self) -> &Path {
        self.tls_dir
            .as_ref()
            .expect("TLS sandbox has a tls dir")
            .path()
    }
}

/// Spawn the settings router behind the production token layer on a random
/// loopback port — the same shape as `WebUi::run` (MP §6.4: `/api/pairing`
/// is merged with the settings routes, before the token layer).
async fn spawn(state: SettingsState, token: Option<&str>) -> SocketAddr {
    let mut app: Router = router(state);
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

async fn get_json(addr: SocketAddr, path: &str, token: Option<&str>) -> reqwest::Response {
    client().get(url(addr, path, token)).send().await.unwrap()
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

// ===========================================================================
// Settings API: remote profiles
// ===========================================================================

/// §9 T10 / §7: a draft with a remote profile round-trips through the
/// settings API — GET, then PUT unchanged, gives the same file. The token
/// travels unmasked in GET (remote tokens are the same class as API keys,
/// §7): no server-side redaction, so an unchanged PUT cannot write a mask
/// into the file.
#[tokio::test]
async fn remote_profile_draft_round_trips_unmasked() {
    let sb = Sandbox::new(parse_global(REMOTE_GLOBAL), "127.0.0.1:7823", None, false);
    sb.write_global(REMOTE_GLOBAL);
    let addr = spawn(sb.state.clone(), None).await;

    // GET: the exact bytes, remote token included (unmasked).
    let resp = get_json(addr, "/api/config?scope=global", None).await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["exists"], true);
    assert_eq!(
        v["text"], REMOTE_GLOBAL,
        "GET returns the file's bytes verbatim"
    );
    assert!(
        v["text"].as_str().unwrap().contains("token: pito"),
        "the remote token is returned to a token holder (§7: same class as API keys)"
    );
    assert_eq!(v["sha256"], sha256_hex(REMOTE_GLOBAL.as_bytes()));

    // PUT unchanged (same text, same base sha256) → 200.
    let resp = put_json(
        addr,
        "/api/config",
        None,
        &serde_json::json!({
            "scope": "global",
            "text": REMOTE_GLOBAL,
            "base_sha256": v["sha256"].as_str().unwrap(),
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "an unchanged remote-profile draft saves"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["sha256"], sha256_hex(REMOTE_GLOBAL.as_bytes()));

    // The file on disk is the same file.
    assert_eq!(
        std::fs::read_to_string(sb.global_path()).unwrap(),
        REMOTE_GLOBAL,
        "GET → PUT unchanged gives the same file"
    );
}

/// §9 T10 / §4.4: a draft with invalid remote fields is rejected with the
/// T3 field-path errors (422 envelope, `["profiles","<n>","remote","<field>"]`
/// paths, exact messages) — and the file is not written.
#[tokio::test]
async fn invalid_remote_fields_rejected_with_t3_paths() {
    let sb = Sandbox::new(parse_global(REMOTE_GLOBAL), "127.0.0.1:7823", None, false);
    sb.write_global(REMOTE_GLOBAL);
    let addr = spawn(sb.state.clone(), None).await;

    // (a) `http` on a non-loopback host → the §4.4 row-3 message at
    //     `profiles.home-dev.remote.url`.
    let bad_url = "\
providers:
  - name: local
    type: ollama
    models:
      - { name: llama3, alias: local }
profiles:
  dev:
    agents:
      team:
        model: local
  home-dev:
    remote:
      url: http://10.2.0.48:7823
      token: pito
      profile: dev
";
    let resp = client()
        .put(url(addr, "/api/config", None))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "scope": "global",
                "text": bad_url,
                "base_sha256": sha256_hex(REMOTE_GLOBAL.as_bytes()),
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422, "a bad remote url is a 422");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "config is not valid");
    let errs = v["errors"].as_array().expect("errors[]");
    let url_err = errs
        .iter()
        .find(|e| e["path"] == serde_json::json!(["profiles", "home-dev", "remote", "url"]))
        .unwrap_or_else(|| panic!("no error at profiles.home-dev.remote.url: {errs:?}"));
    assert_eq!(url_err["file"], "global");
    assert_eq!(
        url_err["message"], URL_LOOPBACK_MESSAGE,
        "the exact §4.4 row-3 message"
    );

    // (b) `https` without a `ca` → the §4.4 row-7 message at
    //     `profiles.home-dev.remote.ca`.
    let missing_ca = "\
providers:
  - name: local
    type: ollama
    models:
      - { name: llama3, alias: local }
profiles:
  dev:
    agents:
      team:
        model: local
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      profile: dev
";
    let resp = client()
        .put(url(addr, "/api/config", None))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "scope": "global",
                "text": missing_ca,
                "base_sha256": sha256_hex(REMOTE_GLOBAL.as_bytes()),
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422, "a missing ca is a 422");
    let v: serde_json::Value = resp.json().await.unwrap();
    let errs = v["errors"].as_array().expect("errors[]");
    let ca_err = errs
        .iter()
        .find(|e| e["path"] == serde_json::json!(["profiles", "home-dev", "remote", "ca"]))
        .unwrap_or_else(|| panic!("no error at profiles.home-dev.remote.ca: {errs:?}"));
    assert_eq!(
        ca_err["message"], CA_MISSING_MESSAGE,
        "the exact §4.4 row-7 message"
    );

    // Neither rejection wrote the file.
    assert_eq!(
        std::fs::read_to_string(sb.global_path()).unwrap(),
        REMOTE_GLOBAL,
        "a rejected draft must not be written"
    );
}

// ===========================================================================
// Pairing links (pure)
// ===========================================================================

/// §8.4 / §9 T10: the pairing link is the server's entry link plus
/// `&ca=sha256:…` — `https://<host>/?token=<t>&ca=sha256:<fp>` — with the
/// first link on `tls::primary_lan_host(bind)`, the rest on the other
/// SANs minus loopback, and `<fp>` = `tls::ca_fingerprint(tls_dir)`.
#[test]
fn pairing_links_build_entry_links_with_ca() {
    let dir = tempfile::tempdir().unwrap();
    let fp = tls::ca_fingerprint(dir.path()).unwrap();
    assert!(fp.starts_with("sha256:"), "the fingerprint is sha256:<hex>");

    let wire = pairing_links(
        "192.168.7.7:7823".parse().unwrap(),
        &["bot.example.ts.net".to_string()],
        "pito",
        Some(dir.path()),
    );
    let links = match wire {
        PairingWire::Available { links } => links,
        other => panic!("a LAN bind with TLS is Available; got {other:?}"),
    };
    assert_eq!(
        links[0],
        format!("https://192.168.7.7/?token=pito&ca={fp}"),
        "the first link is the primary LAN host's entry URL + &ca"
    );
    assert_eq!(
        links[1],
        format!("https://bot.example.ts.net/?token=pito&ca={fp}"),
        "the extra --tls-name SAN follows, same token + ca"
    );
    for link in &links {
        assert!(
            !link.contains("127.0.0.1") && !link.contains("localhost"),
            "loopback names are never phone-reachable links: {link}"
        );
    }
}

/// MP §4.4: a loopback bind is `Unavailable(LoopbackBind)` — checked
/// BEFORE NoTls (a live TLS dir is passed to prove the ordering).
#[test]
fn pairing_links_loopback_bind_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    tls::ca_fingerprint(dir.path()).unwrap();
    let wire = pairing_links(
        "127.0.0.1:7823".parse().unwrap(),
        &[],
        "pito",
        Some(dir.path()),
    );
    assert_eq!(
        wire,
        PairingWire::Unavailable {
            reason: PairingUnavailable::LoopbackBind
        },
        "loopback bind → LoopbackBind, even with TLS on"
    );
}

/// MP §4.4: a non-loopback bind without TLS is `Unavailable(NoTls)`.
#[test]
fn pairing_links_no_tls_unavailable() {
    let wire = pairing_links("192.168.7.7:7823".parse().unwrap(), &[], "pito", None);
    assert_eq!(
        wire,
        PairingWire::Unavailable {
            reason: PairingUnavailable::NoTls
        },
        "no TLS → NoTls (there is no fingerprint to offer)"
    );
}

// ===========================================================================
// GET /api/pairing (HTTP)
// ===========================================================================

/// §8.4 / §9 T10: `GET /api/pairing` answers 200 with
/// `{"Available":{"links":[…]}}` where the first link is
/// `https://<primary host>/?token=<t>&ca=sha256:<fp>` and
/// `<fp> == tls::ca_fingerprint` of the server's cert dir.
#[tokio::test]
async fn pairing_endpoint_serves_token_and_ca_links() {
    let sb = Sandbox::new(
        parse_global(REMOTE_GLOBAL),
        "192.168.7.7:7823",
        Some("pito"),
        true,
    );
    let fp = tls::ca_fingerprint(sb.tls_dir_path()).unwrap();
    let addr = spawn(sb.state.clone(), Some("pito")).await;

    let resp = get_json(addr, "/api/pairing", Some("pito")).await;
    assert_eq!(resp.status(), 200, "GET /api/pairing with the token");
    let v: serde_json::Value = resp.json().await.unwrap();
    let links = v["Available"]["links"]
        .as_array()
        .unwrap_or_else(|| panic!("the Available wire carries links; got {v}"));
    assert!(
        !links.is_empty(),
        "a LAN bind with TLS has at least one link"
    );
    assert_eq!(
        links[0],
        format!("https://192.168.7.7/?token=pito&ca={fp}"),
        "the first link = entry URL + &ca, the ca == tls::ca_fingerprint"
    );
}

/// §6.2 / MP §6.4: `/api/pairing` is token-gated like every settings route
/// (401 without, 401 with a wrong token, 200 with the right one).
#[tokio::test]
async fn pairing_endpoint_is_token_gated() {
    let sb = Sandbox::new(
        parse_global(REMOTE_GLOBAL),
        "192.168.7.7:7823",
        Some("s3cret"),
        true,
    );
    let addr = spawn(sb.state.clone(), Some("s3cret")).await;

    let r = get_json(addr, "/api/pairing", None).await;
    assert_eq!(r.status(), 401, "no token → 401");
    let r = get_json(addr, "/api/pairing?token=wrong", None).await;
    assert_eq!(r.status(), 401, "a wrong token → 401");
    let r = get_json(addr, "/api/pairing", Some("s3cret")).await;
    assert_eq!(r.status(), 200, "the right token → 200");
}

/// MP §4.4: the unavailable wire states carry the locked snake_case
/// reasons — `loopback_bind` (checked first, even with TLS on) and
/// `no_tls` (non-loopback bind, TLS off).
#[tokio::test]
async fn pairing_endpoint_unavailable_reasons() {
    // Loopback bind, TLS on → loopback_bind wins the ordering.
    let sb = Sandbox::new(
        parse_global(REMOTE_GLOBAL),
        "127.0.0.1:7823",
        Some("pito"),
        true,
    );
    let addr = spawn(sb.state.clone(), Some("pito")).await;
    let v: serde_json::Value = get_json(addr, "/api/pairing", Some("pito"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["Unavailable"]["reason"], "loopback_bind",
        "loopback bind → the loopback_bind reason; got {v}"
    );

    // Non-loopback bind, TLS off → no_tls.
    let sb = Sandbox::new(
        parse_global(REMOTE_GLOBAL),
        "192.168.7.7:7823",
        Some("pito"),
        false,
    );
    let addr = spawn(sb.state.clone(), Some("pito")).await;
    let v: serde_json::Value = get_json(addr, "/api/pairing", Some("pito"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["Unavailable"]["reason"], "no_tls",
        "non-loopback without TLS → the no_tls reason; got {v}"
    );
}
