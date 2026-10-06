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

// ── T5 — attachment persistence (file-uploads design §3.2 / §7 T5) ────────
//
// **Status: compile-fail until T5 lands.** `peakbot::attachments::{Attachment,
// AttachmentKind}` (pub fields, design §3.1) and the
// `Conversation::add_user_message_with_attachments` helper (mirroring
// `add_user_message`) do not exist yet; these tests target the locked
// interface and fail to compile — the RED state we want.
//
// The on-disk assertions go through the JSON (the `Message::User` fields are
// private to the conversation module), so no new accessor is assumed.

fn att(
    name: &str,
    mime: &str,
    size: u64,
    kind: peakbot::attachments::AttachmentKind,
) -> peakbot::attachments::Attachment {
    use peakbot::attachments::Attachment;
    Attachment {
        id: uuid::Uuid::new_v4(),
        convo: uuid::Uuid::new_v4(),
        name: name.to_string(),
        mime: mime.to_string(),
        size,
        kind,
    }
}

#[test]
fn old_json_without_attachments_loads_and_resaves_byte_identical() {
    let dir = TempDir::new().unwrap();
    let storage = FileStorage::new(dir.path().to_path_buf()).unwrap();

    let mut conv = Conversation::new(
        "legacy".into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    conv.add_user_message("hello".into());
    conv.add_assistant_message("hi there".into());

    // The serialised form of a text-only user row must NOT carry an
    // `attachments` key (`skip_serializing_if`) — this is exactly the shape
    // of every pre-uploads conversation file on disk.
    let old_json = serde_json::to_string_pretty(&conv).expect("pretty print");
    assert!(
        !old_json.contains("attachments"),
        "text-only rows must not gain an attachments key: {old_json}"
    );

    let id = conv.id;
    std::fs::write(
        storage.storage_dir().join(format!("{id}.json")),
        old_json.clone(),
    )
    .expect("write old-format file");

    // A pre-uploads file (no `attachments` key on the user row) must load
    // unchanged — `#[serde(default)]` fills the missing field.
    let loaded = storage
        .load(id)
        .expect("old JSON without attachments must load");
    assert_eq!(loaded.messages.len(), 2);

    // …and resaves byte-identically: no migration, no key added.
    let resaved = serde_json::to_string_pretty(&loaded).expect("pretty print");
    assert_eq!(
        resaved, old_json,
        "loading old JSON must not change the serialised form"
    );
}

#[test]
fn user_message_with_two_attachments_round_trips() {
    let dir = TempDir::new().unwrap();
    let storage = FileStorage::new(dir.path().to_path_buf()).unwrap();

    let image = att(
        "cat.png",
        "image/png",
        1234,
        peakbot::attachments::AttachmentKind::Image,
    );
    let file = att(
        "spec.pdf",
        "application/pdf",
        2411724,
        peakbot::attachments::AttachmentKind::File,
    );

    let mut conv = Conversation::new(
        "with-attachments".into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    conv.add_user_message("hi".into());
    conv.add_user_message_with_attachments(
        "what's in these?".into(),
        vec![image.clone(), file.clone()],
    );

    storage.save(&conv).expect("save must succeed");

    let final_path = storage.storage_dir().join(format!("{}.json", conv.id));
    let on_disk = std::fs::read(&final_path).expect("saved file must exist on disk");
    let value: serde_json::Value = serde_json::from_slice(&on_disk).expect("valid JSON");

    // The second row carries both attachments, wire shape intact (I6:
    // disk/state/wire share one serde shape).
    let atts = value["messages"][1]["attachments"]
        .as_array()
        .expect("the user row must persist an attachments array");
    assert_eq!(atts.len(), 2, "both attachments must persist: {value:?}");
    assert_eq!(atts[0]["name"], "cat.png");
    assert_eq!(atts[0]["mime"], "image/png");
    assert_eq!(atts[0]["size"], 1234);
    assert_eq!(atts[0]["kind"], "image");
    assert_eq!(atts[1]["name"], "spec.pdf");
    assert_eq!(atts[1]["mime"], "application/pdf");
    assert_eq!(atts[1]["size"], 2411724);
    assert_eq!(atts[1]["kind"], "file");
    // The text-only first row must NOT gain the key.
    assert!(
        value["messages"][0].get("attachments").is_none(),
        "text-only rows stay key-free: {value:?}"
    );

    // Load → resave must be byte-identical (lossless round-trip).
    let loaded = storage.load(conv.id).expect("load must succeed");
    let resaved = serde_json::to_string_pretty(&loaded).expect("pretty print");
    assert_eq!(
        resaved.as_bytes(),
        on_disk,
        "the attachment round-trip must be lossless"
    );
}
