//! T2 + T3 — `peakbot::attachments` RED tests (file-uploads design
//! §3.1 / §5.1 / §7 T2, T3).
//!
//! **Status: compile-fail until T2/T3 land.** `peakbot::attachments` does
//! not exist yet; this file targets the locked interface from design §5.1
//! and fails to compile — the RED state we want.
//!
//! Orchestrator amendment A1 (overrides the doc):
//! - `[img:data:…]` → `AttachError::DataUriToken`.
//! - `[img:http(s)://…]` is KEPT: `parse_image_tokens` returns it as a
//!   normal token (no `UrlToken` variant exists); `collect` downloads it
//!   into the store (basename of the URL path, sanitized, "file" fallback;
//!   bounded by max_file_mb → TooLarge). URL tests run against a local
//!   axum server on 127.0.0.1:0 — no external network.
//!
//! Assumptions encoded:
//! - `UploadStore::store` takes `impl AsyncRead + Unpin`; tests pass `&[u8]`.
//! - `human_size` continues the base-1024 / 1-decimal pattern to GB
//!   ("1.0 GB").
//! - A bare word (`[img:hello]` — no path separator, not a URL) is an
//!   `InvalidToken` (design §7 T3 "bare word→InvalidToken").

use peakbot::attachments::{
    collect, human_size, model_note, parse_image_tokens, sanitize_name, AttachError, Attachment,
    AttachmentKind, UploadStore,
};
use peakbot::config::UploadsConfig;
use std::net::SocketAddr;
use std::path::Path;
use uuid::Uuid;

fn default_limits() -> UploadsConfig {
    UploadsConfig {
        max_file_mb: 50,
        max_files: 10,
    }
}

fn limits_with(max_file_mb: u32) -> UploadsConfig {
    UploadsConfig {
        max_file_mb,
        max_files: 10,
    }
}

/// Minimal valid 1x1 PNG (70 bytes).
fn png_1x1() -> Vec<u8> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("PNG fixture base64 must decode");
    assert!(
        image::guess_format(&bytes).is_ok(),
        "PNG fixture must sniff as a real image"
    );
    bytes
}

/// Minimal valid 1x1 JPEG (160 bytes).
fn jpeg_1x1() -> Vec<u8> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode("/9j/4AAQSkZJRgABAQEAAAAAAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAABAAEBAREA/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAD8AVN//2Q==")
        .expect("JPEG fixture base64 must decode");
    assert!(
        image::guess_format(&bytes).is_ok(),
        "JPEG fixture must sniff as a real image"
    );
    bytes
}

fn pad_to(mut bytes: Vec<u8>, len: usize) -> Vec<u8> {
    assert!(bytes.len() <= len, "fixture larger than target");
    bytes.resize(len, 0);
    bytes
}

async fn stored(store: &UploadStore, convo: Uuid, name: &str, bytes: &[u8]) -> Attachment {
    store
        .store(convo, name, bytes)
        .await
        .expect("store must succeed")
}

/// Entry count of a dir; 0 when the dir is absent.
fn entry_count(dir: &Path) -> usize {
    match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).count(),
        Err(_) => 0,
    }
}

