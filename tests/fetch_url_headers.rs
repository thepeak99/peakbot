//! `fetch_url.headers` parse table + schema snapshots (mobile plan §2.8 /
//! §4.3 test 4) — RED.
//!
//! The Local tool set gains `fetch_url.headers` (both editions): static
//! request headers configured per profile, validated **at parse** — a
//! config with a violating header is a config parse error, never a
//! runtime surprise.
//!
//! **RED today by BEHAVIOUR** (the file compiles in both editions):
//! `FetchUrlConfig` has `deny_unknown_fields` and no `headers` field, so
//! EVERY row below fails today — the OK rows with "unknown field `headers`"
//! and the rejected rows with the wrong (unknown-field) message. That is
//! the expected RED: the feature is absent.
//!
//! **The locked contract (builder L6 implements exactly this):**
//!
//! YAML shape — a list of `{name, value}` pairs under the profile:
//! ```yaml
//! profiles:
//!   dev:
//!     fetch_url:
//!       allow_post: true
//!       headers:
//!         - { name: X-Api-Key, value: secret }
//! ```
//!
//! Limits + precise error messages (asserted verbatim below; the config
//! error machinery may prefix a dotted path, the message itself is exact):
//! * at most **20** headers:
//!   `fetch_url.headers: at most 20 headers are allowed (got {n})`
//! * name at most **64 bytes** (bytes, not chars):
//!   `fetch_url.headers: header name is {n} bytes; the limit is 64`
//! * value at most **8192 bytes** (8 KiB):
//!   `fetch_url.headers: header value for '{name}' is {n} bytes; the limit is 8192`
//! * forbidden names (case-insensitive): `Host`, `Content-Length`, `Cookie`,
//!   `Transfer-Encoding`, `Connection`:
//!   `fetch_url.headers: '{name}' is a forbidden header name`
//!
//! Check order: count first, then per header in config order — name length,
//! forbidden name, value length. Absent `headers` = no headers (unchanged).
//! The headers are request-static: the `fetch_url` TOOL SCHEMA does NOT
//! change (the model never passes them), which the insta snapshots below
//! pin for `allow_post` true and false.
//!
//! **Snapshot note:** `tests/snapshots/fetch_url_headers__*.snap` do not
//! exist yet — the builder creates them by running this target and
//! accepting the new snapshots (`cargo insta accept`), reviewing that they
//! match today's pinned schema. No `.snap.new` files are committed.

use std::path::Path;

use peakbot::config::resolve::{Config, ParseCtx, Source};
use peakbot::config::ConfigErrors;
use peakbot::tools::FetchUrlTool;
use rig_core::tool::Tool;

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

/// One provider + one profile `dev` carrying the `fetch_url` block.
fn global_yaml(allow_post: bool, headers_yaml: &str) -> String {
    format!(
        "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  dev:\n    fetch_url:\n      allow_post: {allow_post}\n{headers_yaml}    agents:\n      main:\n        model: a\n"
    )
}

/// `- { name: <n>, value: <v> }` lines, indented under `fetch_url:`.
fn header_lines(headers: &[(String, String)]) -> String {
    let mut out = String::from("      headers:\n");
    for (n, v) in headers {
        out.push_str(&format!("        - {{ name: \"{n}\", value: \"{v}\" }}\n"));
    }
    out
}

fn parse(yaml: &str) -> Result<Config, ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
}

/// The first validation error's message, for asserting the EXACT text.
fn first_error_message(err: &ConfigErrors) -> (String, String) {
    let e = &err.0[0];
    (e.dotted_path(), e.message.clone())
}

fn assert_rejected(allow_post: bool, headers: &[(String, String)], exact: &str) {
    let yaml = global_yaml(allow_post, &header_lines(headers));
    let err = parse(&yaml).expect_err("the violating config must not parse");
    let (path, message) = first_error_message(&err);
    assert!(
        message == exact,
        "wrong message — path `{path}`:\n  got:      {message}\n  expected: {exact}"
    );
}

