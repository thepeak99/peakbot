// Assumption: the implementation exposes peakbot::ui::web::uploads::router(store: UploadStore) -> axum::Router (T8, design §5.2).
//!
//! T8 — upload HTTP route RED tests (file-uploads design §5.2 / §7 T8).
//!
//! **Status: compile-fail until T8 lands.** `peakbot::ui::web::uploads` and
//! `peakbot::attachments` do not exist yet; this file targets the locked
//! interface from design §5.2 and fails to compile — the RED state we
//! want.
//!
//! Strategy: mount the bare `uploads::router(store)` on a random loopback
//! port (the `spawn_app` pattern from tests/setup_api_tests.rs) and drive
//! it with reqwest. The 401 token-gating test is deliberately NOT here —
//! it is covered in-crate where `require_token` is layered.

use peakbot::attachments::UploadStore;
use peakbot::config::UploadsConfig;
use std::net::SocketAddr;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

fn default_limits() -> UploadsConfig {
    UploadsConfig {
        max_file_mb: 50,
        max_files: 10,
    }
}

/// Minimal valid 1x1 PNG (70 bytes).
fn png_bytes() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("PNG fixture base64 must decode")
}

/// Spawn `uploads::router(store)` on a random loopback port.
async fn spawn_uploads(limits: UploadsConfig) -> (SocketAddr, TempDir, UploadStore) {
    let dir = TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), limits);
    let app = peakbot::ui::web::uploads::router(store.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (addr, dir, store)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// POST a raw body to /api/uploads. `content_type: None` omits the header.
async fn post_upload(
    addr: &SocketAddr,
    convo: &Uuid,
    name: &str,
    body: Vec<u8>,
    content_type: Option<&str>,
) -> reqwest::Response {
    let mut req = client()
        .post(format!("http://{addr}/api/uploads"))
        .query(&[("convo", convo.to_string()), ("name", name)])
        .body(body);
    if let Some(ct) = content_type {
        req = req.header(reqwest::header::CONTENT_TYPE, ct);
    }
    req.send().await.unwrap()
}

/// Upload and return the stored attachment id.
async fn upload_id(addr: &SocketAddr, convo: &Uuid, name: &str, body: Vec<u8>) -> String {
    let resp = post_upload(addr, convo, name, body, Some("application/octet-stream")).await;
    assert_eq!(resp.status(), 200, "upload of {name} must succeed");
    let v: serde_json::Value = resp.json().await.unwrap();
    v["id"]
        .as_str()
        .expect("Attachment JSON must carry an id")
        .to_string()
}

async fn get_file(addr: &SocketAddr, convo: &Uuid, id: &str) -> reqwest::Response {
    client()
        .get(format!("http://{addr}/api/uploads/{convo}/{id}"))
        .send()
        .await
        .unwrap()
}

/// Entry count of a dir; 0 when the dir is absent.
fn entry_count(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).count(),
        Err(_) => 0,
    }
}

// ===========================================================================
// POST /api/uploads
// ===========================================================================

#[tokio::test]
async fn upload_3mb_returns_200_and_attachment_json() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let body = vec![0u8; 3 * 1024 * 1024];

    let resp = post_upload(
        &addr,
        &convo,
        "big.bin",
        body,
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(
        resp.status(),
        200,
        "3 MB must pass — proves DefaultBodyLimit is disabled on POST"
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["name"], "big.bin");
    assert_eq!(v["size"], 3 * 1024 * 1024);
    assert_eq!(v["kind"], "file");
    assert_eq!(v["convo"], convo.to_string());
    assert!(
        v["id"].as_str().is_some(),
        "Attachment JSON must carry an id"
    );
}

#[tokio::test]
async fn upload_over_limit_returns_413_and_leaves_no_dir() {
    let (addr, dir, _store) = spawn_uploads(UploadsConfig {
        max_file_mb: 1,
        max_files: 10,
    })
    .await;
    let convo = Uuid::new_v4();
    let body = vec![0u8; 2 * 1024 * 1024];

    let resp = post_upload(
        &addr,
        &convo,
        "big.bin",
        body,
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(resp.status(), 413);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(
        v["error"].as_str().unwrap().contains("big.bin"),
        "413 error must name the file; got: {v:?}"
    );
    assert_eq!(
        entry_count(&dir.path().join(convo.to_string())),
        0,
        "413 must leave no id dir behind"
    );
}

#[tokio::test]
async fn upload_wrong_content_type_returns_415() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();

    let resp = post_upload(&addr, &convo, "x.txt", b"hi".to_vec(), Some("text/plain")).await;
    assert_eq!(resp.status(), 415);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"], "Content-Type must be application/octet-stream");
}

