//! P3 PR-C RED e2e (p3-design §I.2): the REAL relay in-process
//! (`peakbot_relay::serve` via the relay's own `TestRelay`) + its `FakeIdp`,
//! and TWO peakbot cores in ONE process — "computer" (host, CloudServe) and
//! "phone" (controller). The cores boot through `boot::web_boot` +
//! `boot::boot_cloud` with `type: mock` providers, so no real LLM is called.
//!
//! TWO-CORES-IN-ONE-PROCESS (investigated): `paths::init_roots` is a
//! process-global `OnceLock`. Everything the web core derives from it is
//! injected per core instead — uploads root (`boot::session_deps_in`),
//! conversation `storage_dir` (config), cloud `state_dir` + `MemSeedStore`
//! (`boot_cloud`), and the controller's `Directory`/cloud handle
//! (`WebUi::with_directory`, since `DIRECTORY` and the boot handle are
//! also process-global). What stays shared, by design: ONE roots dir
//! holding the global `config.yaml` that `fresh_attach` re-reads (§5.11)
//! and the image cache dir. No subprocess needed.
//!
//! RED today: CloudServe/Client::Cloud/Directory/boot_cloud are `todo!()`
//! skeletons, so every case fails at boot (`boot_cloud`) — feature absent.

mod cloud_common;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

use cloud::frames::{Op, StreamMsg};
use futures::{SinkExt, StreamExt};
use peakbot::boot::{self, WebBoot, WebSession};
use peakbot::cloud::store::MemSeedStore;
use peakbot::cloud::{CloudHandle, Seed, SeedStore};
use peakbot::config::{Config, ParseCtx, Source};
use peakbot::paths::{self, Roots};
use peakbot::skills::SkillRegistry;
use peakbot::ui::WebUi;
use peakbot::ui::web::remote_types::Directory;
use peakbot::ui::web::settings::TokenSource;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as TMsg;

use cloud_common::host::YAML;
use cloud_common::relay_harness::real::{Mint, TestRelay};

const WAIT: Duration = Duration::from_secs(20);
type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ── process-global roots: ONE shared dir, everything else per core ───────

static ROOTS_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
static ROOTS_ONCE: Once = Once::new();

fn yaml_for(storage: &Path, relay_url: Option<&str>) -> String {
    let mut extra = format!("  conversation:\n    storage_dir: {}\n", storage.display());
    if let Some(url) = relay_url {
        extra.push_str(&format!("  cloud:\n    relay: {url}\n"));
    }
    YAML.replacen("general:\n", &format!("general:\n{extra}"), 1)
}

fn init_shared_roots() {
    ROOTS_ONCE.call_once(|| {
        let t = tempfile::tempdir().expect("roots dir");
        let (c, d, k) = (t.path().join("c"), t.path().join("d"), t.path().join("k"));
        for p in [&c, &d, &k] {
            std::fs::create_dir_all(p).expect("root");
        }
        std::fs::write(c.join("config.yaml"), yaml_for(&d.join("shared"), None)).expect("cfg");
        paths::init_roots(Roots {
            config: c,
            data: d,
            cache: k,
        });
        let _ = ROOTS_DIR.set(t);
    });
}

fn parse(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture config must parse")
}

// ── a core: web server + CloudLink, all paths injected ───────────────────

struct Core {
    addr: SocketAddr,
    handle: CloudHandle,
    _ui: WebUi,
    _tmp: tempfile::TempDir,
}

impl Core {
    async fn start(name: &str, seed: u8, relay: &TestRelay) -> Core {
        init_shared_roots();
        let tmp = tempfile::tempdir().expect("core dir");
        let yaml = yaml_for(&tmp.path().join("convos"), Some(&relay.url()));
        Self::start_with_yaml(name, seed, tmp, yaml, relay).await
    }