fn assert_accepted(allow_post: bool, headers: &[(String, String)]) {
    let yaml = global_yaml(allow_post, &header_lines(headers));
    parse(&yaml).unwrap_or_else(|e| panic!("valid headers must parse, got: {e:?}"));
}

fn h(n: &str, v: &str) -> (String, String) {
    (n.to_string(), v.to_string())
}

// ── count ─────────────────────────────────────────────────────────────────

#[test]
fn no_headers_is_accepted() {
    parse(&global_yaml(false, ""))
        .unwrap_or_else(|e| panic!("absent headers must parse (unchanged), got: {e:?}"));
}

#[test]
fn twenty_headers_are_accepted() {
    let headers: Vec<(String, String)> =
        (0..20).map(|i| h(&format!("X-H{i:02}"), "v")).collect();
    assert_accepted(true, &headers);
}

#[test]
fn twenty_one_headers_are_rejected() {
    let headers: Vec<(String, String)> =
        (0..21).map(|i| h(&format!("X-H{i:02}"), "v")).collect();
    assert_rejected(
        true,
        &headers,
        "fetch_url.headers: at most 20 headers are allowed (got 21)",
    );
}

// ── name length (bytes, not chars) ────────────────────────────────────────

#[test]
fn header_name_at_64_bytes_is_accepted() {
    assert_accepted(false, &[h(&"a".repeat(64), "v")]);
}

#[test]
fn header_name_at_65_bytes_is_rejected() {
    assert_rejected(
        false,
        &[h(&"a".repeat(65), "v")],
        "fetch_url.headers: header name is 65 bytes; the limit is 64",
    );
}

#[test]
fn name_limit_counts_bytes_not_chars() {
    // 21 × '€' = 63 UTF-8 bytes ≤ 64 → accepted; 22 × '€' = 66 bytes → rejected.
    assert_accepted(false, &[h(&"€".repeat(21), "v")]);
    assert_rejected(
        false,
        &[h(&"€".repeat(22), "v")],
        "fetch_url.headers: header name is 66 bytes; the limit is 64",
    );
}

// ── value length (8 KiB) ──────────────────────────────────────────────────

#[test]
fn header_value_at_8192_bytes_is_accepted() {
    assert_accepted(false, &[h("X-Big", &"v".repeat(8192))]);
}

#[test]
fn header_value_at_8193_bytes_is_rejected() {
    assert_rejected(
        false,
        &[h("X-Big", &"v".repeat(8193))],
        "fetch_url.headers: header value for 'X-Big' is 8193 bytes; the limit is 8192",
    );
}

// ── forbidden names (case-insensitive) ────────────────────────────────────

#[test]
fn forbidden_header_names_are_rejected_case_insensitively() {
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
            false,
            &[h(name, "v")],
            &format!("fetch_url.headers: '{name}' is a forbidden header name"),
        );
    }
}

// ── schema snapshots: headers are request-static, the schema does NOT ─────
//
// The `fetch_url` tool definition (name/description/parameters) is
// snapshotted for `allow_post` true and false. L6 must not change the
// schema — the model never passes headers. The `.snap` files are created
// by the builder (cargo insta accept) and reviewed; no `.snap.new` is
// committed. Until accepted these two tests are RED (missing snapshot).

async fn definition_json(allow_post: bool) -> serde_json::Value {
    let yaml = global_yaml(allow_post, "");
    let cfg = parse(&yaml).expect("no-headers config parses");
    let profile = cfg.catalog.profile("dev").expect("profile dev");
    let def = FetchUrlTool::new(&profile.fetch_url)
        .definition(String::new())
        .await;
    serde_json::json!({
        "name": def.name,
        "description": def.description,
        "parameters": def.parameters,
    })
}

#[tokio::test]
async fn fetch_url_definition_snapshot_allow_post_true() {
    insta::assert_json_snapshot!("fetch_url_definition_allow_post_true", definition_json(true).await);
}

#[tokio::test]
async fn fetch_url_definition_snapshot_allow_post_false() {
    insta::assert_json_snapshot!("fetch_url_definition_allow_post_false", definition_json(false).await);
}
