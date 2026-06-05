# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Fixed a compaction bug that could crash the next request with an
  Anthropic "orphaned tool_use" error.** When the compaction boundary
  happened to land *on* a `tool_result` whose `tool_use` was just before
  it, the tool call was correctly preserved but the inserted conversation
  *summary* got wedged between the `tool_use` and its `tool_result`.
  Anthropic requires the `tool_result` to immediately follow the
  `tool_use`, so the very next request failed with
  `tool_use ids were found without tool_result blocks immediately after`.
  The compaction boundary is now snapped forward past any trailing
  `tool_result`(s) so a `tool_use`/`tool_result` pair is never split by
  the summary — applied both where the plan is built and, defensively,
  where it is applied.

- **Fixed a false-negative in the `release-tag` Make target's
  protected-branch path.** When `master` is protected, `release-tag`
  opens and merges a `release/<v>` PR via the Gitea API. It previously
  decided merge success by looking for a top-level `.sha` in the
  `POST /pulls/N/merge` response — but Gitea returns an empty body on a
  successful merge, so the target aborted (`exit 1`) *even though the PR
  had merged*, leaving the tag unpushed and the release half-done. The
  check now queries the authoritative source — `GET /pulls/N` and tests
  `.merged == true` — so a genuine merge is recognised and the release
  proceeds to push the tag and build/publish. The merge response is
  still printed for diagnostics if the merge actually failed.

- **Refreshed the `chat_welcome` REPL snapshot for v0.7.0.** The 0.7.0
  release bump updated `Cargo.toml` but not the insta snapshot that
  renders the welcome banner (`PeakBot v…`), leaving `master` with a
  failing `cargo test`. The snapshot now reflects `v0.7.0`.
