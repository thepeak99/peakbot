# Memory Optimization — Tracker

**Convention:** a task is `done` only when merged. Until then: `pending` (not started) or `in-progress` (has a PR).

## F0 — data loss (independent, ship first)

| # | Title | Status | PR |
|---|---|---|---|
| **T1** | Per-conversation temp path in `FileStorage::save` | pending | — |

## F1 — the payload + the image-reference seam

| # | Title | Status | PR |
|---|---|---|---|
| **T2** | `image_cache` module: `ImageRef`, `dir()`, `spill()`, `path_for()` | in-progress | #310 (open) |
| **T3** | `view_image`: `pub const NAME`, spill on call, optional `image_ref` in output | pending | — |
| **T4** | `ChatMessage.images` + ctor extraction + `format_tool_result` arm + `elide_binary_payload` | pending | — |
| **T5** | `images` on `conversation::Message::ToolResult` + both sync arms | pending | — |
| **T6** | W1 at append and at load | pending | — |
| **T7** | W1 at compaction | pending | — |
| **T8** | Transcript-size regression test | pending | — |

## F2 — the fan-out

| # | Title | Status | PR |
|---|---|---|---|
| **T9** | Replace `subscribers` with `watch::Sender<Arc<AppState>>` | pending | — |
| **T10** | Migrate `forward_state` + stdio loop to `changed()`/`borrow_and_update()` | pending | — |
| **T11** | Migrate out-of-crate subscriber tests | pending | — |

## F3 — the churn engine

| # | Title | Status | PR |
|---|---|---|---|
| **T12** | `PersistClock` + `PERSIST_MIN_INTERVAL` | pending | — |
| **T13** | Collapse `save_conversation`; add persist field; swap 6 call sites | pending | — |
| **T14** | Turn boundary: `flush_persist()` in `set_running(false)` | pending | — |
| **T15** | P3: `flush_persist()` at conversation create/load/delete | pending | — |

## Close-out

| # | Title | Status | PR |
|---|---|---|---|
| **T16** | Docs: release notes + `agents.md` seam lines + `memory.md` durable entry | pending | — |
| **T17** | End-to-end verification run | pending | — |
