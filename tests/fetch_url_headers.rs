//! `fetch_url` headers — TOOL-ARGUMENT contract.
//!
//! **ORCHESTRATOR DECISION (supersedes the config-field version this file
//! replaced):** `headers` is an optional ARGUMENT of the `fetch_url` tool
//! call, not a static per-profile config field. The Local agent must call
//! third-party HTTP APIs with per-request auth headers (no named secrets
//! in v1), so the model passes the headers itself, on every call it wants
//! them on.
//!
//! **The locked contract (implemented exactly this way):**
//!
//! Wire shape — an optional JSON object, a string→string map (the simplest
//! shape for models):
//! ```json
//! { "url": "https://api.example.com/v1",
//!   "headers": { "X-Api-Key": "secret", "Authorization": "Bearer t" } }
//! ```
//!
//! Rules, checked in THIS order (all in `parse`, before any request is
//! sent, in BOTH editions — the error is a tool-result text, coachable,
//! never a runtime surprise):
//! 1. at most **20** headers (21 rejected):
//!    `headers: at most 20 headers are allowed (got {n})`
//! 2. each name at most **64 bytes** (bytes, not chars):
//!    `headers: header name is {n} bytes; the limit is 64`
//! 3. forbidden names, case-insensitive — `Host`, `Content-Length`,
//!    `Cookie`, `Transfer-Encoding`, `Connection`:
//!    `headers: '{name}' is a forbidden header name`
//! 4. each value at most **8192 bytes** (8 KiB; 8193 rejected):
//!    `headers: header value for '{name}' is {n} bytes; the limit is 8192`
//!
//! **The pure validation fn (most of the table runs without network):**
//! ```ignore
//! pub fn peakbot::tools::fetch_url::validate_headers(
//!     headers: &std::collections::HashMap<String, String>,
//! ) -> Result<(), String>
//! ```
//! `Ok(())` when the map satisfies every rule; `Err(<exact message>)`
//! otherwise — the message is the tool-result text verbatim (no config
//! path prefix; these are tool errors, returned to the model as tool
//! results via `FetchUrlError::InvalidArgs`).
//!
//! **`FetchUrlArgs`** gains `headers: Option<HashMap<String, String>>`
//! (`#[serde(default)]` — absent = no headers, unchanged behaviour).
//! `parse` runs `validate_headers` and maps `Err` to
//! `FetchUrlError::InvalidArgs`; `call` applies the validated headers to
//! the outgoing request (GET, and POST when `allow_post`).
//!
//! **Schema:** `definition()` in BOTH editions gains the `headers`
//! property (type object of string→string, description mentioning the
//! limits briefly, NOT in `required`). The insta snapshots below pin the
//! full definition for `allow_post` true and false — they MUST show the
//! `headers` parameter. The `.snap` files are created by the builder
//! (`cargo insta accept`) and reviewed; no `.snap.new` is committed.
//!
//! **RED today by COMPILE:** `src/tools/mod.rs` declares `mod fetch_url;`
//! (private) and `validate_headers` does not exist — this target fails to
//! build until the module is `pub` and the fn lands. Every behaviour below
//! is additionally RED by behaviour on top of that.
//!
//! **Removed with the old version:** the config-field parse table
//! (`profiles.<p>.fetch_url.headers` YAML list) and its `Config::parse`
//! fixtures — headers are no longer config. No `Cargo.toml` changes are
//! needed for this file (tokio/rig-core/serde_json are crate deps, insta
//! is a dev-dep with the `json` feature).

use std::collections::HashMap;

use peakbot::config::FetchUrlConfig;
use peakbot::tools::fetch_url::{FetchUrlArgs, FetchUrlError, FetchUrlTool, validate_headers};
use rig_core::tool::Tool;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn args(value: serde_json::Value) -> FetchUrlArgs {
    serde_json::from_value(value).expect("test args deserialize")
}

/// Assert the REJECT case with the EXACT tool-result message.
fn assert_rejected(m: &HashMap<String, String>, exact: &str) {
    let err = validate_headers(m).expect_err("the violating headers must be rejected");
    assert_eq!(
        err, exact,
        "wrong message —\n  got:      {err}\n  expected: {exact}"
    );
}

fn assert_accepted(m: &HashMap<String, String>) {
    validate_headers(m).unwrap_or_else(|e| panic!("valid headers must pass, got: {e}"));
}

// ── count ─────────────────────────────────────────────────────────────────

#[test]
fn validate_headers_empty_map_is_ok() {
    assert_accepted(&map(&[]));
}

#[test]
fn validate_headers_twenty_is_ok() {
    let pairs: Vec<(String, String)> = (0..20)
        .map(|i| (format!("X-H{i:02}"), "v".to_string()))
        .collect();
    assert_accepted(&pairs.into_iter().collect());
}

