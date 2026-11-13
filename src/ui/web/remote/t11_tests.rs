//! P3 PR-A A6 — the typed `GetTarget` at the `/r/{profile}/…` boundary
//! (p3-design §F.4). RED until A6 lands: `GetTarget::parse`/`path` are
//! `todo!()` skeletons and the proxy boundary does not parse through them
//! yet.
//!
//! Written against the interface the design specifies, NOT an existing impl.
//! Contract: the boundary parses the percent-decoded tail ONCE into a
//! `GetTarget` — the only two shapes are `images/{id}` (the image cache's
//! validated id) and `api/uploads/{convo}/{id}` (both UUIDs). Anything else
//! is a **local 404 and the upstream is never contacted** (a counting fake
//! upstream proves it). This narrows the Direct path, which today forwards
//! any shape: intended.

use super::t78_support::*;
use super::{GetTarget, proxy};
use uuid::Uuid;

const L: &str = "home-dev";
const CONVO: &str = "11111111-1111-4111-8111-111111111111";
const FILE: &str = "22222222-2222-4222-8222-222222222222";
/// 64 lower-hex + `.png` — a valid image-cache id.
const IMG: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1.png";

// ═══════════════════════════════════════════════════════════════════════
// `GetTarget::parse` — the table (segments already percent-decoded)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn get_target_parse_image_id() {
    let t = GetTarget::parse(&format!("images/{IMG}")).expect("images/{id} parses");
    assert_eq!(t, GetTarget::Image(IMG.to_string()));
}

#[test]
fn get_target_parse_upload() {
    let t = GetTarget::parse(&format!("api/uploads/{CONVO}/{FILE}"))
        .expect("api/uploads/{c}/{id} parses");
    assert_eq!(
        t,
        GetTarget::Upload {
            convo: Uuid::parse_str(CONVO).unwrap(),
            id: Uuid::parse_str(FILE).unwrap(),
        }
    );
}

#[test]
fn get_target_parse_refuses_other_paths() {
    for tail in [
        "",                                                 // bare prefix
        "api/settings",                                     // the spec's example
        "api/conversations",                                // no conversations GET target
        "images",                                           // missing id
        "images/a/b",                                       // extra segment
        "api/uploads",                                      // the POST shape is not a GET target
        "api/uploads/11111111-1111-4111-8111-111111111111", // missing id
        "images/{IMG}/x",                                   // trailing segment
    ] {
        assert_eq!(GetTarget::parse(tail), None, "tail {tail:?} must not parse");
    }
}

#[test]
fn get_target_parse_refuses_dot_segments() {
    for tail in ["..", "../images/x", "images/..", "../.."] {
        assert_eq!(GetTarget::parse(tail), None, "tail {tail:?} must not parse");
    }
}

#[test]
fn get_target_parse_refuses_malformed_image_ids() {
    let hex = "a1".repeat(32);
    for id in [
        "abc.png".to_string(),
        hex.clone(),                   // no extension
        format!("{hex}.svg"),          // unknown extension
        format!("{}.PNG", hex),        // upper-case extension
        format!("{}.png", &hex[..63]), // 63 chars
    ] {
        assert_eq!(
            GetTarget::parse(&format!("images/{id}")),
            None,
            "id {id:?} must not parse"
        );
    }
}

#[test]
fn get_target_parse_refuses_malformed_upload_uuids() {
    for (convo, id) in [("not-a-uuid", FILE), (CONVO, "not-a-uuid"), (CONVO, "")] {
        assert_eq!(
            GetTarget::parse(&format!("api/uploads/{convo}/{id}")),
            None,
            "convo={convo:?} id={id:?} must not parse"
        );
    }
}

#[test]
fn get_target_path_renders_the_upstream_path() {
    assert_eq!(
        GetTarget::Image(IMG.to_string()).path(),
        format!("/images/{IMG}")
    );
    assert_eq!(
        GetTarget::Upload {
            convo: Uuid::parse_str(CONVO).unwrap(),
            id: Uuid::parse_str(FILE).unwrap(),
        }
        .path(),
        format!("/api/uploads/{CONVO}/{FILE}")
    );
}

// ═══════════════════════════════════════════════════════════════════════
// The proxy boundary: the two shapes reach the upstream, everything else
// is a local 404 with the upstream NEVER contacted (counting fake).
// ═══════════════════════════════════════════════════════════════════════

async fn env_for(remotes: &[RemoteSpec<'_>]) -> LocalEnv {
    LocalEnv::start(remotes, Some(proxy::router(std::sync::Arc::new(super::Directory::new())))).await
}

/// One remote `dev` as `home-dev`, healthy; its `rec` counts every hit.
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

fn untouched(r: &FakeRemote, what: &str) {
    assert_eq!(r.rec.count(), 0, "{what}: the remote must not be contacted");
}

#[tokio::test]
async fn proxy_image_path_reaches_the_upstream_as_a_target() {
    let (a, env) = one().await;
    let png = vec![0x89, b'P', b'N', b'G'];
    a.can(
        "GET",
        &format!("/images/{IMG}"),
        Canned::new(200, png.clone()).header("content-type", "image/png"),
    );

    let resp = get(&env, &format!("/r/{L}/images/{IMG}")).await;
    let status = resp.status();
    let body = resp.bytes().await.expect("body");
    assert_eq!(status, 200);
    assert_eq!(&body[..], &png[..], "the upstream body comes through");
    assert_eq!(
        a.hits_for("GET", &format!("/images/{IMG}")).len(),
        1,
        "the typed target rendered to the exact upstream path"
    );
}

#[tokio::test]
async fn proxy_upload_path_reaches_the_upstream_as_a_target() {
    let (a, env) = one().await;
    a.can(
        "GET",
        &format!("/api/uploads/{CONVO}/{FILE}"),
        Canned::new(200, b"hello".to_vec()),
    );

    let resp = get(&env, &format!("/r/{L}/api/uploads/{CONVO}/{FILE}")).await;
    let status = resp.status();
    let body = resp.bytes().await.expect("body");
    assert_eq!(status, 200);
    assert_eq!(&body[..], b"hello", "the upstream body comes through");
    assert_eq!(
        a.hits_for("GET", &format!("/api/uploads/{CONVO}/{FILE}"))
            .len(),
        1,
        "the typed target rendered to the exact upstream path"
    );
}

#[tokio::test]
async fn proxy_other_tail_is_local_404_and_upstream_never_contacted() {
    let (a, env) = one().await;
    let resp = get(&env, &format!("/r/{L}/api/settings")).await;
    assert_eq!(resp.status(), 404, "unparseable tail → LOCAL 404");
    untouched(&a, "api/settings");
}

#[tokio::test]
async fn proxy_encoded_traversal_is_local_404_and_upstream_never_contacted() {
    let (a, env) = one().await;
    // Encoded traversal in the tail (`%2e%2e` → `..`, `%2f` → `/`): the
    // decoded segments must not parse to any target.
    for tail in [
        format!("/r/{L}/%2e%2e/images/{IMG}"),
        format!("/r/{L}/images/%2e%2e%2f{IMG}"),
        format!("/r/{L}/api/uploads/%2e%2e/{FILE}"),
    ] {
        let resp = get(&env, &tail).await;
        assert_eq!(resp.status(), 404, "tail {tail} → LOCAL 404");
    }
    untouched(&a, "encoded traversal");
}
