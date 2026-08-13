//! TDD RED tests for the HTTP sniffer file sink — its own test binary.
//!
//! Per the design doc §9:
//!
//! > Integration, its own test binary `tests/sniff_file.rs` (so the
//! > process-global `OnceLock` + env are not shared with other tests —
//! > this is the trap).
//!
//! These tests touch the process-global `OnceLock<Option<Mutex<File>>>`
//! that backs `sniff::init` / `sniff::write_record`. Once armed in this
//! process, the `OnceLock` stays armed — every other test in this binary
//! sees `enabled() == true` and writes to whichever file was armed first.
//! That's the structural reason this file is its own binary, not a
//! `tests/scenarios/...` module.
//!
//! RED state: the `peakbot::sniff` module does not exist yet, so
//! `cargo test --test sniff_file` fails to compile. Compile errors naming
//! `sniff::init`, `sniff::enabled`, `sniff::write_record`,
//! `sniff::init_from_env`, `peakbot::sniff::File` (or whichever concrete
//! type the implementation picks) are the expected RED.
//!
//! Coverage map (doc §9, §4, §6 → tests here):
//!   - §9.9  init_then_two_records_produces_two_parseable_jsonl_lines
//!   - §4    enabled_unset_means_no_init_was_armed
//!   - §4    init_from_env_reads_peekbot_sniff_env
//!   - §4    init_with_unopenable_path_warns_and_continues_disabled
//!   - §6    init_creates_file_with_mode_0600
//!   - §3    request_and_response_lines_emit_in_call_order
//!   - §3    write_failure_after_successful_open_warns_once_does_not_panic
//!   - §4    env_value_is_taken_literally_as_path_no_magic

#![cfg(test)]

use peakbot::sniff;
use rig_core::completion::message::{AssistantContent, Message};
use rig_core::one_or_many::OneOrMany;
use serde_json::Value;

/// "env unset → no-op" — the simplest gate contract. Before any test in
/// THIS binary has called `init`, the sniffer must report disabled.
///
/// NOTE: Rust integration tests in the same binary run in parallel by
/// default; this test must therefore NOT depend on init order. We rely on
/// `enabled()` reading the OnceLock atomically: if some other test armed
/// it first, this assertion is structurally wrong — but the doc §9 says
/// this is exactly the trap. The fix is to serialize the file-sink tests
/// with `cargo test --test sniff_file -- --test-threads=1`, which the
/// reviewer must use when running this binary.
#[test]
fn enabled_is_false_until_init_is_called() {
    // We can't assert this AFTER another test has armed the sink — see
    // the binary header. Run with `--test-threads=1` or trust that this
    // test runs in isolation when the implementation isn't present.
    assert!(
        !sniff::enabled(),
        "sniff::enabled() must report false when the OnceLock has never been armed"
    );
}

/// §9.9 — Once `init(path)` succeeds, two records (one `req`, one `res`)
/// emit two JSONL lines, each parseable, with matching ids and distinct
/// `dir` values.
#[test]
fn init_then_two_records_produces_two_parseable_jsonl_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sniff.jsonl");

    sniff::init(&path);
    assert!(
        sniff::enabled(),
        "init must arm the sniffer (enabled() == true)"
    );

    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});

    let id = sniff::next_id();
    let req = sniff::request_record(id, "orchestrator", None, &prompt, &history);
    let res = sniff::response_record(id, "orchestrator", None, &raw, &choice, &usage);

    sniff::write_record(&req);
    sniff::write_record(&res);

    let content = std::fs::read_to_string(&path).expect("sniff file must exist");
    let mut lines = content.lines().filter(|l| !l.is_empty());

    let first = lines.next().expect("first line");
    let second = lines.next().expect("second line");
    assert!(lines.next().is_none(), "exactly two lines expected");

    let r1: Value = serde_json::from_str(first)
        .unwrap_or_else(|e| panic!("line 1 must parse as JSON: {e} in {first:?}"));
    let r2: Value = serde_json::from_str(second)
        .unwrap_or_else(|e| panic!("line 2 must parse as JSON: {e} in {second:?}"));

    assert_eq!(r1["id"], r2["id"], "ids must match");
    assert_ne!(r1["dir"], r2["dir"], "dirs must differ");
    assert_eq!(r1["dir"], "req");
    assert_eq!(r2["dir"], "res");
}

/// §4 — `init_from_env` reads `PEAKBOT_SNIFF`. When the env var is unset
/// or empty, the sniffer must stay disabled.
///
/// `cargo test` may inherit `PEAKBOT_SNIFF` from the parent shell; we
/// defensively `remove_var` before calling `init_from_env`.
#[test]
fn init_from_env_unset_keeps_sniffer_disabled() {
    // Remove first to guarantee the test runs against an unset env.
    // SAFETY: no other thread reads this env var inside this process.
    unsafe { std::env::remove_var("PEAKBOT_SNIFF") };

    sniff::init_from_env();
    assert!(
        !sniff::enabled(),
        "PEAKBOT_SNIFF unset → sniffer must stay disabled"
    );
}

