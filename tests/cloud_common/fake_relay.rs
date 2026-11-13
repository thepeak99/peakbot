//! The in-test FAKE RELAY: one axum server speaking the `cloud` relay
//! codec plus the fake OIDC endpoints. One account (all tokens join it),
//! so two CloudLinks in the same account see the same chain — exactly the
//! p3 PR-B scenario. See `mod.rs` for the documented test seams.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::devices::b64url;
use axum::{
    Json, Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    response::Response,
    routing::{get, post},
};
use cloud::{
    DeviceId, LetterId,
    card::DeviceCard,
    chain::{self, ChainEntry},
    codec, constants,
};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// One in-flight `/login` flow (relay-design §4.1).
struct Flow {
    challenge: String,
    #[allow(dead_code)]
    redirect_uri: String,
    device_state: String,
}

struct Inner {
    /// The account's chain: raw CE entries, in seq order (one account).
    entries: std::sync::Mutex<Vec<Vec<u8>>>,
    /// Live tokens (b64url) ⇒ the device that Hello'd with them.
    tokens: std::sync::Mutex<HashMap<String, DeviceId>>,
    /// In-flight `/login` flows keyed by `relay_state`.
    logins: std::sync::Mutex<HashMap<String, Flow>>,
    /// One-time codes: sha256(code) hex ⇒ PKCE challenge.
    grants: std::sync::Mutex<HashMap<String, String>>,
    /// Connected sockets: DeviceId ⇒ outbox sender.
    sockets: std::sync::Mutex<HashMap<DeviceId, mpsc::UnboundedSender<cloud::codec::FromRelay>>>,
    /// `HelloPending` cards: DeviceId ⇒ card bytes (until Add/Genesis).
    pending_cards: std::sync::Mutex<HashMap<DeviceId, Vec<u8>>>,
    /// Offline letter mailboxes: DeviceId ⇒ (from, letter_id, sealed).
    mailboxes: std::sync::Mutex<HashMap<DeviceId, Vec<(DeviceId, LetterId, Vec<u8>)>>>,
    /// Per-connection stop flags: token ⇒ flag.
    conns: std::sync::Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Decoded ToRelay messages, in order (the "no Hello on chain Err"
    /// and "ChainFetch before Hello" assertions read this).
    msgs: std::sync::Mutex<Vec<String>>,
    /// When set, `/ws` upgrades block until released (deterministic
    /// `Connecting` state).
    ws_gate: AtomicBool,
    ws_gate_released: tokio::sync::Notify,
    next_letter: std::sync::atomic::AtomicU64,
    // The relay origin string (Hello tbs input); set at spawn.
    origin: std::sync::Mutex<Option<String>>,
}

impl Inner {
    fn verified(&self) -> Option<cloud::chain::VerifiedChain> {
        let entries = self.entries.lock().expect("entries");
        if entries.is_empty() {
            return None;
        }
        cloud::chain::extend(None, &entries, None).ok()
    }

    fn head(&self) -> cloud::Head {
        super::devices::head_of(&self.entries.lock().expect("entries"))
    }

    fn send(&self, to: &DeviceId, msg: cloud::codec::FromRelay) -> bool {
        let tx = self.sockets.lock().expect("sockets").get(to).cloned();
        match tx {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }

    fn broadcast(&self, msg: cloud::codec::FromRelay) {
        let senders: Vec<_> = self
            .sockets
            .lock()
            .expect("sockets")
            .values()
            .cloned()
            .collect();
        for tx in senders {
            let _ = tx.send(msg.clone());
        }
    }

    /// Deliver the stored mail for `to` (oldest first, relay-design §4.2).
    fn deliver_mail(&self, to: &DeviceId) {
        let mut mbox = self.mailboxes.lock().expect("mailboxes");
        if let Some(items) = mbox.get_mut(to) {
            let drain = std::mem::take(items);
            drop(mbox);
            for (from, letter_id, sealed) in drain {
                self.send(
                    to,
                    cloud::codec::FromRelay::Letter {
                        from,
                        letter_id,
                        sealed,
                    },
                );
            }
        }
    }

    fn log(&self, msg: &str) {
        self.msgs.lock().expect("msgs").push(msg.to_string());
    }
}

/// A live fake relay on a random loopback port. Cheap to clone: the
/// `Arc<Inner>` is shared; the first owner keeps the server task alive,
/// clones share it (the task is bound to 127.0.0.1:0 and dies with the
/// test process).
pub struct FakeRelay {
    pub addr: SocketAddr,
    inner: Arc<Inner>,
    server: Option<JoinHandle<()>>,
}

impl Clone for FakeRelay {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr,
            inner: self.inner.clone(),
            server: None,
        }
    }
}

