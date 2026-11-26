# Vector DB with git merge — one knowledge base, many machines

The `doc_index` / `doc_search` tools store your documents in a semantic vector
database. Every new store is **v2**: a `peakbot.vector` metadata record
(fingerprint + manifest) lives inside the file. A store's behavior is derived
purely from **where the DB file lives** — inside a git repository, or outside
one.

Living that store inside your git repository — so multiple clones (and your
machines over time) point at the same vector store and merge it like any other
tracked file — is a **capability** of the v2 store, not a database mode. The
`db_path` you choose decides which contract the store uses; deciding to *commit*
the DB is a separate, opt-in step.

## The two store contracts (both v2)

- **Repo-contained** — the DB file resolves inside a git repository. The store
  anchors to the nearest `.git` above it and keys documents by **repo-relative
  source IDs** (e.g. `src/foo.rs`) — the same in every clone, so the same file
  is the same row everywhere. It keeps a *fingerprint* (which embedding model,
  vector dimension, and chunking it uses) and a *manifest* (which roots were
  indexed and what they cover) so the DB can be validated, merged, and
  reconciled without guessing.
- **Local** — the DB file resolves outside any git repository. Same v2 record
  and same fingerprint, but documents are keyed by **absolute local paths**,
  discovery is a plain filesystem walk (no git, no `.gitignore`), and the store
  keeps no git state and runs no reconcile.

Only files the store is allowed to see are indexed; for a repo-anchored store,
only files inside the git repo that owns it can be indexed — a file outside that
repo is an error, not a silent fall-back to absolute paths.

## The config

`vector_db:` is a per-profile block, exactly as before:

```yaml
profiles:
  docs:
    vector_db:
      db_path: ./.peakbot/vectors.db
      embeddings:
        base_url: https://api.openai.com/v1
        api_key: sk-…
        model: text-embedding-3-small
        dimensions: 1536
```

- `db_path` (required) — the DB location. The conventional repo-contained path
  is `./.peakbot/vectors.db`. Repo vs local is decided from the path on disk at
  open time: a `.git` anywhere above the DB file makes the store repo-anchored
  (repo-relative IDs, gitignore-aware discovery, reconcile); otherwise it is
  local (absolute IDs, plain discovery, no state).
- `embeddings` (required) — `base_url`, `model`, and `dimensions` are required;
  `api_key` is optional (for local or anonymous servers).

The fingerprint deliberately **ignores** `base_url` and `api_key`: serving the
same model from a different endpoint is the same embedding space. Changing the
**model**, **dimensions**, **chunk size**, or **chunk overlap** is a *different*
embedding space — the universal fingerprint check makes the DB refuse to reopen
rather than return wrong matches, so a model change means rebuilding it.

## Committing the DB is opt-in

The repository ignores `.peakbot/vectors.db` by default (a `.gitignore` rule),
and the local reconcile state at `.peakbot/vectors.db.peakbot/` is never tracked.
So the DB is *untracked by default* — committing it is the opt-in step.
To make the DB travel with the code so every clone shares it:

1. **Un-ignore the DB file.** Remove or override the `.gitignore` rule that
   ignores `.peakbot/vectors.db` so that file is tracked. Add the local-state
   path to `.gitignore` so it stays untracked:

   ```gitignore
   .peakbot/vectors.db.peakbot/
   ```

2. **Declare the merge driver** in a repo-root `.gitattributes`:

   ```gitattributes
   .peakbot/vectors.db merge=peakbot-vectors
   ```

3. **Wire the driver to the binary**, once per clone:

   ```bash
   git config merge.peakbot-vectors.driver 'peakbot vectors merge-driver %O %A %B %P'
   ```

   Use the absolute path to `peakbot` if the command is not on your `PATH`; the
   placeholders `%O %A %B %P` (git's standard `original`, `current/ours`,
   `theirs`, and `worktree` files) must stay exactly as-is. This is repo-local
   git config, so it already lives only in each clone.

The merge driver is the *only* mechanism you set up. There are no git hooks to
install and no manual "sync" command to run.

## What git does to the DB on a merge

