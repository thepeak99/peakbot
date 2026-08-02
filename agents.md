# PeakBot Documentation

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.38). It runs a web UI by default and a terminal TUI (`peakbot --tui`) or NDJSON stdio frontend (`peakbot --stdio`), equipped with filesystem, shell, web fetch, and web search tools. It also supports dynamically loading tools from MCP (Model Context Protocol) servers and Agent Skills, conversation persistence, todo management, an event-driven hooks system for cost tracking, an opt-in multi-agent pipeline, a first-run setup wizard, and `peakbot install` / `peakbot service …` verbs for end-user install + start-at-login.

## Architecture

```
User (terminal / web)
  → REPL loop (src/main.rs, src/lib.rs) — input, commands, chat_history, todo state
  → Provider abstraction (src/providers/) — DynAgent enum over
    OpenRouter / OpenAI / Anthropic / LlamaCpp / Ollama, + CostTracker
  → Context manager (src/context_manager.rs) — auto-compaction near the window limit
  → Rig agent — system prompt (skills + env + agents.md), built-in + MCP tools;
    Rig owns the agentic loop (tool dispatch until text or max-turns)
  → Event channel (src/hooks/) — SessionHook emits AgentEvent
    (tokens, cost, tool calls) → AgentRunner → CostTracker
  → Conversation manager (src/conversation_manager.rs) — JSON persistence,
    auto-resume, listing
```

## Core Components

### Provider Abstraction (`src/providers/mod.rs`)

| Provider | Notes | Cost tracking |
|----------|-------|---------------|
| **OpenRouter** | 100+ models via one API | ✅ |
| **OpenAI** | Responses API; configurable endpoint (Azure, proxies) | ✅ |
| **Anthropic** | Messages API; Claude or any compatible server (llama-server `/v1/messages`). The only provider whose tool results carry images — enables `view_image` | ❌ |
| **LlamaCpp** | llama.cpp-compatible endpoints | ✅ |
| **Ollama** | Local models; hookless by type (no events/cost/stop TEE) | ❌ |

Key types: `DynAgent` (enum for runtime provider switching), `CostTracker` (session statistics), `ProviderInfo` (name, model, pricing/vision capability). Agent construction happens in `create_provider()` — read the signature in the source rather than trusting any doc copy; it threads config, tools, filters, pipeline registry, and state through to the per-provider `create_*_agent` constructors, which share `add_builtin_tools`.

### Context Manager (`src/context_manager.rs`)

Compacts long conversations automatically: triggers at 80% context usage (configurable), always keeps the last N messages (default 5), auto-detects the context window from the model name (Claude 200k, GPT-4o 128k, Gemini 1M+; overridable per model).

### Hooks System (`src/hooks/`)

- `session_hook.rs` — `SessionHook` (implements `PromptHook`, emits events), `SessionStats`, `ModelPricing` (fetched from OpenRouter API or defaults)
- `channel.rs` — async event channel + processor
- `events.rs` — `AgentEvent` (CompletionResponse / ToolCall / ToolResult), `TokenUsage`

Flow: `SessionHook` emits an `AgentEvent` per LLM call → async channel → `AgentRunner` → `CostTracker` updates session stats (visible via `/stats`).

### Background Processes (`src/bg_processes.rs`)

Long-running PTY-backed processes spawned by the `bash_bg` tool. Each process gets a numeric id, a ring buffer of captured output, and a per-process `cooldown_secs` (default 60) that paces output injection.

- `bash_bg start` spawns a PTY via `portable-pty` under `sh -c`; a reader thread streams ANSI-stripped lines into the ring buffer and pings the agent loop.
- Between turns, `StateManager::drain_bg_output_into_synthetic_turn` builds a `[bg output]` block per eligible buffer and runs an agent turn over it, persisted with `MessageSource::Background { proc_ids }`.
- Cooldown coalesces chatty output into one flush per window; `cooldown_secs: 0` = real-time (for external-input bridges). **Process exits always bypass the cooldown.** A real user message resets all cooldowns so buffered output flushes alongside it.
- `/new`, `/model`, `/load` kill all bg processes. The registry is in-memory only — nothing survives a restart.

Details: `bash-background.md`.

### Skills System (`src/skills/`)

Skills are modular capability packages: a directory containing a `SKILL.md` (plus optional `scripts/`, `references/`, `assets/`). Discovery (`discovery.rs`) loads from `~/.agents/skills` (global) and `./.agents/skills` (local, relative to session cwd); skills are listed in the system prompt and read on demand.

## Agent Initialization Flow (`src/main.rs` / `src/session.rs`)