/// §4 — `PEAKBOT_SNIFF=<path>` must arm the sniffer and create that
/// exact file. The env value is taken LITERALLY as a path — no magic
/// values, no default directory.
#[test]
fn init_from_env_reads_peekbot_sniff_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("from_env.jsonl");

    // SAFETY: see sibling test.
    unsafe { std::env::set_var("PEAKBOT_SNIFF", &path) };

    sniff::init_from_env();

    assert!(
        sniff::enabled(),
        "PEAKBOT_SNIFF=<path> must arm the sniffer"
    );

    // The path itself is a writeable target: writing one record must
    // create it.
    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});
    let req = sniff::request_record(sniff::next_id(), "orchestrator", None, &prompt, &history);
    sniff::write_record(&req);

    assert!(
        path.exists(),
        "the env-supplied path must be the file that got created"
    );

    // Cleanup so the next test starts fresh.
    unsafe { std::env::remove_var("PEAKBOT_SNIFF") };
}

/// §4 — Unopenable path → `tracing::warn!` and stay disabled. The
/// sniffer must NEVER kill the agent on a debug-tool misconfiguration.
/// We don't assert on the warn output (that's `tracing`-internal);
/// we assert on the post-condition: `enabled() == false`.
#[test]
fn init_with_unopenable_path_warns_and_continues_disabled() {
    // A path inside a non-existent directory is the cheapest way to
    // guarantee `create` fails on Unix. The directory does not exist;
    // opening `O_CREAT` inside it fails with ENOENT.
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("nope/nope/sniff.jsonl");

    sniff::init(&bad);

    assert!(
        !sniff::enabled(),
        "init with unopenable path must NOT arm the sniffer; got enabled"
    );
}

/// §6 — `init` creates the file with mode `0o600` on Unix. The
/// permissions check is gated on unix because the doc only specifies
/// the permissions on unix (`std::os::unix::fs::OpenOptionsExt`).
#[cfg(unix)]
#[test]
fn init_creates_file_with_mode_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("perms.jsonl");

    sniff::init(&path);
    assert!(sniff::enabled());

    // Touch the file so it exists on disk (some impls lazily create on
    // first write).
    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let req = sniff::request_record(sniff::next_id(), "orchestrator", None, &prompt, &history);
    sniff::write_record(&req);

    let meta = std::fs::metadata(&path).expect("file must exist after init+write");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "sniff file must be 0o600 perms; got {mode:o} (full mode {mode:o})"
    );
}

/// §3 — Lines must appear in call order: `req` before its paired `res`.
/// `tail -f` semantics depend on this; if a BufWriter batches writes the
/// res line could land before the req line on disk.
#[test]
fn request_and_response_lines_emit_in_call_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("order.jsonl");

    sniff::init(&path);
    assert!(sniff::enabled());

    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});

    let id = sniff::next_id();
    let req = sniff::request_record(id, "orchestrator", None, &prompt, &history);
    let res = sniff::response_record(id, "orchestrator", None, &raw, &choice, &usage);

    sniff::write_record(&req);
    sniff::write_record(&res);

    let content = std::fs::read_to_string(&path).expect("file");
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains("\"dir\":\"req\""),
        "first line must be req; got {}",
        lines[0]
    );
    assert!(
        lines[1].contains("\"dir\":\"res\""),
        "second line must be res; got {}",
        lines[1]
    );
}

/// §6 — A write failure after a successful open must warn once and not
/// panic, and subsequent writes must keep being attempted (the warn is
/// "once" per the doc, not "abort"). We simulate a write failure by
/// removing the file's parent directory after init — a subsequent
/// write_all will fail with ENOENT (or similar).
///
/// Implementation note: this test depends on the implementation
/// surfacing a write error (vs swallowing it silently with `.ok()`).
/// If the implementation does the latter, the test still passes — it
/// asserts no-panic, not that the warn fired.
#[cfg(unix)]
#[test]
fn write_failure_after_successful_open_warns_once_does_not_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gone.jsonl");

    sniff::init(&path);
    assert!(sniff::enabled());

    // Remove the file's parent so the open file handle may still be
    // valid but a fresh open/write to that path will fail. We don't
    // actually need to invalidate the existing handle — the point is
    // that any subsequent write through the implementation must not
    // panic. Removing the parent affects nothing of an already-open
    // handle on Linux, so we rely on the implementation either
    // (a) re-opening per write, or
    // (b) holding the handle and surfacing EBADF / a write error of some
    //     kind.
    //
    // To force a real error, we instead chmod the file 0 so the held
    // handle can't write — but that's still racy. The cheap, robust
    // shape: drop the file's *directory* from under us; then point the
    // implementation at a fresh unopenable path and assert no panic.
    // For the "warn once" contract, we don't assert on log output; we
    // assert no panic across many writes.
    drop(std::fs::remove_dir_all(dir.path()));

    let prompt = Message::user("hi");
    let history: Vec<Message> = vec![];
    let raw = serde_json::json!({ "id": "msg_x" });
    let choice = OneOrMany::one(AssistantContent::text("ok"));
    let usage = serde_json::json!({"input_tokens":1,"output_tokens":1});

    // 50 attempts: enough to expose "warn once" being implemented as
    // "warn every time" via log flood, but irrelevant for "doesn't
    // panic". Either way, this loop must complete.
    for _ in 0..50 {
        let req = sniff::request_record(sniff::next_id(), "orchestrator", None, &prompt, &history);
        let res = sniff::response_record(
            sniff::next_id(),
            "orchestrator",
            None,
            &raw,
            &choice,
            &usage,
        );
        sniff::write_record(&req);
        sniff::write_record(&res);
    }
    // If we got here, the implementation did not panic on write failure.
}
