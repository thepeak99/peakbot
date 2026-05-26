//! MCP OAuth 2.1 + PKCE + Dynamic Client Registration (Slice 2 of #19).
//!
//! Wires [`rmcp`]'s `AuthorizationManager` + `AuthClient` into
//! [`connect_mcp_http`](super::connect_mcp_http) for streamable-http MCP
//! servers that require the OAuth dance (primary target:
//! `https://mcp.linear.app/mcp`).
//!
//! ## Flow
//!
//! 1. Build a `FilesystemCredentialStore` at
//!    `dirs::cache_dir()/peakbot/mcp-auth/<server-name>.json`.
//! 2. Attach it to a fresh `AuthorizationManager`; call
//!    `initialize_from_store`. **Cache hit → return immediately.** rmcp's
//!    `get_access_token` will silently refresh expired access tokens using
//!    the stored refresh token.
//! 3. Cache miss → discover OAuth metadata (RFC 8414), bind a oneshot
//!    `axum` listener on `127.0.0.1:0`, then DCR with the *real* port in
//!    the `redirect_uri` (OAuth servers validate this exactly), build the
//!    PKCE auth URL, open the browser, await the callback, exchange the
//!    code for tokens. Persistence is automatic — rmcp's
//!    `exchange_code_for_token` calls `credential_store.save()` itself.
//!
//! ## Cache file
//!
//! - **Path:** `dirs::cache_dir()/peakbot/mcp-auth/<server-name>.json`.
//!   No silent fallback — if `dirs::cache_dir()` returns `None` we error.
//! - **Permissions:** `0o600` on Unix (set on save, enforced on load —
//!   any group/world bits set rejects with a remediation message).
//!
//! ## Headless / SSH behaviour
//!
//! When `$SSH_CONNECTION` is set, the browser would open on the wrong
//! machine, so we print the URL with framing instead of calling
//! `open::that()`. The listener still binds on the local loopback — the
//! user can SSH-tunnel (`ssh -L <port>:127.0.0.1:<port>`) to receive the
//! callback, or just authorise from the same machine. Decision history
//! in `autho.md` under "Slice 1.5 deviation locked".

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{Router, extract::Query, response::Html, routing::get};
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use tokio::sync::oneshot;
use tracing::{info, warn};

/// One-shot callback payload received from the browser redirect.
///
/// `axum`'s `Query<T>` extractor URL-decodes percent-escapes automatically,
/// which matters for real-world servers — Linear emits codes shaped like
/// `<uuid>%3A<short>%3A<long>` (the `:` separators come back encoded).
#[derive(Debug, serde::Deserialize)]
pub struct AuthCallback {
    pub code: String,
    pub state: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// FilesystemCredentialStore
// ─────────────────────────────────────────────────────────────────────────────

/// Filesystem-backed credential store for OAuth tokens.
///
/// Persists [`StoredCredentials`] as JSON to
/// `dirs::cache_dir()/peakbot/mcp-auth/<server-name>.json` with mode
/// `0o600` on Unix.
#[derive(Debug)]
pub struct FilesystemCredentialStore {
    path: PathBuf,
}

impl FilesystemCredentialStore {
    /// Build a store rooted at `dirs::cache_dir()` for the given server name.
    ///
    /// Returns `None` if the platform doesn't expose a cache directory
    /// (rare — basically only odd embedded targets). The caller surfaces
    /// this as [`AuthorizationError::NoCacheDir`].
    pub fn for_server(server_name: &str) -> Option<Self> {
        let cache_dir = dirs::cache_dir()?.join("peakbot").join("mcp-auth");
        Some(Self {
            path: cache_dir.join(format!("{server_name}.json")),
        })
    }

    /// Construct a store at an explicit path. Used by tests.
    #[cfg(test)]
    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Borrow the on-disk path (for tests + diagnostics).
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[async_trait]
impl CredentialStore for FilesystemCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        if !self.path.exists() {
            return Ok(None);
        }

        // Reject world/group-readable caches — refresh tokens are
        // long-lived bearer-equivalents. 0o600 only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&self.path)
                .map_err(|e| {
                    AuthError::InternalError(format!(
                        "Cannot stat credential cache {}: {e}",
                        self.path.display()
                    ))
                })?
                .permissions()
                .mode()
                & 0o777;
            if mode & 0o077 != 0 {
                return Err(AuthError::InternalError(format!(
                    "Credential cache {} has mode {:o} but must be 0600. \
                     Refusing to load a world-readable token cache. \
                     Delete the file or secure it with: chmod 600 {}",
                    self.path.display(),
                    mode,
                    self.path.display(),
                )));
            }
        }

