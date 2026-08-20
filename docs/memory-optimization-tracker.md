# Memory optimization — progress tracker

Companion to [`memory-optimization-plan.md`](./memory-optimization-plan.md)
(the locked plan). One row per task. Update **Status** and **PR** as work
lands; do not restate the plan here.

**Status values:** `pending` · `in-progress` · `done`
**Gate for every task:** `cargo fmt --all` → `cargo clippy --all-targets
--all-features -- -D warnings` → `cargo test`.
**Order:** F0 → F1 → F2 → F3 → close-out. FU-* is the follow-up round.

| # | Title | Role | Status | Deps | Acceptance criterion (short) | PR |
|---|---|---|---|---|---|---|
| T1 | Per-conversation temp path in `FileStorage::save` | Junior | done | — | Two threads saving different conversations 50× concurrently ⇒ both files parse, each with its own id; temp name starts `.tmp` and `.` | |
| T2 | `image_cache` module: `ImageRef`, `dir()`, `spill()`, `path_for()` | Mid | pending | — | Same bytes twice ⇒ one file, same id; `path_for` rejects traversal / bad grammar / missing file; read-only dir ⇒ `None`, no panic | |
| T3 | `view_image`: `pub const NAME`, spill on call, optional `image_ref` in output, `image_ref_from_output`, description sentence | Mid | pending | T2 | Spill bytes == source bytes; base64 + `type`/`mimeType` unchanged; both existing rig/Anthropic wire tests pass unchanged | |
| T4 | `ChatMessage.images` + ctor extraction + `format_tool_result` arm + `elide_binary_payload` | Mid | pending | T3 | 2 MB `view_image` row ⇒ `tool_result.len() < 512`, `images` intact, `content == "🖼 shot.png"`; bash row untouched; elide is a fixpoint | |
| T5 | `images` on `conversation::Message::ToolResult` + both sync arms | Junior | pending | T4 | Ref survives `ChatMessage → ConvMsg → JSON → back`; old JSON without the key still parses; no-image rows serialize byte-identically | |
| T6 | W1 at append (`:1988`) and at load (`:1546-1560`) | Mid | pending | T4 | Sub-agent 2 MB result ⇒ row < 512 B with 1 `ImageRef`; orchestrator-lane row byte-preserved and still `ToolResultContent::Image` | |
| T7 | W1 at compaction (`:441-445`) | Mid | pending | T4 | Compacted row is tagged **and** elided **and** keeps its `ImageRef`; rescued tool call untouched; compaction tests pass | |
| T8 | Transcript-size regression test (`tests/transcript_payload_bounded.rs`) | Mid | pending | T6, T7 | 100 × 2.4 MB sub-agent images ⇒ Σ `tool_result.len()` < 1 MB, one ref per row, `sync_to_conversation` output < 2 MB | |
| T9 | `subscribers` → `watch::Sender<Arc<AppState>>`; delete buffer const + pruning loop (**#251**); `subscribe()` + `mark_changed()`; `set_welcome`/`set_final_broadcast` publish | Senior | pending | — | `Arc::ptr_eq` zero-copy fan-out; strong_count ≤ subscribers+1 after 1 000 unread mutations; 1 000-behind subscriber still gets the latest; welcome banner in first snapshot | |
| T10 | Migrate `forward_state` + stdio loop to `changed()`/`borrow_and_update()`, forward the same `Arc` | Mid | pending | T9 | `state_only_in_outbound.rs`, `e2e_tests.rs`, web handshake tests pass unchanged; manual attach renders | |
| T11 | Migrate the two out-of-crate subscriber tests | Junior | pending | T9 | `reasoning_preservation.rs:740,780` keep the same assertions and outcomes | |
| T12 | `PersistClock` + `PERSIST_MIN_INTERVAL` (pure, no wiring) | Junior | pending | — | Touch after a gap ⇒ `true`; a burst ⇒ exactly one `true`; `on_flush()` iff dirty; `persisted()` clears | |
| T13 | Collapse `save_conversation` into `persist_current`; `persist_debounced` + `flush_persist`; swap the 6 mutator call sites | Senior | pending | T12 | `CountingStorage` (test-module double): 20 appends in one burst ⇒ ≤ 2 saves; failed save stays dirty and retries | |
| T14 | Turn boundary: `flush_persist()` in `set_running(false)` after the guard block | Senior | pending | T13 | N round-trips + `set_running(false)` ⇒ all N pairs on disk, exactly one save; watchdog deadlock test passes | |
| T15 | P3: `flush_persist()` first in `create_/load_/delete_conversation` + `add_*` caller audit | Senior | pending | T13 | Append → `/new` (and → `/load`) ⇒ the outgoing conversation on disk contains the append; audit recorded in the PR | |
| T16 | Docs: release notes + `agents.md` seam lines + `memory.md` entry | Junior | pending | T9, T13 | `agents.md` states the three seams verbatim (subscribe/coalesce, turn-boundary persistence, payload-retention + `ImageRef`) | |
| T17 | End-to-end verification run | Senior | pending | T8, T10, T15 | Peak RSS < 1.5 GB, idle < 800 MB, conversation file < 8 MB, no stray temp files; numbers recorded in the PR | |
| FU-1 | `GET /images/{id}` route (token-gated, `path_for`-validated, 404 → placeholder) | Mid | pending | T2, T17 | A valid id returns the bytes with the right `Content-Type`; a malformed/missing id returns 404; no token ⇒ rejected by the existing layer | |
| FU-2 | SPA renders images inline (`WireChatMessage.images` + `<img>` in `Message.tsx`) | Mid | pending | FU-1 | A `view_image` row shows the picture inline; a row without `images` renders exactly as today | |
| FU-3 | Spill sweep (age or total-bytes cap, at startup) | Junior | pending | T2 | The cache dir stays under the cap across repeated sessions; a live session's images are never swept | |
| FU-4 | Wire-side payload skip + user attachments spilled to `ImageRef` | Mid | pending | FU-2 | A `state` frame carries no base64 for any row; a user-attached image renders inline and holds no bytes in `AppState` | |
