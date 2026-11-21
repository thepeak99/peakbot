//! Fresh-enrollment e2e: a cloud-only phone enrolls, is approved from the
//! computer, and then lists the computer's conversations via the cloud picker.
//! This runs in a separate binary from cloud_e2e.rs because it rewrites the
//! process-global config.yaml for the phone's attach.

mod cloud_common;

use std::path::Path;
use std::sync::{Arc, Once, OnceLock};
use std::time::{Duration, Instant};

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
use cloud_common::relay_harness::real::TestRelay;

const WAIT: Duration = Duration::from_secs(20);
type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

#[allow(dead_code)]
struct Core {
    addr: std::net::SocketAddr,
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

    async fn login(&self, relay: &TestRelay) {
        relay
            .idp
            .set_mint(cloud_common::relay_harness::real::Mint::default());
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

    #[allow(dead_code)]
    fn id_hex(&self) -> String {
        hex(&self.handle.device_id().expect("device id"))
    }
}

#[allow(dead_code)]
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

async fn next(ws: &mut Ws, secs: u64) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Ok(Some(Ok(TMsg::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(TMsg::Ping(_) | TMsg::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

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

/// A fresh cloud-only phone enrolls, gets approved, and lists the computer's
/// conversations.
#[tokio::test]
async fn fresh_enrollment_cloud_only_phone_lists_peers_after_activation() {
    let relay = TestRelay::spawn().await;
    let (computer, phone) = pair(&relay).await;

    // Create a conversation on the computer through the phone's real /ws path.
    let (mut tab, convo) = attach(&phone, "dev@computer").await;
    turn(&mut tab, "hello computer").await;

    // Verify the computer sees the conversation.
    let end = Instant::now() + WAIT;
    loop {
        let (_, l) = computer.get("/api/conversations").await;
        let row = l["items"]
            .as_array()
            .and_then(|i| i.iter().find(|r| r["id"] == convo));
        if row.is_some() {
            break;
        }
        assert!(
            Instant::now() < end,
            "the computer must see the conversation: {l}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Cloud picker: the phone sees the computer as a peer.
    let (st, devices) = phone.get("/api/cloud/devices").await;
    assert_eq!(st, 200, "devices: {devices}");
    let _comp = devices["devices"]
        .as_array()
        .and_then(|d| d.iter().find(|d| d["self"] == false))
        .unwrap_or_else(|| panic!("the computer is a peer: {devices}"));

    // Re-attach and list conversations — the fresh phone must see them.
    tab.close(None).await.ok();
    drop(tab);
    let mut lw = ws(&phone).await;
    send(
        &mut lw,
        json!({"type": "attach", "convo": null, "profile": "dev@computer"}),
    )
    .await;
    until(&mut lw, "attached", |f| f["type"] == "attached").await;
    until(&mut lw, "ready", |f| f["type"] == "ready").await;

    send(&mut lw, json!({"type": "request_conversations"})).await;
    let list = until(&mut lw, "the list", |f| f["type"] == "conversations_list").await;
    assert!(
        list["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|r| r["id"] == convo)),
        "the computer's conversation must be listed: {list}"
    );
}

async fn attach(core: &Core, profile: &str) -> (Ws, String) {
    let mut w = ws(core).await;
    send(
        &mut w,
        json!({"type": "attach", "convo": null, "profile": profile}),
    )
    .await;
    let a = until(&mut w, "attached", |f| f["type"] == "attached").await;
    until(&mut w, "ready", |f| f["type"] == "ready").await;
    (w, a["convo"].as_str().expect("convo").to_string())
}

async fn turn(w: &mut Ws, text: &str) {
    send(w, json!({"type": "send_message", "text": text})).await;
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
