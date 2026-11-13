//! The HTTP boundary: `/login`, `/oidc/callback`, `/token`, `/logout`, `/ws`
//! (design §5.1). Holds the in-memory `Logins` (OIDC flows + one-time codes).
//!
//! Logs carry ids and sizes only: never tokens, codes, states or bodies (§4.9).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64U;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::config::RedirectUri;
use crate::hub::{Hub, PermitDenied};
use crate::limits::{CODE_TTL, FLOW_TTL, MAX_LOGINS_IN_FLIGHT, SESSION_TTL};
use crate::oidc::{self, Oidc};
use crate::store::{AccountId, Db};
use crate::{ct_eq, now_secs, random_32};

/// The device half of a login (design §3.3).
struct DeviceAuth {
    /// Decoded S256 challenge.
    challenge: [u8; 32],
    redirect: RedirectUri,
    /// The device's `state`, echoed back.
    state: String,
}

struct PendingFlow {
    flow: oidc::Flow,
    device: DeviceAuth,
    created: u64,
}

struct Grant {
    account: AccountId,
    challenge: [u8; 32],
    created: u64,
}

#[derive(Default)]
struct LoginsInner {
    /// Key = relay→IdP `state`, TTL `FLOW_TTL`.
    flows: HashMap<String, PendingFlow>,
    /// Key = sha256(one-time code), TTL `CODE_TTL`, single use.
    codes: HashMap<[u8; 32], Grant>,
}

/// In-flight `/login` flows and one-time codes (design §3.3). Both maps are
/// capped at `limits::MAX_LOGINS_IN_FLIGHT`. Clones share state.
#[derive(Clone, Default)]
pub struct Logins(Arc<Mutex<LoginsInner>>);

/// A login map is at its cap (→ 503).
struct Full;

impl Logins {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, LoginsInner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn insert_flow(&self, state: String, f: PendingFlow, now: u64) -> Result<(), Full> {
        let mut g = self.lock();
        if g.flows.len() >= MAX_LOGINS_IN_FLIGHT {
            g.flows.retain(|_, v| now < v.created + FLOW_TTL);
        }
        if g.flows.len() >= MAX_LOGINS_IN_FLIGHT {
            return Err(Full);
        }
        g.flows.insert(state, f);
        Ok(())
    }

    /// Remove-and-return: a state is single use. Expired reads as unknown.
    fn take_flow(&self, state: &str, now: u64) -> Option<PendingFlow> {
        let f = self.lock().flows.remove(state)?;
        (now < f.created + FLOW_TTL).then_some(f)
    }

    fn insert_code(&self, hash: [u8; 32], g: Grant, now: u64) -> Result<(), Full> {
        let mut inner = self.lock();
        if inner.codes.len() >= MAX_LOGINS_IN_FLIGHT {
            inner.codes.retain(|_, v| now < v.created + CODE_TTL);
        }
        if inner.codes.len() >= MAX_LOGINS_IN_FLIGHT {
            return Err(Full);
        }
        inner.codes.insert(hash, g);
        Ok(())
    }

    /// Removed before any check, so a code is single use even on failure.
    fn take_code(&self, hash: &[u8; 32]) -> Option<Grant> {
        self.lock().codes.remove(hash)
    }

    /// Drop expired entries (the sweeper, design §4.7).
    pub fn purge(&self, now: u64) {
        let mut g = self.lock();
        g.flows.retain(|_, v| now < v.created + FLOW_TTL);
        g.codes.retain(|_, v| now < v.created + CODE_TTL);
    }
}

/// Everything the handlers share.
#[derive(Clone)]
pub struct AppState {
    pub oidc: Arc<Oidc>,
    pub origin: String,
    pub app_scheme: Option<String>,
    pub db: Db,
    pub hub: Hub,
    pub logins: Logins,
    pub shutdown: watch::Receiver<bool>,
}

/// Build the axum router (design §5.1). All bodies JSON unless stated.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", get(login))
        .route("/oidc/callback", get(callback))
        .route("/token", post(token))
        .route("/logout", post(logout))
        .route("/ws", get(ws))
        .with_state(state)
}