/// Local axum server on 127.0.0.1:0 serving fixed paths (amendment A1 —
/// no external network).
async fn spawn_url_server(files: Vec<(&'static str, Vec<u8>)>) -> SocketAddr {
    let mut router = axum::Router::new();
    for (path, bytes) in files {
        let bytes = bytes;
        router = router.route(path, axum::routing::get(move || async move { bytes }));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    addr
}

// ===========================================================================
// T2 — sanitize_name
// ===========================================================================

const SANITIZE_CASES: &[(&str, &str)] = &[
    ("../../etc/passwd", "passwd"),
    ("a\\b.txt", "b.txt"),
    ("..", "file"),
    ("CON.txt", "_CON.txt"),
    ("prn", "_prn"),
    ("com1", "_com1"),
    ("LPT9.txt", "_LPT9.txt"),
    ("bad\x00name.txt", "bad_name.txt"),
    ("a<b>c:d\"e|f?g*h.txt", "a_b_c_d_e_f_g_h.txt"),
    ("  .hidden  ", "hidden"),
    ("file.txt...", "file.txt"),
    ("", "file"),
    ("/", "file"),
    ("....", "file"),
    ("plain.txt", "plain.txt"),
    ("file name with spaces.txt", "file name with spaces.txt"),
    ("ünicode-ñame.pdf", "ünicode-ñame.pdf"),
];

#[test]
fn sanitize_name_table() {
    for (raw, want) in SANITIZE_CASES {
        let got = sanitize_name(raw);
        assert_eq!(got, *want, "sanitize_name({raw:?})");
    }
}

#[test]
fn sanitize_name_is_idempotent() {
    for (raw, _) in SANITIZE_CASES {
        let once = sanitize_name(raw);
        let twice = sanitize_name(&once);
        assert_eq!(twice, once, "sanitize must be idempotent for {raw:?}");
    }
    // Truncation must be a fixed point too.
    let long = format!("{}{}", "a".repeat(300), ".pdf");
    let once = sanitize_name(&long);
    assert_eq!(sanitize_name(&once), once);
}

#[test]
fn sanitize_name_long_name_keeps_extension_and_caps_at_100_chars() {
    let long = format!("{}{}", "a".repeat(300), ".pdf");
    let out = sanitize_name(&long);
    assert!(
        out.chars().count() <= 100,
        "300-char name must truncate to <= 100 chars; got {}",
        out.chars().count()
    );
    assert!(
        out.ends_with(".pdf"),
        "extension must survive truncation; got {out:?}"
    );
}

// ===========================================================================
// T2 — UploadStore: store / find / remove
// ===========================================================================

#[tokio::test]
async fn store_then_find_round_trips() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let bytes = png_1x1();

    let att = stored(&store, convo, "cat.png", &bytes).await;
    assert_eq!(att.convo, convo);
    assert_eq!(att.name, "cat.png");
    assert_eq!(att.mime, "image/png");
    assert_eq!(att.size, bytes.len() as u64);
    assert_eq!(att.kind, AttachmentKind::Image);

    // On-disk layout: root/convo/id/name, bytes intact.
    let path = store.path(&att);
    assert!(path.exists(), "stored file must exist at path()");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // find returns the identical attachment.
    assert_eq!(store.find(convo, att.id), Some(att.clone()));
    // find under a different convo → None.
    assert_eq!(store.find(Uuid::new_v4(), att.id), None);
}

#[tokio::test]
async fn find_missing_id_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    assert_eq!(store.find(Uuid::new_v4(), Uuid::new_v4()), None);
}

#[tokio::test]
async fn find_dir_with_two_files_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let id = Uuid::new_v4();
    let d = dir.path().join(convo.to_string()).join(id.to_string());
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("a.txt"), b"a").unwrap();
    std::fs::write(d.join("b.txt"), b"b").unwrap();

    assert_eq!(
        store.find(convo, id),
        None,
        "an id dir with two files is malformed and must not resolve"
    );
}

#[tokio::test]
async fn store_too_large_returns_error_and_leaves_no_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), limits_with(1));
    let convo = Uuid::new_v4();
    let big = vec![0u8; 2 * 1024 * 1024];

    let err = store
        .store(convo, "big.bin", &big[..])
        .await
        .expect_err("2 MB against a 1 MB limit must be TooLarge");
    match &err {
        AttachError::TooLarge { name, max_mb, .. } => {
            assert_eq!(name, "big.bin");
            assert_eq!(*max_mb, 1);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert_eq!(
        entry_count(&dir.path().join(convo.to_string())),
        0,
        "TooLarge must leave no id dir behind"
    );
}

#[tokio::test]
async fn remove_conversation_removes_only_that_convo() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let png = png_1x1();
    stored(&store, a, "x.png", &png).await;
    stored(&store, b, "y.png", &png).await;

    store.remove_conversation(a);
    assert!(!dir.path().join(a.to_string()).exists(), "convo a must be gone");
    assert!(dir.path().join(b.to_string()).exists(), "convo b must survive");
}

