//! Upload HTTP routes (file-uploads design §5.2, T8).
//!
//! `POST /api/uploads?convo=<uuid>&name=<urlencoded filename>` stores one
//! raw-body file in the [`UploadStore`]; `GET /api/uploads/{convo}/{id}`
//! serves a stored file with safe headers. Both handlers are thin adapters
//! over [`crate::attachments`] and hold no path logic of their own: the
//! only joins that ever happen are `root / Uuid / Uuid / sanitized_name`
//! (invariant I5), built from typed `Query`/`Path` Uuids and the
//! store-derived name.
//!
//! [`router`] is state-bound (`with_state`), so it merges into the main
//! web router *before* the `require_token` layer — both routes inherit the
//! token gate exactly like `/images/{id}`.

use crate::attachments::{AttachError, UploadStore, sanitize_name};
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::StreamExt;
use percent_encoding::{AsciiSet, CONTROLS, percent_encode};
use serde::Deserialize;
use tokio_util::io::StreamReader;
use uuid::Uuid;

/// `POST /api/uploads?convo=<uuid>&name=<urlencoded filename>`.
///
/// The body is raw bytes. `store()`'s `take(max+1)` is the only size limit
/// in the system, so the route disables axum's 2 MB `DefaultBodyLimit`
/// (and a `Content-Length` over the limit is rejected before reading).
/// A bad or missing `convo`/`name` is a 400 `Query` rejection; a missing
/// or wrong `Content-Type` is a 415 — the strict type forces a CORS
/// preflight, which is the CSRF guard on an open loopback bind.
async fn upload_handler(
    State(store): State<UploadStore>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Content-Type values are case-insensitive and may carry parameters;
    // compare the media type part only.
    let is_octet_stream = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/octet-stream"));
    if !is_octet_stream {
        return json_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/octet-stream",
        );
    }

    let name = sanitize_name(&query.name);
    let limits = store.limits();
    let max_bytes = limits.max_file_mb as u64 * 1024 * 1024;

    // Content-Length is a fast fail: when the header already proves the
    // body is oversize, reject before reading a single byte.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = declared
        && len > max_bytes
    {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &AttachError::TooLarge {
                name,
                size: Some(len),
                max_mb: limits.max_file_mb,
            }
            .to_string(),
        );
    }

    // Stream straight into the store: `take(max+1)` bounds the copy, and a
    // mid-stream failure leaves no id dir behind.
    let stream = body
        .into_data_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let reader = StreamReader::new(stream);
    match store.store(query.convo, &name, reader).await {
        Ok(attachment) => json_response(StatusCode::OK, &attachment),
        Err(e) => match e {
            // `take(max+1)` caught it despite the Content-Length check
            // (absent or lying header).
            AttachError::TooLarge { .. } => {
                json_error(StatusCode::PAYLOAD_TOO_LARGE, &e.to_string())
            }
            other => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not store file: {other}"),
            ),
        },
    }
}

/// `?convo=<uuid>&name=<filename>` — a bad or missing field is a 400
/// `Query` rejection (axum default).
#[derive(Deserialize)]
struct UploadQuery {
    convo: Uuid,
    name: String,
}

/// `GET /api/uploads/{convo}/{id}` — serves the single stored file.
///
/// A missing id and a read error produce the same 404 "not found" on
/// purpose: the response must not be an existence oracle. The path is
/// built only from `store.find`'s result (Uuids + sanitized name), never
/// from request strings.
async fn serve_handler(
    State(store): State<UploadStore>,
    Path((convo, id)): Path<(Uuid, Uuid)>,
) -> Response {
    let not_found = (StatusCode::NOT_FOUND, "not found").into_response();
    let Some(attachment) = store.find(convo, id) else {
        return not_found;
    };
    let Ok(bytes) = tokio::fs::read(store.path(&attachment)).await else {
        // One response for "absent" and "unreadable" — no oracle.
        return not_found;
    };

    let (content_type, disposition, sandbox) = serve_class(&attachment.mime);
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // The bytes are immutable (I1), so a year of private caching is safe.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "{disposition}; filename*=UTF-8''{}",
            percent_encode(attachment.name.as_bytes(), RFC5987_ATTR)
        ))
        .expect("RFC 5987 filename* is ASCII"),
    );
    if sandbox {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("sandbox"),
        );
    }
    response
}

/// The §5.2 header table as a total match on the stored mime:
/// `(Content-Type, disposition, sandbox CSP)`.
///
/// `text/html` must NOT match the text row: it is listed explicitly before
/// the `text/*` arm and lands in the "anything else" row, so html and svg
/// are never rendered inline.
fn serve_class(mime: &str) -> (&'static str, &'static str, bool) {
    match mime {
        // Inline images; the sandbox CSP keeps a hostile payload from
        // scripting the page.
        "image/png" => ("image/png", "inline", true),
        "image/jpeg" => ("image/jpeg", "inline", true),
        "image/gif" => ("image/gif", "inline", true),
        "image/webp" => ("image/webp", "inline", true),
        // Chrome's PDF viewer refuses sandboxed docs, so PDF is the one
        // inline type without a CSP.
        "application/pdf" => ("application/pdf", "inline", false),
        // Explicit exclusion from the text row below.
        "text/html" => ("application/octet-stream", "attachment", true),
        // text/* (minus text/html) plus a fixed list of data formats:
        // safe to render as plain text, sandboxed.
        m if m.starts_with("text/") => ("text/plain; charset=utf-8", "inline", true),
        "application/json"
        | "application/xml"
        | "application/toml"
        | "application/x-yaml"
        | "application/javascript"
        | "application/x-sh" => ("text/plain; charset=utf-8", "inline", true),
        // Anything else (incl. image/svg+xml) downloads as a binary blob.
        _ => ("application/octet-stream", "attachment", true),
    }
}

/// The complement of RFC 5987's `attr-char` within ASCII: every byte
/// `filename*` must percent-encode. Bytes > 0x7F are always encoded by
/// `percent_encode` regardless of the set.
const RFC5987_ATTR: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'{')
    .add(b'}');

/// The uploads sub-router, state-bound on the store. Merged into the main
/// web router before the `require_token` layer, so both routes are
/// token-gated like `/ws`, `/commands` and `/images/{id}`.
pub fn router(store: UploadStore) -> Router {
    Router::new()
        .route(
            "/api/uploads",
            // `store()`'s `take(max+1)` is the only size limit in the
            // system; axum's 2 MB default would reject legitimate uploads.
            post(upload_handler).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/uploads/{convo}/{id}", get(serve_handler))
        .with_state(store)
}

/// Serialize `value` as a JSON response. Hand-rolled because axum in this
/// build is configured without the `json` feature (the `setup.rs` pattern).
fn json_response<T: serde::Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialise response: {e}"),
        )
            .into_response(),
    }
}

/// `{"error": "…"}` — the error envelope for both upload routes.
fn json_error(status: StatusCode, error: &str) -> Response {
    #[derive(serde::Serialize)]
    struct ErrorBody<'a> {
        error: &'a str,
    }
    json_response(status, &ErrorBody { error })
}
