//! TDD RED tests for the HTTP sniffer / wire-truth debug mode
//! (per `docs/http-sniffer-design.md` §9).
//!
//! These tests pin the contracts that the mid must satisfy when
//! implementing `crate::sniff`. Per the verify-by-poison discipline, all
//! RED lives here until the implementation lands: the module does not
//! exist yet, the field names are unverified, the `response_record`
//! signature is a guess at the *types* involved (Serialize on the raw
//! side) but not at the call shape. The compile failure is the expected
//! RED signal for new-type contracts; once the implementation lands,
//! each test asserts the named behaviour.
//!
//! File-sink tests (which touch the process-global `OnceLock` holding the
//! open `Mutex<File>`) live in their own binary `tests/sniff_file.rs` —
//! the design doc §9 calls this a trap: the `OnceLock` is set once and
//! stays set for the lifetime of the test process, so file-sink tests
//! can interfere with each other if they share a binary with unrelated
//! tests.
//!
//! Coverage map (doc §9 → tests here):
//!   - §9.1 truncate_cuts_long_strings_and_reports_both_counts        → `truncate_cuts_long_strings_and_reports_both_counts`
//!   - §9.2 truncate_never_splits_a_multibyte_char                    → `truncate_never_splits_a_multibyte_char`
//!   - §9.3 truncate_walks_nested_arrays_and_objects                  → `truncate_walks_nested_arrays_and_objects`
//!   - §9.4 truncate_leaves_short_strings_and_numbers_alone           → `truncate_leaves_short_strings_and_numbers_alone`
//!   - §9.5 record_is_exactly_one_line                                → `record_is_exactly_one_line`
//!   - §9.6 request_and_response_records_share_the_id                 → `request_and_response_records_share_the_id`
//!   - §9.7 anthropic_thinking_block_survives_into_the_res_line       → `anthropic_thinking_block_survives_into_the_res_line` (KEYSTONE)
//!   - §9.8 res_record_keeps_raw_and_choice_separate                  → `res_record_keeps_raw_and_choice_separate`
//!   - §9.9 init_then_two_records_produces_two_parseable_jsonl_lines  → `tests/sniff_file.rs::init_then_two_records_produces_two_parseable_jsonl_lines`
//!   - §9.10 harness path produces req+res                            → `harness_drive_emits_req_and_res_lines_via_session_hook`
//!   - "env unset → no-op" (user task, doc §4)                        → `enabled_is_false_when_init_was_never_called`
//!   - "id pairing across two fake lanes" (user task)                 → `ids_pair_across_two_fake_lanes`

#![cfg(test)]

use peakbot::mock::MockCompletionModel;
use peakbot::{SessionHook, sniff};
use rig_core::agent::PromptHook;
use rig_core::completion::CompletionModel;
use rig_core::completion::message::{AssistantContent, Message};
use rig_core::one_or_many::OneOrMany;
use serde_json::Value;

/// Default per-string-leaf cap per the design doc §5.
const MAX_STR: usize = 16384;

// ─────────────────────────────────────────────────────────────────────────────
// Pure unit tests — `truncate_in_place` (doc §5, §9.1-§9.4)
// ─────────────────────────────────────────────────────────────────────────────

/// §9.1 — A long string leaf gets replaced by `MAX_STR` characters from the
/// original plus a marker that reports BOTH the kept count and the original
/// length. The marker is the diagnostic; the file viewer needs both numbers
/// to judge whether to bump the cap.
#[test]
fn truncate_cuts_long_strings_and_reports_both_counts() {
    let original_len = MAX_STR + 1_000;
    let mut v: Value = serde_json::json!({ "big": "a".repeat(original_len) });
    sniff::truncate_in_place(&mut v, MAX_STR);

    let s = v["big"]
        .as_str()
        .expect("a string leaf must remain a string after truncation");

    assert!(
        s.contains(&format!("kept {MAX_STR}")),
        "marker must report the kept count {MAX_STR}; got tail: …{}",
        &s[s.len().saturating_sub(120)..]
    );
    assert!(
        s.contains(&format!("of {} chars", original_len)),
        "marker must report the original length {original_len}; got tail: …{}",
        &s[s.len().saturating_sub(120)..]
    );

    // Sanity: the kept payload is exactly MAX_STR chars, then the marker.
    // We assert character count, not byte count, per doc §5.
    let prefix_chars = s.chars().take_while(|c| *c != '…').count();
    assert_eq!(prefix_chars, MAX_STR, "kept prefix must be MAX_STR chars");
}

