//! T6 integration — stored attachments through the full agentic loop
//! (file-uploads design §5.4 / §7 T6), MockCompletionModel harness.
//!
//! **Status: compile-fail until T5/T6 land.** Written against the assumed
//! harness API — the existing `TestHarness` cannot express a user turn with
//! attachments without production changes:
//! - `TestHarness::with_uploads(store)` — constructor variant wiring
//!   `StateManager::with_uploads` (design §5.5).
//! - `TestHarness::run_message_with_attachments(text, atts)` — mirrors
//!   `run_message` but appends via `add_user_message_with_attachments`;
//!   needs a `TestRunner::run_message_with_attachments` seam in
//!   `src/test_runner.rs` (one-line production change).
//! - `StateManager::set_supports_vision(bool)` — design §5.5.
//!
//! The attachment's `convo` is a fresh uuid: the turn builder resolves
//! bytes via `store.path(a)` and never consults the current conversation
//! (invariant I3), so no boot conversation is needed.

use base64::Engine;
use peakbot::attachments::{Attachment, UploadStore};
use peakbot::config::UploadsConfig;
use peakbot::mock::{MockResponse, RecordedRequest};
use rig_core::completion::message::{
    DocumentSourceKind, ImageMediaType, Message as RigMessage, UserContent,
};
use uuid::Uuid;

use super::super::harness::TestHarness;

/// Minimal valid 1x1 PNG (same fixture as `tests/attachments_tests.rs`).
fn png_1x1() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .expect("valid base64 fixture")
}

fn limits() -> UploadsConfig {
    UploadsConfig {
        max_file_mb: 50,
        max_files: 10,
    }
}

/// The prompt the model saw: the last message of the last recorded request.
fn prompt_of(requests: &[RecordedRequest]) -> &RigMessage {
    requests
        .last()
        .expect("at least one LLM request was recorded")
        .chat_history
        .last()
        .expect("the recorded history is non-empty")
}

#[tokio::test]
async fn stored_png_on_vision_harness_produces_image_part() {
    let tmp = tempfile::tempdir().unwrap();
    let store = UploadStore::new(tmp.path().to_path_buf(), limits());
    let mut harness = TestHarness::with_uploads(store.clone());
    harness.state_manager.set_supports_vision(true);

    let convo = Uuid::new_v4();
    let png = png_1x1();
    let att: Attachment = store
        .store(convo, "cat.png", &png[..])
        .await
        .expect("store the PNG");

    harness.add_response(MockResponse::text("a cat"));
    harness
        .run_message_with_attachments("what's in this?", vec![att.clone()])
        .await;

    let requests = harness.get_recorded_requests();
    assert_eq!(requests.len(), 1, "exactly one LLM request");
    let RigMessage::User { content } = prompt_of(&requests) else {
        panic!(
            "the prompt must be a User message: {:?}",
            prompt_of(&requests)
        );
    };

    // §5.4: order stays [Image*, Text].
    assert_eq!(
        content.len(),
        2,
        "vision turn must be [Image, Text]: {content:?}"
    );
    let UserContent::Image(img) = content.first_ref() else {
        panic!("first part must be the image: {content:?}");
    };
    let DocumentSourceKind::Base64(b64) = &img.data else {
        panic!("the image must be base64 of the stored file, not a URL: {img:?}");
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("valid base64"),
        png,
        "the image part must be the stored file bytes"
    );
    assert_eq!(img.media_type, Some(ImageMediaType::PNG));

    let UserContent::Text(t) = content.last_ref() else {
        panic!("second part must be the text: {content:?}");
    };
    assert!(t.text.contains("what's in this?"), "text part: {}", t.text);
    assert!(
        t.text.contains("cat.png"),
        "the note must list the attachment: {}",
        t.text
    );
    assert!(
        t.text.contains(store.path(&att).to_str().unwrap()),
        "the note must carry the absolute stored path: {}",
        t.text
    );
}

#[tokio::test]
async fn stored_png_without_vision_is_text_only_with_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = UploadStore::new(tmp.path().to_path_buf(), limits());
    let mut harness = TestHarness::with_uploads(store.clone());
    harness.state_manager.set_supports_vision(false);

    let convo = Uuid::new_v4();
    let png = png_1x1();
    let att: Attachment = store
        .store(convo, "cat.png", &png[..])
        .await
        .expect("store the PNG");

    harness.add_response(MockResponse::text("a cat, probably"));
    harness
        .run_message_with_attachments("what's in this?", vec![att.clone()])
        .await;

    let requests = harness.get_recorded_requests();
    assert_eq!(requests.len(), 1, "exactly one LLM request");
    let RigMessage::User { content } = prompt_of(&requests) else {
        panic!(
            "the prompt must be a User message: {:?}",
            prompt_of(&requests)
        );
    };

    // The vision fallback is simply the absence of image parts.
    assert_eq!(
        content.len(),
        1,
        "non-vision turn must be [Text] only: {content:?}"
    );
    let UserContent::Text(t) = content.first_ref() else {
        panic!("the single part must be text: {content:?}");
    };
    assert!(t.text.contains("what's in this?"), "text part: {}", t.text);
    assert!(
        t.text.contains(store.path(&att).to_str().unwrap()),
        "the note must carry the absolute stored path so the model can read \
         the file with its tools: {}",
        t.text
    );
}
