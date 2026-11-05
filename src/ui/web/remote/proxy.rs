//! Remote-profiles T8 — the `/r/{profile}/…` proxy (design §5.4, §5.5,
//! §5.12, §6.2, §7).
//!
//! Exactly three routes; everything else under `/r/` (any method) is a 404
//! from THIS router, so the SPA fallback never answers and the remote is
//! never contacted for a non-allowlisted path. Stateless: `{profile}`
//! resolves through the process-wide `Directory` — a fresh global-config
//! parse per request (§5.11). The upstream request carries only the
//! remote's token cookie (plus the browser's Content-Type on the upload
//! POST); response headers come from an allowlist (never Set-Cookie). Both
//! directions stream; the router adds no body limit of its own.

use std::borrow::Cow;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use bytes::Bytes;
use futures::Stream;
use percent_encoding::percent_decode_str;
use uuid::Uuid;

use super::{Client, DIRECTORY, RemoteError, Upstream};
use crate::image_cache::valid_image_id;

/// One entry for the whole `/r/` prefix — axum 0.8's router cannot mix a
/// wildcard route with sibling routes, so the three allowlisted shapes are
/// dispatched by hand in [`under_r`]; everything else is a 404 from this
/// router, so the SPA fallback never answers and the remote is never
/// contacted (§6.2). Merged BEFORE the token layer in `WebUi::run`, so
/// every route is token-gated.
pub(crate) fn router() -> Router {
    Router::new()
        // The catch-all only matches a non-empty tail, so the bare prefix
        // and the empty tail need their own routes (all three land in
        // `under_r`, which 404s anything that is not an allowlisted shape).
        .route("/r", any(under_r))
        .route("/r/", any(under_r))
        .route("/r/{*rest}", any(under_r))
        // No body limit of the proxy's own (a 30 MiB upload must pass).
        .layer(DefaultBodyLimit::disable())
}

/// The path's segments after `/r`, percent-decoded one by one. Raw dot
/// segments stay segments (hyper does not normalise, so `/r/../x` →
/// `["..", "x"]`); the per-shape grammar checks below are the defence.
/// `None` on a non-UTF-8 escape.
fn decoded_segments(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix("/r")?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Some(Vec::new());
    }
    rest.split('/')
        .map(|seg| {
            percent_decode_str(seg)
                .decode_utf8()
                .ok()
                .map(Cow::into_owned)
        })
        .collect()
}

/// The single entry for everything under `/r/`: the three allowlisted
/// routes (their methods, their shapes) are dispatched to their handlers;
/// anything else — other paths, other methods — is a 404 from here.
async fn under_r(req: Request<Body>) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let Some(segments) = decoded_segments(&path) else {
        return not_found().await;
    };
    let segs: Vec<&str> = segments.iter().map(String::as_str).collect();
    match segs.as_slice() {
        [profile, "images", id] if method == Method::GET => image_get(profile, id).await,
        [profile, "api", "uploads", convo, id] if method == Method::GET => {
            upload_get(profile, convo, id).await
        }
        [profile, "api", "uploads"] if method == Method::POST => upload_post(profile, req).await,
        _ => not_found().await,
    }
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// L must name a REMOTE profile; `.`/`..` can never be one (the config
/// refuses them — the check keeps a raw segment safe anyway).
async fn resolve(name: &str) -> Option<Arc<Client>> {
    if matches!(name, "." | "..") {
        return None;
    }
    DIRECTORY.client(name).await.ok()
}

/// `GET /r/{L}/images/{id}` → upstream `GET /images/{id}`. The id is
/// `image_cache`'s grammar (64 lower-hex + known ext) — checked before any
/// network.
async fn image_get(profile: &str, id: &str) -> Response {
    if !valid_image_id(id) {
        return not_found().await;
    }
    let Some(client) = resolve(profile).await else {
        return not_found().await;
    };
    get_upstream(&client, &format!("images/{id}")).await
}

/// `GET /r/{L}/api/uploads/{convo}/{id}` → upstream of the same shape; both
/// segments must be UUIDs.
async fn upload_get(profile: &str, convo: &str, id: &str) -> Response {
    let Ok(convo) = Uuid::parse_str(convo) else {
        return not_found().await;
    };
    let Ok(id) = Uuid::parse_str(id) else {
        return not_found().await;
    };
    let Some(client) = resolve(profile).await else {
        return not_found().await;
    };
    get_upstream(&client, &format!("api/uploads/{convo}/{id}")).await
}

/// `POST /r/{L}/api/uploads?convo=&name=` → upstream with the query REBUILT
/// from the parsed parts only — the browser's query string (any `token=…`
/// included) never reaches the remote (§7).
async fn upload_post(profile: &str, req: Request<Body>) -> Response {
    let mut convo: Option<Uuid> = None;
    let mut name: Option<String> = None;
    for (key, value) in url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes()) {
        match key.as_ref() {
            "convo" if convo.is_none() => convo = Uuid::parse_str(value.as_ref()).ok(),
            "name" if name.is_none() => name = Some(value.into_owned()),
            _ => {}
        }
    }
    let (Some(convo), Some(name)) = (convo, name) else {
        return not_found().await;
    };
    let Some(client) = resolve(profile).await else {
        return not_found().await;
    };
    let content_type = req.headers().get(header::CONTENT_TYPE).cloned();
    let body = reqwest::Body::wrap_stream(req.into_body().into_data_stream());
    match client.post_upload(&convo, &name, content_type, body).await {
        Ok(upstream) => upstream_response(upstream),
        Err(e) => failure_response(e),
    }
}

async fn get_upstream(client: &Arc<Client>, path: &str) -> Response {
    match client.get(path).await {
        Ok(upstream) => upstream_response(upstream),
        Err(e) => failure_response(e),
    }
}

/// The remote's status and body through; the headers from the allowlist
/// only (§5.5: never Set-Cookie, CSP, ACAO or any other browser-facing
/// header).
fn upstream_response(upstream: Upstream) -> Response {
    let length = upstream
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let stream = upstream.body.bytes_stream();
    let body = match length {
        // An exact size hint lets the browser get length framing.
        Some(len) => Body::from_stream(SizedStream {
            inner: stream,
            left: len,
        }),
        None => Body::from_stream(stream),
    };
    let mut response = Response::new(body);
    *response.status_mut() = upstream.status;
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CACHE_CONTROL,
        header::CONTENT_DISPOSITION,
    ] {
        if let Some(value) = upstream.headers.get(name.as_str()) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

/// §5.5/§5.12: a 401 is 502 E-AUTH, an unreachable remote 502 E-DOWN (the
/// exact `RemoteError` Display), a timeout 504 — a 401 at the local origin
/// would read as a LOCAL auth failure.
fn failure_response(error: RemoteError) -> Response {
    let status = match &error {
        RemoteError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        error.to_string(),
    )
        .into_response()
}

/// A body stream with an exact `size_hint`, so a remote `Content-Length`
/// arrives as length framing (hyper would chunk a plain `from_stream`).
struct SizedStream<S> {
    inner: S,
    left: u64,
}

impl<S> Stream for SizedStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if this.left == 0 {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.left = this.left.saturating_sub(chunk.len() as u64);
                Poll::Ready(Some(Ok(chunk)))
            }
            other => other,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let exact = std::cmp::min(self.left, usize::MAX as u64) as usize;
        (exact, Some(exact))
    }
}
