//! Remote-profiles T8 — the `/r/{profile}/…` proxy (design §5.4, §5.5,
//! §5.12, §6.2, §7, §9-T8). RED until the implementation lands.
//!
//! Written against the interface the design specifies, NOT an existing impl.
//!
//! **Interface the implementer must provide** (the only new symbol the tests
//! name): `crate::ui::web::remote::proxy::router() -> axum::Router`
//! (`Router<()>`, stateless: it resolves profiles through the process-wide
//! `Directory` the relay already uses, so every request re-reads the global
//! config — §5.11). `WebUi::run` merges it BEFORE the token layer, next to
//! `uploads::router`. The harness (`t78_support::LocalEnv`) merges it the same
//! way, behind the real `require_token`, with an SPA-like fallback that
//! answers `200 SPA-INDEX` for any unrouted path — so a 404 can only come from
//! the proxy's own catch-all.
//!
//! Contract (L = a REMOTE profile name, percent-decoded by the router):
//!
//! | request                                           | upstream (rebuilt, never copied)        |
//! |---------------------------------------------------|-----------------------------------------|
//! | `GET  /r/L/images/{id}`  id = 64 lower-hex + `.png|jpg|jpeg|gif|webp` | `GET  /images/{id}`            |
//! | `GET  /r/L/api/uploads/{convo}/{id}` both UUIDs    | `GET  /api/uploads/{convo}/{id}`        |
//! | `POST /r/L/api/uploads?convo=<uuid>&name=<s>`      | `POST /api/uploads?convo=<uuid>&name=<s>` |
//! | anything else under `/r/` (any method)             | **404, remote never contacted**         |
//!
//! * Unknown / local profile name, malformed id or UUID → 404 WITHOUT contacting
//!   the remote.
//! * Status mapping (§5.5): remote 404 → 404; remote 401 → **502** with the
//!   locked `RemoteError::Auth` text; unreachable → **502** with the locked
//!   E-DOWN text; (timeout → 504, see GAPS). A GET passes the remote's status
//!   and body through; an upload passes the remote's status AND JSON body
//!   verbatim (ids are never re-minted).
//! * Response headers come from an allowlist only: `Content-Type`,
//!   `Content-Length`, `Cache-Control`, `Content-Disposition`. No `Set-Cookie`.
//! * Upstream carries exactly ONE header the browser did not choose: the
//!   remote's `Cookie: peakbot_token=<remote>`; plus, for the upload POST only,
//!   the request's `Content-Type` (the remote insists on `application/octet-stream`).
//!   No browser cookie / `Authorization` / `?token=` ever goes upstream.
//! * Both directions stream (no full buffering); no body-size limit of its own.

#![allow(clippy::await_holding_lock)] // LocalEnv holds the crate-wide config lock

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use super::proxy;
use super::t78_support::*;
use crate::ui::web::remote::test_support::Captured;

const L: &str = "home-dev";
const L_SPACE: &str = "home dev";
const T_REMOTE_B: &str = "remote-token-b-t78-33cc";
const CONVO: &str = "11111111-1111-4111-8111-111111111111";
const FILE: &str = "22222222-2222-4222-8222-222222222222";

fn img() -> String {
    format!("{}.png", "a1".repeat(32))
}

async fn env_for(remotes: &[RemoteSpec<'_>]) -> LocalEnv {
    LocalEnv::start(remotes, Some(proxy::router())).await
}

/// One remote `dev` as `home-dev`, healthy.
async fn one() -> (FakeRemote, LocalEnv) {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = env_for(&[(L, &a.url, T_REMOTE, "dev")]).await;
    (a, env)
}

async fn get(env: &LocalEnv, path_and_query: &str) -> reqwest::Response {
    http()
        .get(format!("{}{path_and_query}", env.base()))
        .header("cookie", local_cookie())
        .send()
        .await
        .expect("GET")
}

fn upload_post_path(l: &str, convo: &str, name: &str) -> String {
    format!("/r/{l}/api/uploads?convo={convo}&name={name}")
}

async fn post_upload(env: &LocalEnv, path_and_query: &str, body: Vec<u8>) -> reqwest::Response {
    http()
        .post(format!("{}{path_and_query}", env.base()))
        .header("cookie", local_cookie())
        .header("content-type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .expect("POST")
}

fn hdr(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn upstream_header(hit: &Captured, name: &str) -> Vec<String> {
    hit.headers
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
        .collect()
}

fn png_bytes() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend((0..=255u8).cycle().take(5000));
    v
}

// ═══════════════════════════════════════════════════════════════════════
// GET /r/{L}/images/{id}
// ═══════════════════════════════════════════════════════════════════════

// §5.5: rebuilt upstream GET with the REMOTE cookie; body, status,
// content-type, cache-control and length come back.
#[tokio::test]
async fn image_get_returns_the_remote_body_status_and_allowlisted_headers() {
    let (a, env) = one().await;
    let png = png_bytes();
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png.clone())
            .header("content-type", "image/png")
            .header("cache-control", "public, max-age=31536000, immutable"),
    );

    let resp = get(&env, &format!("/r/{L}/images/{}", img())).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(hdr(&resp, "content-type").as_deref(), Some("image/png"));
    assert_eq!(
        hdr(&resp, "cache-control").as_deref(),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        hdr(&resp, "content-length").as_deref(),
        Some(png.len().to_string().as_str())
    );
    assert_eq!(resp.bytes().await.expect("body").to_vec(), png);

    let hits = a.hits_for("GET", &format!("/images/{}", img()));
    assert_eq!(
        hits.len(),
        1,
        "exactly one upstream call: {:?}",
        a.rec.hits()
    );
    assert_eq!(
        upstream_header(&hits[0], "cookie"),
        vec![format!("peakbot_token={T_REMOTE}")],
        "one Cookie header: the remote token"
    );
    assert_eq!(hits[0].uri, format!("/images/{}", img()), "no query copied");
}

