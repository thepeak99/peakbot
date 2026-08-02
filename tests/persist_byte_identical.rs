//! Byte-identical pretty-JSON output for `FileStorage::save`.
//!
//! The streaming refactor (`serde_json::to_writer_pretty` into a fixed
//! `BufWriter<File>`) must NOT change the on-disk format. Save a
//! conversation, read the bytes back, and assert they equal
//! `serde_json::to_string_pretty` of the same conversation.
//!
//! PASSES today; must continue passing after the fix — this is the
//! safety net for JSON formatter drift.

#![cfg(test)]

use peakbot::{Conversation, ConversationStorage, FileStorage};
use tempfile::TempDir;

#[test]
fn save_writes_byte_identical_pretty_json() {
    let dir = TempDir::new().unwrap();
    let storage = FileStorage::new(dir.path().to_path_buf()).unwrap();

    let mut conv = Conversation::new(
        "byte-identical".into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    conv.add_user_message("hello".into());
    conv.add_assistant_message("hi there".into());
    conv.add_tool_call(
        "bash".into(),
        r#"{"command":"ls"}"#.into(),
        Some("call_1".into()),
    );
    conv.add_tool_result(
        "bash".into(),
        r#"{"command":"ls"}"#.into(),
        "file1.txt\nfile2.txt".into(),
        Some("call_1".into()),
    );
    conv.add_assistant_message("done".into());

    storage.save(&conv).expect("save must succeed");

    let expected = serde_json::to_string_pretty(&conv).expect("pretty print");
    let final_path = storage.storage_dir().join(format!("{}.json", conv.id));
    let on_disk = std::fs::read(&final_path).expect("saved file must exist on disk");

    assert_eq!(
        on_disk,
        expected.as_bytes(),
        "on-disk JSON must be byte-identical to serde_json::to_string_pretty",
    );
}