impl FakeRelay {
    pub async fn spawn() -> Self {
        let inner = Arc::new(Inner {
            entries: Default::default(),
            tokens: Default::default(),
            logins: Default::default(),
            grants: Default::default(),
            sockets: Default::default(),
            pending_cards: Default::default(),
            mailboxes: Default::default(),
            conns: Default::default(),
            msgs: Default::default(),
            ws_gate: AtomicBool::new(false),
            ws_gate_released: tokio::sync::Notify::new(),
            next_letter: std::sync::atomic::AtomicU64::new(1),
            origin: std::sync::Mutex::new(None),
        });
        let app = router(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // The origin used in Hello tbs (public_url): scheme://host:port.
        *inner.origin.lock().expect("origin") = Some(format!("http://127.0.0.1:{}", addr.port()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Self {
            addr,
            inner,
            server: Some(server),
        }
    }

    /// `http://127.0.0.1:{port}` — the origin string used in Hello tbs AND
    /// the relay `public_url`.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub fn url(&self) -> String {
        format!("{}/", self.origin())
    }

    /// The account's raw entries.
    pub fn entries(&self) -> Vec<Vec<u8>> {
        self.inner.entries.lock().expect("entries").clone()
    }

    /// The decoded ToRelay log (order = wire order).
    pub fn msgs(&self) -> Vec<String> {
        self.inner.msgs.lock().expect("msgs").clone()
    }

    /// Gate `/ws` upgrades (the socket is accepted but never answers):
    /// pins the device in `Connecting`.
    pub fn hold_ws(&self, on: bool) {
        self.inner.ws_gate.store(on, Ordering::SeqCst);
        if !on {
            self.inner.ws_gate_released.notify_waiters();
        }
    }

    /// Kill every live socket (the reconnect test).
    pub fn drop_all_sockets(&self) {
        for stop in self.inner.conns.lock().expect("conns").values() {
            stop.store(true, Ordering::SeqCst);
        }
    }

    /// Revoke a token (the 401 → `LoggedOut{Expired}` test).
    pub fn revoke_token(&self, token: &str) {
        self.inner.tokens.lock().expect("tokens").remove(token);
    }

    /// Simulate the IdP + `/oidc/callback`: mint a one-time code for the
    /// flow and hand back (code, device_state) so the test can hit the
    /// loopback redirect.
    pub fn issue_code(&self, relay_state: &str) -> (String, String) {
        let flow = self
            .inner
            .logins
            .lock()
            .expect("logins")
            .remove(relay_state)
            .expect("a /login flow is in flight");
        let code_bytes: [u8; 32] = rand::rng().random();
        let code = b64url(&code_bytes);
        let key = hex_sha256(code.as_bytes());
        self.inner
            .grants
            .lock()
            .expect("grants")
            .insert(key, flow.challenge);
        (code, flow.device_state)
    }

    /// Relay-side letter injection (test seam; design I.2 case 6): deliver
    /// `sealed` to `to` as from `from` (connected ⇒ immediate `Letter`,
    /// else mailbox).
    pub async fn inject_letter(&self, to: DeviceId, from: DeviceId, sealed: Vec<u8>) {
        let id = self.inner.next_letter.fetch_add(1, Ordering::SeqCst);
        let letter_id = devices_letter_id(id);
        if self.inner.send(
            &to,
            cloud::codec::FromRelay::Letter {
                from,
                letter_id,
                sealed: sealed.clone(),
            },
        ) {
            return;
        }
        self.inner
            .mailboxes
            .lock()
            .expect("mailboxes")
            .entry(to)
            .or_default()
            .push((from, letter_id, sealed));
    }

    /// Replace the account's chain wholesale and broadcast
    /// `ChainChanged` (fork / genesis-mismatch test seam).
    pub fn set_entries(&self, entries: Vec<Vec<u8>>) {
        *self.inner.entries.lock().expect("entries") = entries.clone();
        let h = chain_head(&entries);
        self.inner
            .broadcast(cloud::codec::FromRelay::ChainChanged { head_seq: h.seq });
    }

    /// Append one entry (a forged-but-valid `Add`/`Revoke`) and broadcast.
    pub fn append_entry(&self, entry: Vec<u8>) {
        let mut entries = self.inner.entries.lock().expect("entries");
        entries.push(entry.clone());
        drop(entries);
        let h = chain_head(&self.inner.entries.lock().expect("entries"));
        self.inner
            .broadcast(cloud::codec::FromRelay::ChainChanged { head_seq: h.seq });
    }
}

fn devices_letter_id(n: u64) -> LetterId {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&n.to_be_bytes());
    id
}

fn chain_head(entries: &[Vec<u8>]) -> cloud::Head {
    super::devices::head_of(entries)
}

fn hex_sha256(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Deserialize)]
struct TokenBody {
    code: String,
    code_verifier: String,
}