// §5.5 / §9-T8: "header allowlist … no Set-Cookie passes".
#[tokio::test]
async fn image_get_copies_only_allowlisted_headers_and_never_set_cookie() {
    let (a, env) = one().await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes())
            .header("content-type", "image/png")
            .header("content-disposition", "inline; filename=\"x.png\"")
            .header("set-cookie", "peakbot_token=evil; Path=/")
            .header("x-evil", "1")
            .header("location", "https://evil.example/")
            .header("content-security-policy", "default-src *")
            .header("access-control-allow-origin", "*"),
    );

    let resp = get(&env, &format!("/r/{L}/images/{}", img())).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        hdr(&resp, "content-disposition").as_deref(),
        Some("inline; filename=\"x.png\""),
        "Content-Disposition is on the allowlist"
    );
    for blocked in [
        "set-cookie",
        "x-evil",
        "location",
        "content-security-policy",
        "access-control-allow-origin",
    ] {
        assert!(
            resp.headers().get(blocked).is_none(),
            "{blocked} must not pass: {:?}",
            resp.headers()
        );
    }
}

// §5.5 "streamed": the first bytes reach the browser while the remote is
// still mid-body.
#[tokio::test]
async fn image_get_streams_instead_of_buffering() {
    let (a, env) = one().await;
    let body = png_bytes();
    let gate = Arc::new(Notify::new());
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, body.clone())
            .header("content-type", "image/png")
            .gated(1000, gate.clone()),
    );

    let url = format!("{}/r/{L}/images/{}", env.base(), img());
    let mut resp = tokio::time::timeout(
        Duration::from_secs(5),
        http().get(url).header("cookie", local_cookie()).send(),
    )
    .await
    .expect("response headers arrive before the remote finished its body")
    .expect("GET");
    assert_eq!(resp.status(), 200);

    let first = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await
        .expect("the first chunk arrives while the remote still holds the rest")
        .expect("chunk")
        .expect("non-empty first chunk");
    assert!(!first.is_empty() && first.len() <= 1000);
    assert_eq!(&first[..], &body[..first.len()]);

    gate.notify_one();
    let mut all = first.to_vec();
    while let Some(c) = resp.chunk().await.expect("chunk") {
        all.extend_from_slice(&c);
    }
    assert_eq!(all, body, "the whole body arrives intact");
}

// §9-T8 "Encoding": a percent-encoded name with a space resolves.
#[tokio::test]
async fn percent_encoded_profile_name_with_a_space_resolves() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = env_for(&[(L_SPACE, &a.url, T_REMOTE, "dev")]).await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes()).header("content-type", "image/png"),
    );

    let resp = get(&env, &format!("/r/home%20dev/images/{}", img())).await;

    assert_eq!(resp.status(), 200, "name decoded to 'home dev'");
    assert_eq!(a.hits_for("GET", &format!("/images/{}", img())).len(), 1);
}

// A dot INSIDE a name is fine (only `.`/`..` as a whole segment is the hole).
#[tokio::test]
async fn profile_name_containing_dots_resolves() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = env_for(&[("v1.2.remote", &a.url, T_REMOTE, "dev")]).await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes()),
    );

    let resp = get(&env, &format!("/r/v1.2.remote/images/{}", img())).await;

    assert_eq!(resp.status(), 200);
}