#[tokio::test]
async fn remove_conversation_of_absent_convo_does_not_panic() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    store.remove_conversation(Uuid::new_v4()); // best-effort, must not panic
}

// ===========================================================================
// T2 — describe classification (sniffing)
// ===========================================================================

#[tokio::test]
async fn png_bytes_named_txt_are_classified_image_png() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let png = png_1x1();

    let att = stored(&store, convo, "x.txt", &png).await;
    assert_eq!(
        att.kind,
        AttachmentKind::Image,
        "magic bytes must beat the extension"
    );
    assert_eq!(att.mime, "image/png", "mime must come from the sniffed format");
}

#[tokio::test]
async fn jpeg_bytes_named_png_get_mime_image_jpeg() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let jpeg = jpeg_1x1();

    let att = stored(&store, convo, ".png", &jpeg).await;
    assert_eq!(att.kind, AttachmentKind::Image);
    assert_eq!(att.mime, "image/jpeg");
    assert_eq!(att.name, "png", "leading dot must be trimmed by sanitize_name");
}

#[tokio::test]
async fn png_over_10mb_is_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let big = pad_to(png_1x1(), 11 * 1024 * 1024);

    let att = stored(&store, convo, "big.png", &big).await;
    assert_eq!(
        att.kind,
        AttachmentKind::File,
        "an 11 MB PNG exceeds MAX_IMAGE_BYTES (10 MB) and must be a File"
    );
    assert_eq!(att.mime, "image/png", "File mime comes from the extension");
}

#[tokio::test]
async fn png_at_exactly_10mb_is_image() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let exact = pad_to(png_1x1(), 10 * 1024 * 1024);

    let att = stored(&store, convo, "exact.png", &exact).await;
    assert_eq!(
        att.kind,
        AttachmentKind::Image,
        "size <= MAX_IMAGE_BYTES is inclusive"
    );
    assert_eq!(att.mime, "image/png");
}

#[tokio::test]
async fn svg_is_file_with_svg_mime() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";

    let att = stored(&store, convo, "icon.svg", svg).await;
    assert_eq!(att.kind, AttachmentKind::File, "SVG is never an Image");
    assert_eq!(att.mime, "image/svg+xml");
}

#[tokio::test]
async fn heic_is_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();
    // ftyp box header, not in image::guess_format's magic table.
    let heic = [0xFF, 0x01, 0x00, 0x00, 0x66, 0x74, 0x79, 0x70, 0x68, 0x65, 0x69, 0x63];

    let att = stored(&store, convo, "photo.heic", &heic).await;
    assert_eq!(att.kind, AttachmentKind::File);
    assert_eq!(att.mime, "image/heic");
}

// ===========================================================================
// T2 — human_size
// ===========================================================================

#[test]
fn human_size_boundaries() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(512), "512 B");
    assert_eq!(human_size(1023), "1023 B");
    assert_eq!(human_size(1024), "1.0 KB");
    assert_eq!(human_size(1234), "1.2 KB");
    assert_eq!(human_size(1024 * 1024), "1.0 MB");
    assert_eq!(human_size(2_411_724), "2.3 MB");
    // Assumption: the pattern continues to GB.
    assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
}

// ===========================================================================
// T3 — parse_image_tokens (pure grammar)
// ===========================================================================

#[test]
fn parse_plain_text_returns_it_unchanged_with_no_tokens() {
    let (text, tokens) = parse_image_tokens("hello world, no tokens here").unwrap();
    assert_eq!(text, "hello world, no tokens here");
    assert!(tokens.is_empty());
}

#[test]
fn parse_multiple_tokens_in_order_with_stripped_text() {
    let (text, tokens) = parse_image_tokens("see [img:/a.png] and [img:~/b.jpg] ok").unwrap();
    assert_eq!(
        tokens,
        vec!["/a.png".to_string(), "~/b.jpg".to_string()],
        "tokens must come back in text order"
    );
    assert!(!text.contains("[img:"), "tokens must be stripped from the text");
    assert!(
        text.contains("see") && text.contains("ok"),
        "surrounding text must survive; got {text:?}"
    );
}