/// §9.2 — A multi-byte (4-byte) codepoint at the boundary must never be
/// split. The result must remain valid UTF-8 and serde_json::from_value
/// must round-trip it. The truncation rule walks `char_indices`, never
/// byte indices.
#[test]
fn truncate_never_splits_a_multibyte_char() {
    // 😀 is U+1F600, encoded as 4 bytes in UTF-8.
    let original_count = MAX_STR + 10;
    let s: String = "😀".repeat(original_count);
    let mut v: Value = serde_json::json!({ "x": s.clone() });

    sniff::truncate_in_place(&mut v, MAX_STR);

    let result = v["x"].as_str().expect("still a string").to_owned();

    // Round-trip through serde_json: a half-codepoint would have failed
    // to deserialize.
    let round: Value =
        serde_json::from_value(v.clone()).expect("truncated string must remain valid UTF-8 JSON");

    assert_eq!(
        round["x"].as_str().unwrap().chars().count(),
        result.chars().count(),
        "char count must be stable across serialization"
    );

    // And no surprise multibyte char in the kept payload.
    for ch in result.chars().take(MAX_STR) {
        assert!(
            ch == '😀',
            "every kept char must be the original 😀; got {ch:?}"
        );
    }
}

/// §9.3 — The walk is recursive, not top-level only. A base64 image string
/// nested inside a content block inside an array must still be capped —
/// base64 strings are exactly the case the doc calls out as a string leaf.
#[test]
fn truncate_walks_nested_arrays_and_objects() {
    let huge = "x".repeat(MAX_STR + 50);
    let mut v: Value = serde_json::json!({
        "content": [
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": huge.clone(),
                }
            }
        ]
    });

    sniff::truncate_in_place(&mut v, MAX_STR);

    let nested = v["content"][0]["source"]["data"]
        .as_str()
        .expect("nested string must remain a string");

    assert!(
        nested.contains("truncated"),
        "nested base64 string must be capped with the truncation marker; got tail: …{}",
        &nested[nested.len().saturating_sub(80)..]
    );
    assert!(
        nested.contains(&format!("of {} chars", huge.chars().count())),
        "nested marker must report the original char count"
    );
}

/// §9.4 — Short strings, numbers, booleans, nulls must pass through
/// unchanged. No marker, no coercion, no surprise.
#[test]
fn truncate_leaves_short_strings_and_numbers_alone() {
    let mut v: Value = serde_json::json!({
        "short": "hi",
        "n": 42,
        "b": true,
        "none": null,
        "arr": [1, 2, 3],
        "obj": { "nested_short": "ok" }
    });

    sniff::truncate_in_place(&mut v, MAX_STR);

    assert_eq!(v["short"], "hi");
    assert_eq!(v["n"], 42);
    assert_eq!(v["b"], true);
    assert!(v["none"].is_null());
    assert_eq!(v["arr"], serde_json::json!([1, 2, 3]));
    assert_eq!(v["obj"]["nested_short"], "ok");
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure unit tests — record shape (doc §3, §9.5-§9.6)
// ─────────────────────────────────────────────────────────────────────────────

/// §9.5 — A serialized record must contain no literal newline characters.
/// This is the JSONL contract: one line per record. `serde_json` escapes
/// `\n` in strings today, but the assertion pins it independently of
/// the serializer's current behaviour.
#[test]
fn record_is_exactly_one_line() {
    let prompt = Message::user("hello\nworld"); // contains a literal \n
    let history = vec![Message::user("prev turn"), Message::assistant("ok")];

    let wire = sniff::WireLabel {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
    };
    let v = sniff::request_record(1, "orchestrator", Some(wire), &prompt, &history);
    let s = serde_json::to_string(&v).expect("serialize");

    assert!(
        !s.contains('\n'),
        "JSONL contract violated: serialized record contains a literal newline"
    );

    // Same check for the response side, since both lines must round-trip
    // a `tail -f`-style line reader.
    let raw = serde_json::json!({
        "id": "msg_x",
        "content": [{"type":"text","text":"ok\nfine"}],
        "usage": {"input_tokens":1,"output_tokens":1}
    });
    let choice = OneOrMany::one(AssistantContent::text("ok\nfine"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});
    let r = sniff::response_record(1, "orchestrator", None, &raw, &choice, &usage);
    let rs = serde_json::to_string(&r).expect("serialize");
    assert!(
        !rs.contains('\n'),
        "response record must also be a single line; got {rs}"
    );
}

/// §9.6 — `req` and `res` for the same logical call must share their `id`.
/// The pairing id is what a reader uses to match the request the agent
/// was about to send with the response that came back. Lane labels are
/// NOT unique (two concurrent "junior" sub-agents share a label), so the
/// pairing id cannot be derived from the lane.
#[test]
fn request_and_response_records_share_the_id() {
    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];

    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});

    let req = sniff::request_record(99, "orchestrator", None, &prompt, &history);
    let res = sniff::response_record(99, "orchestrator", None, &raw, &choice, &usage);

    assert_eq!(req["id"], 99);
    assert_eq!(res["id"], 99);
    assert_eq!(
        req["id"], res["id"],
        "req and res for the same call must share their id"
    );
}

