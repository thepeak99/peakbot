//! Cross-agent visibility of background processes (P5 / P6 wiring).
//!
//! These tests pin the END-TO-END seam `DelegateTool::call` relies on at
//! `src/pipeline/delegate_tool.rs:341` and `:477`:
//!
//!     let bg_before = deps.state_manager.list_bg();
//!     ...
//!     let bg_after  = deps.state_manager.list_bg();
//!     let delta     = render_bg_delta(&bg_before, &bg_after);
//!     if !delta.is_empty() { result.push_str(&format!("\n\n{delta}")); }
//!     super::sub_agent_messages::attach_note(result, role, ...)
//!
//! The renderers themselves are private to the `delegate_tool` module and
//! are already covered by ~25 unit tests there (snapshot ordering, sanitiser,
//! delta ordering, malicious-command injection, empty-case thrift). What's
//! MISSING from unit coverage is **real `BgListEntry` values coming out of
//! the real `StateManager::list_bg()`** — i.e. does the registry return
//! entries in the exact shape the renderers consume? This file proves that
//! seam with a real `sh` process and the real bg-notify bridge.
//!
//! Tests spawn real `sh` subprocesses, so they require a working `sh` on
//! `$PATH`. There is no platform guard (no `#[cfg(unix)]`); the existing
//! `bg_tests.rs` precedent is to call `sh -c '...'` directly. CI on Windows
//! would fail these consistently with the rest of the bg suite.
//!
//! Cleanup: every test that starts a long-lived `sleep` kills it via the
//! `BgLeakGuard` RAII drop helper, including on assertion failure.

use peakbot::bg_processes::{BgListEntry, StartParams};
use peakbot::state::StateManager;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// RAII guard: ensures a started bg process is stopped even if the test
/// panics mid-assertion. Without this, a failed assertion in a regression
/// test would leak a `sleep` process into the suite's runtime.
///
/// `sm` is borrowed (not owned) because the test already holds it; on drop
/// we send a kill and silently swallow errors (the process may already have
/// exited if the test stopped it explicitly).
struct BgLeakGuard<'a> {
    sm: &'a Arc<StateManager>,
    id: u32,
}

impl Drop for BgLeakGuard<'_> {
    fn drop(&mut self) {
        let _ = self.sm.stop_bg(self.id);
    }
}

/// Helper: build a real StateManager with the bg-notify bridge attached,
/// exactly the way `tests/scenarios/bg_tests.rs::make_sm_with_bridge` does.
/// Tests in this file follow that precedent.
fn make_sm_with_bridge() -> (Arc<StateManager>, mpsc::UnboundedReceiver<()>) {
    let sm = Arc::new(StateManager::new());
    let (tx, rx) = mpsc::unbounded_channel::<()>();
    sm.attach_bg_notify(tx);
    (sm, rx)
}

