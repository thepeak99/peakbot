//! `OutboundMessage::State` is constructed in **exactly one place**: the
//! single constructor inside `src/ui/outbound.rs::OutboundRx::next`.
//! Every other producer goes through `OutboundTx::publish_state`, which
//! uses a `watch::Sender` so newer snapshots replace older ones. Anyone
//! typing `OutboundMessage::State { state: ... }` directly would bypass
//! the coalescing contract (the incident root cause) and silently break
//! the memory bound.
//!
//! The `State` variant itself is *defined* in `src/ui/wire.rs` and used
//! as a *match pattern* in tests — both are allowed. The grep counts
//! constructions (`OutboundMessage::State { state:`), not destructure
//! patterns (`OutboundMessage::State { state } =>`), which is the only
//! way to tell them apart at the textual level.

use std::fs;
use std::path::Path;

/// `state:` distinguishes a construction (`{ state: ... }`) from a
/// destructure pattern (`{ state }`); the latter binds `state`, the
/// former *moves into* a fresh `OutboundMessage::State`.
const CONSTRUCT: &str = "OutboundMessage::State { state:";

/// Walks `src/` and asserts every non-comment construction of
/// `OutboundMessage::State { state: ... }` lives in either `wire.rs`
/// (the enum definition, used in tests) or `outbound.rs` (the single
/// constructor). Anything else is a contract violation.
#[test]
fn state_frames_are_constructed_only_in_the_outbound_module() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut allowed: Vec<String> = Vec::new();
    walk_rs(&src, &mut |path| {
        let display = path
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
            .unwrap_or(path)
            .display()
            .to_string();
        // The enum is defined in `wire.rs` and the only constructor lives
        // in `outbound.rs`; both are allowed. Anything else is forbidden.
        let is_allowed = display == "ui/wire.rs" || display == "ui/outbound.rs";
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        for (idx, line) in text.lines().enumerate() {
            if !line.contains(CONSTRUCT) {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let loc = format!("{display}:{}", idx + 1);
            if is_allowed {
                allowed.push(loc);
            } else {
                offenders.push(loc);
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "OutboundMessage::State {{ state: ... }} must only be constructed \
         in src/ui/wire.rs (definition) or src/ui/outbound.rs (single \
         constructor). Offending lines:\n  {}",
        offenders.join("\n  ")
    );
    // Sanity: the test must actually have seen something in the allowed
    // files, otherwise it's silently passing on an empty sweep.
    assert!(
        !allowed.is_empty(),
        "expected the definition in wire.rs and the constructor in \
         outbound.rs to be visible to this test; found none"
    );
}

fn walk_rs(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rs(&path, f);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            f(&path);
        }
    }
}