        let bytes = tokio::fs::read(&self.path).await.map_err(|e| {
            AuthError::InternalError(format!(
                "Cannot read credential cache {}: {e}",
                self.path.display()
            ))
        })?;
        let creds: StoredCredentials = serde_json::from_slice(&bytes).map_err(|e| {
            AuthError::InternalError(format!(
                "Cannot parse credential cache {}: {e}",
                self.path.display()
            ))
        })?;
        Ok(Some(creds))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AuthError::InternalError(format!(
                    "Cannot create cache directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let json = serde_json::to_vec_pretty(&credentials)
            .map_err(|e| AuthError::InternalError(format!("Cannot serialize credentials: {e}")))?;

        tokio::fs::write(&self.path, &json).await.map_err(|e| {
            AuthError::InternalError(format!(
                "Cannot write credential cache {}: {e}",
                self.path.display()
            ))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            tokio::fs::set_permissions(&self.path, perms)
                .await
                .map_err(|e| {
                    AuthError::InternalError(format!(
                        "Cannot set permissions on {}: {e}",
                        self.path.display()
                    ))
                })?;
        }

        info!(
            path = %self.path.display(),
            client_id = %credentials.client_id,
            "OAuth credentials cached to disk",
        );

        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        if self.path.exists() {
            tokio::fs::remove_file(&self.path).await.map_err(|e| {
                AuthError::InternalError(format!(
                    "Cannot delete credential cache {}: {e}",
                    self.path.display()
                ))
            })?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Callback router
// ─────────────────────────────────────────────────────────────────────────────

/// Build the axum router that catches the OAuth redirect.
///
/// `expected_state` is compared against the `?state=` query parameter as a
/// defence-in-depth CSRF check on top of rmcp's own state validation in
/// `exchange_code_for_token`. A mismatch returns 400 with a friendly page
/// rather than feeding the wrong code into the exchange step.
///
/// The sender is wrapped in an `Arc<Mutex<Option<…>>>` because axum
/// handlers must be `Clone`, but `oneshot::Sender` is move-only and
/// consumed by `send`. The mutex lets exactly one request consume it.
fn callback_router(
    tx: Arc<std::sync::Mutex<Option<oneshot::Sender<AuthCallback>>>>,
    expected_state: String,
) -> Router {
    Router::new().route(
        "/callback",
        get(move |Query(params): Query<AuthCallback>| {
            let tx = tx.clone();
            let expected_state = expected_state.clone();
            async move {
                if params.state != expected_state {
                    return Err((
                        axum::http::StatusCode::BAD_REQUEST,
                        "CSRF state mismatch — authorisation rejected. Please try again.",
                    ));
                }
                if let Some(sender) = tx.lock().unwrap().take() {
                    let _ = sender.send(params);
                }
                Ok(Html(
                    "<html><body style=\"font-family:system-ui;padding:2rem;\">\
                         <h1>Authorisation complete</h1>\
                         <p>PeakBot has received the callback. You may close this tab.</p>\
                         </body></html>",
                ))
            }
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Browser launcher
// ─────────────────────────────────────────────────────────────────────────────

/// Open the URL in the default browser, or print it to stderr when:
///
/// - `$SSH_CONNECTION` is set (browser would land on the wrong machine), OR
/// - `open::that()` fails (no desktop available, missing xdg-open, etc.).
///
/// In all cases the listener keeps waiting — the user can finish the
/// authorisation by hand (open the URL on their workstation, set up an
/// SSH port-forward, etc.).
fn open_or_print_url(url: &str, server_name: &str) {
    let is_ssh = std::env::var_os("SSH_CONNECTION").is_some();

    if is_ssh {
        eprintln!(
            "\nMCP server '{server_name}': you appear to be on SSH.\n\
             The OAuth callback listener is on this host's loopback, so a \
             browser opened automatically would land on the wrong machine.\n\
             \n\
             Open this URL to authorise (and set up `ssh -L <port>:127.0.0.1:<port>` \
             if you're authorising from elsewhere):\n\
             \n\
             → {url}\n"
        );
        return;
    }

    match open::that(url) {
        Ok(()) => info!("Opened OAuth authorisation URL in browser"),
        Err(e) => {
            warn!(error = %e, "Could not open browser automatically");
            eprintln!(
                "\nMCP server '{server_name}': could not open browser automatically.\n\
                 Open this URL manually to authorise:\n\
                 \n\
                 → {url}\n"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// The HTTP client returned by [`authorize`], plumbed into
/// `StreamableHttpClientTransport::with_client`. `AuthClient<C>` implements
/// `StreamableHttpClient` whenever `C` does, so a `reqwest::Client` inner
/// satisfies the transport's bound.
pub type AuthorizedClient = AuthClient<reqwest::Client>;

/// Per-server OAuth parameters resolved from config and handed to
/// [`authorize`]. Three knobs control which branch the flow takes:
///
/// * `client_id` **absent** → Dynamic Client Registration (RFC 7591).
///   The server must advertise a `registration_endpoint` in its OAuth
///   metadata. Used by Linear-shaped MCP servers.
/// * `client_id` **present** → static-credentials path. We skip DCR
///   and call rmcp's [`AuthorizationManager::configure_client`] with
///   the user-supplied id (and optional `client_secret` for
///   confidential clients). Used by Google Workspace MCP servers.
///
/// `scopes` is passed verbatim into both `register_client` and
/// `get_authorization_url` so the consent screen requests the exact
/// access the user configured. Empty `scopes` is valid for DCR-style
/// servers that infer scope from the resource (Linear's behaviour) but
/// will produce a useless token for scope-strict servers like Google.
#[derive(Debug, Clone, Default)]
pub struct OauthParams {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

/// Run the full OAuth 2.1 + PKCE authorisation flow for an MCP server.
///
/// Returns a ready-to-use HTTP client that injects `Authorization: Bearer
/// <token>` on every request and silently refreshes when the access token
/// nears expiry.
///
/// See [`OauthParams`] for the DCR-vs-static-credentials branching.
pub async fn authorize(
    server_name: &str,
    mcp_url: &str,
    params: OauthParams,
) -> Result<AuthorizedClient, AuthorizationError> {
    let store =
        FilesystemCredentialStore::for_server(server_name).ok_or(AuthorizationError::NoCacheDir)?;
    info!(
        server = %server_name,
        cache = %store.path().display(),
        "OAuth: starting authorisation flow",
    );

    let mut mgr = AuthorizationManager::new(mcp_url).await?;
    mgr.set_credential_store(store);

    // ── Cache hit fast-path ────────────────────────────────────────────────
    if mgr.initialize_from_store().await? {
        info!(server = %server_name, "OAuth: cached token loaded, skipping browser");
        return Ok(AuthClient::new(reqwest::Client::new(), mgr));
    }

    // ── Discover metadata ──────────────────────────────────────────────────
    info!(server = %server_name, "OAuth: discovering authorisation metadata");
    let metadata = mgr.discover_metadata().await?;
    mgr.set_metadata(metadata);

    // ── Bind callback listener (port FIRST, then DCR with the real URI) ────
    //
    // OAuth servers validate `redirect_uri` byte-for-byte against the
    // value used during DCR / client configuration, so we must know the
    // ephemeral port BEFORE we call `register_client` /
    // `configure_client`. The autho.md draft had these reversed and
    // tried to string-replace the port after the fact — rejected.
    //
    // For the static-credentials path, the user is expected to have
    // pre-registered a loopback redirect URI at the OAuth provider's
    // console. Google's Desktop-app client type explicitly allows
    // `http://127.0.0.1` with *any* port (RFC 8252 §7.3), which is why
    // the ephemeral-port pattern works without coordination.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| AuthorizationError::Internal(format!("cannot bind localhost: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AuthorizationError::Internal(format!("cannot read local_addr: {e}")))?;
    let redirect_uri = format!("http://{local_addr}/callback");
    info!(
        server = %server_name,
        addr = %local_addr,
        "OAuth: callback listener bound on loopback",
    );

    // ── Configure the OAuth client (static creds OR dynamic registration) ──
    //
    // `scopes` is owned `Vec<String>`; rmcp's APIs want `&[&str]`, so we
    // borrow once here and reuse the slice in both `register_client`
    // and `get_authorization_url`. Doing this twice is a footgun:
    // mismatched scopes between the two calls produced "invalid_scope"
    // errors in earlier iterations.
    let scope_refs: Vec<&str> = params.scopes.iter().map(|s| s.as_str()).collect();
    if let Some(client_id) = params.client_id.as_deref() {
        info!(
            server = %server_name,
            client_id = %client_id,
            scopes = ?params.scopes,
            "OAuth: configuring static client credentials (skipping DCR)",
        );
        let mut cfg =
            OAuthClientConfig::new(client_id, &redirect_uri).with_scopes(params.scopes.clone());
        if let Some(secret) = params.client_secret.as_deref() {
            cfg = cfg.with_client_secret(secret);
        }
        mgr.configure_client(cfg)?;
    } else {
        info!(
            server = %server_name,
            scopes = ?params.scopes,
            "OAuth: registering dynamic client",
        );
        mgr.register_client("peakbot", &redirect_uri, &scope_refs)
            .await?;
    }

    // ── Build PKCE authorisation URL ───────────────────────────────────────
    let auth_url = mgr.get_authorization_url(&scope_refs).await?;

    // Extract the CSRF state from the URL for the axum-side guard.
    // rmcp generates the URL with `state=<csrf_token>`; we don't have a
    // direct accessor for the verifier so we parse what we just built.
    let expected_state = extract_state_param(&auth_url).ok_or_else(|| {
        AuthorizationError::Internal(
            "OAuth: could not extract `state` from authorisation URL".into(),
        )
    })?;

    // ── Spin up the one-shot router + serve task ───────────────────────────
    let (tx, rx) = oneshot::channel::<AuthCallback>();
    let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
    let router = callback_router(tx_slot, expected_state);

    let server_task = tokio::spawn(async move {
        // Errors here mean the listener was killed before the callback
        // landed; the outer `rx.await` will surface that as
        // `BrowserTimeout`.
        let _ = axum::serve(listener, router).await;
    });

    // ── Open browser (or print URL on SSH / failure) ───────────────────────
    open_or_print_url(&auth_url, server_name);

    // ── Wait for the callback ──────────────────────────────────────────────
    let callback = rx.await.map_err(|_| {
        AuthorizationError::BrowserTimeout(format!(
            "OAuth authorisation for '{server_name}' was cancelled before the \
             browser callback arrived. Please retry."
        ))
    })?;
    server_task.abort();

    // ── Exchange code → token (auto-persists via the credential store) ─────
    info!(server = %server_name, "OAuth: exchanging authorisation code for token");
    mgr.exchange_code_for_token(&callback.code, &callback.state)
        .await?;

    info!(server = %server_name, "OAuth: authorisation complete, token cached");
    Ok(AuthClient::new(reqwest::Client::new(), mgr))
}

/// Pull `?state=…` out of a URL without dragging in the `url` crate
/// directly (it's already a transitive dep via rmcp `auth`, but we don't
/// need its full Url type for one query-param lookup). Returns the
/// percent-decoded value.
fn extract_state_param(url: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or(url);
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == "state"
        {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal `%XX` percent-decoder. Sufficient for OAuth `state` (URL-safe
/// base64 or hex in practice) and `code` values. axum's `Query<T>`
/// decodes automatically — this helper is only for our own URL parse
/// path above.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' {
            ' '
        } else {
            bytes[i] as char
        });
        i += 1;
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors surfaced during the OAuth authorisation flow.
///
/// Distinct from [`rmcp::transport::auth::AuthError`] which covers
/// token-level failures — this type adds the few setup conditions that
/// matter to the user (no cache dir, browser/listener problems).
#[derive(Debug, thiserror::Error)]
pub enum AuthorizationError {
    /// Underlying rmcp auth error: DCR failed, token exchange failed,
    /// metadata discovery failed, etc.
    #[error("OAuth: {0}")]
    Rmcp(#[from] AuthError),

    /// Platform doesn't expose a cache directory (`dirs::cache_dir()`
    /// returned `None`). Should be extremely rare on real desktops.
    #[error(
        "OAuth: cannot locate a cache directory on this platform. \
         Set $XDG_CACHE_HOME (Linux) or %LOCALAPPDATA% (Windows) to a writable path."
    )]
    NoCacheDir,

    /// The browser/listener round-trip never completed.
    #[error("{0}")]
    BrowserTimeout(String),

    /// Internal precondition failed (couldn't bind localhost, malformed
    /// auth URL, etc.).
    #[error("OAuth internal error: {0}")]
    Internal(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A `StoredCredentials` with no token body — enough to exercise the
    /// save/load round-trip without depending on the (`#[non_exhaustive]`)
    /// `OAuthTokenResponse` constructor surface.
    fn dummy_credentials() -> StoredCredentials {
        StoredCredentials::new(
            "test-client-id".to_string(),
            None, // token_response — fine for cache I/O tests
            vec!["scope-a".to_string()],
            Some(1_700_000_000),
        )
    }

    #[tokio::test]
    async fn filesystem_cred_store_save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::at_path(tmp.path().join("server.json"));

        // Empty before save.
        assert!(store.load().await.unwrap().is_none());

        store.save(dummy_credentials()).await.unwrap();

        let loaded = store
            .load()
            .await
            .unwrap()
            .expect("creds present after save");
        assert_eq!(loaded.client_id, "test-client-id");
        assert_eq!(loaded.granted_scopes, vec!["scope-a".to_string()]);
        assert_eq!(loaded.token_received_at, Some(1_700_000_000));
    }

    #[tokio::test]
    async fn filesystem_cred_store_clear_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::at_path(tmp.path().join("c.json"));
        store.save(dummy_credentials()).await.unwrap();
        assert!(store.path().exists());

        store.clear().await.unwrap();
        assert!(!store.path().exists());

        // Idempotent.
        store.clear().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_cred_store_writes_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store = FilesystemCredentialStore::at_path(tmp.path().join("perms.json"));
        store.save(dummy_credentials()).await.unwrap();

        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filesystem_cred_store_rejects_world_readable_cache() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("loose.json");
        let store = FilesystemCredentialStore::at_path(path.clone());
        store.save(dummy_credentials()).await.unwrap();

        // Loosen perms behind the store's back.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = store
            .load()
            .await
            .expect_err("load must reject world-readable cache");
        let msg = format!("{err}");
        assert!(msg.contains("must be 0600"), "unexpected error: {msg}");
        assert!(
            msg.contains("chmod 600"),
            "expected remediation hint: {msg}"
        );
    }

    #[tokio::test]
    async fn callback_router_extracts_code_and_state() {
        let (tx, rx) = oneshot::channel::<AuthCallback>();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = callback_router(tx_slot, "EXPECTED-STATE".to_string());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // Linear-style: percent-encoded `:` in the code. axum's
        // `Query<T>` must URL-decode it.
        let resp = reqwest::Client::new()
            .get(format!(
                "http://{addr}/callback?code=abc%3Adef%3Aghi&state=EXPECTED-STATE"
            ))
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "got {}", resp.status());

        let received = rx.await.unwrap();
        assert_eq!(received.code, "abc:def:ghi", "URL-decode must happen");
        assert_eq!(received.state, "EXPECTED-STATE");
        server.abort();
    }

    #[tokio::test]
    async fn callback_router_rejects_state_mismatch() {
        let (tx, mut rx) = oneshot::channel::<AuthCallback>();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = callback_router(tx_slot, "EXPECTED".to_string());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/callback?code=xxx&state=WRONG"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "CSRF mismatch must 400");

        // Sender must NOT have fired.
        assert!(
            rx.try_recv().is_err(),
            "oneshot must remain empty on CSRF mismatch",
        );
        server.abort();
    }

    #[test]
    fn extract_state_param_handles_percent_encoding() {
        let url = "https://example.com/auth?response_type=code&state=ab%3Acd&code_challenge=x";
        assert_eq!(extract_state_param(url).as_deref(), Some("ab:cd"));
    }

    #[test]
    fn extract_state_param_returns_none_without_state() {
        let url = "https://example.com/auth?response_type=code";
        assert!(extract_state_param(url).is_none());
    }

    #[test]
    fn percent_decode_handles_plus_and_hex() {
        assert_eq!(percent_decode("a%3Ab"), "a:b");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
        // Trailing % with no hex left untouched (defensive — never seen in
        // practice; if it ever happens we want the call to keep working).
        assert_eq!(percent_decode("oops%"), "oops%");
    }
}