fn sha256(b: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(Sha256::digest(b).as_slice());
    out
}

fn text(status: StatusCode, msg: &'static str) -> Response {
    (status, msg).into_response()
}

fn redirect(to: &url::Url) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.as_str())]).into_response()
}

/// `redirect_uri` + the relay's own params; a device query is preserved.
fn with_params(base: &RedirectUri, params: &[(&str, &str)]) -> url::Url {
    let (RedirectUri::Loopback(u) | RedirectUri::App(u)) = base;
    let mut u = u.clone();
    u.query_pairs_mut().extend_pairs(params);
    u
}

/// `GET /login` (design §4.1 step 1). Every parameter is parsed once; any
/// failure is a 400 with no redirect.
async fn login(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let bad = || text(StatusCode::BAD_REQUEST, "bad login request");
    let Some(challenge) = q.get("code_challenge").and_then(|c| decode_challenge(c)) else {
        return bad();
    };
    let Some(dev_state) = q.get("state").filter(|s| valid_state(s)) else {
        return bad();
    };
    let Some(redirect_raw) = q.get("redirect_uri") else {
        return bad();
    };
    let Ok(redirect_uri) = RedirectUri::parse(redirect_raw, st.app_scheme.as_deref()) else {
        return bad();
    };
    let names = st.oidc.issuer_names();
    let idx = match q.get("issuer") {
        Some(name) => match names.iter().position(|n| n == name) {
            Some(i) => i,
            None => return bad(),
        },
        None if names.len() == 1 => 0,
        None => return chooser(&q, &names),
    };
    let (idp_url, flow) = match st.oidc.authorize(idx).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(reason = ?e, "login: discovery failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "identity provider error");
        }
    };
    let pending = PendingFlow {
        flow: flow.clone(),
        device: DeviceAuth {
            challenge,
            redirect: redirect_uri,
            state: dev_state.clone(),
        },
        created: now_secs(),
    };
    if st
        .logins
        .insert_flow(flow.state, pending, now_secs())
        .is_err()
    {
        return text(StatusCode::SERVICE_UNAVAILABLE, "too many logins in flight");
    }
    redirect(&idp_url)
}

/// 43 b64url chars decoding to 32 bytes (S256 challenge, design §4.1).
fn decode_challenge(s: &str) -> Option<[u8; 32]> {
    if s.len() != 43 {
        return None;
    }
    B64U.decode(s).ok()?.try_into().ok()
}

/// 1..=128 visible ASCII (design §4.1).
fn valid_state(s: &str) -> bool {
    (1..=128).contains(&s.len()) && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// Minimal static HTML list of links; names are `[a-z0-9-]` (config-validated)
/// and the other params are re-encoded, so nothing needs HTML escaping.
fn chooser(q: &HashMap<String, String>, names: &[String]) -> Response {
    let mut body = String::from("<!doctype html><title>Sign in</title><ul>");
    for n in names {
        let mut qs = url::form_urlencoded::Serializer::new(String::new());
        for k in ["code_challenge", "redirect_uri", "state"] {
            if let Some(v) = q.get(k) {
                qs.append_pair(k, v);
            }
        }
        qs.append_pair("issuer", n);
        let href = qs.finish().replace('&', "&amp;");
        body.push_str(&format!("<li><a href=\"/login?{href}\">{n}</a></li>"));
    }
    body.push_str("</ul>");
    Html(body).into_response()
}

/// `GET /oidc/callback` (design §4.1 step 2).
async fn callback(
    State(st): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let now = now_secs();
    let Some(pending) = q.get("state").and_then(|s| st.logins.take_flow(s, now)) else {
        return text(
            StatusCode::BAD_REQUEST,
            "Login expired, start again from the app",
        );
    };
    let dev = &pending.device;
    let denied = || {
        redirect(&with_params(
            &dev.redirect,
            &[("error", "access_denied"), ("state", &dev.state)],
        ))
    };
    let Some(code) = q.get("code").filter(|_| !q.contains_key("error")) else {
        return denied();
    };
    let identity = match st.oidc.finish(&pending.flow, code).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(reason = ?e, "login: IdP verification failed");
            return denied();
        }
    };
    let (iss, sub) = (identity.issuer, identity.subject);
    let account = match st.db.run(move |s| s.upsert_account(&iss, &sub)).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "login: account upsert failed");
            return denied();
        }
    };
    let Ok(raw) = random_32() else {
        return denied();
    };
    let code = B64U.encode(raw);
    let grant = Grant {
        account,
        challenge: dev.challenge,
        created: now,
    };
    if st
        .logins
        .insert_code(sha256(code.as_bytes()), grant, now)
        .is_err()
    {
        return text(StatusCode::SERVICE_UNAVAILABLE, "too many logins in flight");
    }
    tracing::info!(account, "login: code issued");
    redirect(&with_params(
        &dev.redirect,
        &[("code", &code), ("state", &dev.state)],
    ))
}