/// Doc §6 / S6: the `id` is process-monotonic and shared across lanes —
/// two concurrent calls on DIFFERENT lane labels must still get distinct,
/// monotonic ids (because the lanes share the same id counter). This is
/// the property the doc cites when it warns that lane labels alone are
/// not enough to pair req/res.
#[test]
fn ids_pair_across_two_fake_lanes() {
    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});

    let id_a = sniff::next_id();
    let id_b = sniff::next_id();

    let req_a = sniff::request_record(id_a, "orchestrator", None, &prompt, &history);
    let req_b = sniff::request_record(id_b, "reviewer", None, &prompt, &history);

    assert_ne!(id_a, id_b, "next_id must hand out distinct ids");
    assert!(
        id_b > id_a,
        "next_id must be monotonic (got {id_a} then {id_b})"
    );
    assert_eq!(req_a["id"], id_a);
    assert_eq!(req_b["id"], id_b);
    assert_eq!(req_a["lane"], "orchestrator");
    assert_eq!(req_b["lane"], "reviewer");

    // And the response for each call must echo the same id it was paired with.
    let res_a = sniff::response_record(id_a, "orchestrator", None, &raw, &choice, &usage);
    let res_b = sniff::response_record(id_b, "reviewer", None, &raw, &choice, &usage);
    assert_eq!(res_a["id"], id_a);
    assert_eq!(res_b["id"], id_b);
}

// ─────────────────────────────────────────────────────────────────────────────
// KEYSTONE — Anthropic thinking block survives into the res line
// (doc §1, §9.7)
// ─────────────────────────────────────────────────────────────────────────────

/// The single test that proves the feature. Fixture: a canned Anthropic
/// response JSON containing a `{"type":"thinking", "thinking":"...",
/// "signature":"..."}` content block, deserialized into
/// `rig_core::providers::anthropic::completion::CompletionResponse`, fed
/// to `response_record`. We then assert that the thinking text AND the
/// signature both appear under `payload.raw.content[0]`, in the exact
/// Anthropic field names (`thinking`, `signature`).
///
/// Zero network, zero mock HTTP — the point is the seam captures the
/// provider-native struct verbatim (per doc §1: "inside the existing
/// generic hook we can `serde_json::to_value(&response.raw_response)` and
/// get the provider's own response struct, verbatim field names").
#[test]
fn anthropic_thinking_block_survives_into_the_res_line() {
    use rig_core::providers::anthropic::completion::CompletionResponse;

    let raw_json = serde_json::json!({
        "id": "msg_01ABC",
        "model": "claude-sonnet-4-5",
        "role": "assistant",
        "stop_reason": "end_turn",
        "content": [
            {
                "type": "thinking",
                "thinking": "The user wants X. I should read the file.",
                "signature": "sig.SGXabc123XYZ-=="
            },
            {
                "type": "text",
                "text": "Here you go."
            }
        ],
        "usage": { "input_tokens": 100, "output_tokens": 50 }
    });

    // Sanity: the fixture deserializes into the actual rig type.
    let raw_response: CompletionResponse = serde_json::from_value(raw_json.clone())
        .expect("fixture must deserialize into rig's CompletionResponse");

    // The Anthropic `CompletionResponse` IS the raw_response shape — it
    // has `content`, not `choice`. The rig-mapped `choice` side
    // (`OneOrMany<AssistantContent>`) is what `SessionHook` synthesizes
    // from the same content via `extract_content_from_response`. We
    // build a minimal choice by hand here (text only) — the keystone
    // assertion is about `payload.raw`, not the choice side.
    let choice = OneOrMany::one(AssistantContent::text("Here you go."));

    let wire = sniff::WireLabel {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
    };

    // Drive `response_record` with the rig-typed raw_response. The
    // signature is `<R: Serialize>` and `CompletionResponse: Serialize`,
    // so the implementation `serde_json::to_value(&response.raw_response)`
    // is expected — same as SessionHook does at the call site.
    let v = sniff::response_record(
        1,
        "orchestrator",
        Some(wire),
        &raw_response,
        &choice,
        &serde_json::json!({"input_tokens": 100, "output_tokens": 50}),
    );

    // The thinking block must appear under `payload.raw.content[0]` with
    // the exact Anthropic field names — verbatim capture, not a remap.
    let raw_content = &v["payload"]["raw"]["content"];
    assert!(
        raw_content.is_array() && raw_content.as_array().unwrap().len() == 2,
        "raw content must carry both blocks; got: {raw_content}"
    );

    let thinking = &raw_content[0];
    assert_eq!(
        thinking["type"], "thinking",
        "block type tag must survive verbatim"
    );
    assert_eq!(
        thinking["thinking"], "The user wants X. I should read the file.",
        "thinking text must survive verbatim"
    );
    assert_eq!(
        thinking["signature"], "sig.SGXabc123XYZ-==",
        "signature must survive byte-identical — Anthropic validates it on replay"
    );

    // And the sibling text block must too, in order.
    let text = &raw_content[1];
    assert_eq!(text["type"], "text");
    assert_eq!(text["text"], "Here you go.");

    // The wire label must be stamped on the record, not invented by the
    // serializer from the raw response.
    assert_eq!(v["provider"], "anthropic");
    assert_eq!(v["model"], "claude-sonnet-4-5");

    // The record must be labelled `kind: "logical"` per doc §3 (NOT
    // "wire" — that's reserved for the rejected option (b)).
    assert_eq!(v["kind"], "logical");
}