// §5.5 status map: remote 404 → 404 (no existence oracle).
#[tokio::test]
async fn remote_404_passes_through_as_404() {
    let (a, env) = one().await;
    // The fake answers 404 for anything it has no canned response for.
    let resp = get(&env, &format!("/r/{L}/images/{}", img())).await;

    assert_eq!(resp.status(), 404);
    assert_eq!(
        a.hits_for("GET", &format!("/images/{}", img())).len(),
        1,
        "it WAS asked"
    );
    assert!(!resp.text().await.expect("body").contains(T_REMOTE));
}

// §5.5: remote 401 → 502 (a 401 at the local origin means LOCAL auth failed).
#[tokio::test]
async fn remote_401_becomes_502_with_the_locked_auth_string_for_every_get_route() {
    let a = FakeRemote::start("the-real-token-xyz").await;
    let env = env_for(&[(L, &a.url, "stale-token-in-config", "dev")]).await;

    for path in [
        format!("/r/{L}/images/{}", img()),
        format!("/r/{L}/api/uploads/{CONVO}/{FILE}"),
    ] {
        let resp = get(&env, &path).await;
        assert_eq!(resp.status(), 502, "{path}: never 401 at the local origin");
        let text = resp.text().await.expect("body");
        assert_eq!(
            text,
            "Remote profile 'home-dev' rejected its token. Update the token in Settings → Profiles.",
            "{path}: the locked E-AUTH string"
        );
        assert!(!text.contains("stale-token-in-config") && !text.contains("the-real-token-xyz"));
    }
}

// §5.5 / §5.12 E-DOWN: unreachable → 502 with the locked string.
#[tokio::test]
async fn remote_down_is_502_with_the_locked_edown_string_for_every_route() {
    let dead = dead_url();
    let port = dead.rsplit(':').next().expect("port").to_string();
    let env = env_for(&[(L, &dead, T_REMOTE, "dev")]).await;
    let prefix = format!("Can't reach remote profile '{L}' at 127.0.0.1:{port}: ");

    let g1 = get(&env, &format!("/r/{L}/images/{}", img())).await;
    let g2 = get(&env, &format!("/r/{L}/api/uploads/{CONVO}/{FILE}")).await;
    let p = post_upload(&env, &upload_post_path(L, CONVO, "a.png"), b"x".to_vec()).await;

    for (what, resp) in [("image", g1), ("upload get", g2), ("upload post", p)] {
        assert_eq!(resp.status(), 502, "{what}");
        let text = resp.text().await.expect("body");
        assert!(
            text.starts_with(&prefix) && text.ends_with('.'),
            "{what}: locked E-DOWN string, got {text:?}"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// GET /r/{L}/api/uploads/{convo}/{id}
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn upload_get_returns_the_remote_body_status_and_headers() {
    let (a, env) = one().await;
    a.can(
        "GET",
        &format!("/api/uploads/{CONVO}/{FILE}"),
        Canned::new(200, b"%PDF-1.7 not really".to_vec())
            .header("content-type", "application/pdf")
            .header(
                "content-disposition",
                "attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf",
            )
            .header("set-cookie", "x=1"),
    );

    let resp = get(&env, &format!("/r/{L}/api/uploads/{CONVO}/{FILE}")).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        hdr(&resp, "content-type").as_deref(),
        Some("application/pdf")
    );
    assert_eq!(
        hdr(&resp, "content-disposition").as_deref(),
        Some("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf")
    );
    assert!(resp.headers().get("set-cookie").is_none());
    assert_eq!(
        resp.bytes().await.expect("body").as_ref(),
        b"%PDF-1.7 not really"
    );

    let hits = a.hits_for("GET", &format!("/api/uploads/{CONVO}/{FILE}"));
    assert_eq!(hits.len(), 1);
    assert_eq!(
        upstream_header(&hits[0], "cookie"),
        vec![format!("peakbot_token={T_REMOTE}")]
    );
}

#[tokio::test]
async fn upload_get_remote_404_passes_through_as_404() {
    let (a, env) = one().await;
    let resp = get(&env, &format!("/r/{L}/api/uploads/{CONVO}/{FILE}")).await;
    assert_eq!(resp.status(), 404);
    assert_eq!(
        a.hits_for("GET", &format!("/api/uploads/{CONVO}/{FILE}"))
            .len(),
        1
    );
}

// ═══════════════════════════════════════════════════════════════════════
// POST /r/{L}/api/uploads?convo=&name=
// ═══════════════════════════════════════════════════════════════════════

const ATTACHMENT_JSON: &str =
    r#"{"id":"c0ffee00-0000-4000-8000-0000000000aa","name":"a b.png","mime":"image/png","size":5}"#;

// §5.4: the body is forwarded as-is; the remote's JSON comes back byte for
// byte (the remote's id, never re-minted).
#[tokio::test]
async fn upload_post_forwards_the_body_and_returns_the_remote_json_verbatim() {
    let (a, env) = one().await;
    a.can(
        "POST",
        "/api/uploads",
        Canned::new(200, ATTACHMENT_JSON).header("content-type", "application/json"),
    );
    let payload = png_bytes();

    let resp = post_upload(
        &env,
        &upload_post_path(L, CONVO, "a%20b.png"),
        payload.clone(),
    )
    .await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        hdr(&resp, "content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        resp.text().await.expect("body"),
        ATTACHMENT_JSON,
        "the remote's JSON, byte for byte"
    );

    let hits = a.hits_for("POST", "/api/uploads");
    assert_eq!(hits.len(), 1, "one upstream POST: {:?}", a.rec.hits());
    assert_eq!(
        upstream_header(&hits[0], "cookie"),
        vec![format!("peakbot_token={T_REMOTE}")]
    );
    assert_eq!(
        upstream_header(&hits[0], "content-type"),
        vec!["application/octet-stream".to_string()],
        "the remote insists on octet-stream"
    );
    let (_, got) = a
        .bodies()
        .into_iter()
        .find(|(k, _)| k.starts_with("POST /api/uploads"))
        .expect("the remote saw a body");
    assert_eq!(got, payload, "the user's bytes, untouched");
}

// §6.2 "the remote URL is rebuilt from the parsed parts, never copied":
// exactly `convo` + `name`, nothing else — in particular not the local token.
#[tokio::test]
async fn upload_post_rebuilds_the_upstream_query_from_parsed_parts_only() {
    let (a, env) = one().await;
    a.can("POST", "/api/uploads", Canned::new(200, ATTACHMENT_JSON));

    let path = format!(
        "/r/{L}/api/uploads?token={T_LOCAL}&convo={CONVO}&evil=1&name=a%20b%26c.png&name2=x"
    );
    let resp = post_upload(&env, &path, b"12345".to_vec()).await;
    assert_eq!(resp.status(), 200);

    let hits = a.hits_for("POST", "/api/uploads");
    assert_eq!(hits.len(), 1);
    let (path_part, query) = hits[0].uri.split_once('?').expect("upstream query");
    assert_eq!(path_part, "/api/uploads");
    let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("convo".to_string(), CONVO.to_string()),
            ("name".to_string(), "a b&c.png".to_string()),
        ],
        "only convo + name, values decoded and re-encoded safely: {}",
        hits[0].uri
    );
    assert!(!hits[0].uri.contains(T_LOCAL));
}

