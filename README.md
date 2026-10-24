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
(`~/.config/peakbot/` on Linux, `~/Library/Application Support/peakbot/` on
macOS). It has **exactly four top-level blocks** — `providers`, `mcps`,
`profiles`, `general` (unknown keys anywhere are a load error):

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

profiles:
  default:
    agents:
      main:
        model: sonnet        # every agent names its own starting model — no default_model

# mcps: [ … ]               # optional — MCP servers (boot-only)
# general: { web: …, search: …, context: …, … }   # optional — all defaulted
```

There is no `default_model`: the **first** profile is the default, the
**first** agent in a profile is the default, and each agent's `model:` is its
starting model (`/model` may switch it per conversation). Old 0.23 files are
**auto-migrated on first start** — the original is kept as
`config.yaml.pre-0.24`. The web UI's Settings screen (⚙ gear / Ctrl+,) edits
the same file; on the very first run a guided setup walks you through it.

### Profiles & agents

A **profile** is a named working setup declared in the `profiles:` map (an
ordered map — **the first entry is the default**): the tools, MCP access, env,
memory and knowledge (vector DB) that its agents get. Each profile declares an
ordered `agents:` map (again **first = default**); an **agent** is what you
talk to — a starting `model:`, an optional `persona:`/`prompt:`, and optional
`subagents:` (an agent without subagents is a plain single-agent setup).

Selection is per conversation: `/profile <name>` and `/agent <name>` (web: the
📁 chip in the composer). Both lock once the conversation has started; `/new`
keeps the selection, and a fresh session starts at the first profile's first
agent. Booting with `--profile <name>` **pins** a global profile — every
conversation runs under it, `/profile` is refused, and repo files are ignored.

A per-repo `.peakbot/config.yaml` may contain **only** `profiles:`. Repo
profiles are listed first for that repo, and a repo profile with the same name
as a global one **replaces** it entirely (no merge) — a checked-out repo can
ship its own setup without touching the global file.

A profile's blocks: `tools:` / `mcps:` / `skills:` / `ui.tabs:` name filters,
`fetch_url:`, `env:` (base bash env), `memory:`, `vector_db:` (presence = on),
and `agents:`. See [agents.md](agents.md#agents-and-subagents) for the full
reference. Runnable multi-agent examples:
[examples/agent-team/](examples/agent-team/) (a team with subagents) and
[examples/agent-dev-team/](examples/agent-dev-team/).

```yaml
profiles:
  locked:
    tools:
      disabled: [bash]
    mcps:
      enabled: false          # no MCP tools at all
    ui:
      tabs:
        disabled: [files, tasks, bash, agents]
    agents:
      solo:
        model: sonnet
```

### Personas & instructions

Each agent's system prompt is built from, in order: a **persona** (the agent's
`persona:`, or the built-in persona when absent — including agents with
subagents), the built-in **core tool guidance** (unless
`core_guidance: false`, which makes the persona the *whole* static head), the
memory section (when the profile has `memory:` enabled), skills, the
environment block, the repo's `agents.md`, and the agent's `prompt:` rendered
as an `# Instructions` section.

Reach for `core_guidance: false` + `persona:` when your deployment's toolset
makes the built-in guidance wrong — roughly 69% of the built-in core prompt is
coaching for `bash`, `think`, and `todo`, so a research-only profile that
disables those tools shouldn't ship advice for using them. Use `prompt:` for
workflow instructions, not voice — the persona carries the voice, and it
stays current as PeakBot's tools evolve. Sub-agents have no persona: their
`prompt:` **is** their whole persona.

### Name filters (`tools:`, `mcps:`, `skills:`, `ui.tabs:`)

One filter shape with one meaning — a profile's `tools:` (built-in tools),
`mcps:` (the top-level MCP servers), an agent's `skills:`, and `ui.tabs:`
(web tabs):

- `only:` — allowlist. Only the named entries survive.
- `disabled:` — blocklist. The named entries are removed.
- Setting both `only:` and `disabled:` is a config error.
- An empty list, or no filter at all, means **no filtering** — everything is
  allowed. An empty `only:` never means "none"; use `enabled: false` for that.
- `enabled: false` means **nothing is allowed** — the way to say "no built-in
  tools" or "no MCP tools".
- Unknown names are a config error (unknown **skills** are a warning — the
  skill set depends on the directory).

`delegate` is not a filterable tool: it is registered exactly when the agent
declares non-empty `subagents:`.

### Run

```bash
cargo run --release
```

On the very first run with **no** config, the web UI shows a **guided setup**
(Welcome → Provider → Agent → Web → Review → Finish) that writes the config —
`/setup` is the same screen for old bookmarks. In the desktop app the PeakBot
server restarts automatically after finishing; in a browser you see
"Restart PeakBot to start chatting."

- **Headless / SSH, no TTY, or `--stdio`:** refuses to start and prints what
  to configure.
- **`peakbot --tui`:** starts the terminal UI directly — no setup. Use this
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
| `/profile <name>` | Pick a profile (before the first turn) |
| `/agent <name>` | Pick an agent within the profile (before the first turn) |
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
`general.web.token` in `config.yaml`. Subsequent runs read it from there —
you do not need to export `PEAKBOT_WEB_TOKEN` to your shell (the env var and
`--token` outrank it when set).

Pass `--tls` to run the service over HTTPS with the built-in CA (the same
machinery as the foreground `--tls`); `--tls-name <NAME>` (repeatable) adds
extra SANs to the certificate. `service status` reports the state as
`TLS: on` / `TLS: off`, derived from the installed artifact — correct even
while the service is stopped.

Non-loopback binds **require** a token; the loopback/token invariant is
enforced at plan-build time, not as a runtime check.

Note: `general.web.tls: true` in `config.yaml` switches the service to HTTPS at
runtime independently of `--tls` — `service status` reports what the
*artifact* says, not what the config overrides.

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
