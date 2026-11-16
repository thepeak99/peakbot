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
use super::store::{SeedStore, SessionToken};

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
    base: Url,
    tcp: TcpListener,
    plan: LoginPlan,
    cmd: tokio::sync::mpsc::Sender<CloudCmd>,
    store: Arc<dyn SeedStore>,
    last_login_error: Arc<std::sync::Mutex<Option<String>>>,
) {
    let result = tokio::time::timeout(
        Duration::from_secs(300),
        accept_one(base, tcp, &plan, &cmd, &store),
    )
    .await;
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
    base: Url,
    tcp: TcpListener,
    plan: &LoginPlan,
    cmd: &tokio::sync::mpsc::Sender<CloudCmd>,
    store: &Arc<dyn SeedStore>,
) -> Result<(), CloudError> {
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            return Err(CloudError::Io("login listener closed".into()));
        };
        let Ok((path, query, mut stream)) = read_request_head(stream).await else {
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
        let token = redeem_token(&base, plan, code).await?;
        store
            .store_token(&token)
            .map_err(|e| CloudError::Io(e.to_string()))?;
        let _ = cmd.send(CloudCmd::TokenStored(token)).await;
        respond(&mut stream, 200, "OK", "You can close this tab.").await;
        return Ok(());
    }
}

/// Read the HTTP request head (up to `\r\n\r\n`, 8 KiB cap); returns the
/// target's path and query.
async fn read_request_head(
    mut tcp: TcpStream,
) -> Result<(String, String, TcpStream), std::io::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    while buf.len() < 8192 && !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("connection closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let mut parts = first.split_whitespace();
    parts.next(); // method
    let target = parts.next().unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok((path.to_string(), query.to_string(), tcp))
}

async fn respond(tcp: &mut TcpStream, status: u16, text: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} {text}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = tcp.write_all(head.as_bytes()).await;
    let _ = tcp.write_all(body.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// Redeem `code` against `{base}/token` with `plan.verifier` (the relay
/// asserts the S256 relationship server-side).
pub async fn redeem_token(
    base: &Url,
    plan: &LoginPlan,
    code: &str,
) -> Result<SessionToken, CloudError> {
    let url = base
        .join("token")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let resp = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({ "code": code, "code_verifier": plan.verifier }))
        .send()
        .await
        .map_err(|e| CloudError::Io(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(CloudError::Io(format!(
            "token endpoint answered {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let token = body
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| CloudError::Io("no token in reply".into()))?;
    Ok(SessionToken::new(token.to_string()))
}

/// `POST {base}/logout` with Bearer (the relay endpoint is idempotent).
/// Best effort with a 5 s bound: the failure must NOT stop the local wipe
/// (p3-design §G.6), but it IS reported — a dead relay session means the
/// device stays listed, so the API answers `revoked: false`.
pub async fn logout_post(base: &Url, token: &SessionToken) -> Result<(), CloudError> {
    let url = base
        .join("logout")
        .map_err(|e| CloudError::Io(e.to_string()))?;
    let req = reqwest::Client::new()
        .post(url)
        .header("Authorization", format!("Bearer {}", token.as_str()))
        .send();
    let resp = tokio::time::timeout(Duration::from_secs(5), req)
        .await
        .map_err(|_| CloudError::Io("relay /logout timed out".into()))?
        .map_err(|e| CloudError::Io(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(CloudError::Io(format!(
            "relay /logout answered {}",
            resp.status()
        )));
    }
    Ok(())
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