// §5.4: the response status AND JSON come from the remote (limits are the
// remote's), whatever the status.
#[tokio::test]
async fn upload_post_passes_remote_error_status_and_json_through() {
    let (a, env) = one().await;
    for (status, body) in [
        (413, r#"{"error":"file too large (max 25 MB)"}"#),
        (
            415,
            r#"{"error":"Content-Type must be application/octet-stream"}"#,
        ),
        (400, r#"{"error":"bad request"}"#),
        (404, r#"{"error":"nope"}"#),
    ] {
        a.can(
            "POST",
            "/api/uploads",
            Canned::new(status, body).header("content-type", "application/json"),
        );
        let resp = post_upload(&env, &upload_post_path(L, CONVO, "a.png"), b"x".to_vec()).await;
        assert_eq!(resp.status(), status, "remote {status} passes through");
        assert_eq!(resp.text().await.expect("body"), body);
    }
}

#[tokio::test]
async fn upload_post_remote_401_becomes_502_with_the_locked_auth_string() {
    let a = FakeRemote::start("the-real-token-xyz").await;
    let env = env_for(&[(L, &a.url, "stale-token-in-config", "dev")]).await;

    let resp = post_upload(&env, &upload_post_path(L, CONVO, "a.png"), b"x".to_vec()).await;

    assert_eq!(resp.status(), 502);
    assert_eq!(
        resp.text().await.expect("body"),
        "Remote profile 'home-dev' rejected its token. Update the token in Settings → Profiles."
    );
}

// §9-T8: "streaming both ways" — the first part of the body reaches the
// remote while the browser is still sending the rest.
#[tokio::test]
async fn upload_post_streams_the_body_upstream_while_the_browser_is_still_sending() {
    let (a, env) = one().await;
    a.can("POST", "/api/uploads", Canned::new(200, ATTACHMENT_JSON));
    let body: Vec<u8> = (0..=255u8).cycle().take(512 * 1024).collect();
    let first = 128 * 1024;

    let s = start_post(
        env.addr,
        &upload_post_path(L, CONVO, "big.bin"),
        "application/octet-stream",
        &body,
        first,
    )
    .await;

    assert!(
        a.first_upload_chunk_seen(5).await,
        "the remote must see the first chunk before the browser finished sending"
    );
    let resp = finish_post(s, &body[first..]).await;
    assert_eq!(resp.status, 200, "{}", resp.head);
    let (_, got) = a
        .bodies()
        .into_iter()
        .find(|(k, _)| k.starts_with("POST /api/uploads"))
        .expect("the remote saw the body");
    assert_eq!(got, body);
}

// §9-T8 "a 30 MB upload": no body limit of the proxy's own (axum's 2 MB
// default must be off), bytes intact.
#[tokio::test]
async fn upload_post_of_30_mib_arrives_intact() {
    let (a, env) = one().await;
    a.can("POST", "/api/uploads", Canned::new(200, ATTACHMENT_JSON));
    let body: Vec<u8> = (0..30 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();

    let resp = post_upload(&env, &upload_post_path(L, CONVO, "big.bin"), body.clone()).await;

    assert_eq!(resp.status(), 200);
    let (_, got) = a
        .bodies()
        .into_iter()
        .find(|(k, _)| k.starts_with("POST /api/uploads"))
        .expect("the remote saw the body");
    assert_eq!(got.len(), body.len());
    assert!(got == body, "30 MiB must arrive byte-identical");
}

// ═══════════════════════════════════════════════════════════════════════
// 404 without contacting the remote (§6.2, §9-T8)
// ═══════════════════════════════════════════════════════════════════════

fn assert_untouched(remotes: &[&FakeRemote], what: &str) {
    for r in remotes {
        assert_eq!(r.rec.count(), 0, "{what}: the remote must not be contacted");
    }
}

// `{id}` = ImageId::parse (64 lower-hex + known ext); anything else is a
// 404 before any network.
#[tokio::test]
async fn malformed_image_ids_are_404_without_contacting_the_remote() {
    let (a, env) = one().await;
    let hex = "a1".repeat(32);
    let upper = hex.to_uppercase();
    for id in [
        "abc.png".to_string(),
        hex.to_string(),      // no extension
        format!("{hex}.svg"), // unknown extension
        format!("{hex}.png.exe"),
        format!("{upper}.png"),        // upper-case hex
        format!("{}.png", &hex[..63]), // 63 chars
        format!("{hex}0.png"),         // 65 chars
        "..%2F..%2Fetc%2Fpasswd".to_string(),
        format!("..%2F{hex}.png"),
        format!("%2e%2e%2f{hex}.png"),
        format!("{hex}.png%00"),
    ] {
        let resp = get(&env, &format!("/r/{L}/images/{id}")).await;
        assert_eq!(resp.status(), 404, "image id {id:?}");
    }
    assert_untouched(&[&a], "bad image ids");
}

#[tokio::test]
async fn malformed_upload_uuids_are_404_without_contacting_the_remote() {
    let (a, env) = one().await;
    for (convo, id) in [
        ("not-a-uuid", FILE),
        (CONVO, "not-a-uuid"),
        (CONVO, "..%2F..%2Fapi%2Fconfig"),
        ("..", FILE),
        (CONVO, ".."),
        ("", FILE),
    ] {
        let resp = get(&env, &format!("/r/{L}/api/uploads/{convo}/{id}")).await;
        assert_eq!(resp.status(), 404, "GET convo={convo:?} id={id:?}");
    }
    assert_untouched(&[&a], "bad upload GET uuids");
}

// ASSUMPTION (flagged): the POST's `convo` must be a UUID — a bad or missing
// one is a 404 like the GETs (§9-T8 "bad id / UUID → 404"), not the local
// route's 400.
#[tokio::test]
async fn upload_post_with_a_missing_or_malformed_convo_is_404_without_contacting_the_remote() {
    let (a, env) = one().await;
    for path in [
        format!("/r/{L}/api/uploads?convo=not-a-uuid&name=a.png"),
        format!("/r/{L}/api/uploads?convo=..%2F..&name=a.png"),
        format!("/r/{L}/api/uploads?name=a.png"),
        format!("/r/{L}/api/uploads"),
    ] {
        let resp = post_upload(&env, &path, b"x".to_vec()).await;
        assert_eq!(resp.status(), 404, "{path}");
    }
    assert_untouched(&[&a], "bad upload POST convo");
}

// {profile} = a remote L, else 404 — an unknown name, a LOCAL profile name,
// the remote's own target name, and an encoded slash.
#[tokio::test]
async fn unknown_local_or_foreign_profile_names_are_404_without_contacting_the_remote() {
    let (a, env) = one().await;
    for name in [
        "nope",
        "dev",
        "alt",
        "HOME-DEV",
        "home-dev-2",
        "home-dev%2Fx",
        "%2Fhome-dev",
    ] {
        let g1 = get(&env, &format!("/r/{name}/images/{}", img())).await;
        let g2 = get(&env, &format!("/r/{name}/api/uploads/{CONVO}/{FILE}")).await;
        let p = post_upload(&env, &upload_post_path(name, CONVO, "a.png"), b"x".to_vec()).await;
        assert_eq!(g1.status(), 404, "image via {name:?}");
        assert_eq!(g2.status(), 404, "upload get via {name:?}");
        assert_eq!(p.status(), 404, "upload post via {name:?}");
    }
    assert_untouched(&[&a], "unknown profile names");
}

// SECURITY: browsers collapse `/r/../api/config` to `/api/config`; if a raw
// `.`/`..` segment ever arrives it must be refused with 404 — never proxied,
// never answered by the SPA fallback. Raw sockets: reqwest would normalise.
#[tokio::test]
async fn dot_segments_where_the_profile_belongs_are_404_without_contacting_the_remote() {
    let (a, env) = one().await;
    let id = img();
    for target in [
        "/r/../api/config".to_string(),
        "/r/./api/config".to_string(),
        format!("/r/../images/{id}"),
        format!("/r/./images/{id}"),
        format!("/r/%2e%2e/images/{id}"),
        format!("/r/%2E%2E/images/{id}"),
        format!("/r/%2e/images/{id}"),
        format!("/r/../api/uploads/{CONVO}/{FILE}"),
        format!("/r/%2e%2e/api/uploads/{CONVO}/{FILE}"),
        format!("/r/{L}/../api/config"),
        format!("/r/{L}/images/../../api/config"),
    ] {
        let resp = raw_request(env.addr, "GET", &target).await;
        assert_eq!(
            resp.status,
            404,
            "{target}: got {} {:?}",
            resp.status,
            resp.text()
        );
        assert!(!resp.text().contains("SPA-INDEX"), "{target}: not the SPA");
    }
    for q in ["..", ".", "%2e%2e"] {
        let target = format!("/r/{q}/api/uploads?convo={CONVO}&name=a.png");
        let resp = raw_request(env.addr, "POST", &target).await;
        assert_eq!(resp.status, 404, "POST {target}");
    }
    assert_untouched(&[&a], "dot segments");
}

// ═══════════════════════════════════════════════════════════════════════
// The allowlist is EXACTLY three routes (§6.2)
// ═══════════════════════════════════════════════════════════════════════

// "everything else under /r/ — 404 (the /api/* catch-all style)": nothing
// else reaches the remote, and nothing falls through to the SPA.
#[tokio::test]
async fn everything_else_under_r_is_404_and_never_reaches_the_remote() {
    let (a, env) = one().await;
    let id = img();
    let gets = [
        format!("/r/{L}/api/config"),
        format!("/r/{L}/api/config/reload"),
        format!("/r/{L}/api/settings"),
        format!("/r/{L}/ws"),
        format!("/r/{L}/api/conversations"),
        format!("/r/{L}/api/conversations/{CONVO}/kill"),
        format!("/r/{L}/api/system/status"),
        format!("/r/{L}/commands"),
        format!("/r/{L}/peakbot-ca.crt"),
        format!("/r/{L}/index.html"),
        format!("/r/{L}/"),
        format!("/r/{L}"),
        "/r/".to_string(),
        format!("/r/{L}/images/"),
        format!("/r/{L}/images/{id}/extra"),
        format!("/r/{L}/images"),
        format!("/r/{L}/api/uploads"), // GET on the POST-only path
        format!("/r/{L}/api/uploads/"),
        format!("/r/{L}/api/uploads/{CONVO}"),
        format!("/r/{L}/api/uploads/{CONVO}/{FILE}/extra"),
        format!("/r/{L}/images/{id}/../../api/config"),
        format!("/r/{L}//api/config"),
        format!("/r/{L}/API/uploads/{CONVO}/{FILE}"),
    ];
    for path in &gets {
        let resp = get(&env, path).await;
        assert_eq!(resp.status(), 404, "GET {path}");
        assert!(
            !resp.text().await.expect("body").contains("SPA-INDEX"),
            "{path}"
        );
    }
    assert_untouched(&[&a], "non-allowlisted GETs");
}

// Wrong METHOD on an allowlisted path is also "everything else" → 404.
// ASSUMPTION (flagged): 404, not 405 — the doc has one non-route answer.
#[tokio::test]
async fn wrong_methods_on_the_three_routes_are_404_and_never_reach_the_remote() {
    let (a, env) = one().await;
    let client = http();
    let id = img();
    for (method, path) in [
        ("POST", format!("/r/{L}/images/{id}")),
        ("PUT", format!("/r/{L}/images/{id}")),
        ("DELETE", format!("/r/{L}/images/{id}")),
        ("POST", format!("/r/{L}/api/uploads/{CONVO}/{FILE}")),
        ("DELETE", format!("/r/{L}/api/uploads/{CONVO}/{FILE}")),
        (
            "PUT",
            format!("/r/{L}/api/uploads?convo={CONVO}&name=a.png"),
        ),
        (
            "DELETE",
            format!("/r/{L}/api/uploads?convo={CONVO}&name=a.png"),
        ),
        ("POST", format!("/r/{L}/api/conversations/{CONVO}/kill")),
        ("POST", format!("/r/{L}/api/config")),
        ("POST", format!("/r/{L}/ws")),
    ] {
        let resp = client
            .request(
                method.parse().expect("method"),
                format!("{}{path}", env.base()),
            )
            .header("cookie", local_cookie())
            .body("x")
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 404, "{method} {path}");
    }
    assert_untouched(&[&a], "wrong methods");
}

// ═══════════════════════════════════════════════════════════════════════
// Local auth gate (§6.2: "gated like /api/uploads")
// ═══════════════════════════════════════════════════════════════════════

// GUARD (green today via the harness's real `require_token`; it pins that
// `proxy::router()` must be mounted BEFORE the token layer): no / wrong local
// token → 401 on every /r/ shape, and nothing goes upstream.
#[tokio::test]
async fn without_the_local_token_every_r_route_is_401_and_nothing_goes_upstream() {
    let (a, env) = one().await;
    let id = img();
    let client = http();
    let paths = [
        ("GET", format!("/r/{L}/images/{id}")),
        ("GET", format!("/r/{L}/api/uploads/{CONVO}/{FILE}")),
        (
            "POST",
            format!("/r/{L}/api/uploads?convo={CONVO}&name=a.png"),
        ),
        ("GET", format!("/r/{L}/api/config")),
        ("GET", format!("/r/nope/images/{id}")),
    ];
    for cookie in [None, Some("peakbot_token=wrong-token".to_string())] {
        for (method, path) in &paths {
            let mut req = client
                .request(
                    method.parse().expect("method"),
                    format!("{}{path}", env.base()),
                )
                .body("x");
            if let Some(c) = &cookie {
                req = req.header("cookie", c);
            }
            let resp = req.send().await.expect("request");
            assert_eq!(resp.status(), 401, "{method} {path} with cookie {cookie:?}");
        }
    }
    assert_untouched(&[&a], "no local token");
}

// ═══════════════════════════════════════════════════════════════════════
// Token isolation (§7, RP6)
// ═══════════════════════════════════════════════════════════════════════

// The browser smuggles its LOCAL token every way it can (cookie, ?token=,
// Authorization, extra cookies, X-* headers). Upstream sees exactly the
// remote cookie — and the remote token appears nowhere else.
#[tokio::test]
async fn the_local_token_never_reaches_the_remote_on_any_route() {
    let (a, env) = one().await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes()),
    );
    a.can(
        "GET",
        &format!("/api/uploads/{CONVO}/{FILE}"),
        Canned::new(200, b"f".to_vec()),
    );
    a.can("POST", "/api/uploads", Canned::new(200, ATTACHMENT_JSON));
    let client = http();
    let smuggle = |b: reqwest::RequestBuilder| {
        b.header(
            "cookie",
            format!("{}; other=1; peakbot_token_x={T_LOCAL}", local_cookie()),
        )
        .header("authorization", format!("Bearer {T_LOCAL}"))
        .header("x-peakbot-token", T_LOCAL)
        .header("x-forwarded-for", "10.9.9.9")
    };

    let r1 = smuggle(client.get(format!(
        "{}/r/{L}/images/{}?token={T_LOCAL}&x=1",
        env.base(),
        img()
    )))
    .send()
    .await
    .expect("GET image");
    let r2 = smuggle(client.get(format!(
        "{}/r/{L}/api/uploads/{CONVO}/{FILE}?token={T_LOCAL}",
        env.base()
    )))
    .send()
    .await
    .expect("GET upload");
    let r3 = smuggle(client.post(format!(
        "{}/r/{L}/api/uploads?token={T_LOCAL}&convo={CONVO}&name=a.png",
        env.base()
    )))
    .header("content-type", "application/octet-stream")
    .body("payload")
    .send()
    .await
    .expect("POST upload");
    assert_eq!(
        (
            r1.status().as_u16(),
            r2.status().as_u16(),
            r3.status().as_u16()
        ),
        (200, 200, 200)
    );

    let hits = a.rec.hits();
    assert_eq!(hits.len(), 3, "exactly the three upstream calls: {hits:?}");
    for hit in &hits {
        assert!(
            !hit.uri.contains(T_LOCAL),
            "local token in URL: {}",
            hit.uri
        );
        assert!(
            !hit.uri.contains("x=1") && !hit.uri.contains("token"),
            "query copied: {}",
            hit.uri
        );
        assert_eq!(
            upstream_header(hit, "cookie"),
            vec![format!("peakbot_token={T_REMOTE}")],
            "the ONLY cookie is the remote's: {hit:?}"
        );
        assert!(
            upstream_header(hit, "authorization").is_empty(),
            "Authorization copied: {hit:?}"
        );
        assert!(
            upstream_header(hit, "x-peakbot-token").is_empty(),
            "x-* copied: {hit:?}"
        );
        assert!(
            upstream_header(hit, "x-forwarded-for").is_empty(),
            "x-forwarded-for copied: {hit:?}"
        );
        for (name, value) in &hit.headers {
            assert!(
                !value.contains(T_LOCAL),
                "local token in header {name}: {value}"
            );
            if value.contains(T_REMOTE) {
                assert_eq!(name, "cookie", "remote token outside Cookie: {name}");
            }
        }
    }
}

// The remote's token never travels back to the browser either.
#[tokio::test]
async fn the_remote_token_never_reaches_the_browser() {
    let (a, env) = one().await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes()),
    );

    let ok = get(&env, &format!("/r/{L}/images/{}", img())).await;
    let missing = get(&env, &format!("/r/{L}/images/{}", "b2".repeat(32) + ".png")).await;
    let unknown = get(&env, &format!("/r/nope/images/{}", img())).await;

    for resp in [ok, missing, unknown] {
        for (name, value) in resp.headers() {
            assert!(
                !value.to_str().unwrap_or("").contains(T_REMOTE),
                "remote token in response header {name}"
            );
        }
        let bytes = resp.bytes().await.expect("body");
        assert!(!String::from_utf8_lossy(&bytes).contains(T_REMOTE));
    }
}