#[derive(serde::Deserialize)]
struct TokenReq {
    code: String,
    code_verifier: String,
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({ "error": "invalid_grant" })),
    )
        .into_response()
}

/// RFC 7636 verifier: 43..=128 chars of `[A-Za-z0-9-._~]`.
fn valid_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `POST /token` (design §4.1 step 3). Any failure is `invalid_grant`.
async fn token(
    State(st): State<AppState>,
    body: Result<axum::Json<TokenReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(axum::Json(req)) = body else {
        return invalid_grant();
    };
    let now = now_secs();
    // Consumed before it is checked: single use even on failure.
    let Some(grant) = st.logins.take_code(&sha256(req.code.as_bytes())) else {
        return invalid_grant();
    };
    if now >= grant.created + CODE_TTL || !valid_verifier(&req.code_verifier) {
        return invalid_grant();
    }
    if !ct_eq(&sha256(req.code_verifier.as_bytes()), &grant.challenge) {
        return invalid_grant();
    }
    let Ok(raw) = random_32() else {
        return text(StatusCode::INTERNAL_SERVER_ERROR, "rng failure");
    };
    let hash = sha256(&raw);
    let account = grant.account;
    if let Err(e) = st
        .db
        .run(move |s| s.create_session(account, hash, now))
        .await
    {
        tracing::error!(error = %e, "token: session insert failed");
        return text(StatusCode::INTERNAL_SERVER_ERROR, "session error");
    }
    tracing::info!(account, "session created");
    axum::Json(serde_json::json!({
        "token": B64U.encode(raw),
        "expires_in": SESSION_TTL,
    }))
    .into_response()
}

/// `Authorization: Bearer <43-char b64url>` → `sha256` of the 32 raw bytes.
/// `None` when missing or malformed (design §5.1: 401 before any lookup).
fn bearer_hash(headers: &HeaderMap) -> Option<[u8; 32]> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let tok = v.strip_prefix("Bearer ")?;
    if tok.len() != 43 {
        return None;
    }
    let raw = B64U.decode(tok).ok()?;
    (raw.len() == 32).then(|| sha256(&raw))
}

/// `POST /logout` (design §6.1): idempotent, no oracle.
async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let Some(hash) = bearer_hash(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match st.db.run(move |s| s.revoke_session(hash)).await {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(error = %e, "logout: revoke failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    st.hub.close_session(hash, 1008);
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /ws`: 401 (token), 503 (global cap), 429 (account cap), else upgrade
/// with the frame caps (design §4.1 step 4, §6.5).
async fn ws(
    State(st): State<AppState>,
    headers: HeaderMap,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let Some(hash) = bearer_hash(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = now_secs();
    // Slides the expiry (design §6.1).
    let Some(account) = st.db.run(move |s| s.session(hash, now)).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let permit = match st.hub.permit(account) {
        Ok(p) => p,
        Err(PermitDenied::Global) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(PermitDenied::Account) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
    };
    let upgrade = match upgrade {
        Ok(u) => u,
        Err(rejection) => return rejection.into_response(),
    };
    let cap = 1 + cloud::constants::MAX_ENVELOPE_PIPE;
    let shutdown = st.shutdown.clone();
    upgrade
        .max_message_size(cap)
        .max_frame_size(cap)
        .on_upgrade(move |socket| crate::conn::run_conn(socket, permit, account, hash, shutdown))
}