#[tokio::test]
async fn upload_missing_content_type_returns_415() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();

    let resp = post_upload(&addr, &convo, "x.txt", b"hi".to_vec(), None).await;
    assert_eq!(
        resp.status(),
        415,
        "a missing Content-Type is 'anything else' and must be 415"
    );
}

#[tokio::test]
async fn upload_bad_convo_uuid_returns_400() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;

    let resp = client()
        .post(format!("http://{addr}/api/uploads"))
        .query(&[("convo", "not-a-uuid"), ("name", "x.txt")])
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(b"hi".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "bad convo uuid must be a 400 Query rejection"
    );
}

#[tokio::test]
async fn upload_path_traversal_name_stored_sanitized() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();

    let resp = post_upload(
        &addr,
        &convo,
        "../../x",
        b"data".to_vec(),
        Some("application/octet-stream"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["name"], "x", "name must be sanitized to the bare leaf");
}

// ===========================================================================
// GET /api/uploads/{convo}/{id}
// ===========================================================================

#[tokio::test]
async fn get_round_trip_bytes_equal() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

    let id = upload_id(&addr, &convo, "rt.bin", body.clone()).await;
    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap(), body);
}

#[tokio::test]
async fn get_png_headers_inline_with_sandbox() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(&addr, &convo, "cat.png", png_bytes()).await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disp.starts_with("inline"), "png must be inline; got {disp}");
    let csp = resp
        .headers()
        .get(reqwest::header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        csp.contains("sandbox"),
        "png must carry a sandbox CSP; got {csp}"
    );
}

#[tokio::test]
async fn get_pdf_headers_inline_without_sandbox() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(&addr, &convo, "spec.pdf", vec![b'%'; 1024]).await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "application/pdf"
    );
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disp.starts_with("inline"), "pdf must be inline; got {disp}");
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .is_none(),
        "pdf must NOT carry a sandbox CSP (Chrome's PDF viewer refuses sandboxed docs)"
    );
}

#[tokio::test]
async fn get_md_served_as_text_plain_inline_with_sandbox() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(&addr, &convo, "notes.md", b"# hello".to_vec()).await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8",
        "text/* (except text/html) is served as text/plain"
    );
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disp.starts_with("inline"), "md must be inline; got {disp}");
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("sandbox"),
        "md must carry a sandbox CSP"
    );
}

#[tokio::test]
async fn get_html_served_as_octet_stream_attachment() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(
        &addr,
        &convo,
        "page.html",
        b"<html><body>hi</body></html>".to_vec(),
    )
    .await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "application/octet-stream",
        "text/html must NOT match the text row"
    );
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        disp.starts_with("attachment"),
        "html must be attachment; got {disp}"
    );
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("sandbox"),
        "html must carry a sandbox CSP"
    );
}

#[tokio::test]
async fn get_svg_served_as_octet_stream_attachment() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(
        &addr,
        &convo,
        "icon.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec(),
    )
    .await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        "application/octet-stream",
        "image/svg+xml is in the 'anything else' row"
    );
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        disp.starts_with("attachment"),
        "svg must be attachment; got {disp}"
    );
    assert!(
        resp.headers()
            .get(reqwest::header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("sandbox"),
        "svg must carry a sandbox CSP"
    );
}

#[tokio::test]
async fn get_unknown_id_returns_404_not_found() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();

    let resp = get_file(&addr, &convo, &Uuid::new_v4().to_string()).await;
    assert_eq!(resp.status(), 404);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("not found"),
        "404 must be the shared 'not found' response (no existence oracle); got: {body}"
    );
}

#[tokio::test]
async fn get_filename_star_percent_encodes_unicode_name() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(&addr, &convo, "naïve résumé.pdf", vec![b'%'; 64]).await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    let disp = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        disp.contains("filename*=UTF-8''na%C3%AFve%20r%C3%A9sum%C3%A9.pdf"),
        "filename* must be the RFC 5987 pct-encoding of the name; got {disp}"
    );
}

#[tokio::test]
async fn get_always_sends_nosniff_and_immutable_cache_headers() {
    let (addr, _dir, _store) = spawn_uploads(default_limits()).await;
    let convo = Uuid::new_v4();
    let id = upload_id(&addr, &convo, "cat.png", png_bytes()).await;

    let resp = get_file(&addr, &convo, &id).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(
        resp.headers().get(reqwest::header::CACHE_CONTROL).unwrap(),
        "private, max-age=31536000, immutable"
    );
}