fn router(inner: Arc<Inner>) -> Router {
    Router::new()
        .route("/login", get(login_start))
        .route("/token", post(token_redeem))
        .route("/ws", get(ws_upgrade))
        .route("/logout", post(logout))
        .with_state(inner)
}

// ── OIDC ───────────────────────────────────────────────────────────────────

async fn login_start(
    State(inner): State<Arc<Inner>>,
    axum::extract::Query(q): axum::extract::Query<LoginQuery>,
) -> Response {
    // relay-design §4.1 step 1: parse once; any failure → 400 text.
    let ok_challenge = q
        .code_challenge
        .as_deref()
        .is_some_and(|c| c.len() == 43 && super::devices::b64url_decode_len(c) == 32);
    let ok_state = q
        .state
        .as_deref()
        .is_some_and(|s| (1..=128).contains(&s.len()));
    let ok_redirect = q
        .redirect_uri
        .as_deref()
        .is_some_and(|u| u.starts_with("http://127.0.0.1:"));
    if !ok_challenge || !ok_state || !ok_redirect {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::from("bad login params"))
            .expect("resp");
    }
    let rs: [u8; 16] = rand::rng().random();
    let relay_state = super::devices::b64url(&rs);
    inner.logins.lock().expect("logins").insert(
        relay_state.clone(),
        Flow {
            challenge: q.code_challenge.expect("checked"),
            redirect_uri: q.redirect_uri.expect("checked"),
            device_state: q.state.expect("checked"),
        },
    );
    // 302 → the (fake) IdP authorize URL. The test follows it via
    // `issue_code`, not with a browser.
    Response::builder()
        .status(StatusCode::FOUND)
        .header(
            header::LOCATION,
            format!("/idp/authorize?relay_state={relay_state}"),
        )
        .body(axum::body::Body::empty())
        .expect("resp")
}

#[derive(Deserialize)]
struct LoginQuery {
    code_challenge: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    #[serde(default)]
    issuer: Option<String>,
}