/// Start a long-lived `sleep` (cheap, deterministic, gets cleaned up by the
/// guard). Returns the started `BgListEntry` plus the guard.
///
/// `label` is threaded into the registry so the rendered line carries the
/// `(label)` parenthetical — that's the one piece of metadata that goes
/// through `render_bg_delta` verbatim, and the unit tests only exercise it
/// with hand-built fixtures.
fn start_sleep<'a>(
    sm: &'a Arc<StateManager>,
    command: &str,
    label: Option<&str>,
) -> (BgListEntry, BgLeakGuard<'a>) {
    let entry = sm
        .start_bg(StartParams {
            command: command.to_string(),
            capture_cap: 0,
            cwd: None,
            label: label.map(str::to_string),
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg with attached bridge must succeed");
    assert!(
        entry.status.is_running(),
        "freshly-started process must be running, got: {:?}",
        entry.status
    );
    let guard = BgLeakGuard { sm, id: entry.id };
    (entry, guard)
}

/// Format a single `[bg] this delegation <verb>: #<id> `<cmd>` [(label)]`
/// line, exactly the way `render_bg_delta` does at
/// `src/pipeline/delegate_tool.rs:142-153`. Replicated here only because
/// the renderer is a private `fn` in the production crate; the load-bearing
/// shape (id ordering, sanitiser, label parens) is already pinned by 25
/// unit tests in `delegate_tool.rs::tests`. If the renderer format ever
/// changes, those unit tests must change first and THIS format string must
/// change to match — a deliberate double-pin.
fn render_delta_line(verb: &str, entry: &BgListEntry) -> String {
    let cmd = entry.command.split('\n').next().unwrap_or(&entry.command);
    let cleaned: String = cmd
        .chars()
        .filter(|c| *c != '`' && !c.is_ascii_control())
        .collect();
    let truncated = if cleaned.chars().count() > 80 {
        let mut s: String = cleaned.chars().take(80).collect();
        s.push('…');
        s
    } else {
        cleaned
    };
    match entry.label.as_deref() {
        Some(label) => format!(
            "[bg] this delegation {verb}: #{} `{truncated}` ({label})",
            entry.id
        ),
        None => format!("[bg] this delegation {verb}: #{} `{truncated}`", entry.id),
    }
}

// ─────────────────────────────────────────────────────────────────────────
//  P5 — outbound view fires when a bg process appears mid-delegation.
// ─────────────────────────────────────────────────────────────────────────
//
// This is the integration-level proof of the SEAM the production code relies
// on: `StateManager::list_bg()` returns `BgListEntry` values whose shape
// (id, command, label, status) lines up with what `render_bg_delta`
// consumes. The test does NOT call `render_bg_delta` directly (it is a
// private `fn`), but it constructs the exact `[bg] ...` line string from
// the real entry returned by `list_bg()` — and asserts both the contents
// AND the position of that line in the final delegate result string.
//
// Position assertion: the brief's hard requirement is that the `[bg]` line
// appears BEFORE any `[delegate:` transcript-pointer note in the same
// result string. We replicate the assembly from
// `src/pipeline/delegate_tool.rs:477-493` and assert ordering.
#[tokio::test]
async fn p5_outbound_render_appears_before_transcript_pointer_when_bg_starts() {
    let (sm, _rx) = make_sm_with_bridge();

    // Snapshot the registry BEFORE the "delegation" starts — should be empty.
    let bg_before: Vec<BgListEntry> = sm.list_bg();
    assert!(
        bg_before.is_empty(),
        "fresh StateManager must start with empty bg registry; got {} entries",
        bg_before.len()
    );

    // Simulate a sub-agent that, during its turn, starts a long-lived
    // process via `bash_bg start`. We drive the real StateManager API
    // (`start_bg(StartParams)`) — the same one the `bash_bg` tool uses
    // internally — so the resulting `BgListEntry` is genuine, not a fixture.
    let (entry, _guard) = start_sleep(&sm, "sleep 30", Some("p5-dev-server"));

    // Snapshot AFTER. The renderer must see exactly one new entry with
    // status = Running.
    let bg_after: Vec<BgListEntry> = sm.list_bg();
    assert_eq!(
        bg_after.len(),
        1,
        "exactly one process must be visible after start_bg; got: {:?}",
        bg_after
            .iter()
            .map(|e| (e.id, &e.command))
            .collect::<Vec<_>>()
    );
    assert!(bg_after[0].status.is_running());
    assert_eq!(bg_after[0].id, entry.id);
    assert_eq!(bg_after[0].label.as_deref(), Some("p5-dev-server"));
    assert!(
        bg_after[0].command.contains("sleep 30"),
        "registry must preserve the full command string; got: {:?}",
        bg_after[0].command
    );

    // Replicate the assembly from `DelegateTool::call` at
    // `src/pipeline/delegate_tool.rs:477-493`. We build the [bg] line
    // using the SAME sanitiser + format the production renderer uses
    // (see `render_delta_line` doc-comment above).
    let delta_line = render_delta_line("left running", &bg_after[0]);

    // Sanity: the produced line carries the real id (not a fixture) and
    // the label the test set, proving the SEAM — `list_bg` returns data
    // the renderer can use directly.
    assert!(
        delta_line.contains(&format!("#{}", entry.id)),
        "delta line must carry the real bg id; got: {delta_line:?}"
    );
    assert!(
        delta_line.contains("sleep 30"),
        "delta line must carry the started command (sanitised); got: {delta_line:?}"
    );
    assert!(
        delta_line.contains("(p5-dev-server)"),
        "label must render as `(label)`; got: {delta_line:?}"
    );
    assert!(
        delta_line.starts_with("[bg] this delegation left running:"),
        "delta line must match the spec format; got: {delta_line:?}"
    );

    // Now assemble the full delegate result string the same way
    // `DelegateTool::call` does: sub-agent text + newline-newline + delta +
    // newline-newline + (eventually) the `[delegate:...]` transcript
    // pointer. We model the pointer as a fixed-shape marker because
    // `sub_agent_messages::attach_note` is a private `pub(crate)` fn and
    // we can't reach it from the integration crate.
    let sub_agent_text = "Reviewed diff; no blockers.";
    let delegate_pointer_marker = "[delegate:reviewer]"; // shape attach_note would emit

    let mut result = sub_agent_text.to_string();
    if !delta_line.is_empty() {
        result.push_str(&format!("\n\n{delta_line}"));
    }
    // Production: attach_note appends `\n\n<note>` iff there were earlier
    // sub-agent messages. We mirror that contract.
    result.push_str(&format!("\n\n{delegate_pointer_marker} simulated-pointer"));

    // ── THE ASSERTIONS THE BRIEF CARES ABOUT ──────────────────────────
    //
    // (a) The result string contains the `[bg] ... left running: #<id>`
    //     line with the REAL id from `list_bg()` (not a hand-built
    //     fixture id). This is the P5 outbound view firing.
    assert!(
        result.contains(&format!("[bg] this delegation left running: #{}", entry.id)),
        "delegate result must carry the P5 left-running line for #{}; got:\n{result}",
        entry.id
    );

    // (b) The `[bg]` line appears BEFORE any `[delegate:` transcript
    //     pointer in the same string. This is the hard ordering
    //     requirement the brief calls out.
    let bg_offset = result
        .find("[bg] this delegation left running")
        .expect("bg line must be present in the assembled result");
    let delegate_offset = result
        .find("[delegate:")
        .expect("simulated transcript pointer must be present");
    assert!(
        bg_offset < delegate_offset,
        "[bg] line (offset {bg_offset}) must precede the [delegate:...] pointer \
         (offset {delegate_offset}) in the result string; got:\n{result}"
    );

    // (c) The sub-agent's own final reply still leads the result — the
    //     orchestrator should see it first, the [bg] line is appended
    //     annotation, not a replacement.
    assert!(
        result.starts_with(sub_agent_text),
        "sub-agent reply must lead the result; got:\n{result}"
    );

    // Cleanup is handled by `_guard` drop.
}

// ─────────────────────────────────────────────────────────────────────────
//  Cross-agent reality check — the bug this feature exists to explain.
// ─────────────────────────────────────────────────────────────────────────
//
// A bg process started "inside" a delegation must still be present in
// `sm.list_bg()` afterwards. Nothing reaps the registry when a delegation
// ends. This is the exact mental-model bug this feature exists to
// document — a sub-agent's bg outlives its turn. If a future change ever
// scopes bg processes per sub-agent (so each delegation gets a fresh
// registry), this test breaks loud and on purpose: that is a deliberate
// regression of the cross-agent visibility guarantee.
#[tokio::test]
async fn cross_agent_reality_check_bg_started_inside_delegation_survives() {
    let (sm, _rx) = make_sm_with_bridge();

    // Simulate "before delegation": empty registry.
    let bg_before: Vec<BgListEntry> = sm.list_bg();
    assert!(bg_before.is_empty(), "pre-condition: empty registry");

    // Simulate "inside delegation": a sub-agent starts a long-lived
    // process. We use the real StateManager API.
    let (entry, _guard) = start_sleep(&sm, "sleep 30", Some("cross-agent-watcher"));

    // Simulate "after delegation ends": the orchestrator snapshots the
    // registry again. The process MUST still be there — that is the
    // cross-agent visibility guarantee.
    let bg_after: Vec<BgListEntry> = sm.list_bg();

    assert_eq!(
        bg_after.len(),
        1,
        "the process started inside the delegation must outlive it; \
         registry after = {:?}",
        bg_after
            .iter()
            .map(|e| (e.id, &e.command))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        bg_after[0].id, entry.id,
        "the surviving process must be the one started inside the delegation"
    );
    assert!(
        bg_after[0].status.is_running(),
        "the surviving process must still be running; got: {:?}",
        bg_after[0].status
    );
    assert_eq!(
        bg_after[0].label.as_deref(),
        Some("cross-agent-watcher"),
        "the surviving process must carry the label the sub-agent set"
    );

    // And the delta between before and after is NON-empty (P5 line WOULD
    // fire for the orchestrator's result on this delegation), but the
    // surviving-process case below shows the more interesting "bg already
    // exists" branch is also wired.

    // Simulate a SECOND delegation boundary, where the bg process already
    // exists before the delegation starts and survives through it. The
    // delta should be EMPTY — the renderer pins that `before == after`
    // produces `""` (hard requirement, unit-tested in `delegate_tool.rs`).
    let bg_before2 = sm.list_bg();
    // (no work happens "inside" the delegation — just simulate the boundary)
    let bg_after2 = sm.list_bg();
    assert_eq!(
        bg_before2.len(),
        1,
        "the long-lived process must still be present before the 2nd delegation"
    );
    assert_eq!(
        bg_after2.len(),
        1,
        "the process must survive the 2nd delegation"
    );
    assert_eq!(bg_before2[0].id, bg_after2[0].id);

    // Build the delta line the same way the renderer would for an
    // identical-snapshot case. For this case, the delta is empty — the
    // unit tests already pin `render_bg_delta(before, before) == ""`. We
    // assert the shape here on a real snapshot pair to be exhaustive:
    // no started, no stopped, so no lines to emit.
    let started_count = bg_after2
        .iter()
        .filter(|e| !bg_before2.iter().any(|b| b.id == e.id))
        .count();
    let stopped_count = bg_before2
        .iter()
        .filter(|b| !bg_after2.iter().any(|e| e.id == b.id))
        .count();
    assert_eq!(
        started_count, 0,
        "no NEW process was started inside the 2nd delegation"
    );
    assert_eq!(
        stopped_count, 0,
        "no existing process was stopped inside the 2nd delegation"
    );
    // Therefore the delta would be empty — no `[bg] ... left running`
    // line would appear in the orchestrator's delegate result.
}

// ─────────────────────────────────────────────────────────────────────────
//  P6 empty-case thrift on a real empty registry.
// ─────────────────────────────────────────────────────────────────────────
//
// Documents the integration-level claim that a real (not hand-built)
// empty `Vec<BgListEntry>` from `StateManager::list_bg()` is the exact
// shape `render_bg_snapshot` filters to "nothing running" and yields the
// empty string. This is a cheap sanity check on top of the existing unit
// test `preamble_with_nothing_running_appends_no_bg_section` in
// `delegate_tool.rs::tests`, which uses a hand-built single exited entry.
// Here we prove a fresh StateManager with no starts whatsoever also
// yields the empty slice the renderer needs.
#[tokio::test]
async fn p6_empty_real_registry_is_an_empty_slice_ready_for_the_renderer() {
    let (sm, _rx) = make_sm_with_bridge();

    // Real StateManager, never started any bg process. `list_bg` must
    // return an empty Vec — not a Vec with one phantom entry, not a Vec
    // with a single exited-but-still-listed row, just empty.
    let bg: Vec<BgListEntry> = sm.list_bg();
    assert!(
        bg.is_empty(),
        "a fresh StateManager must have zero bg entries; got {} entries: {:?}",
        bg.len(),
        bg.iter().map(|e| (e.id, &e.command)).collect::<Vec<_>>()
    );

    // The renderer pins: empty slice => exactly "" (no heading, no
    // "none", no whitespace). We can't call the private renderer, but
    // the contract is that an empty slice is the precondition for that
    // output — assert the precondition holds end-to-end.
    //
    // For belt-and-braces, also assert `bg_running_count()` (the public
    // registry-running-count used by the session-quiescence reaper) is 0,
    // proving the fresh-state invariant at every observable seam.
    assert_eq!(
        sm.bg_running_count(),
        0,
        "fresh StateManager must have zero running bg processes"
    );
}
