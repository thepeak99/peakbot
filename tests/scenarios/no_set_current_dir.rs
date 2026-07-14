//! The process-global cwd is never mutated anywhere in `src/`. The
//! session cwd is per-session (held by `StateManager.session_cwd`); the
//! shell tools spawn each call with `cmd.cwd(session_cwd)`.
//! `set_current_dir` is a forbidden process-global mutation that would
//! race concurrent web sessions sharing one process.
//!
//! If you legitimately need to chdir a child process, use
//! `std::process::Command::current_dir(...)` — that's the right
//! primitive. If you think you need `set_current_dir`, you almost
//! certainly don't, and you should redesign the call site so the
//! per-session cwd carries the change instead.

use std::fs;
use std::path::Path;

const FORBIDDEN: &str = "set_current_dir";

/// Walks `src/` and counts non-comment, non-doc occurrences of
/// `set_current_dir`. The call site count must be **zero** — any
/// nonzero count is a violation of the per-session-only cwd rule.
///
/// Comment/doc mentions are filtered by inspecting the line content:
/// only Rust comments and the doc-comment markers `///` and `//` are
/// skipped. The check is deliberately conservative — false positives
/// (a comment that *looks* like a call) are caught by a hand review of
/// the diff, false negatives (a real call hidden behind macros) are
/// caught by `cargo clippy` and a careful audit of any new code that
/// touches process-global state. This test exists to lock the obvious
/// case: nobody types `std::env::set_current_dir(...)` and forgets.
#[test]
fn no_set_current_dir_in_src() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    walk_rs(&src, &mut |path| {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        for (idx, line) in text.lines().enumerate() {
            if !line.contains(FORBIDDEN) {
                continue;
            }
            let trimmed = line.trim_start();
            // Skip comment lines: `//` (block-ish) and `///` / `//!`
            // (doc). `cargo fmt` aligns these, so `trim_start` then
            // the prefix check is the right shape.
            if trimmed.starts_with("//") {
                continue;
            }
            offenders.push(format!("{}:{}", path.display(), idx + 1));
        }
    });
    assert!(
        offenders.is_empty(),
        "process-global cwd is forbidden in src/ — every cwd change \
         must go through `state_manager.session_cwd`. Offending lines:\n  {}",
        offenders.join("\n  ")
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