async fn token_redeem(State(inner): State<Arc<Inner>>, Json(body): Json<TokenBody>) -> Response {
    // relay-design §4.1 step 3: the grant is removed BEFORE the check
    // (single-use even on failure); `sha256(code_verifier) == challenge`.
    let key = hex_sha256(body.code.as_bytes());
    let challenge = inner.grants.lock().expect("grants").remove(&key);
    let verifier_ok = (43..=128).contains(&body.code_verifier.len());
    match challenge.filter(|c| {
        verifier_ok && &b64url(Sha256::digest(body.code_verifier.as_bytes()).as_slice()) == c
    }) {
        Some(_) => {
            let t: [u8; 32] = rand::rng().random();
            let token = b64url(&t);
            inner
                .tokens
                .lock()
                .expect("tokens")
                .insert(token.clone(), [0; 32]);
            Json(serde_json::json!({ "token": token, "expires_in": 1_209_600u64 })).into_response()
        }
        None => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid_grant" })),
        )
            .into_response(),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

async fn logout(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> StatusCode {
    match bearer(&headers) {
        Some(token) => {
            if inner
                .tokens
                .lock()
                .expect("tokens")
                .remove(&token)
                .is_some()
            {
                if let Some(id) = inner
                    .tokens
                    .lock()
                    .expect("tokens")
                    .iter()
                    .map(|(_, d)| *d)
                    .next()
                {
                    let _ = id;
                }
                if let Some(stop) = inner.conns.lock().expect("conns").remove(&token) {
                    stop.store(true, Ordering::SeqCst);
                }
            }
            // 204 even when the token is unknown (idempotent, no oracle).
            StatusCode::NO_CONTENT
        }
        None => StatusCode::UNAUTHORIZED,
    }
}

// ── WebSocket ──────────────────────────────────────────────────────────────

async fn ws_upgrade(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let token = match bearer(&headers) {
        Some(t) if inner.tokens.lock().expect("tokens").contains_key(&t) => t,
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    };
    // The deterministic-`Connecting` gate.
    if inner.ws_gate.load(Ordering::SeqCst) {
        inner.ws_gate_released.notified().await;
    }
    ws.on_upgrade(move |socket| conn(inner, token, socket))
}

async fn conn(inner: Arc<Inner>, token: String, mut ws: WebSocket) {
    let stop = Arc::new(AtomicBool::new(false));
    inner
        .conns
        .lock()
        .expect("conns")
        .insert(token.clone(), stop.clone());

    let nonce: [u8; 32] = rand::rng().random();
    let _ = ws
        .send(Message::binary(codec::encode_from_relay(
            &cloud::codec::FromRelay::Challenge { nonce },
        )))
        .await;

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<cloud::codec::FromRelay>();
    let mut id: Option<DeviceId> = None;
    let origin = {
        let mut addr = inner.tokens.lock().expect("tokens");
        addr.get_mut(&token).copied().unwrap_or([0; 32]);
        // The origin lives on the FakeRelay; the conn task only needs it
        // for Hello verification, so plumb it through the token map's
        // sibling: recompute from the server addr is not available here —
        // use the flow's origin instead (stored at login). Kept simple:
        // the actor signs with the SAME string, so we derive it once.
        origin_str(&inner, &token)
    };

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {
            maybe_msg = ws.recv() => {
                let Some(Ok(Message::Binary(bytes))) = maybe_msg else {
                    break;
                };
                let Ok(msg) = codec::decode_to_relay(&bytes) else {
                    let _ = ws.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 1002, reason: "protocol".into() }))).await;
                    break;
                };
                dispatch(&inner, &origin, nonce, &mut id, msg, &out_tx, &stop, &mut ws).await;
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            Some(out) = out_rx.recv() => {
                if ws.send(Message::binary(codec::encode_from_relay(&out))).await.is_err() {
                    break;
                }
            }
        }
    }

    if let Some(id) = id {
        remove_socket(&inner, &id);
        inner
            .pending_cards
            .lock()
            .expect("pending_cards")
            .remove(&id);
    }
    inner.conns.lock().expect("conns").remove(&token);
}

fn origin_str(inner: &Arc<Inner>, _token: &str) -> String {
    // The origin is constant per FakeRelay; recover it from any stored
    // login flow's redirect target is overkill — the relay origin equals
    // the `public_url`, which the test built the CloudConfig from. We store
    // it lazily: the first /login sets `inner`'s origin via the flow's
    // redirect_uri host. Simplest faithful source: re-derive from the
    // tokens map is impossible, so we keep it in a dedicated slot.
    inner
        .origin
        .lock()
        .expect("origin")
        .clone()
        .unwrap_or_else(|| "http://127.0.0.1:0".into())
}

fn remove_socket(inner: &Arc<Inner>, id: &DeviceId) {
    inner.sockets.lock().expect("sockets").remove(id);
}

