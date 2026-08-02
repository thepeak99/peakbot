//! Salvage file for sub-agent history that would otherwise be lost.
//!
//! `history_snapshot()` is taken before the last wire request, so it never
//! contains the sub-agent's final reply — that reply is the delegate return
//! value itself, and is deliberately not duplicated into this file.
//!
//! Hookless Ollama sub-agents produce an empty snapshot → no file, no note
//! (degrades to prior behavior).

use rig_core::completion::message::{AssistantContent, Message};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many of the sub-agent's own messages we keep. Its final reply is not
/// among them — that is the delegate result the orchestrator already has.
const KEEP: usize = 10;

/// Monotonic counter so concurrent or back-to-back calls never collide on
/// `file_name`.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Scratch root, shared with the bash tool: `<temp>/peakbot`.
fn temp_root() -> PathBuf {
    std::env::temp_dir().join("peakbot")
}

/// Every non-empty assistant text turn, oldest first. Tool calls, reasoning,
/// images, the orchestrator's task, and tool results are all excluded — the
/// goal is the sub-agent's prose report, not its plumbing. Multiple text
/// blocks inside one turn join with "\n" and count as ONE message.
fn assistant_texts(history: &[Message]) -> Vec<String> {
    let mut out = Vec::new();
    for msg in history {
        if let Message::Assistant { content, .. } = msg {
            let mut texts = Vec::new();
            for c in content.iter() {
                if let AssistantContent::Text(t) = c {
                    texts.push(t.text.as_str());
                }
            }
            let joined = texts.join("\n");
            if !joined.trim().is_empty() {
                out.push(joined);
            }
        }
    }
    out
}

/// `delegate_{role}_{pid}_{counter}.txt`; role is sanitised to
/// `[A-Za-z0-9_-]` so a config role name is always a legal file name.
fn file_name(role: &str) -> String {
    let sanitised: String = role
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let pid = std::process::id();
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("delegate_{sanitised}_{pid}_{counter}.txt")
}