#[test]
fn validate_headers_twenty_one_is_rejected_with_count_message() {
    let pairs: Vec<(String, String)> = (0..21)
        .map(|i| (format!("X-H{i:02}"), "v".to_string()))
        .collect();
    assert_rejected(
        &pairs.into_iter().collect(),
        "headers: at most 20 headers are allowed (got 21)",
    );
}

// ── name length (bytes, not chars) ────────────────────────────────────────

#[test]
fn validate_headers_name_at_64_bytes_is_ok() {
    assert_accepted(&map(&[(&"a".repeat(64), "v")]));
}

#[test]
fn validate_headers_name_at_65_bytes_is_rejected_with_byte_count() {
    assert_rejected(
        &map(&[(&"a".repeat(65), "v")]),
        "headers: header name is 65 bytes; the limit is 64",
    );
}

#[test]
fn validate_headers_name_limit_counts_bytes_not_chars() {
    // 21 × '€' = 63 UTF-8 bytes ≤ 64 → accepted; 22 × '€' = 66 bytes → rejected.
    assert_accepted(&map(&[(&"€".repeat(21), "v")]));
    assert_rejected(
        &map(&[(&"€".repeat(22), "v")]),
        "headers: header name is 66 bytes; the limit is 64",
    );
}

// ── value length (8 KiB) ──────────────────────────────────────────────────

#[test]
fn validate_headers_value_at_8192_bytes_is_ok() {
    assert_accepted(&map(&[("X-Big", &"v".repeat(8192))]));
}

#[test]
fn validate_headers_value_at_8193_bytes_is_rejected_naming_the_header() {
    assert_rejected(
        &map(&[("X-Big", &"v".repeat(8193))]),
        "headers: header value for 'X-Big' is 8193 bytes; the limit is 8192",
    );
}

// ── forbidden names (case-insensitive) ────────────────────────────────────

#[test]
fn validate_headers_forbidden_names_are_rejected_case_insensitively() {
    for name in [
        "Host",
        "host",
        "HOST",
        "Content-Length",
        "content-length",
        "Cookie",
        "cookie",
        "Transfer-Encoding",
        "transfer-encoding",
        "Connection",
        "cOnNeCtIoN",
    ] {
        assert_rejected(
            &map(&[(name, "v")]),
            &format!("headers: '{name}' is a forbidden header name"),
        );
    }
}

// ── check order ───────────────────────────────────────────────────────────

#[test]
fn validate_headers_count_is_checked_before_per_header_rules() {
    // 21 headers, one of them forbidden → the COUNT error wins.
    let mut pairs: Vec<(String, String)> = (0..20)
        .map(|i| (format!("X-H{i:02}"), "v".to_string()))
        .collect();
    pairs.push(("Host".to_string(), "v".to_string()));
    assert_rejected(
        &pairs.into_iter().collect(),
        "headers: at most 20 headers are allowed (got 21)",
    );
}

#[test]
fn validate_headers_name_length_is_checked_before_forbidden_name() {
    // 65-byte name that ALSO reads as a forbidden name → the length error wins.
    assert_rejected(
        &map(&[(&format!("Host{}", "x".repeat(61)), "v")]),
        "headers: header name is 65 bytes; the limit is 64",
    );
}

#[test]
fn validate_headers_forbidden_name_is_checked_before_value_length() {
    // Forbidden name AND an 8193-byte value → the forbidden-name error wins.
    assert_rejected(
        &map(&[("Cookie", &"v".repeat(8193))]),
        "headers: 'Cookie' is a forbidden header name",
    );
}

#[test]
fn validate_headers_one_bad_among_good_reports_the_bad_one() {
    // Only one violator → the message is deterministic despite HashMap order.
    let mut m = map(&[("X-Ok-1", "v"), ("X-Ok-2", "v")]);
    m.insert("Connection".to_string(), "v".to_string());
    assert_rejected(&m, "headers: 'Connection' is a forbidden header name");
}

#[test]
fn validate_headers_empty_name_passes_the_locked_rules() {
    // The locked rule set is exhaustive: no rule forbids an empty name, so
    // one passes. (Locks the table against "invent an extra rule".)
    assert_accepted(&map(&[("", "v")]));
}

// ── call(): validation runs BEFORE any network IO ─────────────────────────
//
// The URL is 127.0.0.1:9 (discard, closed): if validation were skipped the
// call would attempt a connection and fail with `FetchUrlError::Request`.
// Getting the exact `InvalidArgs` message proves the args were parsed (and
// the headers rejected) before any request was sent.

