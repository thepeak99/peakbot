//! PKCE login (p3-design §G.1): plan (S256 verifier/challenge + random
//! state) → open the browser → ONE `127.0.0.1:0` loopback `/cb` within
//! 5 min → `POST /token` → store the token → the actor reconnects.
//! Login runs in the caller's task: the handle returns as soon as the
//! URL is opened, the listener continues in the background, so the
//! actor never blocks on a browser.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use rand_core::CryptoRng;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::{Url, form_urlencoded};

use super::link::{CloudCmd, CloudError};
use super::store::SeedStore;

/// How long a pending login (loopback or deep link) waits for its callback.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// A prepared login: the auth URL to open, and the secrets the loopback
/// listener needs to redeem the code. `verifier` is exposed (not secret
/// once the challenge is derived from it in the S256 flow) so tests can
/// pin the PKCE relationship.
pub struct LoginPlan {
    pub auth_url: Url,
    pub redirect_uri: Url,
    pub challenge: String,
    pub state: String,
    pub verifier: String,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the login URL `{base}/login?code_challenge&redirect_uri&state`
/// with a fresh S256 PKCE pair and a random 32-byte b64url `state`.
pub fn plan_login(
    base: &Url,
    redirect_uri: &Url,
    rng: &mut impl CryptoRng,
) -> Result<LoginPlan, CloudError> {
    let mut vb = [0u8; 32];
    rng.fill_bytes(&mut vb);
    let verifier = b64url(&vb);
    let challenge = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
    let mut sb = [0u8; 32];
    rng.fill_bytes(&mut sb);
    let state = b64url(&sb);
    let mut url = base
        .join("login")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("redirect_uri", redirect_uri.as_str())
        .append_pair("state", &state);
    Ok(LoginPlan {
        auth_url: url,
        redirect_uri: redirect_uri.clone(),
        challenge,
        state,
        verifier,
    })
}

/// The background loopback listener: accept ONE `/cb` (state checked;
/// `error=…` fails; stray requests keep listening), redeem the code,
/// `store_token`, tell the actor to reconnect. Records the failure in
/// `last_login_error` (G.1 step 5).
pub async fn listener(
    relay: Arc<super::relay_http::RelayHttp>,
    tcp: TcpListener,
    plan: LoginPlan,
    cmd: tokio::sync::mpsc::Sender<CloudCmd>,
    store: Arc<dyn SeedStore>,
    last_login_error: Arc<std::sync::Mutex<Option<String>>>,
) {
    let result =
        tokio::time::timeout(LOGIN_TIMEOUT, accept_one(relay, tcp, &plan, &cmd, &store)).await;
    match result {
        Ok(Ok(())) => {
            // A fresh successful login clears the previous failure.
            if let Ok(mut slot) = last_login_error.lock() {
                *slot = None;
            }
        }
        Ok(Err(e)) => {
            if let Ok(mut slot) = last_login_error.lock() {
                *slot = Some(e.to_string());
            }
        }
        Err(_) => {
            if let Ok(mut slot) = last_login_error.lock() {
                *slot = Some("login timed out".into());
            }
        }
    }
}

async fn accept_one(
    relay: Arc<super::relay_http::RelayHttp>,
    tcp: TcpListener,
    plan: &LoginPlan,
    cmd: &tokio::sync::mpsc::Sender<CloudCmd>,
    store: &Arc<dyn SeedStore>,
) -> Result<(), CloudError> {
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            return Err(CloudError::Io("login listener closed".into()));
        };
        let Some((path, query, mut stream)) = read_request_head(stream).await else {
            continue;
        };
        if path != "/cb" {
            respond(&mut stream, 404, "Not Found", "no such path").await;
            continue;
        }
        let params: std::collections::HashMap<String, String> =
            form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
        if params.get("state") != Some(&plan.state) {
            // A stray request must not kill the flow (G.1 step 3).
            respond(&mut stream, 400, "Bad Request", "state mismatch").await;
            continue;
        }
        if let Some(err) = params.get("error") {
            return Err(CloudError::Io(format!("idp error: {err}")));
        }
        let Some(code) = params.get("code") else {
            respond(&mut stream, 400, "Bad Request", "missing code").await;
            continue;
        };
        finish(&relay, plan, code, cmd, store).await?;
        respond(&mut stream, 200, "OK", "You can close this tab.").await;
        return Ok(());
    }
}

/// Read the HTTP request head (up to `\r\n\r\n`, 8 KiB cap); returns the
/// target's path and query.
async fn read_request_head(stream: TcpStream) -> Option<(String, String, TcpStream)> {
    let mut buf = [0u8; 8192];
    let mut stream = stream;
    let n = stream.read(&mut buf).await.ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let end = head.find("\r\n\r\n")?;
    let lines: Vec<&str> = head[..end].lines().collect();
    let first = lines.first()?;
    let parts: Vec<&str> = first.split_whitespace().collect();
    let target = parts.get(1)?;
    let (path, query) = match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i + 1..].to_string()),
        None => (target.to_string(), String::new()),
    };
    Some((path, query, stream))
}

async fn respond(tcp: &mut TcpStream, status: u16, text: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        text,
        body.len(),
        body
    );
    let _ = tcp.write_all(msg.as_bytes()).await;
}

/// The one redemption path both callback sources share: redeem `code`,
/// persist the token, tell the actor to reconnect.
pub async fn finish(
    relay: &super::relay_http::RelayHttp,
    plan: &LoginPlan,
    code: &str,
    cmd: &tokio::sync::mpsc::Sender<CloudCmd>,
    store: &Arc<dyn SeedStore>,
) -> Result<(), CloudError> {
    let token = super::relay_http::redeem_token(relay, plan, code).await?;
    store
        .store_token(&token)
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let _ = cmd.send(CloudCmd::TokenStored(token)).await;
    Ok(())
}

/// Constant-time string equality (length is not secret).
pub fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Parse an inbound deep link against the app redirect (scheme + path must
/// match, no host) → its query params. Errors never echo the URL (it
/// carries the code).
pub fn parse_deep_link(
    redirect: &Url,
    raw: &str,
) -> Result<std::collections::HashMap<String, String>, CloudError> {
    let url =
        Url::parse(raw.trim()).map_err(|_| CloudError::Io("malformed callback link".into()))?;
    if url.scheme() != redirect.scheme() || url.path() != redirect.path() || url.has_host() {
        return Err(CloudError::Io("callback link is not for this app".into()));
    }
    Ok(url.query_pairs().into_owned().collect())
}

/// P5 C1: where the PKCE callback comes from. SKELETON — `spawn_with_callback`
/// does not honour `DeepLink` yet.
#[derive(Debug, Clone)]
pub enum CallbackSource {
    /// Desktop: a one-shot `127.0.0.1:0` listener.
    Loopback,
    /// Mobile: the OS delivers `sh.shifu.mobile:/cb?code&state`.
    DeepLink(AppRedirect),
}

/// Parsed once at boot: scheme `sh.shifu.mobile`, path `/cb`, no host/fragment.
#[derive(Debug, Clone)]
pub struct AppRedirect(Url);

impl AppRedirect {
    /// The mobile app's redirect `sh.shifu.mobile:/cb` (design `AppRedirect::MOBILE`;
    /// a fn because `Url` is not const-constructible).
    pub fn mobile() -> Self {
        Self(Url::parse("sh.shifu.mobile:/cb").expect("static redirect"))
    }

    pub fn url(&self) -> &Url {
        &self.0
    }
}