// Two remotes: each request goes to ITS server with ITS token, nothing else.
#[tokio::test]
async fn requests_go_to_the_named_profiles_server_with_its_own_token_only() {
    let a = FakeRemote::start(T_REMOTE).await;
    let b = FakeRemote::start(T_REMOTE_B).await;
    let env = env_for(&[
        (L, &a.url, T_REMOTE, "dev"),
        ("vps", &b.url, T_REMOTE_B, "web"),
    ])
    .await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, b"from-A".to_vec()),
    );
    b.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, b"from-B".to_vec()),
    );

    let ra = get(&env, &format!("/r/{L}/images/{}", img())).await;
    let rb = get(&env, &format!("/r/vps/images/{}", img())).await;

    assert_eq!(ra.bytes().await.expect("A").as_ref(), b"from-A");
    assert_eq!(rb.bytes().await.expect("B").as_ref(), b"from-B");
    assert_eq!(a.rec.count(), 1);
    assert_eq!(b.rec.count(), 1);
    assert_eq!(
        upstream_header(&a.rec.hits()[0], "cookie"),
        vec![format!("peakbot_token={T_REMOTE}")]
    );
    assert_eq!(
        upstream_header(&b.rec.hits()[0], "cookie"),
        vec![format!("peakbot_token={T_REMOTE_B}")]
    );
}

// §5.11: `/r/` re-reads the global config on every request — a profile
// added after boot resolves; one removed is a 404 again.
#[tokio::test]
async fn profiles_added_or_removed_in_the_config_apply_on_the_next_request() {
    let a = FakeRemote::start(T_REMOTE).await;
    a.can(
        "GET",
        &format!("/images/{}", img()),
        Canned::new(200, png_bytes()),
    );
    let env = env_for(&[]).await;
    let path = format!("/r/{L}/images/{}", img());

    assert_eq!(get(&env, &path).await.status(), 404, "not configured yet");

    std::fs::write(env.config_path(), yaml(&[(L, &a.url, T_REMOTE, "dev")])).expect("rewrite");
    assert_eq!(
        get(&env, &path).await.status(),
        200,
        "added profile resolves"
    );

    std::fs::write(env.config_path(), yaml(&[])).expect("rewrite");
    assert_eq!(
        get(&env, &path).await.status(),
        404,
        "removed profile is gone"
    );
}