#[tokio::test]
async fn call_rejects_over_limit_headers_before_any_network_io() {
    let tool = FetchUrlTool::default();
    let mut headers = serde_json::Map::new();
    for i in 0..21 {
        headers.insert(format!("X-H{i:02}"), serde_json::json!("v"));
    }
    let err = tool
        .call(args(
            serde_json::json!({ "url": "http://127.0.0.1:9/x", "headers": headers }),
        ))
        .await
        .expect_err("21 headers must be refused before any request is sent");
    match err {
        FetchUrlError::InvalidArgs(msg) => assert_eq!(
            msg, "headers: at most 20 headers are allowed (got 21)",
            "the tool-result text is the exact validation message"
        ),
        other => panic!("expected InvalidArgs, got: {other:?}"),
    }
}

#[tokio::test]
async fn call_rejects_forbidden_header_name_before_any_network_io() {
    let tool = FetchUrlTool::new(&FetchUrlConfig { allow_post: true });
    let err = tool
        .call(args(serde_json::json!({
            "url": "http://127.0.0.1:9/x",
            "headers": { "Host": "evil.example" }
        })))
        .await
        .expect_err("a forbidden header name must be refused before any request");
    match err {
        FetchUrlError::InvalidArgs(msg) => assert_eq!(
            msg, "headers: 'Host' is a forbidden header name",
            "the tool-result text is the exact validation message"
        ),
        other => panic!("expected InvalidArgs, got: {other:?}"),
    }
}

#[tokio::test]
async fn call_rejects_oversized_header_value_before_any_network_io() {
    let tool = FetchUrlTool::default();
    let err = tool
        .call(args(serde_json::json!({
            "url": "http://127.0.0.1:9/x",
            "headers": { "X-Big": "v".repeat(8193) }
        })))
        .await
        .expect_err("an 8193-byte value must be refused before any request");
    match err {
        FetchUrlError::InvalidArgs(msg) => assert_eq!(
            msg, "headers: header value for 'X-Big' is 8193 bytes; the limit is 8192",
            "the tool-result text is the exact validation message"
        ),
        other => panic!("expected InvalidArgs, got: {other:?}"),
    }
}

#[tokio::test]
async fn call_rejects_non_string_header_value_at_parse() {
    // The map is string→string: a non-string value is a malformed argument,
    // refused at parse — never a request, never a coerced string.
    let result: Result<FetchUrlArgs, _> = serde_json::from_value(serde_json::json!({
        "url": "http://127.0.0.1:9/x",
        "headers": { "X-Num": 42 }
    }));
    assert!(
        result.is_err(),
        "a non-string header value must not deserialize (string→string map)"
    );
}

// ── valid headers are SENT on the request (loopback server) ───────────────
//
// One-shot loopback HTTP server (same pattern as the module's own
// round-trip tests): reads the request (headers until the `\r\n\r\n`
// boundary, then `Content-Length` body bytes), replies `HTTP/1.1 201
// Created` with body `ok`, and hands the captured (headers, body) back
// over a oneshot. No wiremock in the lock — plain tokio TcpListener.

async fn loopback_once() -> (String, tokio::sync::oneshot::Receiver<(String, String)>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        // The body may arrive in the same read as the headers, so scan
        // for the boundary instead of checking the buffer tail.
        let mut header_end = None;
        while header_end.is_none() {
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut chunk)
                .await
                .expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
        }
        let header_end = header_end.expect("header/body boundary");
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut chunk)
                .await
                .expect("read body");
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(content_length);
        let _ = tokio::io::AsyncWriteExt::write_all(
            &mut socket,
            b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok",
        )
        .await;
        let _ = tx.send((headers, String::from_utf8_lossy(&body).into_owned()));
    });
    (format!("http://{addr}"), rx)
}

/// Find a captured request-header value by name (case-insensitive).
fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

#[tokio::test]
async fn get_round_trip_sends_valid_headers() {
    let (base, rx) = loopback_once().await;
    let tool = FetchUrlTool::default(); // GET-only edition — headers work here too
    let out = tool
        .call(args(serde_json::json!({
            "url": format!("{base}/x"),
            "headers": { "X-Api-Key": "secret", "X-Custom": "a b" }
        })))
        .await
        .expect("GET with valid headers succeeds");

    let (headers, _) = rx.await.expect("server captured the request");
    assert_eq!(
        header_value(&headers, "X-Api-Key").as_deref(),
        Some("secret"),
        "the model's X-Api-Key must arrive verbatim; captured: {headers}"
    );
    assert_eq!(
        header_value(&headers, "X-Custom").as_deref(),
        Some("a b"),
        "values keep their spaces; captured: {headers}"
    );
    assert!(
        out.starts_with("HTTP 201 Created\n\nok"),
        "output format is unchanged, got: {out}"
    );
}