#[test]
fn parse_unterminated_token_stays_literal() {
    let (text, tokens) = parse_image_tokens("foo [img: bar").unwrap();
    assert!(tokens.is_empty());
    assert!(
        text.contains("[img:"),
        "unterminated [img: must stay literal (today's behaviour)"
    );
}

#[test]
fn parse_data_uri_token_is_data_uri_error() {
    let err = parse_image_tokens("[img:data:image/png;base64,AAA]").unwrap_err();
    assert!(
        matches!(err, AttachError::DataUriToken),
        "amendment A1: data: tokens must be DataUriToken; got {err:?}"
    );
}

#[test]
fn parse_http_url_token_is_kept() {
    let (text, tokens) = parse_image_tokens("[img:http://example.com/a.png]").unwrap();
    assert_eq!(
        tokens,
        vec!["http://example.com/a.png".to_string()],
        "amendment A1: http: URLs are normal tokens"
    );
    assert!(!text.contains("[img:"));
}

#[test]
fn parse_https_url_token_is_kept() {
    let (_, tokens) = parse_image_tokens("[img:https://example.com/b.jpg]").unwrap();
    assert_eq!(tokens, vec!["https://example.com/b.jpg".to_string()]);
}

#[test]
fn parse_bare_word_token_is_invalid() {
    let err = parse_image_tokens("[img:hello]").unwrap_err();
    assert!(
        matches!(err, AttachError::InvalidToken(_)),
        "a bare word is neither a path nor a URL; got {err:?}"
    );
}

// ===========================================================================
// T3 — collect (controller ingest)
// ===========================================================================

#[tokio::test]
async fn collect_count_check_happens_before_any_copy() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits()); // max_files = 10
    let src = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();

    let mut tokens = Vec::new();
    for i in 0..11 {
        let p = src.path().join(format!("f{i}.png"));
        std::fs::write(&p, png_1x1()).unwrap();
        tokens.push(p.to_string_lossy().into_owned());
    }

    let err = collect(&store, convo, src.path(), tokens, vec![])
        .await
        .expect_err("11 tokens against max_files 10 must be TooMany");
    match &err {
        AttachError::TooMany { count, max } => {
            assert_eq!(*count, 11);
            assert_eq!(*max, 10);
        }
        other => panic!("expected TooMany, got {other:?}"),
    }
    assert_eq!(
        entry_count(&dir.path().join(convo.to_string())),
        0,
        "the count check must run before any copy — the store must be empty"
    );
}

#[tokio::test]
async fn collect_relative_token_resolves_against_given_cwd() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();

    let p = cwd.path().join("x.png");
    std::fs::write(&p, png_1x1()).unwrap();

    let atts = collect(&store, convo, cwd.path(), vec!["./x.png".into()], vec![])
        .await
        .expect("./x.png must resolve against the given cwd, not the process cwd");
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].name, "x.png");
    assert!(store.path(&atts[0]).exists());
}

#[tokio::test]
async fn collect_directory_token_is_not_a_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();

    let d = cwd.path().join("adir");
    std::fs::create_dir(&d).unwrap();

    let err = collect(&store, convo, cwd.path(), vec![d.to_string_lossy().into_owned()], vec![])
        .await
        .expect_err("a directory token must be NotAFile");
    match err {
        AttachError::NotAFile(p) => assert_eq!(p, d),
        other => panic!("expected NotAFile, got {other:?}"),
    }
}

#[tokio::test]
async fn collect_unknown_id_is_unknown_upload() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let id = Uuid::new_v4();

    let err = collect(&store, convo, cwd.path(), vec![], vec![id])
        .await
        .expect_err("an id with no stored file must be UnknownUpload");
    match err {
        AttachError::UnknownUpload(u) => assert_eq!(u, id),
        other => panic!("expected UnknownUpload, got {other:?}"),
    }
}

#[tokio::test]
async fn collect_orders_ids_before_tokens() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let png = png_1x1();

    let a = stored(&store, convo, "a.png", &png).await;
    let b = stored(&store, convo, "b.png", &png).await;
    let c = cwd.path().join("c.png");
    std::fs::write(&c, png).unwrap();

    let atts = collect(
        &store,
        convo,
        cwd.path(),
        vec![c.to_string_lossy().into_owned()],
        vec![b.id, a.id],
    )
    .await
    .expect("collect must succeed");
    let names: Vec<&str> = atts.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["b.png", "a.png", "c.png"],
        "ids in client order first, then tokens in text order"
    );
}

