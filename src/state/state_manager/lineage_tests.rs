//! Stage 3 — durable-lineage tests that need the private compaction seam.
//!
//! Declared as a child module of `state::state_manager` so `apply_compaction`
//! (private) is reachable. Wire/parse/cursor tests live in `ui::wire.rs`;
//! these cover the Conversation-level lineage contract.
//!
//! Contract under test:
//!   * `Conversation.history_epoch` is a durable lineage token.
//!   * Compaction (a wholesale message-sequence rewrite that inserts a
//!     summary and tags rows) MUST change the epoch, so a client cursor from
//!     the pre-compaction base is rejected and the next attach is full.
//!   * The conversation id is unchanged by compaction (lineage, not identity).

use super::*;

fn new_sm() -> Arc<StateManager> {
    let sm = StateManager::new_arc();
    sm.create_conversation(
        "lineage".to_string(),
        "openrouter".to_string(),
        "anthropic/claude-3.7-sonnet".to_string(),
        "/tmp".to_string(),
    );
    sm
}

#[test]
fn compaction_changes_history_epoch() {
    let sm = new_sm();
    sm.add_user_message("a".to_string());
    sm.add_assistant_message("b".to_string());
    sm.add_user_message("c".to_string());

    let epoch_before = sm.get_current_conversation().unwrap().history_epoch;
    let id_before = sm.get_current_conversation().unwrap().id;

    let plan = crate::context_manager::CompactionPlan {
        summary: "summary of a/b/c".to_string(),
        boundary: 3,
    };
    sm.apply_compaction(&plan);

    let after = sm.get_current_conversation().unwrap();
    assert_ne!(
        epoch_before, after.history_epoch,
        "compaction must change history_epoch (a cursor from the old base must be rejected)"
    );
    assert_eq!(
        id_before, after.id,
        "compaction must not change the conversation id"
    );
}

#[test]
fn repeated_compactions_keep_moving_the_epoch() {
    let sm = new_sm();
    sm.add_user_message("a".to_string());
    sm.add_assistant_message("b".to_string());
    sm.add_user_message("c".to_string());

    let e0 = sm.get_current_conversation().unwrap().history_epoch;
    sm.apply_compaction(&crate::context_manager::CompactionPlan {
        summary: "first".to_string(),
        boundary: 3,
    });
    let e1 = sm.get_current_conversation().unwrap().history_epoch;
    sm.apply_compaction(&crate::context_manager::CompactionPlan {
        summary: "second".to_string(),
        boundary: 3,
    });
    let e2 = sm.get_current_conversation().unwrap().history_epoch;

    assert_ne!(e0, e1, "first compaction must move the epoch");
    assert_ne!(e1, e2, "second compaction must move the epoch again");
}

#[test]
fn clear_chat_changes_history_epoch_but_keeps_id() {
    let sm = new_sm();
    sm.add_user_message("a".to_string());
    let before = sm.get_current_conversation().unwrap();
    sm.clear_chat();
    let after = sm.get_current_conversation().unwrap();
    assert_ne!(
        before.history_epoch, after.history_epoch,
        "clear must break lineage"
    );
    assert_eq!(before.id, after.id, "clear must keep the conversation id");
}

/// The bump must be written in the same save as the rewritten messages, so a
/// cold restart (reap or process death) still rejects pre-compaction cursors.
#[test]
fn compaction_epoch_is_persisted_with_the_rewritten_history() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap());
    let sm = StateManager::new_arc_with_storage(store.clone());
    sm.create_conversation(
        "durable".to_string(),
        "openrouter".to_string(),
        "anthropic/claude-3.7-sonnet".to_string(),
        "/tmp".to_string(),
    );
    sm.add_user_message("a".to_string());
    sm.add_assistant_message("b".to_string());
    sm.add_user_message("c".to_string());
    let id = sm.get_current_conversation().unwrap().id;
    let epoch_before = sm.get_current_conversation().unwrap().history_epoch;

    sm.apply_compaction(&crate::context_manager::CompactionPlan {
        summary: "summary".to_string(),
        boundary: 3,
    });
    sm.save_conversation();

    let loaded = store.load(id).expect("load after save");
    assert_ne!(
        loaded.history_epoch, epoch_before,
        "the rewritten conversation must persist the new epoch"
    );
    assert_eq!(
        loaded.history_epoch,
        sm.get_current_conversation().unwrap().history_epoch,
        "disk and memory agree on the lineage token"
    );
}

/// A load is a wholesale replacement: the current conversation's epoch must
/// become the loaded one, not a bump of the previous conversation's token.
#[test]
fn load_replaces_epoch_with_the_loaded_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = crate::storage::FileStorage::new(tmp.path().join("storage")).unwrap();
    let loaded_conv = Conversation::new(
        "loaded".to_string(),
        "openrouter".to_string(),
        "anthropic/claude-3.7-sonnet".to_string(),
        "/tmp".to_string(),
    );
    store.save(&loaded_conv).unwrap();
    let loaded_epoch = loaded_conv.history_epoch;

    let storage: Arc<dyn crate::storage::ConversationStorage> = Arc::new(store);
    let sm = StateManager::new_arc_with_storage(storage);
    sm.create_conversation(
        "current".to_string(),
        "openrouter".to_string(),
        "anthropic/claude-3.7-sonnet".to_string(),
        "/tmp".to_string(),
    );
    let current_epoch = sm.get_current_conversation().unwrap().history_epoch;
    assert_ne!(current_epoch, loaded_epoch);

    sm.load_conversation(loaded_conv.id).unwrap();
    assert_eq!(
        sm.get_current_conversation().unwrap().history_epoch,
        loaded_epoch,
        "load must adopt the loaded conversation's epoch"
    );
    assert_eq!(
        sm.get_state().history_epoch,
        loaded_epoch,
        "AppState must expose the loaded epoch"
    );
}
