# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Added a `--stdio` frontend (#54). `peakbot --stdio` runs an NDJSON stdin/stdout View for IDE integrations (e.g. the VSCode extension) instead of the terminal UI, reusing the same agent, providers, tools, skills, MCP servers, and conversation persistence. Absorbed from the standalone `peakbot-stdio` crate into `src/ui/stdio` — no new crate or binary; logs are routed to stderr so stdout stays a clean protocol channel.
- Fixed compacted conversations breaking on `/load` (#59). Context compaction now persists the per-message `compacted` flag and the inserted summary, so a conversation compacted in a previous session reloads with its compacted state intact instead of resurrecting the full history and forcing manual re-compaction.
