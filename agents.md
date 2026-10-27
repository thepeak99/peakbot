# PeakBot Documentation

## Overview

PeakBot is a single-agent coding assistant built with [Rig](https://github.com/0xPlaygrounds/rig) (`rig-core` v0.38). It runs a web UI by default and a terminal TUI (`peakbot --tui`) or NDJSON stdio frontend (`peakbot --stdio`), equipped with filesystem, shell, web fetch, and web search tools. It also supports dynamically loading tools from MCP (Model Context Protocol) servers and Agent Skills, conversation persistence, todo management, an event-driven hooks system for cost tracking, multi-agent teams (a profile's agents, each with optional subagents), a guided first-run setup in the web UI, and `peakbot install` / `peakbot service …` verbs for end-user install + start-at-login.

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

Key types: `DynAgent` (enum for runtime provider switching), `CostTracker` (session statistics), `ProviderInfo` (name, model, pricing/vision capability). Agent construction happens in `create_provider()` — read the signature in the source rather than trusting any doc copy; it threads the resolved model, the prompt, the per-profile `ToolEnv` (tools, filters, MCP, env, vector store) and state through to the per-provider `create_*_agent` constructors, which share `add_builtin_tools`.

### Context Manager (`src/context_manager.rs`)

Compacts long conversations automatically: triggers at 80% context usage (configurable), always keeps the last N messages (default 5), auto-detects the context window from the model name (Claude 200k, GPT-4o 128k, Gemini 1M+; overridable per model).

### Hooks System (`src/hooks/`)

- `session_hook.rs` — `SessionHook` (implements `PromptHook`, emits events), `SessionStats`, `ModelPricing` (fetched from OpenRouter API or defaults)
- `channel.rs` — async event channel + processor
- `events.rs` — `AgentEvent` (CompletionResponse / ToolCall / ToolResult), `TokenUsage`

Flow: `SessionHook` emits an `AgentEvent` per LLM call → async channel → `AgentRunner` → `CostTracker` updates session stats (visible via `/stats`).

### Background Processes (`src/bg_processes.rs`)

Long-running PTY-backed processes spawned by the background-process tool (`bash_bg` on Bash hosts, `powershell_bg` on PowerShell hosts — the wire name is per-shell, `ShellKind::bg_tool_name`). Each process gets a numeric id, a ring buffer of captured output, and a per-process `cooldown_secs` (default 60) that paces output injection.

- `start` spawns a PTY via `portable-pty` under the detected shell; a reader thread streams ANSI-stripped lines into the ring buffer and pings the agent loop.
- Between turns, `StateManager::drain_bg_output_into_synthetic_turn` builds a `[bg output]` block per eligible buffer and runs an agent turn over it, persisted with `MessageSource::Background { proc_ids }`.
- Cooldown coalesces chatty output into one flush per window; `cooldown_secs: 0` = real-time (for external-input bridges). **Process exits always bypass the cooldown.** A real user message resets all cooldowns so buffered output flushes alongside it.
- `/new`, `/model`, `/load` kill all bg processes. The registry is in-memory only — nothing survives a restart.

### Skills System (`src/skills/`)

Skills are modular capability packages: a directory containing a `SKILL.md` (plus optional `scripts/`, `references/`, `assets/`). Discovery (`discovery.rs`) loads from `~/.agents/skills` (global) and `./.agents/skills` (local, relative to session cwd); skills are listed in the system prompt and read on demand.

## Agent Initialization Flow (`src/main.rs` / `src/session.rs`)

1. Load configuration (`config.yaml` + env vars)
2. Load skills; build the system prompt (persona + core guidance + memory section + skills + env + agents.md + instructions — see the [prompt recipes](#prompt-recipes))
3. Load MCP servers (async), extract tools
4. `create_provider()` builds the `DynAgent` + `CostTracker` + todo state + event channel
5. `AgentRunner` runs the loop

State: `chat_history` (Vec<Message>) in `AgentRunner`; shared `Arc<Mutex<TodoList>>` for todos; `SessionStats` behind `Arc<Mutex<>>`. The agent is stateless between tool turns — Rig dispatches tool calls and loops until text or the turn limit.

## Tools

All tools live in `src/tools/` and implement `rig::tool::Tool` (`NAME`, `Args`, `Output`, `Error`, `definition()`, `call()`). Tool errors are returned to the model as tool results — the model self-corrects; they never crash the agent.

**The authoritative tool list is `BUILTIN_TOOL_NAMES` in `src/config/mod.rs`** — keep any table below in sync with it, never the other way round.

Exactly **one shell + bg tool pair** is registered per detected shell: Bash hosts get `bash` + `bash_bg`, PowerShell hosts get `powershell` + `powershell_bg`, never both. `tools:` filter names are **per-shell** — `disabled: [bash_bg]` does NOT disable `powershell_bg` (they are separate names in `BUILTIN_TOOL_NAMES`).

| Tool | File | Notes |
|------|------|-------|
| `bash` | `bash.rs` | Shell execution with timeout; output truncated to last ~50k chars, full output saved to /tmp/peakbot/ |
| `powershell` | `powershell.rs` | Windows counterpart of `bash` — one shell tool is registered based on the detected shell (`shell_detect.rs`), never both |
| `bash_bg` / `powershell_bg` | `shell_bg.rs` | Long-running PTY processes (start/stop/list/send_line); synthetic `[bg output]` turns, per-process cooldown. Wire name is per-shell (`ShellKind::bg_tool_name`) |
| `file_create` / `file_str_replace` / `file_insert` | `file_edit/` | Create / replace (whitespace-flexible fallback) / insert-at-line |
| `file_read` | `file_read.rs` | Read with line ranges |
| `pdf_read` | `pdf_read.rs` | PDF → text/markdown (`pdf_oxide`), page ranges, 50k-char cap |
| `list_directory` | `list_directory.rs` | Listing with recursion (max depth 3) |
| `fetch_url` | `fetch_url.rs` | HTTP GET (POST when `fetch_url.allow_post`), raw body |
| `fetch_page` | `fetch_page.rs` | Fetch a web page → clean Markdown, with retry/backoff |
| `web_search` | `search.rs` | SearXNG-based search (needs `general.search:` config) |
| `think` | `think.rs` | Reasoning scratchpad |
| `todo` | `todo.rs` | Todo list management |
| `doc_index` / `doc_search` | `doc_index.rs` / `doc_search.rs` | Semantic vector store — registered only when the profile's `vector_db:` is set |
| `view_image` | `view_image.rs` | Load a local image into vision context — registered only on the Anthropic provider (the lone rig provider whose tool results deliver images) |
| `delegate` | `pipeline/delegate_tool.rs` | Run one sub-agent to completion — registered only when the agent declares `subagents:` |

Plus optional **MCP tools** from configured servers (wrapped in `LoggingToolDyn` for tracing).

### Time budgets

Every tool — built-in and MCP alike — is wrapped at registration in `TimeBudget` (`src/tools/time_budget.rs`); there is no unbudgeted state. `timeouts.tool_secs` (default 30 min) is the ceiling; tools that own a longer deadline get a derived one *above* it so their own message wins: `bash`/`powershell` get `max(tool_secs, 7500s)` (above their own 7200s clamp), `delegate` gets `timeouts.delegate_secs + 300s` (above the loop's own budget, leaving room for the INTERRUPTED handoff).

Expiry returns `⏱ TIMEOUT: …` as a normal tool result — the model self-corrects; the turn is not killed. A tool that *panics* mid-call is caught at the same boundary and returned as `💥 PANIC: …` — also a normal tool result, so the turn is not killed. Design: `docs/tool-time-budget-design.md`.

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
- **Tool panics** → caught at the tool boundary (`TimeBudget`) and returned to the model as `💥 PANIC: …` — orchestrator, sub-agent, or `delegate` itself; the innermost wrapper wins. The model self-corrects and the turn continues. The `agent_loop` `catch_unwind` is only a backstop for panics *outside* any tool (those abort the turn like Stop).
- **API errors** (auth, network) → surface as `PromptError`, printed; loop continues. Transient errors are retried with backoff (`is_transient_prompt_error`).
- **Max turns exceeded** → printed as an error; user retries or rephrases.

## Configuration

`config.yaml` in the platform config dir has **exactly four top-level blocks** — `providers` (models), `mcps` (MCP servers), `profiles` (named working setups, each an ordered list of agents, each with optional subagents), `general` (everything else). Unknown keys anywhere are a load error. A per-repo `.peakbot/config.yaml` may contain **only** `profiles:` — repo profiles are listed first for conversations started in that repo, and a repo profile with the same name as a global one **replaces** it entirely (no merge).

Old 0.23-layout files are **auto-migrated**, with no command and no button: the global file once at boot (every mode), a repo file when a session first sees it (boot for the boot cwd, on `/cd`/`/new`/`/model`/`/load` for the target cwd). The original bytes are kept create-only as `config.yaml.pre-0.24` (mode `0600`, never overwritten), and every semantic change is reported as a note (YAML header comment in the new file, stderr, and system messages in the first session). The old `web-token` file is retired — its token becomes `general.web.token`. Migration is the only legacy-layout hard error: a file that fails to migrate stops boot (windowed web shows the reason in first-run instead).

Defaults are positional: the **first** profile is the default, and inside a profile the **first** agent is the default. Env vars: `PEAKBOT_WEB_TOKEN` (web token), `RUST_LOG` (log verbosity).

`RUST_LOG` controls `tracing`-style log verbosity: unset → `peakbot` warnings and other crates' errors (`--tui`: errors only, since its logs share the stdout ratatui draws on — set `RUST_LOG` explicitly to override). Transient-retry warnings (main agent and sub-agent) are therefore recorded by default in web/stdio mode. For a service install consider `RUST_LOG=peakbot=info` (systemd: `systemctl --user edit peakbot` → `[Service]` / `Environment=RUST_LOG=peakbot=info`).

### Live reload on session verbs (`/new`, `/model`, `/cd`, `/load`)

Editing `config.yaml` (global or the repo file) or skills does not require a restart: each session verb re-reads both config files (auto-migrating a legacy repo file first), re-scans skills, and re-resolves + rebuilds the agent on the session's bound `(profile, agent)` — except `/cd`, which re-binds for the target tree (see [Selection](#selection)) — per-session, never mutating the process-wide state. A parse error warns and keeps the previous config. A **legacy-layout** file is never migrated on reload (migration is boot-only) — the warning tells you to restart PeakBot to migrate it. `/new` rebuilds on the **currently-active** model (it does not bounce you to the agent's configured model). In-flight conversations keep their selection; a selection change (`/profile`, `/agent`) applies only to a conversation that has not started (locked after the first turn), and a repo's first profile is the default for conversations started in it — at session start and on `/cd`; `/new`, `/model`, `/load` never switch your selection.

| Reload-safe (next session verb) | Boot-only (`⚠ <key> change ignored — restart to apply.`) |
|---------------------------------|-----------------------------------------------------------|
| `providers:` (new aliases resolve) | `mcps` (live subprocesses) |
| `profiles.*` — tools, mcps, fetch_url, env, memory, vector_db, ui.tabs, agents | `general.web` (bind / token / TLS, read once by the server) |
| `general.search`, `general.context`, `general.agent_max_turns` | `general.http` (published once into the client factory) |
| `general.cost_tracking`, `general.retry`, `general.timeouts` | `general.uploads` (max_file_mb / max_files, read once) |
| skills + system prompt | `general.conversation` (auto_save / storage_dir, built at boot) |

`vector_db` is reload-safe: a process-wide handle registry keyed by the resolved `db_path` shares one DB across profiles and reloads (the same `db_path` with different embeddings is a parse error).

There is deliberately no filesystem watcher — reload is explicit, to avoid heisenbugs mid-turn.

### Multi-model with `/model`

Declare a list of providers, each with models. There is no `default_model` — every agent names its own starting model:

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
```

Rules:
- `alias` optional; without it a model is addressable only as `<provider_name>/<model_name>`. Aliases are globally unique, match `^[A-Za-z0-9_./:-]+$`; the literal `unknown` is reserved.
- Per-model overrides: `max_tokens`, `temperature`, `context_size` (token window; the legacy field names `context_window_override` / `context_window` are read as aliases), `extra_params` (LlamaCpp), `prompt_caching` (Anthropic: `auto`/`auto_1h`/`manual`/`off`), `vision` (force image support on/off; omit for auto-detection), `preserve_reasoning` / `display_reasoning` (Anthropic thinking blocks).

`/model` semantics: no arg lists aliases (active marked `→`); `/model <alias>` starts a **new conversation** on that model (confirm overlay if the chat has content; the web UI's typed `/model <alias>` needs no confirm and just starts a new conversation); unknown alias / current alias are safe no-ops with a message. The active alias is bound to the conversation metadata — `/load` re-activates it and rejects the load if the alias no longer resolves. An agent's `model:` is its **starting** model only — there is no model lock, so `/model` works in every conversation, including one running an agent with subagents; subagents that inherit their agent's model follow the switched model. MCP servers persist across switches.

### The `general:` block

Everything that is not a model, an MCP server, or a profile lives under `general:` (all keys optional, all defaulted):

```yaml
general:
  web:                             # boot-only — the ONLY web-server config
    bind: 127.0.0.1:7823           # CLI --bind > this > default
    # token: "…"                   # CLI --token > $PEAKBOT_WEB_TOKEN > this
    tls: false                     # CLI --tls OR this
    session_ttl_secs: 600
    reaper_tick_secs: 60
  search:                          # SearXNG — presence = web_search on, absent = off
    base_url: https://searx.example.com
    # timeout_seconds: 30          # also: max_results: 10, bearer_token: "…"
  context:                         # conversation compaction
    enabled: true
    threshold: 0.8                 # compact at 80% usage (0 < x ≤ 1)
    keep_recent: 5
    # compaction_model: qwen       # alias; absent = the conversation's current model
  conversation:                    # boot-only
    auto_save: true
    # storage_dir: ~/.local/share/peakbot/conversations
  agent_max_turns: 50
  cost_tracking: true              # effective iff the active model's provider is OpenRouter
  retry: { max_retries: 3, initial_delay_ms: 1000, max_delay_ms: 30000, backoff_factor: 2.0 }
  timeouts:                        # wall-clock ceilings on agent work; see
    tool_secs: 1800                # `http:` for per-socket network timeouts
    delegate_secs: 7200            # 1..=86400 each
  http: { connect_timeout_secs: 30, read_timeout_secs: 1800 }    # boot-only
  uploads: { max_file_mb: 50, max_files: 10 }                    # boot-only (vision limits)
```

The per-profile capability blocks — `tools:`, `mcps:`, `fetch_url:`, `env:`, `memory:`, `vector_db:`, `ui.tabs:` — live on each profile, not at the top level; see [Agents and subagents](#agents-and-subagents).

`read_timeout` bounds *silence*, not duration — but completions are
non-streaming (`agent.prompt`), so nothing arrives until the model is done and
for LLM calls it acts as a ceiling on a single generation. Raise it if you run
models that legitimately think for longer than 30 minutes. Without it, an
upstream that accepts a request and never answers wedges the turn until the
process dies — Stop still works (the per-turn cancellation token drops the
in-flight request) but the user waits for the wrapper's read to die before the
turn unwinds. Tools that set their own shorter total `.timeout()` (`fetch_url`,
`fetch_page`, `web_search`) are unaffected: whichever fires first wins.

### MCP servers

```yaml
mcps:                              # boot-only (live subprocesses)
  - name: filesystem                       # stdio subprocess (type omitted = stdio)
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

Servers start at boot and are process-wide; which servers' tools reach a given agent is gated per profile by the profile's `mcps:` name filter (subagents included — see [Agents and subagents](#agents-and-subagents)).

OAuth notes: first connect opens the browser; tokens cached under `~/.cache/peakbot/mcp-auth/<name>.json` (0600), refresh is silent. Under `$SSH_CONNECTION` the auth URL is printed instead — forward the callback port (`ssh -L 9999:127.0.0.1:9999 …`). The deprecated top-level `auth_token:` still works but warns; setting both forms is a hard error.

### REPL Commands

| Command | Description |
|---------|-------------|
| `/stats` | Session statistics (tokens, cost, per-lane breakdown) |
| `/context` | Context usage |
| `/compact` | Force context compaction |
| `/bg` | List background processes |
| `/pause` | Pause the running sub-agent at its next step (Ctrl+P in TUI) |
| `/resume` | Resume a paused sub-agent (Ctrl+P in TUI) |
| `/model [alias]` | List / switch models (starts a new conversation) |
| `/profile [name]` | List / select a profile (first turn only; web: selection chip — top bar, or bottom bar on phones) |
| `/agent [name]` | List / select an agent within the active profile (first turn only) |
| `/cd [path]` | Show / change session cwd (re-binds to the target's default agent) |
| `/new`, `/load`, `/conversations` | Conversation management |
| `exit` | Quit |

`/pipeline` and `/subagents` are gone (no alias) — selection is `/profile` + `/agent`. Both take the **rest of the line** as the name (spaces work), are case-sensitive, and lock once the conversation has started.

### Web UI (`peakbot`, the default)

Serves the embedded SPA + live chat over WebSocket. Use `peakbot --tui` for the
terminal UI.

**Settings** (⚙ in the top bar, or **Ctrl+,** / **⌘+,**): edits the same
`config.yaml` through the web UI. Nav panes: Providers & Models · MCP servers ·
Profiles (list → profile page → agent page) · General · System · Raw YAML, with
a Global / This-repo scope switch (in repo scope only Profiles is editable;
the other panes show the global values read-only). Saves are validated
server-side by the same `Config::parse` as boot; the save bar says which keys
apply "on next /new" vs "restart required" (the server computes the boot-only
list — the client keeps none). The gear is hidden when the active profile's
`ui.tabs` filters out the `settings` tab, and the whole settings API is refused
under a `--profile` pin. In the desktop app the machine group gains a
"Desktop" pane (`settings/desktopPane.tsx`) — the desktop's own option (keep
running in background), read and toggled through three scoped Tauri commands
(`settings_get`, `toggle_background`, `theme_set`; only on the desktop's
loopback `:7823` backend, otherwise the pane offers a button to the bundled
settings page). The web theme is reported to the desktop so its bundled pages
follow it. The web-server settings live in Settings → General.

**First run**: with no global config (or a failed migration) the chat is
replaced by a guided setup — Welcome → Provider → Agent → Web → Review →
Finish — that writes the config; `/setup` is the same screen for old bookmarks.
`/ws` keeps answering 503 until the server restarts with the new config. The
old `/api/setup*` routes are 404 tombstones; install/service actions moved to
`/api/system/install` and `/api/system/service` (token-gated like everything
else).

| Flag | Default | Description |
|------|---------|-------------|
| `--tui` | off | Terminal UI instead of the default web UI |
| `--bind <host:port>` | `127.0.0.1:7823` | Overrides `general.web.bind`; non-loopback bind **requires** a token |
| `--token <secret>` | — | Overrides `PEAKBOT_WEB_TOKEN` > `general.web.token`; guards every route |
| `--tls` / `--tls-name <NAME>` | off | HTTPS with the built-in CA (OR `general.web.tls`); `--tls-name` adds SANs (repeatable) |
| `--profile <name>` | — | Pins a global profile: every conversation runs under it, `/profile` is refused, repo files are ignored, settings API disabled |

**Sticky sessions** (#118): a conversation is addressed as `?convo=<uuid>`; a live session (StateManager + controller loop) stays bound to it independently of any socket. Reconnects and multiple tabs re-attach to the same running session. A session is reaped only when **fully idle** — no sockets, agent not mid-turn, no live `bash_bg` children — for `general.web.session_ttl_secs` (default 600; `reaper_tick_secs` 60). See `src/ui/web/registry.rs`. Killing a live session (✕ on a conversation row) is refused while a turn runs — Stop first; running turns, including delegations, continue with no browser attached.

**Auth**: the token is presented once as `?token=…`; the server sets a `peakbot_token` cookie which authenticates everything after, including the `/ws` upgrade. Single-operator shared secret, not a user system. Token precedence: `--token` > `PEAKBOT_WEB_TOKEN` > `general.web.token`.

**TLS** (`--tls` or `general.web.tls: true`): PeakBot self-signs a long-lived CA in `dirs::cache_dir()/peakbot/tls/` (key 0600, never silently regenerated — delete the dir to rotate) and mints a fresh leaf each boot with SANs for loopback, the LAN IP, any bind IP, and `<hostname>.local`. The CA public cert is served at the tokenless route `GET /peakbot-ca.crt` for one-time install on phones (iOS additionally needs Settings → General → About → Certificate Trust Settings). TLS complements the token, never replaces it. Implementation: `src/ui/web/tls.rs` (`rcgen` + `rustls`/aws-lc-rs, no OpenSSL).

**Testing the web UI**: `make dev` runs backend (`:8080`) + Vite HMR (`:5173`); drive it with the `playwright-cli` skill (accessibility snapshots, console logs, WS traces). For pure server-side WS probing, `websocat ws://localhost:7823/ws`.

## Extending

### Adding a new built-in tool

1. Create `src/tools/my_tool.rs`: `Args` (Deserialize), error type (thiserror), tool struct implementing `rig::tool::Tool` (`NAME`, `definition()` with JSON Schema, `call()`)
2. Export from `src/tools/mod.rs`
3. Register in `add_builtin_tools()` in `src/providers/mod.rs`
4. Add the wire name to `BUILTIN_TOOL_NAMES` in `src/config/mod.rs` (the `tools:` filter validates against it)

### Adding Agent Skills / MCP tools

Prefer skills or MCP servers over baking tools into the binary. Skills: drop a directory with a `SKILL.md` into `~/.agents/skills/` or `./.agents/skills/` — discovered at startup and on session verbs. MCP: add to `mcps:` in config (see above); tools load at boot.

## File Map (directory level)

```
src/
├── main.rs / lib.rs        # entry, AgentRunner, system prompt building
├── session.rs              # SessionDeps + create_session factory
├── config/                 # Config::parse (the one boundary), migration, BUILTIN_TOOL_NAMES
├── providers/              # DynAgent, create_provider, per-provider constructors, add_builtin_tools
├── hooks/                  # SessionHook, event channel, AgentEvent
├── pipeline/               # delegate tool + delegation handoff (multi-agent)
├── state/                  # StateManager (single source of truth for sessions)
├── tools/                  # all built-in tools + ThoughtGate + LoggingToolDyn
├── skills/                 # skill discovery + SKILL.md parsing
├── vector/                 # VectorStore, embeddings client, parse/chunk
├── ui/                     # app_state, wire types; repl/ (ratatui), web/ (axum+WS+TLS), stdio/
├── bg_processes.rs         # BgRegistry (background-process PTY processes)
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

### Worktree Workflow (Mandatory)

Every new task gets its own git worktree — never implement a task directly in the main checkout.

1. **Create** — branch off up-to-date master in a fresh worktree: `git worktree add ../peakbot-<slug> -b <type>/<slug>`.
2. **Implement** — do the work inside that worktree (TDD, Zen review, etc. per this doc), then push and open a PR.
3. **Watch CI** — poll the run to green or failed (see "CI (Gitea Actions)" below) before considering the task done. Fix and re-push on failure; keep watching.
4. **Merge**:
   - Explicitly instructed to merge automatically → merge once CI is green.
   - Otherwise → do not merge. Start a `bash_bg` watcher polling the PR state until a human merges (or closes) it.
5. **Cleanup** — only after the branch is actually merged (auto, or noticed by the watcher): remove the worktree (`git worktree remove`) and delete the now-stale branch, local and remote. Never remove a worktree or branch that isn't merged — verify first (see 2026-10-01 worktree cleanup in `memory.md` for the audit pattern: clean status, no unpushed commits, PR state == merged).

### Release notes workflow (mandatory)

`current.md` is the working draft for the *next* release; the tag/release body comes from `release-notes/<version>.md`, never `current.md`. Promotion is not hand-rolled — `make release-bump` does it:

```bash
# "do a release" (human or agent): branch + bump + notes promotion + push
make release-bump VERSION=0.3.0
#   cuts release/0.3.0 from origin/master; bumps Cargo.toml + Cargo.lock and
#   desktop/Cargo.toml + desktop/Cargo.lock; promotes current.md ->
#   release-notes/0.3.0.md (working-draft header stripped, content from the
#   first `## ` heading); writes a fresh current.md; commits + pushes the branch
# open a PR; wait for ci.yml green; merge
# on up-to-date master (HEAD == origin/master, notes file committed):
make release-tag
#   annotated tag 0.3.0, message = release-notes/0.3.0.md, pushed — the tag
#   push triggers .gitea/workflows/release.yml
```

The notes file is **required** — `release-check` fails the run when `release-notes/<v>.md` is missing. There is no `Release <v>` fallback.

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
| `Dockerfile.windows` | `rust:1.91-bookworm` + clang/lld/llvm + cargo-xwin (MSVC SDK, static CRT) | cross to `x86_64-pc-windows-msvc` — the ONE Windows build: server asset AND the desktop sidecar are the same build (cache hit in the release pipeline) |
| `Dockerfile.macos` | `cargo-zigbuild` | zig + macOS SDK; universal2 fat binary |
| `Dockerfile.android` | `rust:1.91-bookworm` + NDK r27c | `cargo-ndk`; API level via `CARGO_NDK_PLATFORM` (24), **not** `-p` (collides with cargo `--package`); works clean because PeakBot is pure rustls/ring |

All four end `FROM scratch` with `--output type=local,dest=./output`. **Gotcha:** `ARG` does not cross `FROM` stage boundaries — each Dockerfile redeclares `ARG TARGET` in its scratch stage. `CONTAINER_BUILDER` auto-detects podman (preferred), falls back to docker.

### Make targets

`make help` for the full list. Highlights: `make` / `make build` (all four platforms), `make build-{linux,windows,macos,android}`, `make clean`, `make web` (SPA bundle for the default web mode), `make dev` (backend under cargo-watch `:8080` + Vite HMR `:5173`; requires cargo-watch + Node 22+). Release: `make release-bump VERSION=X.Y.Z` (cut branch, bump versions, promote notes, push), `make release-tag` (tag from up-to-date master, push → CI publishes), `make release TAG=X.Y.Z` (local re-run of the CI job). Non-release builds produce unversioned filenames; the release flow injects semver via `--build-arg VERSION`.

### Release pipeline

Fully CI-driven: `release-bump` branch → PR green → merged → `make release-tag` on up-to-date master (HEAD == origin/master) pushes the annotated tag → the push triggers `.gitea/workflows/release.yml` (runner `dind`, Alpine host driving the host docker daemon — no job container, 180-min timeout, secret `RELEASE_TOKEN` → env `GITEA_TOKEN`) which runs:

```bash
make release TAG=0.3.0 PRUNE=1 CONTAINER_BUILDER=docker GITEA_URL=… OWNER=ai-bots REPO=peakbot
```

`make release` = `release-check` (TAG == Cargo version == desktop version, `release-notes/<TAG>.md` exists) → per platform: `release-build-<p>` → `desktop-<p>` → prune, for linux, macos, windows → `release-build-android` → `release-publish`. Sequential by design — never run with `-j`.

`release-publish` asserts all 7 assets exist **before** any API call — server: `peakbot-<v>-{linux-amd64, windows-amd64.exe, macos-universal2, android-arm64}`; desktop: `PeakBot-Desktop-<v>-{linux-x86_64.AppImage, macos-universal.zip, windows-x86_64.exe}` — then creates a DRAFT release, uploads all 7, and publishes; any failure deletes the draft, so a release is complete or absent, never partial.

Release body + tag message = `release-notes/<v>.md` (read via `jq --rawfile` — any Markdown is safe). No `NOTES=` override, no fallback. Local re-run of the CI job: `make release TAG=X.Y.Z GITEA_TOKEN=…` (if the tag already went out, delete it locally + from origin first). Host needs: git, cargo, podman/docker, curl, jq, awk.

## Vision (image input)

Attachments are files stored per conversation under
`<data_local_dir>/peakbot/uploads/<convo>/<id>/<name>` and persisted as
references on the user message, so they survive `/load` and any session
reload. Two kinds: **Image** — `png`/`jpg`/`gif`/`webp`, ≤ 10 MB each —
and **File** — anything else.

How the model sees them:
- **Vision models** get the image bytes inline on the message parts.
- **Non-vision models** get the image as a path reference (a file, not
  image data), so they never refuse it.
- Every attachment is listed, by absolute path, in an
  `[Attached files — read them with your file tools]` note on the turn
  prompt.

Inputs: web uploads go through `POST /api/uploads` and are served back by
the token-gated `GET /api/uploads/{convo}/{id}`. In the TUI, `[img:/path]`
and `[img:https://…]` copy the file/URL into the store. `[img:data:…]` is
no longer accepted.

Gating (see `src/vision.rs`): `supports_vision_for(provider, model)` is a
conservative name-substring detector (gpt-4o/claude-3+/gemini/pixtral/
llava/qwen-vl…; unknown names default to **false**). The `anthropic`
provider gates on transport, not model name — its Messages transport
carries images natively, so any model name works there. Per-model
`vision: true|false` overrides auto-detection via
`providers::resolve_supports_vision` and also gates `view_image`
(Anthropic transport only). Limits (`general.uploads.{max_file_mb, max_files}`,
boot-only): ≤ 10 MB per file by default, plus a per-message cap on
attachments + `[img:]` tokens. Failures surface as system messages,
never silently.

## Agents and subagents

A conversation runs exactly one resolved `(profile, agent)` pair, and everything about it is derived from that pair: the tools, MCP servers, env, memory and knowledge (from the profile), and the prompt recipe, starting model and subagent roster (from the agent). Profiles live under `profiles:` — an ordered map, **first = default** — and each profile declares an ordered `agents:` map (again **first = default**). An agent is what you talk to; it may declare `subagents:`. Runnable examples: `examples/agent-team/`, `examples/agent-dev-team/`.

### Config shape

```yaml
profiles:
  dev:
    tools: { disabled: [powershell, powershell_bg] }   # NameFilter over built-in tools (no `delegate`)
    mcps: { only: [playwright] }                       # NameFilter over the top-level `mcps:` names
    fetch_url: { allow_post: false }
    env: { GIT_PAGER: cat }                            # base bash env for the agent AND its subagents
    memory: { enabled: true, threshold_bytes: 51200 }
    # vector_db: absent → no doc_index/doc_search for this profile
    ui: { tabs: { disabled: [bash] } }                 # NameFilter over TAB_NAMES
    agents:
      team:
        model: sonnet                        # REQUIRED alias — the STARTING model (/model may switch it)
        # persona: | …                     # optional; absent = the built-in persona
        prompt: Research first, then design, test, build.   # optional → "# Instructions"
        # core_guidance: true              # default; false = persona is the WHOLE static head
        # skills: { only: [github] }       # NameFilter over discovered skills
        # agents_md: true                  # default true for agents
        # env: { FOO: bar }                # overlays profile.env for THIS agent only
        subagents:                           # optional; non-empty ⇒ `delegate` is registered
          researcher:
            # model: qwen                   # optional; absent = the agent's configured model
            prompt: Gather background, cite sources, hand back a brief.   # REQUIRED, non-blank
            # skills: { only: [github] }
            # agents_md: false              # default false for subagents
            # env: { … }                    # per-subagent, overlaid on profile.env
      solo:
        model: sonnet                        # no subagents → single agent, no delegate tool
```

**Validation** (all in `Config::parse`; one error shape, keyed by field path, for CLI, reload and the Settings API):
- **Names** (profiles, agents, subagents): `^[A-Za-z0-9_ .-]{1,64}$`, no leading/trailing space; duplicates and the reserved subagent name `orchestrator` are errors.
- **Agent `model`** is required and must resolve; unknown aliases are named together with the declared ones.
- **`core_guidance: false` needs a `persona`** — with core guidance off, the persona is the whole prompt head.
- **Subagent `prompt`** is required, non-blank; a subagent `persona` is an error (the prompt is its whole persona).
- **NameFilters** (`tools:`, `mcps:`, `skills:`, `ui.tabs:`): `only:` XOR `disabled:`; unknown tool/MCP/tab names are errors, unknown **skill** names a warning (the skill set depends on the cwd).
- **`delegate` is not filterable** — naming it in `tools:` gets a dedicated error.
- **`vector_db`**: presence = on (there is no `enabled:` key); the same resolved `db_path` with different embeddings in two profiles is an error.

### Selection

Selection is per-conversation: `(profile, agent)`, persisted on the conversation, and **locked after the first turn** (flipping `delegate` or the profile mid-conversation desyncs the tool list from wire history).

- **`/profile <name>`** / **`/agent <name>`** — pick before the first turn; the name is the **rest of the line** (spaces work), case-sensitive.
- **Web UI** — the selection chip in the top bar (bottom bar on phones) opens a Profile › Agent › Team › Model drill-down (repo profiles first) with per-member model override before the first message; it locks after the first message. The Agents tab shows the configured roster.
- **`/new`** keeps the current selection; a fresh session starts at the first profile's first agent.
- **`/cd <dir>`** starts a fresh conversation bound for the target tree. If the target has repo profiles you land on its first profile's first agent, unless you're already on one of them (kept, re-read). Without repo profiles your profile is kept, or you get the first profile if it isn't configured there. The model follows the new agent, subagent model overrides reset when the agent changes, and an `ℹ Switched to …` line says so.
- **`--profile <name>`** pins a global profile: every conversation runs under it, `/profile` is refused, and repo files are ignored.
- **Repo profiles** are listed first for that cwd; a repo profile with the same name as a global one **replaces** it entirely (no merge).

### The model

An agent's `model:` is its **starting** model — there is no lock. `/model <alias>` works in every conversation (including one running an agent with subagents) and starts a new conversation on that model. Before the first message the selection chip can also override the orchestrator's and each subagent's model; overrides travel with the conversation (/new keeps them, /load restores them, picking another agent clears them).

### The delegate tool

- The agent you talk to gets a **`delegate(role, task, parent_task_id)`** tool iff it declares non-empty `subagents:` (derived — there is no flag to set).
- It runs one sub-agent — *(model alias, prompt)* + optional `env:`/`skills:`/`agents_md:` — to completion on a **fresh context** and returns one string. Sequential by construction; no parallel mode. A sub-agent has no memory of prior delegations — everything it needs goes in `task`.
- On completion, the sub-agent's last ≤10 earlier assistant messages (excluding its final reply) are saved to `<temp>/peakbot/delegate_{role}_{pid}_{counter}.txt`; a one-line pointer is appended to the delegate result string so the agent can `file_read` it. Write failures degrade silently; hookless Ollama sub-agents get no file (empty snapshot).

### Subagent tools — and the MCP caveat

Sub-agents get the same tools as the agent they belong to, **minus `delegate`** (no nested delegation) — including the profile's **MCP tools**, through the shared server handles. The caveat: MCP servers are process-wide, so a **stateful** server sees the main agent's calls and every delegation on one connection; delegation is sequential, so the calls never interleave, but a server with per-connection session state does not get per-subagent isolation. Fresh todo list; isolated bash env (`env:` never leaks across roles). No sandbox — a sub-agent can write and run bash.

Stop during a delegation aborts the **whole turn** — sub-agent and agent unwind together. With #183 the abort is **mid-tool**, not just at the next LLM boundary: dropping the turn future unwinds the in-flight delegation and its sub-tool (`bash`/etc.) along with it, so the sub-agent's PTY child dies via `PtyHandle::drop` at the same instant. Stop or session teardown during a delegation persists an `INTERRUPTED` result for the in-flight `delegate` call, quoting the sub-agent's last message. Any tool call with no recorded result (crash, old transcripts) reaches the model as `INTERRUPTED` at the wire boundary (`sanitize_tool_pairs`) and is never silently dropped.

Pause (`/pause`, `Ctrl+P` in the TUI) is a **cooperative** gate, sub-agents only: the sub-agent finishes the step it is on, then parks before its next LLM call or tool dispatch until `/resume` (or `Ctrl+P` again). Paused time never counts against `general.timeouts.delegate_secs`, and hookless Ollama sub-agents are not pausable. Stop is unchanged: it aborts everything **including a paused sub-agent** — the parked wait is part of the turn future, so it drops with it.

Failures are handled like the agent's own: transient wire errors are retried in place (`general.retry.*`, shared `providers::retry`) and **resume from the last completed step** — the retry replays the failed request from the sub-agent hook's history snapshot, so finished tool calls never re-run (hookless Ollama sub-agents have no snapshot and restart from the task). Only consecutive failures *without progress* count toward `retry.max_retries`; a completed step resets the budget, and the status line shows `<role>: retrying (n/max) in Xs — resuming from step K` while backing off. Unknown tool names and tool errors go back to the sub-agent as tool results it can self-correct from. What survives that ends the delegation and comes back as a summarised `INTERRUPTED` result — see `src/pipeline/handoff.rs`. The whole delegation is bounded by `general.timeouts.delegate_secs` (default 2 h); on expiry the transcript takes that same path — summarised and returned as `INTERRUPTED`, not discarded.

### The isolation invariant

Three views that a single-agent chat conflates are pulled apart:

1. **Display transcript** — the agent's turns *and* every sub-agent's turns, tagged by `MessageSource::SubAgent { role }` (rendered `🧩 <role>`; web mirrors via the `sub_agent` wire source).
2. **Agent wire context** — its own turns + each delegate ToolCall/result string. **Never** sub-agent internals.
3. **Sub-agent wire context** — role prompt + one task + its own tool round-trips; discarded after the call.

The load-bearing guarantee: sub-agent turns are filtered out of the agent's wire history by a single lane filter in `get_agent_history` (`msg.source.is_orchestrator_lane()`). Regression test: `get_agent_history_excludes_sub_agent_lane_keeps_background`.

Delegation tokens/cost roll into the parent `/stats` lane-agnostically (exception: Ollama sub-agents are hookless — untracked).

Orchestrator reply: rig returns only the last response's text, so prose from responses that also carried tool calls is staged by the hook (`StateManager::stage_prose`) and committed with the final text as one row (`commit_reply`) at the end of the turn (also on Stop/Error). An empty final reply is re-requested once with a wire-only nudge, then a `⚠` system row is shown.

### Context meter

The Session tab context meter is sourced from the **main agent's lane** only (sub-agent turns do not move it). This is the same signal the compaction gate reads. Labelled "Orchestrator context" with a tooltip explaining the distinction. With no subagents, behaviour is identical to a single agent.

### Prompt recipes

`build_agent_prompt` (`src/lib.rs`) composes the main agent's prompt in order: persona, core tool guidance, memory.md workflow (if `profile.memory.enabled`), skills (filtered by the agent's `skills:`), env block, `agents.md` (if the agent's `agents_md:`, default true), and the agent's `prompt:` rendered as `# Instructions` (single agents too). A missing `persona:` means the **built-in persona — for every agent, including agents with subagents**; `core_guidance: false` makes the persona the whole static head (no core guidance follows).

| Piece | Single agent | Agent with subagents | Sub-agent |
|-------|:------------:|:--------------------:|:---------:|
| persona (built-in when absent) | ✅ | ✅ | ❌ (its `prompt:` is the whole persona) |
| core tool guidance (`system_prompt_core.txt`) | ✅ | ✅ | ❌ |
| memory.md workflow (if enabled) | ✅ | ✅ | ❌ |
| skills | ✅ filtered | ✅ filtered | ⚙️ per-role filtered |
| env block (cwd/time/OS/shell) | ✅ | ✅ | ✅ |
| `agents.md` | ✅ | ✅ | ⚙️ per-role `agents_md:` (default false) |
| `prompt:` as `# Instructions` | ✅ if set | ✅ if set | — |

A sub-agent's preamble (`build_sub_agent_preamble` in `src/pipeline/delegate_tool.rs`, rebuilt fresh per delegation) is `role.prompt` + live env block + filtered skills + the caller's background-process snapshot (+ agents.md if opted in) — deliberately lean: no persona, core guidance, or memory. The framing is recomputed at the single agent-rebuild seam (`rebuild_agent_for_resolved`), so selection changes, `/cd`, and `/model` all produce the right prompt.

Where it lives: `src/pipeline/{delegate_tool,handoff}.rs`, `build_sub_agent` in `src/providers/mod.rs`, `ToolEnv` in `src/tools/env.rs`, `MessageSource` + lane filter in `src/ui/app_state.rs` / `src/state/state_manager.rs`.

## CI (Gitea Actions)

Two workflows on runner `shinpachi` (label `dind`):

- **`ci.yml`** — `rust:1.95` container. Job `ci` = release contract tests (`sh tests/release.sh`, hermetic — Makefile targets + `release.yml`) → Node 22 frontend gate (`web/`: `npm ci` + lint/typecheck/test/build) → `cargo fmt --all -- --check` → `cargo clippy --all-targets --locked -- -D warnings` → `cargo test --workspace --locked`. Job `desktop` = the full desktop workspace (`--manifest-path desktop/Cargo.toml --workspace`) with the same fmt/clippy/test trio — after apt-installing the Tauri webkit deps (same set as `Dockerfile.desktop-linux`) and `touch`ing a placeholder sidecar binary (tauri-build requires `bundle.externalBin` to exist) — plus the Windows portable-exe static tests (`desktop/scripts/test-build-desktop-windows.sh`). No node/web build there: `desktop/ui` is committed static files.
- **`release.yml`** — on tag push (`push: tags: ['*']`), on the `dind` runner (Alpine host driving the host docker daemon, no job container, `timeout-minutes: 180`, secret `RELEASE_TOKEN` → env `GITEA_TOKEN`): pure-git checkout, tool check, then `make release TAG=$GITHUB_REF_NAME PRUNE=1 CONTAINER_BUILDER=docker GITEA_URL=… OWNER=… REPO=…`.

(Reproduce `ci` locally with exactly those commands; autofix with `cargo fmt --all` + `cargo clippy --fix --allow-dirty [--tests]`.)

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
6. **Tests needing network/external binaries must be `#[ignore = "reason"]`** — the container has no `uvx`; egress works (the ci job apt-installs Node 22, so npm exists only in that job). Run ignored tests locally with `cargo test -- --ignored`.
7. **Don't copy `working-directory:` from monorepos** — peakbot's `Cargo.toml` is at the root.
