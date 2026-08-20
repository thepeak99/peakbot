//! Concurrent `FileStorage::save` for *distinct* conversations must not
//! clobber each other.
//!
//! The current implementation in `src/storage/file_storage.rs` uses a
//! single shared temp file `<storage_dir>/.tmp.json` for every save.
//! Two threads saving different conversations concurrently will:
//!   - truncate each other's temp file (`File::create` re-opens it), and
//!   - race the rename step: whichever thread renames second gets
//!     `ENOENT` (the source file is already gone), so its `save` returns
//!     `Err`; and the file that *does* land on disk may hold the *other*
//!     conversation's bytes.
//!
//! Either way — data loss or a failed save.
//!
//! Acceptance criterion (this test's contract): two threads each save a
//! DIFFERENT conversation 50× concurrently, and afterwards both
//! conversation files parse and each contains its own conversation id.
//!
//! The fix (per-conversation temp path `.tmp.<uuid>.json`) must keep the
//! temp name dot-prefixed and `.tmp`-prefixed (so `cleanup_temp_files`
//! and `rebuild_index` still recognise/skip it), preserve the atomic
//! rename, and leave `save`'s signature unchanged. This test pins only
//! the observable contract: concurrent saves of distinct conversations
//! succeed, and afterwards each conversation file holds exactly its own
//! conversation (id, name, and message count).

#![cfg(test)]

use peakbot::{Conversation, ConversationStorage, FileStorage};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

/// Iterations per thread, per the locked plan's acceptance criterion.
const SAVES_PER_THREAD: usize = 50;

/// One user message + one assistant message per side, each carrying a
/// 32 KiB ASCII payload (10 pairs ⇒ ~640 KiB per conversation).
///
/// The payload widens each save's `create → write → rename` window so
/// the two threads reliably overlap. Without enough per-save work, a
/// small race window can dodge the bug across 50 iterations and the
/// test would become flaky.
const MESSAGES_PER_SIDE: usize = 10;
const PAYLOAD_BYTES: usize = 32 * 1024;

fn build_conversation(name: &str) -> Conversation {
    let mut conv = Conversation::new(
        name.into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    let chunk = "x".repeat(PAYLOAD_BYTES);
    for i in 0..MESSAGES_PER_SIDE {
        conv.add_user_message(format!("u-{i}-{chunk}"));
        conv.add_assistant_message(format!("a-{i}-{chunk}"));
    }
    conv
}

/// Run one thread's worth of saves. Records the FIRST save error it
/// sees (if any) but keeps running all `SAVES_PER_THREAD` iterations so
/// both threads always call `barrier.wait()` the same number of times —
/// the buggy code makes one thread start returning `Err` from `save`
/// very early; if we returned early on error we'd deadlock the other
/// thread at its next `barrier.wait()`.
fn run_thread(
    storage: Arc<FileStorage>,
    conv: Conversation,
    label: &'static str,
    barrier: Arc<Barrier>,
) -> Option<String> {
    let mut first_err: Option<String> = None;
    for i in 0..SAVES_PER_THREAD {
        // Align both threads so each save starts at the same instant,
        // maximising overlap of the create → write → rename window.
        barrier.wait();
        if let Err(e) = storage.save(&conv)
            && first_err.is_none()
        {
            first_err = Some(format!("thread {label} save #{i} failed: {e}"));
        }
    }
    first_err
}

#[test]
fn save_concurrent_distinct_conversations_do_not_clobber_each_other() {
    let dir = TempDir::new().unwrap();
    let storage = Arc::new(FileStorage::new(dir.path().to_path_buf()).unwrap());

    let conv_a = build_conversation("concurrent-a");
    let conv_b = build_conversation("concurrent-b");
    assert_ne!(
        conv_a.id, conv_b.id,
        "fixture sanity: distinct conversation ids"
    );
    let expected_messages = conv_a.messages.len();
    assert_eq!(
        conv_b.messages.len(),
        expected_messages,
        "fixture sanity: same message count"
    );

    let barrier = Arc::new(Barrier::new(2));

    let handle_a = {
        let storage = Arc::clone(&storage);
        let conv = conv_a.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || run_thread(storage, conv, "a", barrier))
    };
    let handle_b = {
        let storage = Arc::clone(&storage);
        let conv = conv_b.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || run_thread(storage, conv, "b", barrier))
    };

    let err_a = handle_a.join().expect("thread a must not panic");
    let err_b = handle_b.join().expect("thread b must not panic");

    // 1) Every single save must succeed — no `Err` from concurrent saves
    //    of distinct conversations.
    if let Some(e) = err_a {
        panic!("{e}");
    }
    if let Some(e) = err_b {
        panic!("{e}");
    }

    // 2) Both conversation files exist, parse as valid JSON, and contain
    //    THEIR OWN id, name, and message count. The id assertion is the
    //    literal acceptance criterion; name + message count catch the
    //    cross-contamination variant where a file ends up with the other
    //    thread's bytes but the wrong conversation's id (or, after a
    //    future regression, the right id but wrong contents).
    for (conv, label) in [(conv_a.clone(), "a"), (conv_b.clone(), "b")] {
        let path = dir.path().join(format!("{}.json", conv.id));
        let on_disk = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "conversation file for thread {label} (id={}) must exist on disk: {e}",
                conv.id
            )
        });
        let parsed: Conversation = serde_json::from_slice(&on_disk).unwrap_or_else(|e| {
            panic!(
                "conversation file for thread {label} (id={}) must parse as JSON Conversation: {e}",
                conv.id
            )
        });
        assert_eq!(
            parsed.id, conv.id,
            "thread {label} file must contain its own conversation id, not the other thread's"
        );
        assert_eq!(
            parsed.name, conv.name,
            "thread {label} file must contain its own name, not the other thread's"
        );
        assert_eq!(
            parsed.messages.len(),
            expected_messages,
            "thread {label} file must contain its own message count, not the other thread's"
        );
    }
}