1. Load configuration (`config.yaml` + env vars)
2. Load skills; build the system prompt (persona + core guidance + memory section + skills + env + agents.md — see the [pipeline prompt recipes](#the-orchestrator-vs-sub-agent-prompt))
3. Load MCP servers (async), extract tools
4. `create_provider()` builds the `DynAgent` + `CostTracker` + todo state + event channel
5. `AgentRunner` runs the loop

State: `chat_history` (Vec<Message>) in `AgentRunner`; shared `Arc<Mutex<TodoList>>` for todos; `SessionStats` behind `Arc<Mutex<>>`. The agent is stateless between tool turns — Rig dispatches tool calls and loops until text or the turn limit.

## Tools

All tools live in `src/tools/` and implement `rig::tool::Tool` (`NAME`, `Args`, `Output`, `Error`, `definition()`, `call()`). Tool errors are returned to the model as tool results — the model self-corrects; they never crash the agent.

**The authoritative tool list is `BUILTIN_TOOL_NAMES` in `src/config/mod.rs`** — keep any table below in sync with it, never the other way round.

| Tool | File | Notes |
|------|------|-------|
| `bash` | `bash.rs` | Shell execution with timeout; output truncated to last ~50k chars, full output saved to /tmp/peakbot/ |
| `powershell` | `powershell.rs` | Windows counterpart of `bash` — one shell tool is registered based on the detected shell (`shell_detect.rs`), never both |
| `bash_bg` | `bash_bg.rs` | Long-running PTY processes (start/stop/list/send_line); synthetic `[bg output]` turns, per-process cooldown |
| `file_create` / `file_str_replace` / `file_insert` | `file_edit/` | Create / replace (whitespace-flexible fallback) / insert-at-line |
| `file_read` | `file_read.rs` | Read with line ranges |
| `pdf_read` | `pdf_read.rs` | PDF → text/markdown (`pdf_oxide`), page ranges, 50k-char cap |
| `list_directory` | `list_directory.rs` | Listing with recursion (max depth 3) |
| `fetch_url` | `fetch_url.rs` | HTTP GET, raw body |
| `fetch_page` | `fetch_page.rs` | Fetch a web page → clean Markdown, with retry/backoff |
| `web_search` | `search.rs` | SearXNG-based search (needs `searxng:` config) |
| `think` | `think.rs` | Reasoning scratchpad |
| `todo` | `todo.rs` | Todo list management |
| `doc_index` / `doc_search` | `doc_index.rs` / `doc_search.rs` | Semantic vector store — registered only when `vector_db:` is configured |
| `view_image` | `view_image.rs` | Load a local image into vision context — registered only on the Anthropic provider (the lone rig provider whose tool results deliver images) |
| `delegate` | `pipeline/delegate_tool.rs` | Run one sub-agent to completion — registered only when a `pipeline:` is configured AND the conversation opted in |

Plus optional **MCP tools** from configured servers (wrapped in `LoggingToolDyn` for tracing).

### Time budgets

Every tool — built-in and MCP alike — is wrapped at registration in `TimeBudget` (`src/tools/time_budget.rs`); there is no unbudgeted state. `timeouts.tool_secs` (default 30 min) is the ceiling; tools that own a longer deadline get a derived one *above* it so their own message wins: `bash`/`powershell` get `max(tool_secs, 7500s)` (above their own 7200s clamp), `delegate` gets `timeouts.delegate_secs + 300s` (above the loop's own budget, leaving room for the INTERRUPTED handoff).

Expiry returns `⏱ TIMEOUT: …` as a normal tool result — the model self-corrects; the turn is not killed. Design: `docs/tool-time-budget-design.md`.

### The `thought` field (ThoughtGate)

No tool declares a `thought` parameter itself. Every built-in **and** MCP tool is wrapped at registration in `ThoughtGate` (`src/tools/thought_gate.rs`):

1. **Schema injection** — `definition()` adds a required `thought` string property (idempotent if the inner tool already has one).
2. **Strip before delegate** — `thought` is synthetic; it's removed from args before the inner call (strict MCP servers may reject unknown params).
3. **Soft nudge, never a hard error** — a missing/blank `thought` never blocks the call; a one-line reminder is appended to the result.

Two exceptions, registered ungated: `think` (its `thought` *is* the payload) and `todo` (task text already carries the plan; some models structurally refuse the extra field).

### Vector store (`src/vector/`)

`doc_index`/`doc_search` share one `VectorStore`: a `ruvector-core` `VectorDB` (HNSW + redb) behind `Arc`, plus an `EmbeddingsClient` hitting any OpenAI-compatible `/v1/embeddings` endpoint. The DB is created **lazily on first write** — enabling `vector_db:` touches no disk until the first chunk is indexed. Sync DB ops run under `spawn_blocking`. Chunk size (1000 chars), overlap (200), Cosine distance, and default `k` (5) are constants, not config.

## Error Handling

- **Tool errors** → returned to the model as tool results; it self-corrects.
- **API errors** (auth, network) → surface as `PromptError`, printed; loop continues. Transient errors are retried with backoff (`is_transient_prompt_error`).
- **Max turns exceeded** → printed as an error; user retries or rephrases.

## Configuration

Loaded from `config.yaml` in the platform config dir, merged with a per-repo `.peakbot/config.yaml`; environment variables take precedence. Key env vars: `PROVIDER` (JSON provider config), `AGENT_MAX_TURNS`, `MCP_SERVERS` (JSON), `SEARXNG_BASE_URL`, `PEAKBOT_WEB_TOKEN`.

### Live reload on session verbs (`/new`, `/model`, `/cd`, `/load`)

Editing `config.yaml` or skills does not require a restart: each session verb re-reads config and re-scans skills before rebuilding the agent (per-session — never mutates the process-wide `SessionDeps`). Malformed YAML or an invalid `default_model` warns and keeps the previous config. `/new` rebuilds on the **currently-active** model (it does not bounce you to `default_model`).

| Reload-safe (next session verb) | Boot-only (`⚠ … ignored — restart to apply.`) |
|---------------------------------|-----------------------------------------------|
| `providers:` / `default_model` (new aliases resolve) | `mcp_servers` (live subprocesses) |
| skills + system prompt | `vector_db` (redb/HNSW handle) |
| `searxng.*`, `bash.env`, `agent_max_turns` | `web.*` (read once by the session reaper) |
| `cost_tracking`, `context.*`, `retry.*`, `memory.*`, `timeouts.*` | `pipeline` (built sub-agent registry) |
| `tools.*` (built-in filter) | `provider` (legacy block) |
| | `http.*` (published once into the client factory) |

There is deliberately no filesystem watcher — reload is explicit, to avoid heisenbugs mid-turn.

### Multi-model with `/model`

Declare a list of providers, each with models, plus `default_model` for the boot alias:

```yaml
providers:
  - name: openrouter
    type: openrouter            # types: openrouter | openai | anthropic | llamacpp | ollama
    api_key: sk-or-v1-xxx
    # base_url: …               # openai/anthropic/llamacpp/ollama endpoints are configurable
    models:
      - name: anthropic/claude-3.7-sonnet
        alias: sonnet
        max_tokens: 8192
      - name: google/gemini-2.0-flash-001   # no alias → address as openrouter/google/gemini-2.0-flash-001
default_model: sonnet
```

Rules:
- `alias` optional; without it a model is addressable only as `<provider_name>/<model_name>`. Aliases are globally unique, match `^[A-Za-z0-9_./:-]+$`; the literal `unknown` is reserved.
- `default_model` is required iff any models are declared and must reference a declared alias.
- Per-model overrides: `max_tokens`, `temperature`, `num_ctx` (Ollama), `extra_params` (LlamaCpp), `context_window_override`, `prompt_caching` (Anthropic: `auto`/`auto_1h`/`manual`/`off`), `vision` (force image support on/off; omit for auto-detection).

`/model` semantics: no arg lists aliases (active marked `→`); `/model <alias>` starts a **new conversation** on that model (confirm overlay if the chat has content); unknown alias / current alias are safe no-ops with a message. The active alias is bound to the conversation metadata — `/load` re-activates it and rejects the load if the alias no longer resolves. MCP servers persist across switches.

The legacy single-provider block (`provider: { type: …, config: { … } }`) is still supported — same fields, one model, no aliases.

### Other config blocks

```yaml
bash:
  env: { MY_API_KEY: "…" }      # extra env for all shell commands

searxng:
  base_url: https://searx.example.com
  enabled: true                  # also: timeout_seconds, max_results, bearer_token

vector_db:
  enabled: true
  db_path: ./.peakbot/vectors.db          # relative: resolves per session cwd; absolute stays global
  embeddings:
    base_url: https://api.openai.com/v1   # any OpenAI-compatible endpoint
    api_key: sk-...
    model: text-embedding-3-small
    dimensions: 1536             # must match the model AND any existing DB at db_path

context:
  enabled: true
  threshold: 0.8                 # compact at 80% usage
  keep_recent: 5
  context_window: 200000         # 0 = auto-detect

cost_tracking: true              # OpenRouter only

memory:                          # governs the whole memory.md feature
  enabled: true                  # false = no prompt section, no auto-compaction
  threshold_bytes: 51200         # compact memory.md above this

tools:                           # built-in tool filter — pick ONE list
  disabled: [bash_bg, web_search]   # blocklist, XOR with:
  # only: [file_read, file_str_replace, bash]
  # Names = wire names from BUILTIN_TOOL_NAMES; unknown names rejected at load.

timeouts:                        # wall-clock ceilings on agent work; see
  tool_secs: 1800                # `http:` for per-socket network timeouts
  delegate_secs: 3600            # 1..=86400 each; 0 rejected at load

http:                            # outbound timeouts for EVERY client (LLM,
  connect_timeout_secs: 30       # embeddings, MCP auth, web tools). 0 = disabled.
  read_timeout_secs: 1800        # seconds of silence, reset on each read
```

`read_timeout` bounds *silence*, not duration — but completions are
non-streaming (`agent.prompt`), so nothing arrives until the model is done and
for LLM calls it acts as a ceiling on a single generation. Raise it if you run
models that legitimately think for longer than 30 minutes. Without it, an
upstream that accepts a request and never answers wedges the turn until the
process dies — Stop can't help, because `stop_requested` is only checked *after*
the call returns. Tools that set their own shorter total `.timeout()`
(`fetch_url`, `fetch_page`, `web_search`) are unaffected: whichever fires first
wins.

### MCP servers

```yaml
mcp_servers:
  - name: filesystem                       # stdio subprocess
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path"]

  - name: my-bearer-mcp                    # streamable-http + static bearer
    type: streamable-http
    url: https://example.com/mcp
    auth: { type: bearer, token: "xxx" }

  - name: linear                           # OAuth 2.1 + DCR + PKCE
    type: streamable-http
    url: https://mcp.linear.app/mcp
    auth: { type: oauth }                  # add client_id/client_secret/scopes for
                                           # providers without DCR (e.g. Google)
```

OAuth notes: first connect opens the browser; tokens cached under `~/.cache/peakbot/mcp-auth/<name>.json` (0600), refresh is silent. Under `$SSH_CONNECTION` the auth URL is printed instead — forward the callback port (`ssh -L 9999:127.0.0.1:9999 …`). The deprecated top-level `auth_token:` still works but warns; setting both forms is a hard error.

### REPL Commands

| Command | Description |
|---------|-------------|
| `/stats` | Session statistics (tokens, cost, per-lane breakdown) |
| `/context` | Context usage |
| `/compact` | Force context compaction |
| `/bg` | List background processes |
| `/model [alias]` | List / switch models |
| `/cd [path]` | Show / change session cwd |
| `/new`, `/load`, `/conversations` | Conversation management |
| `/subagents on\|off` | Per-conversation sub-agent opt-in (before first turn; needs `pipeline:`) |
| `exit` | Quit |

### Web UI (`peakbot`, the default)

Serves the embedded SPA + live chat over WebSocket. Use `peakbot --tui` for the terminal UI.

| Flag | Default | Description |
|------|---------|-------------|
| `--tui` | off | Terminal UI instead of the default web UI |
| `--bind <host:port>` | `127.0.0.1:7823` | Non-loopback bind **requires** a token |
| `--token <secret>` | — | Guards every route; prefer `PEAKBOT_WEB_TOKEN` (keeps it out of shell history) |
| `--tls` / `--tls-name <NAME>` | off | HTTPS with the built-in CA; `--tls-name` adds SANs (repeatable) |

**Sticky sessions** (#118): a conversation is addressed as `?convo=<uuid>`; a live session (StateManager + controller loop) stays bound to it independently of any socket. Reconnects and multiple tabs re-attach to the same running session. A session is reaped only when **fully idle** — no sockets, agent not mid-turn, no live `bash_bg` children — for `web.session_ttl_secs` (default 600; `web.reaper_tick_secs` 60). See `src/ui/web/registry.rs`.

**Auth**: the token is presented once as `?token=…`; the server sets a `peakbot_token` cookie which authenticates everything after, including the `/ws` upgrade. Single-operator shared secret, not a user system.

**TLS** (`--tls` or `web.tls: true`): PeakBot self-signs a long-lived CA in `dirs::cache_dir()/peakbot/tls/` (key 0600, never silently regenerated — delete the dir to rotate) and mints a fresh leaf each boot with SANs for loopback, the LAN IP, any bind IP, and `<hostname>.local`. The CA public cert is served at the tokenless route `GET /peakbot-ca.crt` for one-time install on phones (iOS additionally needs Settings → General → About → Certificate Trust Settings). TLS complements the token, never replaces it. Implementation: `src/ui/web/tls.rs` (`rcgen` + `rustls`/aws-lc-rs, no OpenSSL).

**Testing the web UI**: `make dev` runs backend (`:8080`) + Vite HMR (`:5173`); drive it with the `playwright-cli` skill (accessibility snapshots, console logs, WS traces). For pure server-side WS probing, `websocat ws://localhost:7823/ws`.

## Extending

### Adding a new built-in tool

1. Create `src/tools/my_tool.rs`: `Args` (Deserialize), error type (thiserror), tool struct implementing `rig::tool::Tool` (`NAME`, `definition()` with JSON Schema, `call()`)
2. Export from `src/tools/mod.rs`
3. Register in `add_builtin_tools()` in `src/providers/mod.rs`
4. Add the wire name to `BUILTIN_TOOL_NAMES` in `src/config/mod.rs` (the `tools:` filter validates against it)

### Adding Agent Skills / MCP tools

Prefer skills or MCP servers over baking tools into the binary. Skills: drop a directory with a `SKILL.md` into `~/.agents/skills/` or `./.agents/skills/` — discovered at startup and on session verbs. MCP: add to `mcp_servers:` in config (see above); tools load at boot.

## File Map (directory level)

```
src/
├── main.rs / lib.rs        # entry, AgentRunner, system prompt building
├── session.rs              # SessionDeps + create_session factory
├── config/                 # config loading/merging, ToolsConfig, BUILTIN_TOOL_NAMES
├── providers/              # DynAgent, create_provider, per-provider constructors, add_builtin_tools
├── hooks/                  # SessionHook, event channel, AgentEvent
├── pipeline/               # delegate tool, SubAgentRegistry (multi-agent)
├── state/                  # StateManager (single source of truth for sessions)
├── tools/                  # all built-in tools + ThoughtGate + LoggingToolDyn
├── skills/                 # skill discovery + SKILL.md parsing
├── vector/                 # VectorStore, embeddings client, parse/chunk
├── ui/                     # app_state, wire types; repl/ (ratatui), web/ (axum+WS+TLS), stdio/
├── bg_processes.rs         # BgRegistry (bash_bg PTY processes)
├── context_manager.rs      # compaction
├── conversation*.rs        # conversation data + persistence + titles
├── vision.rs               # [img:…] parsing, model vision detection
├── system_prompt_*.txt     # persona / core / memory prompt pieces
└── mcp_auth.rs             # MCP OAuth flow + token cache
tests/                      # integration harness (MockCompletionModel, scenarios), repl_tests (insta)
web/                        # SPA frontend (state.ts wire mirror, adapt.ts, components)
```

## Testing

Integration tests (`tests/integration.rs`) use a `MockCompletionModel` (no API calls): pre-configured text/tool-call/streamed responses, request tracking. `TestHarness` builds an agent with the mock + `InMemoryStorage` + isolated todo state:

```rust
let harness = TestHarness::new().with_text_response("Hello!").build();
let (output, _) = harness.run_prompt("Say hello");
assert!(output.contains("Hello!"));
```

```bash
cargo test                      # everything
cargo test --test integration   # integration only
```

Scenarios cover message roundtrips, stats/cost tracking, storage, todo tool, and error handling.

## Zen of Engineering Review (Mandatory)

**No implementation begins without a Zen review.** Before writing any code — feature, bugfix, or refactor — run the proposed change through the `zen-engineering` skill. It enforces: simplicity first, no code before the plan is locked, what can be removed, what will confuse people, clean boundaries (parse at entry, illegal states unrepresentable, DRY only for the necessarily-same), and YAGNI.

Provide the problem statement, planned approach, and tradeoffs. If the review flags significant issues, fix the plan before touching source. Models gold-plate by default — the Zen review is the first line of defense against complexity creep.

## Comment Style

Comments are signal, not narration:

- **2–3 lines tops, ideally 1.** Longer explanations belong in docs, not inline.
- **Explain *why*, not *what*.** Comment the non-obvious reason, the gotcha, the invariant.
- **No plans, stages, or temporal narration** (`// Step 1:`, `// now we…`, `// changed to fix X`). Version control is the record of evolution; comments describe the code as it is *now*.
- **Fix stale/bloated comments on sight** when touching a file.

## Commit Procedure

**All changes go through Pull Requests** — no direct commits to master. Every PR adds an entry to `release-notes/current.md` describing what changed.

### Release notes workflow (mandatory)

`current.md` is the working draft for the *next* release; the release pipeline reads `release-notes/<version>.md`, never `current.md`. Before `make release`:

```bash
git mv release-notes/current.md release-notes/0.3.0.md   # promote (strip the working-draft header)
git commit -m "docs: 0.3.0 release notes"
# recreate an empty current.md (header + "## Changes"), commit it
make release VERSION=0.3.0                                # picks up release-notes/0.3.0.md
```

Skipping the promotion ships the release with a fallback body (`Release <version>`) and loses the changelog.

### Pre-commit gate

Run from the repo root before *any* commit — same commands CI runs:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

If clippy fires, **fix it**. The only escape hatches: a scoped `#[allow(clippy::lint_name)]` with a one-line justification (e.g. `src/providers/mod.rs` allows `too_many_arguments`/`type_complexity` for provider constructors), or `#[allow(dead_code)]` on test utilities with a comment naming the future use. Never blanket-allow at crate level; never silence without a comment.

Why: the release flow builds four platforms — a warning that compiles on Linux can be a hard error elsewhere; `-D warnings` forces fixes at introduction time; `cargo fmt` keeps diffs reviewable.

### Internal Documentation

Analysis documents (plans, reviews, forensics, post-mortems) are **internal only** — never committed to the repo or referenced in public-facing content.

### Snapshot tests

`tests/repl_tests.rs` uses `insta` golden files. After a version bump or deliberate UI change, review the `.snap.new` diffs with `cargo insta review` (or `mv` a single eyeballed snapshot). Never commit `.snap.new` files.

## Build & Release

PeakBot ships as a single static binary for **Linux x86_64**, **Windows x86_64**, **macOS universal2**, and **Android arm64** (Termux/`adb shell` terminal binary, not an APK) — all built from Linux via container builds. Driver: top-level `Makefile`; cross-compilation: four sibling Dockerfiles.

| File | Builder | Notes |
|------|---------|-------|
| `Dockerfile.linux` | `rust:1.88-bookworm` | native; dummy-main caching trick |
| `Dockerfile.windows` | `rust:1.88-bookworm` + mingw | cross via gcc-mingw |
| `Dockerfile.macos` | `cargo-zigbuild` | zig + macOS SDK; universal2 fat binary |
| `Dockerfile.android` | `rust:1.91-bookworm` + NDK r27c | `cargo-ndk`; API level via `CARGO_NDK_PLATFORM` (24), **not** `-p` (collides with cargo `--package`); works clean because PeakBot is pure rustls/ring |

All four end `FROM scratch` with `--output type=local,dest=./output`. **Gotcha:** `ARG` does not cross `FROM` stage boundaries — each Dockerfile redeclares `ARG TARGET` in its scratch stage. `CONTAINER_BUILDER` auto-detects podman (preferred), falls back to docker.

### Make targets

`make help` for the full list. Highlights: `make` / `make build` (all four platforms), `make build-{linux,windows,macos,android}`, `make clean`, `make web` (SPA bundle for the default web mode), `make dev` (backend under cargo-watch `:8080` + Vite HMR `:5173`; requires cargo-watch + Node 22+). Non-release builds produce unversioned filenames; the release flow injects semver via `--build-arg VERSION`.

### Release pipeline

`make release` runs: `release-bump` (validate semver, refuse dirty tree unless `ALLOW_DIRTY=1`, bump `Cargo.toml`+lock, commit) → `release-tag` (annotated bare-semver tag, push; handles protected master by cutting a `release/X.Y.Z` branch + PR automatically) → `release-build-{linux,windows,macos,android}` → `release-publish` (Gitea release via REST + asset upload). In-flight state in `.release-version` (gitignored); each phase re-runnable — if publish dies mid-upload, just `make release-publish`.

```bash
export GITEA_TOKEN=...                    # required
make release VERSION=0.2.0                # GITEA_URL/OWNER/REPO derived from origin
```

Release body + tag message come from `NOTES=<path>`, else `release-notes/<v>.md`, else the literal `Release <v>`. The notes file is read via `jq --rawfile` — any Markdown is safe. Host needs: git, cargo, podman/docker, curl, jq, awk. To fully restart a failed release: delete local+remote tag, reset the chore commit, `rm .release-version`.

## Vision (image input)

Attach images inline with `[img:TOKEN]` tokens: filesystem path (`/`, `~`, `./` prefix), URL (`://`), or `data:` URI. Limits (`src/vision.rs`): 10 MB/image, 8 images/turn, png/jpg/gif/webp. Failures surface as system messages, never silently.

Gating rules (the part worth memorizing):

- `model_supports_vision(model)` is a conservative name-substring detector (gpt-4o/claude-3+/gemini/pixtral/llava/qwen-vl…); unknown names default to **false**.
- **The `anthropic` provider gates on transport, not model name**: its Messages transport carries images natively, so `[img:…]` works for *any* model name there (local GGUFs, gateway models). `supports_vision_for(provider, model) = provider=="anthropic" || model_supports_vision(model)`.
- Per-model `vision: true|false` overrides auto-detection via `providers::resolve_supports_vision` — the single point feeding both the `[img:…]` gate and `view_image` registration. `view_image` only delivers on the Anthropic transport, so `vision: true` elsewhere enables `[img:…]` but not `view_image`.
- Provider quirks: Anthropic requires base64 (refuses URLs); OpenAI accepts both; OpenRouter **silently substitutes a placeholder** for tool-result images; Ollama drops them.

Images persist **inline** as base64 in the conversation JSON. Internals: `vision.rs::parse_attachments_inline` → `state_manager.rs::user_content_from_attachment` → same prompt path as text turns.

## Multi-agent pipeline (orchestrator + sub-agents)

Opt-in via the `pipeline:` config block — absent or `enabled: false` means none of this exists and `delegate` isn't registered. Runnable example: `examples/pipeline-team/`.

### Per-conversation opt-in (default off)

Configuring `pipeline:` makes sub-agents **available**, not **on**. Each conversation opts in (web Agents-panel checkbox or `/subagents on|off`), **only before the first turn** — after that it's locked (flipping `delegate` mid-conversation would desync the tool list from wire history). Two distinct facts drive it: `pipeline_available` (config, boot-only) and `subagents_enabled` (per-conversation, persisted, serde-default false). `delegate` registers iff **both** are true — gated in `session::create_session` and `rebuild_agent_for_resolved`.

### The model

- You talk to the **orchestrator** — your normal top-level agent (whatever `default_model` resolves to; never listed under `pipeline.agents`).
- It gets a **`delegate(role, task)`** tool: runs one sub-agent — *(model alias, prompt)* + optional `env:`/`skills:`/`agents_md:` — to completion on a **fresh context**, returns one string. Sequential by construction; no parallel mode. A sub-agent has no memory of prior delegations — everything it needs goes in `task`.
- On completion, the sub-agent's last ≤10 earlier assistant messages (excluding its final reply) are saved to `<temp>/peakbot/delegate_{role}_{pid}_{counter}.txt`; a one-line pointer is appended to the delegate result string so the orchestrator can `file_read` it. Write failures degrade silently; hookless Ollama sub-agents get no file (empty snapshot).

### The isolation invariant

Three views that a single-agent chat conflates are pulled apart:

1. **Display transcript** — orchestrator turns *and* every sub-agent's turns, tagged by `MessageSource::SubAgent { role }` (rendered `🧩 <role>`; web mirrors via the `sub_agent` wire source).
2. **Orchestrator wire context** — its own turns + each delegate ToolCall/result string. **Never** sub-agent internals.
3. **Sub-agent wire context** — role prompt + one task + its own tool round-trips; discarded after the call.

The load-bearing guarantee: sub-agent turns are filtered out of the orchestrator's wire history by a single lane filter in `get_agent_history` (`msg.source.is_orchestrator_lane()`). Regression test: `get_agent_history_excludes_sub_agent_lane_keeps_background`.

Delegation tokens/cost roll into the parent `/stats` lane-agnostically (exception: Ollama sub-agents are hookless — untracked).

### Config shape

```yaml
pipeline:
  enabled: true
  orchestrator_prompt: |        # optional; appended to the orchestrator's prompt
    You lead a small team. Delegate research and review; keep the main thread on planning.
  agents:
    researcher:
      model: flash              # alias from providers: (omit → default_model)
      prompt: "You research codebases and the web. Return a tight brief."
      skills: { only: [github] }        # per-role skill gate: only: XOR disabled:, or enabled: false
      agents_md: true                   # opt this role into the repo's agents.md (default false)
    reviewer:
      model: sonnet
      prompt: "You review diffs and critique."
      env: { REVIEW_STRICT: "1" }       # merged into THIS role's bash env only
```

Validation at load (unknown alias, empty prompt/role, bad skills filter = boot error). `pipeline:` is **boot-only**.

### Prompt recipes

`build_system_prompt` (`src/lib.rs`) composes three recipes from the same pieces:

| Piece | Agentless | Orchestrator | Sub-agent |
|-------|:---------:|:------------:|:---------:|
| persona (`system_prompt_persona.txt`) | ✅ | ❌ | ❌ |
| core tool guidance (`system_prompt_core.txt`) | ✅ | ✅ | ❌ |
| memory.md workflow (if enabled) | ✅ | ✅ | ❌ |
| skills | ✅ all | ✅ all | ⚙️ per-role filtered |
| env block (cwd/time/OS/shell) | ✅ | ✅ | ✅ |
| `agents.md` | ✅ | ✅ | ⚙️ per-role `agents_md:` |
| `orchestrator_prompt` | — | ✅ if set | — |

A sub-agent's preamble (`build_sub_agent_preamble`, rebuilt fresh per delegation) is `role.prompt` + live env block + filtered skills (+ agents.md if opted in) — its `prompt` is its whole persona. The framing is recomputed at the single agent-rebuild seam (`rebuild_agent_for_resolved`), so toggling sub-agents, `/cd`, and `/model` all produce the right prompt.

### Tools, isolation, stop

Sub-agents get the full built-in toolset **minus `delegate`** (no nested delegation) and no MCP tools; fresh todo list; isolated bash env (`env:` never leaks across roles). No sandbox in v1 — a sub-agent can write and run bash. Stop during a delegation aborts the **whole turn** — sub-agent and orchestrator unwind together (stop routes to the innermost hook via `ActiveSubAgentHook`); there is no resumption path.

Failures are handled like the orchestrator's own: transient wire errors are retried in place (`retry.*`, shared `providers::retry`), unknown tool names and tool errors go back to the sub-agent as tool results it can self-correct from. What survives that ends the delegation and comes back as a summarised `INTERRUPTED` result — see `src/pipeline/handoff.rs`. The whole delegation is bounded by `timeouts.delegate_secs` (default 1 h); on expiry the transcript takes that same path — summarised and returned as `INTERRUPTED`, not discarded.

Where it lives: `src/pipeline/{delegate_tool,registry}.rs`, `build_sub_agent` in `src/providers/mod.rs`, `MessageSource` + lane filter in `src/ui/app_state.rs` / `src/state/state_manager.rs`.

## CI (Gitea Actions)

Workflow: `.gitea/workflows/ci.yml` — single job on runner `shinpachi` (label `dind`), `rust:1.95` container. Gate = `cargo fmt --all -- --check` → `cargo clippy --all-targets --locked -- -D warnings` → `cargo test --workspace --locked` (reproduce locally with exactly those; autofix with `cargo fmt --all` + `cargo clippy --fix --allow-dirty [--tests]`).

### Inspecting a failed run

`GITEA_URL`/`GITEA_TOKEN` are in the shell env; owner `ai-bots`, repo `peakbot`.

```bash
# 1. latest runs — note the internal `id` (NOT the UI run_number; every API below wants `id`)
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/tasks?page=1&limit=5" \
  | jq '.workflow_runs[] | {id, run_number, status, conclusion, head_sha: .head_sha[0:8]}'

# 2. job id — /actions/runs/{id}/jobs returns EMPTY on our Gitea; use /actions/jobs
#    (the ?run= filter is ignored — returns every job; match on head_sha)
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/jobs?run=x" \
  | jq '.jobs[] | {id, head_sha: .head_sha[0:8], status, conclusion}'

# 3. log (404 "job not found" = wrong id, probably the run id)
curl -s -H "Authorization: token $GITEA_TOKEN" \
  "$GITEA_URL/api/v1/repos/ai-bots/peakbot/actions/jobs/$JOB_ID/logs" -o /tmp/peakbot/job.log

# 4. failure is right before "##[error]Process completed with exit code N";
#    read upward to the nearest "::group::Run …" for the failing step
grep -E "::group|::error|Process completed|error\[|test result|FAILED|panicked" /tmp/peakbot/job.log | tail -60
```

After pushing a fix, poll `actions/tasks?limit=1` for your head_sha reaching success/failure — warm container ~90 s, cold 3–6 min; don't poll faster than 10 s. Runner sanity: the repo/org `actions/runners` endpoints return empty (runner is instance-level) — that's normal here.

### Gitea-Actions vs GitHub-Actions: the must-knows

1. **`actions/checkout` is mandatory** — the workspace starts empty. We use a pure-git fetch fallback (no `node` needed in the container).
2. **JS actions need `node`** — slim images don't have it; prefer shell steps.
3. **`GITHUB_TOKEN` is NOT auto-exported** into `run:` env — mirror it: `env: { GITHUB_TOKEN: ${{ github.token }} }`.
4. **Toolchains arrive minimal** — `rustup component add rustfmt clippy` if the gate needs them.
5. **`runs-on:` must match an advertised runner label** (ours: `dind`).
6. **Tests needing network/external binaries must be `#[ignore = "reason"]`** — the container has no `uvx`/`npm`/general egress; run locally with `cargo test -- --ignored`.
7. **Don't copy `working-directory:` from monorepos** — peakbot's `Cargo.toml` is at the root.
