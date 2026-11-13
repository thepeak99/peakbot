//! PKCE login (p3-design §G.1): random verifier → S256 challenge, loopback
//! catch on `127.0.0.1:0` (single request, 5 min timeout, `state` checked),
//! `POST /token`, `store_token`, actor reconnect.
//!
//! Login runs in the CALLER's task (the actor never blocks on a browser).
//! A second `login` while one is running cancels the first (one listener at
//! a time, held in the handle's `Mutex<Option<AbortHandle>>`).

use rand_core::CryptoRng;
use url::Url;

use super::link::CloudError;
use super::store::SessionToken;

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

/// Build the login URL `{base}/login?code_challenge&redirect_uri&state`
/// with a fresh S256 PKCE pair and a random 32-byte b64url `state`.
pub fn plan_login(base: &Url, rng: &mut impl CryptoRng) -> Result<LoginPlan, CloudError> {
    todo!("p3 PR-B: plan_login (PKCE S256 + loopback redirect_uri)")
}

/// Bind `127.0.0.1:0`, open the browser with `plan.auth_url`, accept ONE
/// request on `/cb` within 5 min (`state` checked; `error=…` fails;
/// `invalid_grant` / network / timeout → `Err`), `POST {base}/token
/// {code, code_verifier}`, then `store_token` and tell the actor to
/// reconnect. Returns after the URL is opened — the listener continues in
/// the background; the result is delivered through `out`.
pub async fn run_login(
    base: &Url,
    plan: &LoginPlan,
    open_browser: impl FnOnce(Url),
    out: tokio::sync::oneshot::Sender<Result<(), CloudError>>,
) {
    todo!("p3 PR-B: run_login (loopback catch + /token redeem)")
}

/// Redeem `code` against `{base}/token` with `plan.verifier` (the fake
/// relay asserts the S256 relationship server-side).
pub async fn redeem_token(
    base: &Url,
    plan: &LoginPlan,
    code: &str,
) -> Result<SessionToken, CloudError> {
    todo!("p3 PR-B: redeem_token")
}

/// `POST {base}/logout` with Bearer, best effort, 5 s (the relay endpoint
/// is idempotent). A failure must NOT stop the local wipe (p3-design §G.6).
pub async fn logout_post(base: &Url, token: &SessionToken) -> Result<(), CloudError> {
    todo!("p3 PR-B: logout_post")
}