async fn dispatch(
    inner: &Arc<Inner>,
    origin: &str,
    nonce: [u8; 32],
    id: &mut Option<DeviceId>,
    msg: cloud::codec::ToRelay,
    out_tx: &mpsc::UnboundedSender<cloud::codec::FromRelay>,
    stop: &AtomicBool,
    ws: &mut WebSocket,
) {
    use cloud::codec::{FromRelay, ToRelay};
    match msg {
        ToRelay::ChainFetch { from_seq } => {
            inner.log(&format!("ChainFetch{{{from_seq}}}"));
            let entries = inner.entries.lock().expect("entries");
            let batch: Vec<Vec<u8>> = entries
                .iter()
                .filter(|e| entry_seq(e) >= from_seq)
                .take(constants::MAX_CHAIN_BATCH)
                .cloned()
                .collect();
            let _ = out_tx.send(FromRelay::Chain { entries: batch });
        }
        ToRelay::Hello { device, sig } => {
            inner.log(&format!("Hello{{{device:?}}}"));
            let chain = inner.verified().expect("hello needs a chain");
            let card = match chain.active.get(&device) {
                Some(c) => c,
                None => {
                    bad_hello(ws, out_tx, stop).await;
                    return;
                }
            };
            let pk = card.public_key().expect("card pk");
            let tbs = codec::hello_tbs(origin, nonce, device, 0);
            if codec::verify_hello(&pk, &tbs, &sig).is_err() {
                bad_hello(ws, out_tx, stop).await;
                return;
            }
            *id = Some(device);
            let head = inner.head();
            let _ = out_tx.send(FromRelay::Welcome {
                head_seq: Some(head.seq),
                pending: false,
            });
            register_socket(inner, device, out_tx.clone());
            inner.deliver_mail(&device);
        }
        ToRelay::HelloPending { card, sig } => {
            inner.log(&format!("HelloPending"));
            let card = match DeviceCard::parse(&card) {
                Ok(c) => c,
                Err(_) => {
                    bad_hello(ws, out_tx, stop).await;
                    return;
                }
            };
            let device = card.device_id();
            let chain = inner.verified();
            let known = chain
                .is_some_and(|c| c.active.contains_key(&device) || c.revoked.contains(&device));
            if known {
                bad_hello(ws, out_tx, stop).await;
                return;
            }
            let pk = card.public_key().expect("card pk");
            let tbs = codec::hello_tbs(origin, nonce, device, 1);
            if codec::verify_hello(&pk, &tbs, &sig).is_err() {
                bad_hello(ws, out_tx, stop).await;
                return;
            }
            *id = Some(device);
            inner
                .pending_cards
                .lock()
                .expect("pending_cards")
                .insert(device, card.encode());
            let head = inner.head();
            let _ = out_tx.send(FromRelay::Welcome {
                head_seq: Some(head.seq),
                pending: true,
            });
            register_socket(inner, device, out_tx.clone());
            inner.deliver_mail(&device);
        }
        ToRelay::EnrollGenesis { entry } => {
            inner.log("EnrollGenesis");
            let Some(sender) = *id else {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            };
            let e = match ChainEntry::parse(&entry) {
                Ok(e) => e,
                Err(_) => {
                    let _ = out_tx.send(FromRelay::Error {
                        code: constants::relay_error::INVALID_ENTRY,
                    });
                    return;
                }
            };
            let mut entries = inner.entries.lock().expect("entries");
            let pc = inner.pending_cards.lock().expect("pending_cards");
            let card_ok = e
                .card
                .as_ref()
                .is_some_and(|c| pc.get(&sender).is_some_and(|s| s == c));
            drop(pc);
            if !entries.is_empty() || !card_ok {
                drop(entries);
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::CONFLICT,
                });
                return;
            }
            if chain::extend(None, std::slice::from_ref(&entry), None).is_err() {
                drop(entries);
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::INVALID_ENTRY,
                });
                return;
            }
            entries.push(entry);
            drop(entries);
            inner
                .pending_cards
                .lock()
                .expect("pending_cards")
                .remove(&sender);
            inner.broadcast(FromRelay::ChainChanged { head_seq: e.seq });
        }
        ToRelay::ChainAppend { entry } => {
            inner.log("ChainAppend");
            let Some(sender) = *id else {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            };
            let e = match ChainEntry::parse(&entry) {
                Ok(e) => e,
                Err(_) => {
                    let _ = out_tx.send(FromRelay::Error {
                        code: constants::relay_error::INVALID_ENTRY,
                    });
                    return;
                }
            };
            let cur = inner.verified().expect("append needs a chain");
            if e.seq != cur.head.seq + 1 {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::CONFLICT,
                });
                return;
            }
            let mut entries = inner.entries.lock().expect("entries");
            if chain::extend(
                Some(cur.clone()),
                std::slice::from_ref(&entry),
                Some(cur.genesis),
            )
            .is_err()
            {
                drop(entries);
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::INVALID_ENTRY,
                });
                return;
            }
            if e.op == cloud::chain::ChainOp::Add {
                let added = e.card.as_ref().and_then(|c| DeviceCard::parse(c).ok());
                let pc = inner.pending_cards.lock().expect("pending_cards");
                let allowed = added
                    .as_ref()
                    .is_some_and(|c| pc.contains_key(&c.device_id()));
                drop(pc);
                if !allowed {
                    drop(entries);
                    let _ = out_tx.send(FromRelay::Error {
                        code: constants::relay_error::NOT_PERMITTED,
                    });
                    return;
                }
                if let Some(c) = added {
                    inner
                        .pending_cards
                        .lock()
                        .expect("pending_cards")
                        .remove(&c.device_id());
                }
            }
            if e.op == cloud::chain::ChainOp::Revoke {
                if let Some(target) = e.target {
                    inner.mailboxes.lock().expect("mailboxes").remove(&target);
                }
            }
            entries.push(entry);
            drop(entries);
            inner.broadcast(FromRelay::ChainChanged { head_seq: e.seq });
        }
        ToRelay::Pipe { to, bytes } => {
            inner.log(&format!("Pipe{{{to:?}}}"));
            let Some(from) = *id else {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            };
            let chain = inner.verified().expect("pipe needs a chain");
            if to == from || !chain.active.contains_key(&to) {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            }
            if inner.send(&to, FromRelay::Pipe { from, bytes }) {
                // delivered
            } else {
                let _ = out_tx.send(FromRelay::Unreachable { to });
            }
        }
        ToRelay::Post {
            to,
            letter_id,
            sealed,
        } => {
            inner.log("Post");
            let Some(from) = *id else {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            };
            let chain = inner.verified().expect("post needs a chain");
            let to_pending = inner
                .pending_cards
                .lock()
                .expect("pending_cards")
                .contains_key(&to);
            let allowed = chain.active.contains_key(&to) || to_pending;
            if !allowed {
                let _ = out_tx.send(FromRelay::Error {
                    code: constants::relay_error::NOT_PERMITTED,
                });
                return;
            }
            if inner.send(
                &to,
                FromRelay::Letter {
                    from,
                    letter_id,
                    sealed: sealed.clone(),
                },
            ) {
                // delivered
            } else {
                inner
                    .mailboxes
                    .lock()
                    .expect("mailboxes")
                    .entry(to)
                    .or_default()
                    .push((from, letter_id, sealed));
            }
        }
        ToRelay::Ack { letter_id } => {
            inner.log("Ack");
            let mut mbox = inner.mailboxes.lock().expect("mailboxes");
            for items in mbox.values_mut() {
                items.retain(|(_, lid, _)| *lid != letter_id);
            }
        }
    }
}

fn entry_seq(e: &[u8]) -> u64 {
    ChainEntry::parse(e).map(|c| c.seq).unwrap_or(u64::MAX)
}

fn register_socket(
    inner: &Arc<Inner>,
    id: DeviceId,
    tx: mpsc::UnboundedSender<cloud::codec::FromRelay>,
) {
    inner.sockets.lock().expect("sockets").insert(id, tx);
}

async fn bad_hello(
    ws: &mut WebSocket,
    out_tx: &mpsc::UnboundedSender<cloud::codec::FromRelay>,
    stop: &AtomicBool,
) {
    let _ = out_tx.send(cloud::codec::FromRelay::Error {
        code: constants::relay_error::BAD_HELLO,
    });
    let _ = ws
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: 1008,
            reason: "bad hello".into(),
        })))
        .await;
    stop.store(true, Ordering::SeqCst);
}
