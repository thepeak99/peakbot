# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Fixed a `peakbot --web` deadlock: a `todo_list ↔ state` lock-order inversion could permanently wedge the process when a todo mutation raced a conversation persist (e.g. a web tab reconnect during an active tool loop). `todo_list` is now a leaf lock — `sync_todo_to_ui` snapshots under it and releases before taking `state.write`, mirroring the existing `stats` fix. Added the `update_todo_and_persist_current_do_not_deadlock` regression test.