/// Header + numbered messages. Pure.
fn render(role: &str, kept: &[String], total: usize) -> String {
    let kept_n = kept.len();
    let count_phrase = if kept_n == total {
        format!("all {kept_n}")
    } else {
        format!("last {kept_n} of {total}")
    };
    let mut out = String::new();
    out.push_str(&format!(
        "[delegate:{role}] The sub-agent's own messages, oldest first — {count_phrase}. \
         Its final reply is NOT here (that is the delegate result you already have).\n"
    ));
    for (i, text) in kept.iter().enumerate() {
        out.push('\n');
        out.push_str(&format!("===== message {}/{kept_n} =====\n", i + 1));
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// create_dir_all + write. Returns the path. `dir` is a parameter purely so
/// the failure path is testable.
fn save(dir: &Path, role: &str, body: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(role));
    std::fs::write(&path, body)?;
    Ok(path)
}

/// The one-line pointer appended to the delegate result. Pure.
fn note(role: &str, kept: usize, path: &Path) -> String {
    let (noun, verb) = if kept == 1 {
        ("earlier message", "was")
    } else {
        ("earlier messages", "were")
    };
    format!(
        "[delegate:{role}] Its {kept} {noun} {verb} saved to {} — \
         if the result above is not the full report (e.g. just \"done\" or a one-liner), \
         `file_read` that path to recover it.",
        path.display()
    )
}

/// Testable core.
fn attach_note_in(dir: &Path, result: String, role: &str, history: &[Message]) -> String {
    let texts = assistant_texts(history);
    if texts.is_empty() {
        return result;
    }
    let total = texts.len();
    let kept_start = total.saturating_sub(KEEP);
    let kept = &texts[kept_start..];
    let body = render(role, kept, total);

    // NOTE: deliberately NO writer-side size cap — `file_read`
    // (`src/tools/file_read.rs`) caps reads at 50_000 chars and offers
    // start_line/end_line pagination, so the orchestrator can chunk-recover a
    // long salvage file. A cap here would silently drop the sub-agent's real
    // report, which is exactly what this feature exists to prevent.
    match save(dir, role, &body) {
        Ok(path) => format!("{result}\n\n{}", note(role, kept.len(), &path)),
        Err(e) => {
            tracing::warn!(
                target: "peakbot",
                role = %role,
                error = %e,
                "Failed to save sub-agent salvage file; returning delegate result unchanged"
            );
            result
        }
    }
}

/// THE seam. Save the sub-agent's own earlier messages and append a pointer
/// to `result`. Returns `result` byte-identical when there is nothing to save
/// or the write fails — a delegation is never failed by this.
pub(crate) fn attach_note(result: String, role: &str, history: &[Message]) -> String {
    attach_note_in(&temp_root(), result, role, history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::OneOrMany;
    use rig_core::completion::message::{AssistantContent, Text, UserContent};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    // ── fixtures ───────────────────────────────────────────────────────────

    /// Make a `Message::User` carrying a single text block — the orchestrator's
    /// task or any tool result. Both ride in `UserContent` and must be
    /// excluded from `assistant_texts`.
    fn user_text(t: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: t.to_string(),
                additional_params: None,
            })),
        }
    }

    /// Make a `Message::Assistant` carrying a single text block.
    fn assistant_text(t: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: t.to_string(),
                additional_params: None,
            })),
        }
    }

    /// Make a `Message::Assistant` carrying exactly two text blocks — used
    /// to assert the "\n"-join contract.
    fn assistant_two_texts(a: &str, b: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Text(Text {
                    text: a.to_string(),
                    additional_params: None,
                }),
                AssistantContent::Text(Text {
                    text: b.to_string(),
                    additional_params: None,
                }),
            ])
            .expect("two items is non-empty"),
        }
    }

    /// Per-test isolated dir under `<temp>`. The prefix is flat (no path
    /// separators) so `tempfile::Builder::new()` can create it directly
    /// under the system temp dir. `TempDir` cleans up on drop — no leaked
    /// directories on CI.
    fn isolated_dir(label: &str) -> (TempDir, PathBuf) {
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let prefix = format!("peakbot_sub_agent_messages_tests_{pid}_{label}_{n}_");
        let td = tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir()
            .expect("create temp test dir");
        let path = td.path().to_path_buf();
        (td, path)
    }

    // ── 1. assistant_texts: user/system are excluded, order is preserved ──

    #[test]
    fn assistant_texts_returns_assistant_texts_oldest_first_and_excludes_user_and_system() {
        let h = vec![
            Message::System {
                content: "you are a helper".to_string(),
            },
            user_text("orchestrator task"),
            assistant_text("first answer"),
            user_text("tool result"),
            assistant_text("second answer"),
            Message::System {
                content: "another system".to_string(),
            },
        ];

        let got = assistant_texts(&h);
        assert_eq!(
            got,
            vec!["first answer".to_string(), "second answer".to_string()],
            "oldest-first, user/system/tool results excluded"
        );
    }

    // ── 2. assistant_texts: tool-call / reasoning / image turns are skipped ──

    #[test]
    fn assistant_texts_excludes_tool_call_reasoning_and_image_assistant_blocks() {
        use rig_core::completion::message::{Reasoning, ToolCall, ToolFunction};
        let tool_only = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: "call-1".to_string(),
                call_id: None,
                function: ToolFunction {
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                },
                signature: None,
                additional_params: None,
            })),
        };
        let reasoning_only = Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Reasoning(Reasoning::new("thinking…"))),
        };
        let h = vec![tool_only, reasoning_only, assistant_text("kept text")];

        let got = assistant_texts(&h);
        assert_eq!(got, vec!["kept text".to_string()]);
    }

    // ── 3. assistant_texts: two text blocks in one turn → one joined entry ─

    #[test]
    fn assistant_texts_joins_multiple_text_blocks_in_one_turn_with_newline() {
        let h = vec![
            assistant_two_texts("line one", "line two"),
            assistant_text("alone"),
        ];

        let got = assistant_texts(&h);
        assert_eq!(
            got,
            vec!["line one\nline two".to_string(), "alone".to_string()],
            "two text blocks in one turn join with \\n and count as ONE message"
        );
    }

    // ── 4. assistant_texts: whitespace-only turns are dropped, not emitted ──

    #[test]
    fn assistant_texts_drops_whitespace_only_text_turns() {
        let h = vec![
            assistant_text("   \n\t  "),
            assistant_text(""),
            assistant_text("real content"),
            assistant_text(" \n "),
        ];

        let got = assistant_texts(&h);
        assert_eq!(got, vec!["real content".to_string()]);
    }

    // ── 5. assistant_texts: empty history → empty vec ─────────────────────

    #[test]
    fn assistant_texts_empty_history_returns_empty_vec() {
        assert!(assistant_texts(&[]).is_empty());
    }

    // ── 6. file_name: counter increments between consecutive calls ────────

    #[test]
    fn file_name_two_consecutive_calls_yield_different_names() {
        let a = file_name("researcher");
        let b = file_name("researcher");
        assert_ne!(a, b, "counter must advance so two calls never collide");
    }

    // ── 7. file_name: hostile role names sanitise to [A-Za-z0-9_-] only ───

    #[test]
    fn file_name_sanitises_role_path_separators_and_spaces() {
        for hostile in ["../etc/passwd", "weird/role name", "a b c", "x/y/../z"] {
            let name = file_name(hostile);
            assert!(
                name.starts_with("delegate_") && name.ends_with(".txt"),
                "name must keep the delegate_ prefix and .txt suffix: {name:?}"
            );
            // No path separator anywhere in the basename.
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "no path separator survives: {name:?}"
            );
            // Strip the "delegate_" prefix and ".txt" suffix; the rest is
            // "{role}_{pid}_{counter}". Peel the trailing "_<digits>_<digits>"
            // off the body to recover the sanitised role segment — without
            // assuming the impl chose `_` (vs `-` or stripping) to replace
            // disallowed chars inside the role itself.
            let body = name
                .strip_prefix("delegate_")
                .and_then(|s| s.strip_suffix(".txt"))
                .unwrap_or_else(|| panic!("shape: {name:?}"));
            // Walk back from the end: skip digits, expect a single separator
            // (`_` or `-`), skip digits, expect a single separator, then take
            // everything before it as the role. We accept either `_` or `-`
            // because the impl's choice of replacement char for hostile inputs
            // is an implementation detail; both are in the allowed set.
            let (after_counter, _counter) = peel_numeric_segment(body);
            let (after_pid, _pid) = peel_numeric_segment(&after_counter);
            let role = after_pid;
            assert!(!role.is_empty(), "role segment must be non-empty: {name:?}");
            assert!(
                role.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "role chars must be [A-Za-z0-9_-]: role={role:?} name={name:?}"
            );
        }
    }

    /// Peel the LAST trailing `<sep><digits>` segment off `s`, return
    /// `(head, peeled)` where `head` is `s` with the segment removed and
    /// `peeled` is the dropped segment (e.g. `"_0"`). The separator may be
    /// `_` or `-`. Used by test 7 to walk back from the counter to recover
    /// the role segment without depending on the impl's internal choice of
    /// replacement char inside the role itself.
    fn peel_numeric_segment(s: &str) -> (String, String) {
        let bytes = s.as_bytes();
        // Skip trailing digits.
        let mut i = bytes.len();
        while i > 0 && bytes[i - 1].is_ascii_digit() {
            i -= 1;
        }
        assert!(i < bytes.len(), "expected trailing digits: {s:?}");
        // The char before the digits is the segment's separator.
        assert!(i > 0, "no separator before digits: {s:?}");
        let sep = s.as_bytes()[i - 1];
        assert!(
            sep == b'_' || sep == b'-',
            "expected `_` or `-` before digits, got {:?}: {s:?}",
            sep as char
        );
        let head = s[..i - 1].to_string();
        let peeled = s[i - 1..].to_string();
        (head, peeled)
    }

    // ── 8. render: every kept message body appears verbatim ───────────────

    #[test]
    fn render_contains_every_kept_message_body_verbatim() {
        let kept: Vec<String> = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let body = render("researcher", &kept, kept.len());
        for k in &kept {
            assert!(body.contains(k.as_str()), "missing {k:?} in:\n{body}");
        }
    }

    // ── 9. render: header reports both counts when truncated, "all N" otherwise ──

    #[test]
    fn render_header_says_last_x_of_y_when_truncated_and_all_n_otherwise() {
        let kept: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
        let truncated = render("researcher", &kept, 14);
        assert!(
            truncated.contains("last 10 of 14"),
            "truncated header must say 'last 10 of 14':\n{truncated}"
        );

        let complete = render("researcher", &kept, 10);
        assert!(
            complete.contains("all 10"),
            "non-truncated header must say 'all 10':\n{complete}"
        );
        assert!(
            !complete.contains("last 10 of 10"),
            "non-truncated header must NOT say 'last 10 of 10':\n{complete}"
        );
    }

    // ── 10. render: numbered banner "===== message N/N =====" for each ────

    #[test]
    fn render_contains_numbered_message_banners_one_through_n() {
        let kept: Vec<String> = vec!["x".into(), "y".into(), "z".into()];
        let body = render("researcher", &kept, kept.len());
        for i in 1..=kept.len() {
            let banner = format!("===== message {i}/{} =====", kept.len());
            assert!(
                body.contains(&banner),
                "missing banner {banner:?} in:\n{body}"
            );
        }
    }

    // ── 11. note: contains the role, count, exact path, and `file_read` ──

    #[test]
    fn note_contains_role_count_exact_path_and_file_read_keyword() {
        let p = PathBuf::from("/tmp/peakbot/delegate_researcher_31337_0.txt");
        let n = note("researcher", 8, &p);
        assert!(n.contains("researcher"), "missing role: {n}");
        assert!(n.contains("8"), "missing count: {n}");
        assert!(
            n.contains(p.to_str().unwrap()),
            "missing exact path string: {n}"
        );
        assert!(n.contains("file_read"), "missing `file_read`: {n}");
    }

    // ── 12. note: singular "message was" when kept == 1 ──────────────────

    #[test]
    fn note_uses_singular_message_was_when_kept_is_one() {
        let p = PathBuf::from("/tmp/peakbot/delegate_one_1_0.txt");
        let n = note("one", 1, &p);
        assert!(n.contains("message was"), "expected singular: {n}");
        assert!(!n.contains("messages were"), "must not be plural: {n}");
    }

    // ── 13. attach_note_in: no assistant text → byte-identical, no file ──

    #[test]
    fn attach_note_in_returns_result_unchanged_and_creates_no_file_when_no_assistant_text() {
        let (_td, dir) = isolated_dir("no_text");
        let result = "delegate finished\n\nhere is the report".to_string();
        let h: Vec<Message> = vec![
            user_text("orchestrator task"),
            Message::System {
                content: "sys".to_string(),
            },
        ];

        let out = attach_note_in(&dir, result.clone(), "researcher", &h);
        assert_eq!(out, result, "must return input BYTE-IDENTICAL");

        let entries: Vec<_> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "no file should be created, found: {:?}",
            entries.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    // ── 14. attach_note_in: 3 assistant texts → result grows, file on disk ──

    #[test]
    fn attach_note_in_with_three_assistant_texts_appends_pointer_and_writes_file_with_all_three() {
        let (_td, dir) = isolated_dir("three");
        let result = "final report from sub-agent".to_string();
        let h = vec![
            user_text("task"),
            assistant_text("draft one"),
            assistant_text("draft two"),
            assistant_text("draft three"),
        ];

        let out = attach_note_in(&dir, result.clone(), "researcher", &h);
        assert!(
            out.starts_with(&result),
            "original result must be the prefix of returned string; got {out:?}"
        );
        assert!(out.len() > result.len(), "note must extend the result");
        // Find the saved path in the appended note and assert it lives on disk.
        let path_token = out
            .split("saved to ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .expect("note must contain 'saved to <path>'");
        let saved = PathBuf::from(path_token);
        assert!(saved.exists(), "saved file must exist on disk: {saved:?}");

        let body = fs::read_to_string(&saved).expect("read saved file");
        for txt in ["draft one", "draft two", "draft three"] {
            assert!(body.contains(txt), "saved file missing {txt:?} in:\n{body}");
        }
    }

    // ── 15. attach_note_in: 14 texts → keep last 10, oldest dropped, note says 10 ──

    #[test]
    fn attach_note_in_with_fourteen_assistant_texts_keeps_last_ten_and_drops_earliest() {
        let (_td, dir) = isolated_dir("fourteen");
        let result = "final".to_string();
        // Mark each turn with its own number so we can assert which made it.
        let h: Vec<Message> = (1..=14)
            .map(|i| assistant_text(&format!("turn-{i:02}")))
            .collect();

        let out = attach_note_in(&dir, result, "researcher", &h);

        // Resolve the saved path from the appended note.
        let path_token = out
            .split("saved to ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .expect("note must contain 'saved to <path>'");
        let saved = PathBuf::from(path_token);
        let body = fs::read_to_string(&saved).expect("read saved file");

        // Header should say "last 10 of 14".
        assert!(
            body.contains("last 10 of 14"),
            "header must say 'last 10 of 14':\n{body}"
        );
        // Note itself must say 10.
        assert!(
            out.contains("Its 10 earlier"),
            "note must say 'Its 10': {out}"
        );

        // Surviving messages: turns 5..=14.
        for i in 5..=14 {
            let token = format!("turn-{i:02}");
            assert!(
                body.contains(&token),
                "kept window must include {token:?}:\n{body}"
            );
        }
        // Dropped message: turn 1.
        assert!(
            !body.contains("turn-01"),
            "earliest message (turn-01) must be dropped:\n{body}"
        );
    }

    // ── 16. attach_note_in: the result (final reply) is NOT in the file ──

    #[test]
    fn attach_note_in_saved_file_does_not_contain_the_final_reply() {
        let (_td, dir) = isolated_dir("no_dup");
        // Sentinel string the sub-agent's final reply would contain.
        let result = "SENTINEL_FINAL_REPLY_zX7q9B".to_string();
        let h = vec![
            user_text("task"),
            assistant_text("draft one"),
            assistant_text("draft two"),
        ];

        let out = attach_note_in(&dir, result.clone(), "researcher", &h);

        let path_token = out
            .split("saved to ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .expect("note must contain 'saved to <path>'");
        let saved = PathBuf::from(path_token);
        let body = fs::read_to_string(&saved).expect("read saved file");
        assert!(
            !body.contains(&result),
            "saved file must NOT duplicate the final reply:\n{body}"
        );
    }

    // ── 17. attach_note_in: write failure degrades silently, no panic ────

    #[test]
    fn attach_note_in_degrades_silently_when_target_dir_cannot_be_created() {
        // `dir` must point at a path whose parent is a FILE — so
        // `create_dir_all(dir)` cannot succeed. We deliberately avoid chmod
        // 000 because CI containers run as root, where an unwritable dir is
        // still writable.
        let (_td, base) = isolated_dir("cant_mkdir");
        let blocker = base.join("blocker");
        fs::write(&blocker, b"i am a file, not a directory").expect("create blocker file");
        let impossible = blocker.join("subdir"); // parent is a regular file

        let result = "untouched final result".to_string();
        let h = vec![user_text("task"), assistant_text("draft")];

        let out = attach_note_in(&impossible, result.clone(), "researcher", &h);
        assert_eq!(
            out, result,
            "must return input BYTE-IDENTICAL on write failure"
        );
        // The blocking file must still exist — nothing destructive happened.
        assert!(blocker.exists());
        // And nothing was written beneath it.
        assert!(!blocker.join("anything").exists());
    }

    // ── 18. attach_note_in: two back-to-back calls → two distinct paths ───

    #[test]
    fn attach_note_in_two_back_to_back_calls_write_to_different_paths() {
        let (_td, dir) = isolated_dir("twice");
        let result = "r".to_string();
        let h = vec![user_text("task"), assistant_text("a"), assistant_text("b")];

        let out1 = attach_note_in(&dir, result.clone(), "researcher", &h);
        let out2 = attach_note_in(&dir, result, "researcher", &h);

        let p1 = PathBuf::from(
            out1.split("saved to ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .expect("p1 path"),
        );
        let p2 = PathBuf::from(
            out2.split("saved to ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .expect("p2 path"),
        );
        assert_ne!(p1, p2, "two calls must produce two distinct paths");
        assert!(p1.exists(), "first file must exist: {p1:?}");
        assert!(p2.exists(), "second file must exist: {p2:?}");
        // Neither call clobbered the other — both payloads are recoverable.
        let b1 = fs::read_to_string(&p1).expect("read p1");
        let b2 = fs::read_to_string(&p2).expect("read p2");
        assert!(b1.contains("a") && b1.contains("b"));
        assert!(b2.contains("a") && b2.contains("b"));
    }
}
