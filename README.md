# ⚔️ PeakBot

> A terminal-first AI coding assistant forged in Rust.

PeakBot is a single-agent coding companion that lives in your terminal. Built with
[Rig](https://github.com/0xPlaygrounds/rig) and a rich TUI, it reads, writes, and
executes code — all without leaving your shell.

![Rust](https://img.shields.io/badge/rust-2024-orange?logo=rust)
![Version](https://img.shields.io/badge/version-0.5.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)

---

## ✨ Features

- **🤖 Multi-Provider LLMs** — OpenRouter (100+ models), OpenAI, Ollama, LlamaCpp.
  Switch models mid-conversation with `/model`.
- **🛠️ 11 Built-in Tools** — File creation, editing, reading, shell execution,
  web search, directory listing, background processes, todo management, and more.
- **🔌 MCP Support** — Dynamically load tools from external
  [Model Context Protocol](https://modelcontextprotocol.io/) servers.
- **🖼️ Vision** — Attach images inline with `[img:path]` and ask the model about them.
- **💬 Rich TUI** — Markdown rendering, syntax-highlighted code blocks, conversation
  history, and a live todo side-panel powered by [ratatui](https://github.com/ratatui/ratatui).
- **🧠 Context Compaction** — Automatically summarizes long conversations to stay
  within context window limits.
- **💰 Cost Tracking** — Real-time token usage and cost estimation (OpenRouter/OpenAI).
- **📝 Conversation Persistence** — Auto-saved sessions with `/conversations` to list
  and `/load <id>` to resume.
- **🛰️ Background Processes** — Spawn long-running PTY-backed processes (servers,
  watchers, bridges) and receive output as synthetic chat turns.
- **🎓 Agent Skills** — Extend capabilities via modular skill packages discovered
  from `~/.agents/skills` or `./.agents/skills`.

---

## 🚀 Quick Start

### Installation

```bash
# Clone the repository
git clone https://github.com/your-org/peakbot.git
cd peakbot

# Build locally
cargo build --release

# Or use the Makefile for cross-platform builds
make build        # Linux, Windows, macOS
make build-linux  # Linux x86_64 only
```

### Configuration

Create `config.yaml` in your platform config directory
(`~/.config/peakbot/` on Linux, `~/Library/Application Support/peakbot/` on macOS):

```yaml
providers:
  - name: openrouter
    type: openrouter
    api_key: sk-or-v1-xxx
    models:
      - name: anthropic/claude-3.7-sonnet
        alias: sonnet
        max_tokens: 8192
      - name: google/gemini-2.0-flash-001
        alias: flash

  - name: local
    type: ollama
    base_url: http://localhost:11434
    models:
      - name: qwen2.5-coder:14b
        alias: local
        temperature: 0.4

default_model: sonnet
```

Or set the provider via environment variable:

```bash
export PROVIDER='{"type":"openrouter","api_key":"sk-or-v1-xxx","model":"anthropic/claude-3.7-sonnet"}'
```

### Profiles

A **profile** is a named config overlay declared in the master config's
`profiles:` map and selected at boot with `--profile <name>`. It is applied
**last** — after the per-repo `.peakbot/config.yaml` merge — and re-applied on
every session-verb reload (`/cd`, `/new`, `/model`, `/load`), so it is a
ceiling a checked-out repo cannot void. Profiles are master-config only; a
`profiles:` block in a per-repo config is ignored with a boot warning.

A profile can override four keys: `tools:`, `memory:`, `pipelines:`, and
`system_prompt:`. The first three each use the shared filter documented in
[Tool, Skill & Pipeline Filters](#tool-skill--pipeline-filters); the fourth is
its own thing, documented in [System Prompt](#system-prompt) below.

```yaml
profiles:
  locked:
    tools:
      disabled: [bash]
    pipelines:
      enabled: false
```

The `pipelines:` gate names pipelines from the **master config's** `pipelines:`
list — a gate can only name teams the master declares, and an unknown name is a
boot error listing the known ones. The gate caps the *effective* list, so a
per-repo config's own pipelines are still capped, and `enabled: false` blocks
every team regardless of where it was declared. If the gate leaves you with no
pipelines, PeakBot runs single-agent; a conversation whose selected pipeline
was gated away continues without one. Invalid filters are fatal at boot (on a
session-verb reload PeakBot warns and keeps the previous config). When a
profile is active, boot prints one line to stderr:

```
ℹ profile 'web' active — overrides: tools, memory, pipelines.
```

### System Prompt

The built-in system prompt has two parts: a short **persona** paragraph (voice
and tone) followed by **core tool guidance** on how to use the built-in tools.
Two keys — valid at the top level of a config file and inside a profile —
replace one or the other:

- `persona:` replaces only the persona; the built-in tool guidance still
  follows it.
- `system_prompt:` replaces the persona **and** the tool guidance — the
  entire static head of the prompt. Everything dynamic still follows it: the
  memory section (when enabled), skills, the environment block, `agents.md`,
  and, for an orchestrator, `# Orchestrator Instructions` from the pipeline's
  own prompt.

Reach for `system_prompt:` when your deployment's toolset makes the built-in
guidance wrong — roughly 69% of the built-in core prompt is coaching for
`bash`, `think`, and `todo`, so a research-only or web-only profile that
disables those tools shouldn't ship advice for using them. If you only want a
different voice, use `persona:` instead — it keeps the built-in guidance
current as PeakBot's tools evolve across releases.

`persona:` and `system_prompt:` fill the **same slot** and are mutually
exclusive: whichever key a source sets takes the slot and clears its sibling,
so a per-repo `persona:` overrides a master `system_prompt:` and vice versa;
a profile is applied last, so its choice is the ceiling. Setting both keys in
the same file is a config error naming the file, as is a blank or
whitespace-only `system_prompt:` — remove the key to get the built-in prompt.

A `system_prompt:` also outranks a multi-agent pipeline's own orchestrator
persona — the deployment's choice is a ceiling the team cannot override —
while the pipeline's own `orchestrator.prompt` still appends afterward as
`# Orchestrator Instructions`. Sub-agents are unaffected: a role's `prompt:`
is already its entire preamble.

```yaml
profiles:
  research:
    system_prompt: |
      You are a research assistant. You search the web and summarise findings
      with citations. You do not write or execute code.
    tools:
      only: [think, todo, web_search, fetch_page, fetch_url]
    memory:
      enabled: false
    pipelines:
      enabled: false
```

### Tool, Skill & Pipeline Filters

`tools:` in the config, a role's `skills:`, and a profile's `pipelines:` all
share one filter with one meaning:

- `only:` — allowlist. Only the named entries survive.
- `disabled:` — blocklist. The named entries are removed.
- Setting both `only:` and `disabled:` is a config error.
- An empty list, or no filter at all, means **no filtering** — everything is
  allowed. An empty `only:` never means "none"; use `enabled: false` for that.
- `enabled: false` means **nothing is allowed** — the way to say "no built-in
  tools" or "single-agent only, no pipelines".

`tools:` newly accepts `enabled:` (it previously had no way to say "no tools"
short of listing every tool in `disabled:`). Existing `tools:` and `skills:`
configs behave exactly as before.

### Run

```bash
cargo run --release
```

On the very first run with **no** config (and no provider key in the env), PeakBot
decides based on the surface it's launched on:

- **Desktop session, web UI (default):** opens the browser at `/setup` — a guided
  wizard that writes the config, installs the binary to your per-user app dir,
  and (optionally) registers a start-at-login service. You do not edit YAML by
  hand.
- **Headless / SSH, no TTY, or `--stdio`:** refuses to start and prints the
  commands to set a provider key or run the wizard from a desktop session.
- **`peakbot --tui`:** starts the terminal UI directly — no wizard. Use this
  when you already have a config and just want a session.

---

## 🖥️ Usage

PeakBot runs as an interactive terminal REPL. Type naturally — the model decides
when to use tools.

```
peakbot> create a rust function that reverses a string

peakbot> what's the error in src/main.rs line 42?

peakbot> search for "rust async stream patterns"

peakbot> run `cargo test` and tell me what failed
```

### Slash Commands

| Command | Description |
|---------|-------------|
| `/model` | List available models |
| `/model <alias>` | Switch to a different model (starts new conversation) |
| `/stats` | Show session token usage and cost |
| `/context` | Show context window usage |
| `/compact` | Force context compaction |
| `/conversations` | List saved conversations |
| `/load <id>` | Resume a saved conversation |
| `/bg` | List active background processes |
| `exit` | Quit PeakBot |

### Attaching Images

```
peakbot> what's in [img:~/screenshots/error.png]?

peakbot> compare [img:/tmp/before.png] and [img:/tmp/after.png]
```

---

## 📦 Install & Service

Beyond `cargo run`, PeakBot ships verbs that put it on `PATH` and keep it running
across logins. All three are idempotent — re-run to update.

### `peakbot install`

Copies the running binary to a stable per-user location so it survives `cargo
clean` and reboots:

- **Linux / macOS:** `~/.local/bin/peakbot` (add `~/.local/bin` to `PATH` if it
  isn't already; the command reports current `PATH` membership).
- **Windows:** `%LOCALAPPDATA%\Programs\peakbot\peakbot.exe`.

Re-run any time to overwrite with a freshly-built binary. Requires no config and
runs before `Config::load()` — use it on a fresh machine.

### `peakbot service install | uninstall | status`

Registers PeakBot to start automatically at login (a single shared-secret web
server, no interactive prompt). The exact mechanism is per-platform:

- **Linux:** a `systemd --user` unit at `~/.config/systemd/user/peakbot.service`.
  It runs in your login session. To keep it alive **after logout / at boot**,
  enable lingering once: `loginctl enable-linger $USER`.
- **macOS:** a launchd LaunchAgent at
  `~/Library/LaunchAgents/com.peakbot.agent.plist`. LaunchAgents live in the
  GUI session — there is no per-user linger; the agent stops at logout.
- **Windows:** a Task Scheduler logon task named `PeakBot`. Because PeakBot is
  a console-subsystem binary, **a console window opens at sign-in and stays
  open** — that is PeakBot running. `service status` may report `unknown` here;
  open the URL in your browser to confirm it is actually live.

`peakbot service install` accepts `--bind <addr>` and `--token <secret>` so the
service is self-contained; pass `--token` once and it is written to
`<config_dir>/web-token` (`0600`). The token file is the source for subsequent
runs — you do not need to export `PEAKBOT_WEB_TOKEN` to your shell.

Non-loopback binds **require** a token; the loopback/token invariant is
enforced at plan-build time, not as a runtime check.

---

## 🏗️ Architecture

```
┌─────────────┐     ┌─────────────────┐     ┌─────────────────┐
│   User      │────▶│   REPL / TUI    │────▶│  Agent (Rig)    │
│  (stdin)    │     │  (ratatui)      │     │                 │
└─────────────┘     └─────────────────┘     └────────┬────────┘
                                                     │
                          ┌──────────────────────────┼──────────┐
                          │                          │          │
                          ▼                          ▼          ▼
                   ┌─────────────┐           ┌─────────────┐  ┌─────────────┐
                   │ Built-in    │           │ MCP Tools   │  │  Skills     │
                   │  Tools      │           │  (external) │  │  (prompt)   │
                   └─────────────┘           └─────────────┘  └─────────────┘
```

Key components:

- **`src/providers/`** — Unified abstraction over OpenRouter, OpenAI, Ollama, LlamaCpp.
- **`src/tools/`** — 11 built-in tools: file ops, bash, search, todo, think, etc.
- **`src/hooks/`** — Event-driven cost tracking and session statistics.
- **`src/context_manager.rs`** — Automatic context compaction via summarization.
- **`src/skills/`** — Dynamic skill discovery and loading.
- **`src/ui/`** — Full TUI with markdown rendering and conversation management.

---

## 🧪 Development

### Running Tests

```bash
# All tests
cargo test

# Integration tests only
cargo test --test integration

# With output
cargo test -- --nocapture
```

### Web UI dev mode

The web UI (`peakbot`) ships as an embedded React + Vite bundle. For
iterating on it, `make dev` runs both halves with hot reload:

```bash
make dev
```

This starts the backend under `cargo watch` (on `127.0.0.1:8080`) and the Vite
dev server (on `localhost:5173`) together. **Open http://localhost:5173** — Vite
serves the app with HMR and proxies the `/ws` WebSocket to the backend.

- Editing a file under `web/src/` hot-swaps in the browser in <1s (no full reload).
- Editing Rust rebuilds and restarts the backend (~seconds), which drops the live
  WebSocket session — the browser reconnects on its own.
- Requires `cargo install cargo-watch` and Node.js 22+.

Production uses `make web` to build the static bundle that bare `cargo run --`
embeds and serves on `:7823`. Use `cargo run -- --tui` for the terminal UI.
`make dev` touches no Rust code paths.

### Pre-commit Gate

Before committing, run:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Cross-Platform Builds

```bash
make build-linux    # output/peakbot-linux-amd64
make build-windows  # output/peakbot-windows-amd64.exe
make build-macos    # output/peakbot-macos-universal2
```

---

## 📁 Project Structure

```
peakbot/
├── src/
│   ├── main.rs              # Entry point
│   ├── lib.rs               # AgentRunner, system prompt builder
│   ├── providers/           # LLM provider abstraction
│   ├── tools/               # Built-in tools
│   ├── hooks/               # Event hooks & cost tracking
│   ├── skills/              # Skill discovery & loading
│   ├── ui/                  # TUI (ratatui)
│   └── context_manager.rs   # Context compaction
├── tests/                   # Integration tests with mock provider
├── Dockerfile.{linux,windows,macos}
├── Makefile                 # Build & release automation
└── agents.md                # Full internal documentation
```

---

## 🤝 Contributing

All changes go through Pull Requests. Every PR must:

1. Pass `cargo fmt`, `cargo clippy -D warnings`, and `cargo test`.
2. Add a changelog entry to `release-notes/current.md`.

See `agents.md` for the complete contributor guide.

---

## 📜 License

MIT — see [LICENSE](LICENSE) for details.

---

> *"Clean, precise, and purposeful — code forged for the glory of good software."*
