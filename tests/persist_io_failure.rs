//! A failed `save` must NOT clobber the previously-saved good file.
//!
//! The refactor introduces a `BufWriter<File>` whose `Drop` silently
//! swallows I/O errors. If `save` only catches the error inside
//! `BufWriter::drop` and continues, it can return `Ok(())` while having
//! truncated the conversation file. This test guards against that by
//! forcing a genuine I/O error (planting a directory at the temp-path
//! `save` writes to, so `fs::write` returns `IsADirectory`) and asserting
//! (a) `save` returns `Err`, and (b) the previously-saved good file is
//! still byte-identical and still parses.
//!
//! The failure-forcing trick: `FileStorage::save` writes a temp file at
//! `<storage_dir>/.tmp.json` then renames it to `<storage_dir>/<uuid>.json`.
//! After the first good save, `.tmp.json` no longer exists (renamed away).
//! Pre-creating a DIRECTORY at that path makes the next `fs::write` to it
//! return `IsADirectory` — a portable I/O error that does not depend on
//! uid (root bypasses chmod-based tricks, this works for everyone).

#![cfg(test)]

use peakbot::{Conversation, ConversationStorage, FileStorage};
use std::fs;
use tempfile::TempDir;

#[test]
fn save_returns_err_and_preserves_existing_file_on_write_failure() {
    let dir = TempDir::new().unwrap();
    let storage_dir = dir.path().to_path_buf();
    let storage = FileStorage::new(storage_dir.clone()).unwrap();

    let mut good = Conversation::new(
        "io-failure".into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    good.add_user_message("original".into());
    storage.save(&good).expect("first save must succeed");

    // Sanity: first save wrote a file that loads cleanly.
    let loaded = storage
        .load(good.id)
        .expect("first save must produce a loadable file");
    assert_eq!(loaded.messages.len(), 1);
    let expected_msg_count = good.messages.len();

    // Plant a directory at the temp-path the next save will try to write to.
    let temp_path = storage_dir.join(".tmp.json");
    fs::create_dir(&temp_path).expect("must be able to plant a dir at .tmp.json");

    // A modified conversation so the second save would differ from the first.
    let mut modified = good.clone();
    modified.add_user_message("added-after-failure".into());

    let result = storage.save(&modified);
    assert!(
        result.is_err(),
        "save must return Err when .tmp.json is a directory (got Ok)",
    );

    // The good file at the original path is byte-for-byte unchanged.
    let final_path = storage_dir.join(format!("{}.json", good.id));
    let on_disk = fs::read(&final_path).expect("good file must still exist");
    let expected = serde_json::to_string_pretty(&good).expect("pretty print");
    assert_eq!(
        on_disk,
        expected.as_bytes(),
        "good file must be byte-for-byte unchanged after the failed save",
    );

    // And the good file still parses to a valid Conversation with the
    // original message count.
    let parsed: Conversation =
        serde_json::from_slice(&on_disk).expect("good file must still parse");
    assert_eq!(parsed.messages.len(), expected_msg_count);
}