    /// Boot one core with an explicit YAML (the phone fixture has zero local
    /// profiles, like the real controller-only install).
    async fn start_with_yaml(
        name: &str,
        seed: u8,
        tmp: tempfile::TempDir,
        yaml: String,
        _relay: &TestRelay,
    ) -> Core {
        let deps = boot::session_deps_in(
            parse(&yaml),
            SkillRegistry::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            tmp.path().join("uploads"),
        );
        let web = WebBoot {
            addr: "127.0.0.1:0".parse().expect("addr"),
            token: None,
            token_source: TokenSource::None,
            tls: false,
            tls_names: Vec::new(),
            config_dir: tmp.path().to_path_buf(),
            boot_cwd: tmp.path().to_path_buf(),
        };
        let ui = boot::web_boot(web, WebSession::Ready(deps)).expect("web_boot");
        let store = Arc::new(MemSeedStore::new());
        store.store_seed(&Seed::new([seed; 64])).expect("seed");
        let handle = boot::boot_cloud(
            &parse(&yaml),
            tmp.path().join("cloud"),
            name,
            store,
            ui.cloud_inbound(),
        )
        .expect("general.cloud is set ⇒ the CloudLink is spawned");
        let dir = Arc::new(Directory::new().with_cloud(Some(handle.clone())));
        let ui = ui.with_cloud(Some(handle.clone())).with_directory(dir);
        let router = ui.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        Core {
            addr,
            handle,
            _ui: ui,
            _tmp: tmp,
        }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let r = reqwest::get(format!("http://{}{path}", self.addr))
            .await
            .expect("GET");
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(format!("http://{}{path}", self.addr))
            .json(&body)
            .send()
            .await
            .expect("POST");
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    /// Poll `/api/cloud/state` until `ok(state)` (or fail after WAIT).
    async fn state_where(&self, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + WAIT;
        loop {
            let (_, s) = self.get("/api/cloud/state").await;
            if ok(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// G.1 without a browser: POST login → follow the auth URL (no
    /// redirects) → the FakeIdp mints a code → relay callback → the
    /// device's loopback `/cb`.
    async fn login(&self, relay: &TestRelay) {
        relay.idp.set_mint(Mint::default());
        let (st, body) = self.post("/api/cloud/login", json!({})).await;
        assert_eq!(st, 200, "login starts: {body}");
        let auth = url::Url::parse(body["auth_url"].as_str().expect("auth_url")).expect("url");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");
        let r = http.get(auth.clone()).send().await.expect("auth url");
        let loc = url::Url::parse(r.headers()["location"].to_str().expect("loc")).expect("idp");
        let q: std::collections::HashMap<_, _> = loc.query_pairs().into_owned().collect();
        let code = relay.idp.issue_code(&q["nonce"], &q["code_challenge"]);
        let cb = http
            .get(format!("{}/oidc/callback", relay.url()))
            .query(&[("code", code.as_str()), ("state", q["state"].as_str())])
            .send()
            .await
            .expect("oidc callback");
        let back = cb.headers()["location"].to_str().expect("back").to_string();
        let _ = reqwest::get(back).await.expect("device /cb");
    }

    fn id_hex(&self) -> String {
        hex(&self.handle.device_id().expect("device id"))
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Computer (genesis) + phone (login → Pending → approved): both Active.
async fn pair(relay: &TestRelay) -> (Core, Core) {
    let computer = Core::start("computer", 1, relay).await;
    let phone = Core::start("phone", 2, relay).await;
    computer.login(relay).await;
    computer
        .state_where("computer pending", |s| s["state"] == "pending")
        .await;
    let (st, b) = computer.post("/api/cloud/genesis", json!({})).await;
    assert_eq!(st, 200, "genesis: {b}");
    phone.login(relay).await;
    let s = computer
        .state_where("an enroll request", |s| {
            s["requests"].as_array().is_some_and(|r| !r.is_empty())
        })
        .await;
    let letter = s["requests"][0]["letter_id"].clone();
    let (st, b) = computer
        .post("/api/cloud/enroll/approve", json!({ "letter_id": letter }))
        .await;
    assert_eq!(st, 200, "approve: {b}");
    for c in [&computer, &phone] {
        c.state_where("active with 2 devices", |s| {
            s["state"] == "active" && s["devices"].as_array().is_some_and(|d| d.len() == 2)
        })
        .await;
    }
    (computer, phone)
}

// ── controller-side browser: the phone's real /ws ─────────────────────────

async fn ws(core: &Core) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", core.addr))
        .await
        .expect("ws upgrade");
    ws
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(TMsg::Text(v.to_string().into()))
        .await
        .expect("send");
}

/// Next JSON frame; `None` = closed/errored/timed out.
async fn next(ws: &mut Ws, secs: u64) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Ok(Some(Ok(TMsg::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(TMsg::Ping(_) | TMsg::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

/// Read frames until `want(frame)`; panics on an `error` frame unless asked.
async fn until(ws: &mut Ws, what: &str, want: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..300 {
        let f = next(ws, 30)
            .await
            .unwrap_or_else(|| panic!("closed before {what}"));
        if want(&f) {
            return f;
        }
        assert_ne!(f["type"], "error", "error frame before {what}: {f}");
    }
    panic!("never saw {what}");
}

/// Attach `profile` (e.g. `dev@computer`) and return (ws, convo id).
async fn attach(core: &Core, profile: &str) -> (Ws, String) {
    let mut w = ws(core).await;
    send(
        &mut w,
        json!({"type":"attach","convo":null,"profile":profile}),
    )
    .await;
    let a = until(&mut w, "attached", |f| f["type"] == "attached").await;
    until(&mut w, "ready", |f| f["type"] == "ready").await;
    (w, a["convo"].as_str().expect("convo").to_string())
}

async fn turn(w: &mut Ws, text: &str) {
    send(w, json!({"type":"send_message","text":text})).await;
    until(w, "the mock reply", |f| {
        f["type"] == "state"
            && f["state"]["chat"]["messages"].as_array().is_some_and(|m| {
                m.iter().any(|m| {
                    m["role"] == "agent" && m["content"].to_string().contains("MOCK_REPLY")
                })
            })
    })
    .await;
}

// ── raw tunnel (a "cloud peer" below the controller code) ────────────────

async fn raw_attach(
    phone: &Core,
    computer: &Core,
    profile: &str,
    relayed: bool,
) -> cloud_stream::S {
    let mut s = phone
        .handle
        .open(computer.handle.device_id().expect("id"), Op::Attach)
        .await
        .expect("open Attach");
    s.send(StreamMsg::Frame(
        json!({"type":"attach","profile":profile,"agent":"team","relayed":relayed}).to_string(),
    ))
    .await
    .expect("attach frame");
    s
}

mod cloud_stream {
    pub type S = peakbot::cloud::stream::CloudStream;
}

async fn raw_frame(s: &mut cloud_stream::S) -> Option<Value> {
    match tokio::time::timeout(WAIT, s.next())
        .await
        .expect("raw frame in time")
    {
        Some(StreamMsg::Frame(f)) => serde_json::from_str(&f).ok(),
        Some(StreamMsg::Reset(_) | StreamMsg::End) | None => None,
        Some(other) => panic!("non-frame on an attach stream: {other:?}"),
    }
}

// ═══ case 1: parity ═══════════════════════════════════════════════════════

/// Via the phone on `dev@computer`: list shows the computer's row tagged
/// `dev@computer`; attach + type reaches the mock model; kill stops the run;
/// image GET and upload→GET bytes are identical; the profiles list is the
/// computer's LOCAL profiles.
#[tokio::test]
async fn parity_list_attach_type_kill_image_upload_profiles() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;

    // attach + type: the mock model answers over the tunnel.
    let (mut tab, convo) = attach(&phone, "dev@computer").await;
    turn(&mut tab, "hello computer").await;

    // list: the computer's row, tagged profile@device, on the phone's list.
    let mut lw = ws(&phone).await;
    send(&mut lw, json!({"type":"request_conversations"})).await;
    let list = until(&mut lw, "the list", |f| f["type"] == "conversations_list").await;
    let row = list["items"]
        .as_array()
        .and_then(|i| i.iter().find(|r| r["id"] == convo))
        .unwrap_or_else(|| panic!("computer's convo {convo} must be listed: {list}"));
    assert_eq!(
        row["profile"], "dev@computer",
        "rows are tagged profile@device"
    );

    // image GET: identical bytes through /r/dev@computer/images/{id}.
    let png = tiny_png();
    let img = peakbot::image_cache::spill(
        &png,
        rig_core::completion::message::ImageMediaType::PNG,
        "t.png",
    )
    .expect("spill");
    let got = reqwest::get(format!(
        "http://{}/r/dev@computer/images/{}",
        phone.addr, img.id
    ))
    .await
    .expect("image GET");
    assert_eq!(got.status(), 200, "image GET through the tunnel");
    assert_eq!(
        got.bytes().await.expect("body").to_vec(),
        png,
        "image bytes equal"
    );

    // upload then GET: bit-identical.
    let payload: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
    let up = reqwest::Client::new()
        .post(format!(
            "http://{}/r/dev@computer/api/uploads?convo={convo}&name=blob.bin",
            phone.addr
        ))
        .header("content-type", "application/octet-stream")
        .body(payload.clone())
        .send()
        .await
        .expect("upload POST");
    assert_eq!(up.status(), 200, "upload through the tunnel");
    let id = up.json::<Value>().await.expect("json")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let back = reqwest::get(format!(
        "http://{}/r/dev@computer/api/uploads/{convo}/{id}",
        phone.addr
    ))
    .await
    .expect("upload GET");
    assert_eq!(
        back.bytes().await.expect("body").to_vec(),
        payload,
        "upload bytes equal"
    );

    // profiles: the computer's local profiles only.
    let (st, p) = phone
        .get(&format!(
            "/api/cloud/devices/{}/profiles",
            computer.id_hex()
        ))
        .await;
    assert_eq!(st, 200, "profiles: {p}");
    let text = p.to_string();
    assert!(
        text.contains("dev") && text.contains("alt"),
        "local profiles: {text}"
    );
    assert!(
        !text.contains("home-dev"),
        "never the remote profile: {text}"
    );

    // kill: the computer's run stops (checked on the COMPUTER's own server).
    send(
        &mut lw,
        json!({"type":"kill_session","convo":convo,"profile":"dev@computer"}),
    )
    .await;
    let end = Instant::now() + WAIT;
    loop {
        let (_, l) = computer.get("/api/conversations").await;
        let row = l["items"]
            .as_array()
            .and_then(|i| i.iter().find(|r| r["id"] == convo));
        if row.is_none_or(|r| r["active"] == false) {
            break;
        }
        assert!(
            Instant::now() < end,
            "the kill must stop the computer's session: {l}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn tiny_png() -> Vec<u8> {
    // A valid 1×1 PNG.
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

// ═══ case 2: Shutdown dropped on the computer ═════════════════════════════

/// A cloud peer's `shutdown` is answered with the error frame; the
/// computer keeps serving and the session still works.
#[tokio::test]
async fn shutdown_from_a_cloud_peer_is_dropped_and_the_computer_keeps_serving() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;
    let mut s = raw_attach(&phone, &computer, "dev", true).await;
    while raw_frame(&mut s)
        .await
        .is_some_and(|f| f["type"] != "ready")
    {}

    s.send(StreamMsg::Frame(json!({"type":"shutdown"}).to_string()))
        .await
        .expect("send");
    // Skip benign post-ready frames (`state`, `models_available`) the
    // server emits around the session, like the serve test (E-C2).
    let mut f = None;
    for _ in 0..8 {
        let g = raw_frame(&mut s).await.expect("the session survives");
        if g["type"] == "error" {
            f = Some(g);
            break;
        }
    }
    let f = f.expect("the refusal frame");
    assert_eq!(f["type"], "error", "refused: {f}");
    assert!(f.to_string().contains("not allowed over Cloud Mode"), "{f}");

    let (st, _) = computer.get("/api/cloud/state").await;
    assert_eq!(st, 200, "the computer is still serving");
}

// ═══ case 3: chaining refused ═════════════════════════════════════════════

/// The computer has a `remote:` profile (`home-dev`): a raw tunnel attach
/// with `relayed:false` is refused, and the Profiles op never lists it.
#[tokio::test]
async fn chaining_through_the_computers_remote_profile_is_refused() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;

    let mut s = raw_attach(&phone, &computer, "home-dev", false).await;
    let f = raw_frame(&mut s).await.expect("a refusal frame");
    assert_eq!(f["type"], "error", "chaining must be refused: {f}");
    assert!(f.to_string().contains("chaining"), "names the reason: {f}");

    let (_, p) = phone
        .get(&format!(
            "/api/cloud/devices/{}/profiles",
            computer.id_hex()
        ))
        .await;
    assert!(!p.to_string().contains("home-dev"), "never listed: {p}");
}

// ═══ case 4: revoke kills a live tab ══════════════════════════════════════

/// A live tab phone→computer; the phone revokes the computer → the tab's
/// WS closes within 2 s, and the computer sees `Trust(Revoked)`.
#[tokio::test]
async fn revoke_kills_a_live_tab_within_two_seconds() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;
    let (mut tab, _) = attach(&phone, "dev@computer").await;

    let (st, b) = phone
        .post(
            &format!("/api/cloud/devices/{}/revoke", computer.id_hex()),
            json!({}),
        )
        .await;
    assert_eq!(st, 200, "revoke: {b}");

    let t0 = Instant::now();
    while next(&mut tab, 2).await.is_some() {
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "tab must close within 2 s"
        );
    }
    let s = computer
        .state_where("trust revoked", |s| s["state"] == "trust")
        .await;
    assert!(
        s["trust_error"]
            .to_string()
            .to_lowercase()
            .contains("revoked"),
        "the computer is told it was revoked: {s}"
    );
}

// ═══ case 5: offline computer → clear error ═══════════════════════════════

/// Computer stopped: the phone's attach fails with "device offline", and the
/// merged list returns (bounded) without the computer's rows.
#[tokio::test]
async fn offline_computer_gives_a_clear_error_and_a_bounded_list() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;
    computer.handle.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut w = ws(&phone).await;
    send(
        &mut w,
        json!({"type":"attach","convo":null,"profile":"dev@computer"}),
    )
    .await;
    let f = until_error(&mut w).await;
    assert!(f.to_string().contains("device offline"), "clear error: {f}");

    let t0 = Instant::now();
    let mut lw = ws(&phone).await;
    send(&mut lw, json!({"type":"request_conversations"})).await;
    let list = until(&mut lw, "the list", |f| f["type"] == "conversations_list").await;
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "bounded per-device timeout"
    );
    assert!(
        !list.to_string().contains("@computer"),
        "an offline device contributes no rows: {list}"
    );
}

async fn until_error(w: &mut Ws) -> Value {
    for _ in 0..50 {
        let f = next(w, 30).await.expect("an error frame before close");
        if f["type"] == "error" {
            return f;
        }
        assert_ne!(f["type"], "ready", "must not become ready: {f}");
    }
    panic!("no error frame");
}

// ═══ case 7: SAS equality ═════════════════════════════════════════════════

/// During enrollment the phone's SAS words equal the computer's request SAS.
#[tokio::test]
async fn sas_words_are_equal_on_both_sides_during_enrollment() {
    let relay = TestRelay::spawn().await;
    let computer = Core::start("computer", 1, &relay).await;
    let phone = Core::start("phone", 2, &relay).await;
    computer.login(&relay).await;
    computer
        .state_where("pending", |s| s["state"] == "pending")
        .await;
    computer.post("/api/cloud/genesis", json!({})).await;
    phone.login(&relay).await;

    let mine = phone
        .state_where("phone sas", |s| {
            s["state"] == "pending" && s["sas"].is_array()
        })
        .await;
    let theirs = computer
        .state_where("request sas", |s| {
            s["requests"].as_array().is_some_and(|r| !r.is_empty())
        })
        .await;
    assert_eq!(
        mine["sas"], theirs["requests"][0]["sas"],
        "SAS words must match"
    );
}

// ═══ case 8: a pending phone can't control the computer ═══════════════════

/// Before approval the phone cannot open a tunnel or attach `dev@computer`.
#[tokio::test]
async fn a_pending_phone_cannot_control_the_computer_until_approved() {
    let relay = TestRelay::spawn().await;
    let computer = Core::start("computer", 1, &relay).await;
    let phone = Core::start("phone", 2, &relay).await;
    computer.login(&relay).await;
    computer
        .state_where("pending", |s| s["state"] == "pending")
        .await;
    computer.post("/api/cloud/genesis", json!({})).await;
    phone.login(&relay).await;
    phone
        .state_where("pending", |s| s["state"] == "pending")
        .await;

    let err = phone
        .handle
        .open(computer.handle.device_id().expect("id"), Op::Attach)
        .await
        .expect_err("a pending device has no tunnel to the computer");
    let _ = err;

    let mut w = ws(&phone).await;
    send(
        &mut w,
        json!({"type":"attach","convo":null,"profile":"dev@computer"}),
    )
    .await;
    let f = until_error(&mut w).await;
    assert!(
        !f.to_string().is_empty(),
        "a clear refusal, never a session: {f}"
    );
}

/// Opening an EXISTING conversation from the phone must deliver `attached`,
/// `ready` and — with no further client verb — the conversation's `state`.
/// This is the SPA's `switchConvo` wire exactly: `attach{convo, profile}` on a
/// fresh browser socket.
#[tokio::test]
async fn reopen_existing_convo_delivers_its_state() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;

    // Create a conversation through the phone's real /ws path.
    let (mut tab, convo) = attach(&phone, "dev@computer").await;
    turn(&mut tab, "hello computer").await;
    tab.close(None).await.ok();
    drop(tab);

    // Phase A: the PC session is still live → registry.attach shares it.
    let mut w = ws(&phone).await;
    send(
        &mut w,
        json!({"type":"attach","convo":convo,"profile":"dev@computer"}),
    )
    .await;
    until(&mut w, "attached (live share)", |f| f["type"] == "attached").await;
    until(&mut w, "ready (live share)", |f| f["type"] == "ready").await;
    let st = until(&mut w, "state (live share)", |f| f["type"] == "state").await;
    assert_eq!(st["state"]["conversation"]["id"], convo.as_str());
    assert!(
        st["state"]["chat"]["messages"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "a live reopen must carry the transcript: {st}"
    );
    w.close(None).await.ok();
    drop(w);

    // Phase B: end the live session, so the reopen is a disk resume.
    let (status, body) = computer
        .post(&format!("/api/conversations/{convo}/kill"), json!({}))
        .await;
    assert_eq!(status, 200, "kill: {body}");

    let mut w = ws(&phone).await;
    send(
        &mut w,
        json!({"type":"attach","convo":convo,"profile":"dev@computer"}),
    )
    .await;
    let a = until(&mut w, "attached (resume)", |f| f["type"] == "attached").await;
    assert_eq!(a["convo"], convo.as_str(), "resume binds the same id");
    until(&mut w, "ready (resume)", |f| f["type"] == "ready").await;
    let st = until(&mut w, "state (resume)", |f| f["type"] == "state").await;
    assert_eq!(st["state"]["conversation"]["id"], convo.as_str());
    assert!(
        st["state"]["chat"]["messages"]
            .as_array()
            .is_some_and(|m| !m.is_empty()),
        "a disk resume must carry the transcript: {st}"
    );
}

/// A finished non-Attach op must release its mux slot (S4: the stream closes
/// when BOTH directions end). The SPA polls the list every 4 s through one
/// long-lived cloud session, so a one-slot-per-poll leak exhausts
/// `MAX_STREAMS` and every later open — including a conversation Attach — is
/// served from an unregistered stream id.
#[tokio::test]
async fn cloud_session_reclaims_finished_non_attach_streams() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;
    let device = computer.handle.device_id().expect("id");

    for i in 0..cloud::constants::MAX_STREAMS {
        let mut s = phone
            .handle
            .open(device, Op::List)
            .await
            .unwrap_or_else(|e| panic!("open list {i}: {e:?}"));
        loop {
            match tokio::time::timeout(WAIT, s.next())
                .await
                .unwrap_or_else(|_| panic!("op {i} reply in time"))
            {
                Some(StreamMsg::Head { .. } | StreamMsg::Chunk(_)) => {}
                Some(StreamMsg::End) => break,
                other => panic!("op {i}: unexpected {other:?}"),
            }
        }
    }

    // The reported wire: opening an existing conversation is an Attach. It
    // must still be served after MAX_STREAMS completed list polls.
    let mut s = phone
        .handle
        .open(device, Op::Attach)
        .await
        .expect("a completed op returns its slot");
    s.send(StreamMsg::Frame(
        json!({"type":"attach","profile":"dev","relayed":true}).to_string(),
    ))
    .await
    .expect("attach frame");
    let mut ready = false;
    while let Some(msg) = tokio::time::timeout(WAIT, s.next())
        .await
        .unwrap_or_else(|_| panic!("attach reply in time after MAX_STREAMS ops"))
    {
        match msg {
            StreamMsg::Frame(f) => {
                if serde_json::from_str::<Value>(&f).is_ok_and(|v| v["type"] == "ready") {
                    ready = true;
                    break;
                }
            }
            StreamMsg::Reset(code) => panic!(
                "attach reset after {MAX_STREAMS} list polls: {code}",
                MAX_STREAMS = cloud::constants::MAX_STREAMS
            ),
            _ => {}
        }
    }
    assert!(ready, "attach reached the computer after MAX_STREAMS ops");
}
