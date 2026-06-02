# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Semantic document memory — new `doc_index` / `doc_search` tools
  (Phase 1).** PeakBot can now build a persistent, per-repo semantic
  index over documents and retrieve the most relevant chunks by meaning.
  `doc_index` parses a file or directory (txt, md, source code, HTML,
  PDF, DOCX), splits the text into overlapping chunks, embeds them, and
  stores them; `doc_search` embeds a query and returns the top-k most
  similar chunks with their source and similarity score. Re-indexing is
  **idempotent**: each chunk's id is a stable hash of
  `(source_path, chunk_index)` and the file's content `sha256` is stored
  in metadata, so pointing `doc_index` at a folder again reports
  `indexed N, updated K, skipped M (unchanged)` and never duplicates.
  Re-indexing a file that **shrank** to fewer chunks now reaps the
  orphaned trailing chunk rows (deleting from the new chunk count upward
  until a missing id), so a shrunken or emptied file never leaves stale
  chunks behind to surface in search.
  Both tools are **opt-in** — they are only registered when a
  `vector_db:` block is present and `enabled`; otherwise they are not
  exposed at all (no silent no-op). Built-in tool count is now **13**.
  The DB file is created **lazily on the first index** — merely enabling
  `vector_db` no longer writes `.peakbot/vectors.db` at startup. A
  read-only session (`doc_search` before anything is indexed) returns no
  hits and touches neither disk nor the embeddings endpoint; only the
  first chunk written materializes the store.

  Configure with an OpenAI-compatible embeddings endpoint (independent
  of the chat provider — OpenAI, llama.cpp, Ollama, LM Studio, TEI, …):

  ```yaml
  vector_db:
    enabled: true
    db_path: ./.peakbot/vectors.db          # per-repo; default if omitted
    embeddings:
      base_url: https://api.openai.com/v1
      api_key: sk-...                        # optional for local servers
      model: text-embedding-3-small
      dimensions: 1536                        # must match the model
  ```

  Storage is [`ruvector-core`](https://crates.io/crates/ruvector-core)
  (embedded HNSW index + redb persistence), pulled with
  `default-features = false` so it drags in **no** duplicate reqwest and
  **no** `simsimd` C library — keeping the Linux/Windows/macOS
  cross-compiles clean. PDF text extraction uses the pure-Rust
  `pdfsink-rs`; DOCX uses `docx-lite`; HTML reuses the existing
  `spider_transformations` text extractor. The vector DB file is
  gitignored. Dimension mismatches (model vs. existing DB) surface as a
  clear, actionable error rather than silent corruption.

- **New `fetch_page` tool — websites as clean Markdown.** Added a
  dedicated tool for fetching web pages and converting them to Markdown
  (`markdown: true` by default; set `false` for raw HTML). It uses the
  [`spider`](https://crates.io/crates/spider) crate's single-page
  primitive (`Page::new_page`) — a one-shot HTTP fetch, *not* the
  crawler — plus `spider_transformations` for the HTML→Markdown
  conversion. Output is prefixed with the HTTP status and truncated to
  50,000 chars, matching `fetch_url`. The existing `fetch_url` tool is
  unchanged and remains the right choice for JSON/REST APIs, XML, and
  other raw-data endpoints; the tool descriptions steer the agent to
  pick `fetch_page` for human-readable pages and `fetch_url` for raw
  data. Retry policy (no headless browser involved): transient failures
  (429 rate-limit, 408/425, or any 5xx) are retried up to 3 times with
  exponential backoff + jitter; a `403 Forbidden` is retried once with a
  realistic browser user-agent (some sites only serve browser-shaped
  UAs). Permanent client errors (400/401/404/…) are not retried.
  Built-in tool count is now **11**. The spider dependency is
  pulled with `default-features = false` + `reqwest_rustls_tls` so it
  reuses peakbot's existing reqwest 0.13 / rustls stack and drags in no
  headless-browser, sqlx, or sysinfo baggage.

- **MCP OAuth static client credentials — Slice 3a (#19).** Extends the
  OAuth 2.1 wiring so PeakBot can connect to MCP servers that don't
  support Dynamic Client Registration — primary target: Google
  Workspace MCP servers (Gmail, Drive, Calendar). The `auth:` block
  gains three optional fields on the `oauth` variant: `client_id`,
  `client_secret`, and `scopes`. All-absent reproduces the Linear-shape
  DCR flow byte-for-byte; `client_id` present switches to rmcp's
  `AuthorizationManager::configure_client` path with the user-supplied
  credentials. `client_secret` without `client_id` is rejected at boot.
  `scopes` (when set) flow into both the registration call and the
  authorisation URL so the consent screen requests exactly what the
  user configured. The ephemeral-loopback redirect URI is unchanged
  and works with Google's Desktop-app OAuth client type (RFC 8252 §7.3
  loopback exception). Four new config-shape tests pin: static-creds
  round-trip, public-client no-secret allowed, `client_secret`-without-
  `client_id` rejection, and `deny_unknown_fields` on the oauth arm
  (typo `scope:` is loud, not silent). Test count up to **733**.

- **MCP OAuth 2.1 support — Slice 2 (#19).** PeakBot can now connect to
  streamable-HTTP MCP servers that require OAuth 2.1 + Dynamic Client
  Registration (RFC 7591) + PKCE (RFC 7636) — primary target:
  [Linear's MCP server](https://mcp.linear.app/mcp). Configure with
  `auth: { type: oauth }`; on first connect PeakBot opens your browser
  to authorise, then caches the tokens at
  `dirs::cache_dir()/peakbot/mcp-auth/<server-name>.json` (mode `0600`
  on Unix; group/world-readable caches are rejected on load with a
  remediation hint). Subsequent runs skip the browser entirely; rmcp's
  `AuthorizationManager` silently refreshes expired access tokens via
  the stored refresh token. The callback listener binds on
  `127.0.0.1:0` (ephemeral) and registers that exact port via DCR
  before opening the browser, so the redirect URI matches byte-for-byte.
  On SSH (`$SSH_CONNECTION` set) the URL is printed instead of opening
  a browser — forward the callback port with `ssh -L PORT:127.0.0.1:PORT`
  to complete the flow from a workstation. New module `src/mcp_auth.rs`
  with 9 unit tests (filesystem cred store round-trip, 0600 mode,
  world-readable reject, axum callback router happy + CSRF mismatch,
  `state` extraction, percent-decoder); test count up to **729**.

- **MCP auth config shape — Slice 1 of OAuth support (#19).** Added the
  locked `auth: { type: bearer | oauth, token?: "…" }` block on
  `mcp_servers[*]`. Bearer is a drop-in replacement for the legacy
  top-level `auth_token` field; `oauth` parses but bails at connect time
  with an actionable error until Slice 2 wires the flow through a new
  `src/mcp_auth.rs`. The legacy `auth_token` field keeps working for one
  release with a deprecation warning at connect time; setting both
  `auth_token` and `auth` is a hard config error. A defensive `Bearer `
  prefix strip lands in the same pass: pasting `auth_token: "Bearer xxx"`
  no longer produces a doubled `Authorization: Bearer Bearer xxx` header.