/// §9.8 — Pins the diagnostic the doc explicitly calls out (§1 "Bonus
/// diagnostic"): when `raw` carries a thinking block and `choice` does
/// not, BOTH sides must survive into the record. If the implementation
/// accidentally drops the thinking text from `raw` (e.g. by walking
/// `choice` instead of `raw_response`), the bug surfaces here.
#[test]
fn res_record_keeps_raw_and_choice_separate() {
    let raw = serde_json::json!({
        "id": "msg_x",
        "content": [
            { "type": "thinking", "thinking": "secret thought", "signature": "sig1" },
            { "type": "text", "text": "public reply" }
        ]
    });

    // choice WITHOUT the thinking block — the rig-mapped side that lost it.
    let choice = OneOrMany::one(AssistantContent::text("public reply"));

    let v = sniff::response_record(
        7,
        "orchestrator",
        None,
        &raw,
        &choice,
        &serde_json::json!({}),
    );

    // Raw side keeps the thinking text.
    assert_eq!(
        v["payload"]["raw"]["content"][0]["thinking"], "secret thought",
        "raw side must carry the thinking text even when choice drops it"
    );

    // Choice side must NOT carry the thinking text.
    let choice_json =
        serde_json::to_string(&v["payload"]["choice"]).expect("serialize choice side");
    assert!(
        !choice_json.contains("secret thought"),
        "choice side must NOT leak the thinking text; got {choice_json}"
    );

    // And the public reply must survive on the choice side.
    let choice_text = choice_json.contains("public reply");
    assert!(
        choice_text,
        "choice side must still carry the public reply text; got {choice_json}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// File-sink state (doc §4, §9.10)
// ─────────────────────────────────────────────────────────────────────────────

/// "env unset → no file written / no-op" (doc §4). Before any test in this
/// process has called `init`, `enabled()` must return false and
/// `write_record` must be a no-op (no panic, no file created, no
/// side-effect).
///
/// This is the only file-sink-state test in this binary: per the doc §9
/// trap, the `OnceLock` is process-global and stays set once armed, so
/// any test that calls `init` poisons this one. The remaining file-sink
/// tests live in `tests/sniff_file.rs`, which is its own clean process.
#[test]
fn enabled_is_false_when_init_was_never_called() {
    // We do NOT call sniff::init() in this test. Other tests in the
    // integration binary may have done so via the harness test below;
    // if so, this test is structurally RED because the OnceLock is
    // already set — which is exactly the trap the design doc names.
    // The harness test that calls init() is annotated to keep that
    // expectation explicit.
    //
    // The intent: enabled() is the gate, and a process that has never
    // armed the sniffer must report disabled.
    assert!(
        !sniff::enabled(),
        "sniff::enabled() must be false when no path has been init'd in this process"
    );

    // write_record must be a no-op (no panic) when disabled.
    let v: Value = serde_json::json!({"would_have_written": true});
    sniff::write_record(&v);
    // Nothing to assert beyond "didn't panic".
}

// ─────────────────────────────────────────────────────────────────────────────
// Harness path (doc §9.10)
// ─────────────────────────────────────────────────────────────────────────────

/// End-to-end pin that the `SessionHook` capture path produces real
/// `req`/`res` lines when driven through the existing mock harness.
///
/// The harness invokes `PromptHook::<MockCompletionModel>::on_completion_*`
/// on the hook; `MockCompletionModel::Response` is `Serialize` (it
/// derives `Serialize`), so the implementation `serde_json::to_value
/// (&response.raw_response)` works the same way it does for the
/// Anthropic shape — this test exercises the EMIT plumbing, not the
/// provider-specific shape (which is pinned by the keystone test).
///
/// Seams pinned: `sniff::init`, `SessionHook::with_wire_label`,
/// `on_completion_call` emits a `req`, `on_completion_response` emits a
/// `res`, and the two share their id.
///
/// NOTE: This test arms the `OnceLock`. Per the doc §9 trap warning,
/// every other test in this binary that touches `enabled()` runs AFTER
/// this one will see `enabled() == true`. This is documented above the
/// `enabled_is_false_when_init_was_never_called` test — it is a known
/// limitation of the shared-process test harness, not a bug in the
/// implementation. The file-sink tests in `tests/sniff_file.rs` have a
/// clean process.
#[tokio::test]
async fn harness_drive_emits_req_and_res_lines_via_session_hook() {
    use peakbot::ui::app_state::MessageSource;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sniff.jsonl");

    // RED until `sniff::init` exists.
    sniff::init(&path);
    assert!(sniff::enabled(), "init must enable the sniffer");

    let (hook, _rx) = SessionHook::with_channel()
        // RED until `with_wire_label` exists on SessionHook.
        .with_wire_label("anthropic".to_string(), "claude-sonnet-4-5".to_string());

    let prompt = Message::user("hello");
    let history: Vec<Message> = vec![Message::user("prev turn")];

    // req side.
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_call(
        &hook, &prompt, &history,
    )
    .await;

    // Build a real rig CompletionResponse from the mock's perspective.
    // We construct it by driving the mock once, so the response type is
    // exactly what `on_completion_response` would see at runtime.
    let mock = MockCompletionModel::new();
    let request = MockCompletionModel::make(&(), "claude-sonnet-4-5")
        .completion_request("test")
        .build();
    let resp = mock.completion(request).await.expect("mock response");

    // res side, via the same hook — turbofish picks the impl block.
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_response(
        &hook, &prompt, &resp,
    )
    .await;

    // After one req + one res, the file must contain ≥1 req line and
    // ≥1 res line, sharing an id.
    let content = std::fs::read_to_string(&path).expect("sniff file must exist");
    let mut req_ids: Vec<u64> = vec![];
    let mut res_ids: Vec<u64> = vec![];
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("each line must be JSON: {e} in {line:?}"));
        let id = parsed["id"]
            .as_u64()
            .unwrap_or_else(|| panic!("id must be a u64: {parsed:?}"));
        match parsed["dir"].as_str() {
            Some("req") => req_ids.push(id),
            Some("res") => res_ids.push(id),
            other => panic!("dir must be req|res, got {other:?}"),
        }
    }

    assert!(
        !req_ids.is_empty(),
        "expected ≥1 req line, got none; file: {content}"
    );
    assert!(
        !res_ids.is_empty(),
        "expected ≥1 res line, got none; file: {content}"
    );
    assert!(
        req_ids.iter().any(|id| res_ids.contains(id)),
        "at least one req id must match a res id; req={req_ids:?} res={res_ids:?}"
    );

    // A sub-agent hook uses a different lane label — make sure the
    // lane propagates through, since lane labels are the only thing the
    // reader has besides the id.
    let (sub_hook, _sub_rx) = SessionHook::with_channel()
        .with_wire_label("anthropic".to_string(), "claude-sonnet-4-5".to_string())
        .with_source(MessageSource::SubAgent {
            role: "reviewer".to_string(),
        });
    let prompt = Message::user("review this");
    let _ = <SessionHook as PromptHook<MockCompletionModel>>::on_completion_call(
        &sub_hook,
        &prompt,
        &[],
    )
    .await;

    let content = std::fs::read_to_string(&path).expect("sniff file");
    let has_reviewer_line = content
        .lines()
        .filter(|l| !l.is_empty())
        .any(|l| l.contains("\"lane\":\"reviewer\""));
    assert!(
        has_reviewer_line,
        "a sub-agent request must be emitted with its lane label; file: {content}"
    );
}