// ===========================================================================
// T3 — collect: URL tokens (amendment A1, local axum server only)
// ===========================================================================

#[tokio::test]
async fn collect_downloads_url_token_into_store() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let png = png_1x1();
    let addr = spawn_url_server(vec![("/cat.png", png.clone())]).await;

    let atts = collect(
        &store,
        convo,
        cwd.path(),
        vec![format!("http://{addr}/cat.png")],
        vec![],
    )
    .await
    .expect("URL token must download into the store");
    assert_eq!(atts.len(), 1);
    assert_eq!(
        atts[0].name, "cat.png",
        "name must be the basename of the URL path"
    );
    assert_eq!(atts[0].kind, AttachmentKind::Image);
    assert_eq!(atts[0].mime, "image/png");
    assert_eq!(std::fs::read(store.path(&atts[0])).unwrap(), png);
    assert_eq!(store.find(convo, atts[0].id), Some(atts[0].clone()));
}

#[tokio::test]
async fn collect_url_without_basename_stored_as_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let addr = spawn_url_server(vec![("/", b"root bytes".to_vec())]).await;

    let atts = collect(&store, convo, cwd.path(), vec![format!("http://{addr}/")], vec![])
        .await
        .expect("a URL with an empty path basename must fall back to 'file'");
    assert_eq!(atts[0].name, "file");
}

#[tokio::test]
async fn collect_url_with_query_strips_query() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let png = png_1x1();
    let addr = spawn_url_server(vec![("/cat.png", png.clone())]).await;

    let atts = collect(
        &store,
        convo,
        cwd.path(),
        vec![format!("http://{addr}/cat.png?v=2")],
        vec![],
    )
    .await
    .expect("query string must not leak into the stored name");
    assert_eq!(atts[0].name, "cat.png");
}

#[tokio::test]
async fn collect_url_too_large_is_too_large_and_leaves_no_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), limits_with(1));
    let cwd = tempfile::TempDir::new().unwrap();
    let convo = Uuid::new_v4();
    let big = vec![0u8; 2 * 1024 * 1024];
    let addr = spawn_url_server(vec![("/big.bin", big.clone())]).await;

    let err = collect(
        &store,
        convo,
        cwd.path(),
        vec![format!("http://{addr}/big.bin")],
        vec![],
    )
    .await
    .expect_err("a 2 MB URL against a 1 MB limit must be TooLarge");
    assert!(
        matches!(err, AttachError::TooLarge { .. }),
        "expected TooLarge, got {err:?}"
    );
    assert_eq!(
        entry_count(&dir.path().join(convo.to_string())),
        0,
        "a too-large URL download must leave no dir behind"
    );
}

// ===========================================================================
// T3 — model_note
// ===========================================================================

#[tokio::test]
async fn model_note_exact_golden_string() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    let convo = Uuid::new_v4();

    // 2411724 bytes → "2.3 MB" (the §5.2 example size).
    let spec = stored(&store, convo, "spec.pdf", &vec![b'%'; 2_411_724]).await;
    // 1234 bytes → "1.2 KB" (the §5.3 example size); real PNG magic so it
    // classifies as Image like the golden example.
    let cat_bytes = pad_to(png_1x1(), 1234);
    let cat = stored(&store, convo, "cat.png", &cat_bytes).await;

    let note = model_note(&store, &[spec.clone(), cat.clone()]);
    let expected = format!(
        "[Attached files — read them with your file tools]\n\
         - spec.pdf (application/pdf, 2.3 MB): {}\n\
         - cat.png (image/png, 1.2 KB): {}",
        store.path(&spec),
        store.path(&cat),
    );
    assert_eq!(note, expected);
}

#[tokio::test]
async fn model_note_empty_for_no_attachments() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = UploadStore::new(dir.path().to_path_buf(), default_limits());
    assert_eq!(model_note(&store, &[]), "");
}
