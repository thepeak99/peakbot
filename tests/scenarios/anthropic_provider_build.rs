//! Regression lock — provider-build seam honours `preserve_reasoning`.
//!
//! The 14 tests in `reasoning_preservation.rs` exercise `SessionHook`
//! directly via the public builder
//! (`SessionHook::with_context_tracking(...).with_preserve_reasoning(true)`).
//! They all pass on current `master` because they never touch the bug.
//!
//! The bug lives in `src/providers/mod.rs` `create_anthropic_agent`: it
//! builds the rig `Agent` with `.hook(hook.clone())` at ~line 863 and only
//! *after* the agent is built applies `let hook = hook
//! .with_preserve_reasoning(info.preserve_reasoning);` at ~line 909.
//! `SessionHook` is `#[derive(Clone)]` and `AgentBuilder::hook` stores the
//! value, so the agent's embedded hook keeps `preserve_reasoning: false`
//! forever. The capture seam in `src/hooks/session_hook.rs` (`let thinking
//! = if self.preserve_reasoning { parts.thinking } else { Vec::new() };`)
//! therefore always strips thinking blocks in production, even though
//! `resolve_preserve_reasoning(None, None)` returns `true` by default.
//!
//! The agent's hook field is private, so the test cannot introspect it
//! directly. The contract pinned here is the provider-build helper that
//! `create_anthropic_agent` MUST be refactored to use: a function that
//! takes the already-resolved `preserve_reasoning` bool and returns a
//! `SessionHook` with that value applied *before* it ever reaches
//! `.hook(...)`. A public `SessionHook::preserve_reasoning()` getter on
//! the returned hook is the only observation seam.
//!
//! RED evidence today: the test does not compile — neither
//! `peakbot::build_anthropic_session_hook` nor
//! `SessionHook::preserve_reasoning` exist. After the mid refactors
//! `create_anthropic_agent` to extract `build_anthropic_session_hook` and
//! call it *before* `.hook(hook.clone())` (with the resolved
//! `preserve_reasoning` value passed through, not patched on afterwards),
//! the test compiles and passes — GREEN.
//!
//! This is the verify-by-poison pattern `reasoning_preservation.rs` uses
//! throughout (see `capture_keeps_signature_byte_identical_via_session_hook`
//! for the same shape against not-yet-existing types).

#![cfg(test)]
// The tester's assertions deliberately pass references to helper functions;
// removing them would alter the spec's source style without changing meaning.
#![allow(clippy::needless_borrow)]

use peakbot::{SessionHook, SessionStats, SourcedEvent, StateManager};
use std::sync::{Arc, Mutex};

#[test]
fn create_anthropic_agent_hook_honours_preserve_reasoning_default() {
    // The provider-build seam — `peakbot::build_anthropic_session_hook`
    // — must build a SessionHook with the resolved `preserve_reasoning`
    // value applied. `resolve_preserve_reasoning(None, None) == true`
    // (the default), so the Anthropic boot path with no override must
    // produce a hook whose `preserve_reasoning` getter is `true`.
    let session_stats = Arc::new(Mutex::new(SessionStats::new()));
    let sm = Arc::new(StateManager::new());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel::<SourcedEvent>();

    let hook: SessionHook = peakbot::build_anthropic_session_hook(
        sender,
        session_stats,
        &sm,
        true, // resolve_preserve_reasoning(None, None)
    );

    assert!(
        hook.preserve_reasoning(),
        "build_anthropic_session_hook must apply preserve_reasoning=true (the \
         resolve_preserve_reasoning(None, None) default) to the returned SessionHook. \
         Today the value is patched onto the returned hook AFTER the rig Agent has \
         already captured the default-false clone via `.hook(hook.clone())` — the \
         agent's embedded hook stays preserve_reasoning=false forever, silently \
         stripping every thinking block at the capture seam."
    );
}

#[test]
fn create_anthropic_agent_hook_honours_preserve_reasoning_false_override() {
    // Knob-off must round-trip cleanly too: when the provider config
    // forces `preserve_reasoning = false` (e.g. a deployment that 400s
    // on thinking blocks), the helper must produce a hook whose getter
    // is `false`. This is the symmetric half of the contract — pins
    // that the helper doesn't accidentally always force `true`.
    let session_stats = Arc::new(Mutex::new(SessionStats::new()));
    let sm = Arc::new(StateManager::new());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel::<SourcedEvent>();

    let hook: SessionHook = peakbot::build_anthropic_session_hook(
        sender,
        session_stats,
        &sm,
        false, // resolve_preserve_reasoning(Some(false), None) == false
    );

    assert!(
        !hook.preserve_reasoning(),
        "build_anthropic_session_hook must apply preserve_reasoning=false when \
         called with preserve_reasoning=false — the helper must not hard-code true."
    );
}