#[tokio::test]
async fn post_round_trip_sends_valid_headers_when_allow_post() {
    let (base, rx) = loopback_once().await;
    let tool = FetchUrlTool::new(&FetchUrlConfig { allow_post: true });
    let out = tool
        .call(args(serde_json::json!({
            "url": format!("{base}/x"),
            "method": "POST",
            "body": "{\"a\":1}",
            "headers": { "X-Api-Key": "secret" }
        })))
        .await
        .expect("POST with valid headers succeeds when allow_post");

    let (headers, body) = rx.await.expect("server captured the request");
    let request_line = headers.lines().next().unwrap_or("");
    assert!(
        request_line.starts_with("POST /x"),
        "request line must be POST /x, got: {request_line}"
    );
    assert_eq!(
        header_value(&headers, "X-Api-Key").as_deref(),
        Some("secret"),
        "the model's X-Api-Key must arrive verbatim on POST; captured: {headers}"
    );
    assert_eq!(body, "{\"a\":1}", "the body must arrive verbatim");
    assert!(
        out.starts_with("HTTP 201 Created\n\nok"),
        "output format is unchanged, got: {out}"
    );
}

#[tokio::test]
async fn get_round_trip_without_headers_is_unchanged() {
    // Absent `headers` = no headers (unchanged behaviour).
    let (base, rx) = loopback_once().await;
    let tool = FetchUrlTool::default();
    let out = tool
        .call(args(serde_json::json!({ "url": format!("{base}/x") })))
        .await
        .expect("GET without headers succeeds");

    let (headers, _) = rx.await.expect("server captured the request");
    assert!(
        header_value(&headers, "X-Api-Key").is_none(),
        "no invented headers may appear; captured: {headers}"
    );
    assert!(
        out.starts_with("HTTP 201 Created\n\nok"),
        "output format is unchanged, got: {out}"
    );
}

// ── schema: BOTH editions expose the `headers` parameter ──────────────────

async fn definition(allow_post: bool) -> rig_core::completion::ToolDefinition {
    let tool = if allow_post {
        FetchUrlTool::new(&FetchUrlConfig { allow_post: true })
    } else {
        FetchUrlTool::default()
    };
    tool.definition(String::new()).await
}

fn assert_headers_property(def: &rig_core::completion::ToolDefinition, allow_post: bool) {
    let props = def.parameters["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("allow_post={allow_post}: missing properties object"));
    let headers = props
        .get("headers")
        .unwrap_or_else(|| panic!("allow_post={allow_post}: the `headers` parameter is missing"));
    assert_eq!(headers["type"], "object", "headers is a JSON object");
    assert_eq!(
        headers["additionalProperties"],
        serde_json::json!({ "type": "string" }),
        "headers is a string→string map"
    );
    let desc = headers["description"].as_str().unwrap_or("");
    for limit in ["20", "64", "8192"] {
        assert!(
            desc.contains(limit),
            "allow_post={allow_post}: the headers description must mention the {limit} limit: {desc}"
        );
    }
    let required = def.parameters["required"]
        .as_array()
        .unwrap_or_else(|| panic!("allow_post={allow_post}: missing required array"));
    assert!(
        !required.iter().any(|v| v == "headers"),
        "headers is optional, never required: {required:?}"
    );
}

#[tokio::test]
async fn definition_allow_post_true_exposes_headers_parameter() {
    assert_headers_property(&definition(true).await, true);
}

#[tokio::test]
async fn definition_allow_post_false_exposes_headers_parameter() {
    assert_headers_property(&definition(false).await, false);
}

// ── snapshots: the full definition, both editions ─────────────────────────
//
// These MUST show the `headers` parameter (the assertions above lock that
// independently of the snapshot). The `.snap` files are created by the
// builder (`cargo insta accept`); no `.snap.new` is committed. Until
// accepted these two tests are RED (missing snapshot).

async fn definition_json(allow_post: bool) -> serde_json::Value {
    let def = definition(allow_post).await;
    serde_json::json!({
        "name": def.name,
        "description": def.description,
        "parameters": def.parameters,
    })
}

#[tokio::test]
async fn fetch_url_definition_snapshot_allow_post_true() {
    insta::assert_json_snapshot!(
        "fetch_url_definition_allow_post_true",
        definition_json(true).await
    );
}

#[tokio::test]
async fn fetch_url_definition_snapshot_allow_post_false() {
    insta::assert_json_snapshot!(
        "fetch_url_definition_allow_post_false",
        definition_json(false).await
    );
}