When two clones diverge and you merge, git calls `peakbot vectors merge-driver`.
It is **fully offline**: it reads no config, never touches the keyring, and never
embeds or touches the network. Its policy is conservative and deterministic:

- **Ours wins** on any source group both sides touched — the overlapping rows
  (vectors, text, and index metadata) are kept exactly as ours.
- **Theirs-only** sources are adopted with their original rows, so a file added
  in the other branch lands in the store.
- The **manifest is a deterministic union**: roots sorted by path (ours wins
  when both sides recorded the same root), and a sorted, de-duplicated source set.
- If the other side is **unusable** — a v1 DB, a different fingerprint
  (wrong model), or a corrupt file — it is *not* mixed in; ours wins and the DB
  stays safe.
- If the DB is corrupt on *both* sides, the current file is left untouched for
  inspection rather than corrupted further.

After every merge attempt the driver writes a small **pending** marker next to
the DB (`.peakbot/vectors.db.peakbot/pending`). That marker is what makes the
next use self-healing.

## Staying fresh — automatic reconcile, no manual sync

You never run a sync command. The store keeps itself current in two places:

1. **At the start of every turn** — each user turn and each background-output
   turn checks the store before the first agent call.
2. **Before every `doc_index` / `doc_search` call.**

On each of those moments the store reconciles, triggered by: the pending marker
left by a merge, a missing local-state directory (a fresh clone or a cleaned
cache), a `HEAD` change, or the DB file being replaced on disk. Reconcile
replays the manifest and:

- **Reuses hashes** — a file whose text (plus metadata) is unchanged is *not*
  re-embedded at all; only the search query itself is embedded.
- **Embeds only changed** chunks, and **prunes** chunks of files that were
  deleted.

If the embeddings **provider fails** during reconcile (network, a 401, a quota
error), the reconcile stops and the pending marker is *left set*, so the next
turn simply retries. It never half-updates the DB.

## Search refuses stale results

Because reconcile runs before any read, a search on a store that is still stale
or unusable — fingerprint mismatch (wrong model), a corrupt DB, or a worktree
mid-merge — **errors instead of returning possibly-wrong matches**. A clean,
stable checkout always resolves to a fresh store within one turn.

## What git merge does *not* do

- **Existing v1 stores are only temporary read-only compatibility.** A populated
  pre-v2 store (no `peakbot.vector` record) can still be searched, but
  `doc_index` and any other write returns `MigrationRequired`. The migration
  lands in PR2, and both PRs ship in the same release. A zero-row empty store
  is adopted into v2 on its first write; a populated v1 store is never migrated
  in place and never given full-root coverage.
- **Relocating a store across the repo/local boundary does not mix rows.**
  Reading existing rows is still possible (search works), but writes and
  reconcile are rejected so relative and absolute source IDs are never mixed.
- **No git hooks, no manual command.** Everything above is automatic via the
  `.gitattributes` + `git config` driver and the per-turn reconcile — there is
  nothing else to remember.
- **It only ever indexes one repo's knowledge.** A repo-anchored store is tied
  to the repo that contains its DB file; a file outside that repo is an error.

## Don't merge while another session is actively indexing

The merge driver writes its result to a temp file and renames it atomically
over the DB. If you merge while another PeakBot session is actively indexing
the DB — on **any OS** — git can race an in-flight write, or a live
process can be left holding a DB inode that no longer points at the file git
just replaced. Reconcile detects that drift and repairs it on the next turn,
but the safe move is to **stop the session that is indexing before the
merge.**

### Windows: close every PeakBot instance before merging

On Windows this is stricter: replacing a file a process still has open can
fail outright (sharing violation), so git itself aborts the merge. **Close
PeakBot — every instance (TUI, web, CLI), not just the window you're in —
before any merge that touches the DB, then reopen it afterward.** The
driver's pending marker makes the reopen safe: the first turn reconciles
automatically, so nothing is lost.

---

### Files involved

| Path | Status |
|------|--------|
| `.peakbot/vectors.db` | The DB — *untracked by default*; *you choose* to commit it |
| `.peakbot/vectors.db.peakbot/` | Local reconcile state (`state.json`, `pending`) — **always untracked**; none for local (non-git) stores |
