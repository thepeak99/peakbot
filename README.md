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

### Run

```bash
cargo run --release
```

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

The web UI (`peakbot --web`) ships as an embedded React + Vite bundle. For
iterating on it, `make dev` runs both halves with hot reload:

```bash
make dev
```

This starts the backend under `cargo watch` (on `127.0.0.1:7823`) and the Vite
dev server (on `localhost:5173`) together. **Open http://localhost:5173** — Vite
serves the app with HMR and proxies the `/ws` WebSocket to the backend.

- Editing a file under `web/src/` hot-swaps in the browser in <1s (no full reload).
- Editing Rust rebuilds and restarts the backend (~seconds), which drops the live
  WebSocket session — the browser reconnects on its own.
- Requires `cargo install cargo-watch` and Node.js 22+.

Production is unchanged: `make web` builds the static bundle that `cargo run -- --web`
embeds and serves on `:7823`. `make dev` touches no Rust code paths.

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
