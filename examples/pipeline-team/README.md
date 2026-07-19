# PeakBot Software-Team Pipeline (example)

A multi-agent setup: you talk to **one** orchestrator, which delegates tasks to
specialised **sub-agents** — a software team of PM, architect, developer, and
tester.

## The mental model

- **You talk to one agent: the orchestrator.** It's just your normal top-level
  agent (`default_model` in the config — `sonnet` here). It is **not** a
  pipeline role.
- The orchestrator has a **`delegate(role, task)`** tool (present only while
  `pipeline.enabled: true`). It calls sub-agents **one at a time** (sequential —
  there is no parallel mode).
- Each delegation runs the role's agent to completion on a **fresh** context
  (role prompt + that one task + its own tool loop) and returns **one string**.
- **You see every sub-agent's turns** in the transcript, tagged `🧩 <role>`. The
  **orchestrator sees only the returned string** — never the sub-agent's
  internal steps.

## Roles

The orchestrator is the top-level agent — **not** listed below. These are the
sub-agents it can delegate to:

| Role | Model alias | Responsibility |
|------|-------------|----------------|
| **pm** | `sonnet-3.5` | Turns a request into a product spec |
| **architect** | `sonnet` | Designs the system |
| **developer** | `sonnet` | Implements the design |
| **tester** | `flash` | Writes tests (has a demo `env:` var) |

## Typical flow

The orchestrator decides the order based on the request, e.g.:

```
you → orchestrator → delegate("pm", …)         → spec
                   → delegate("architect", …)  → design
                   → delegate("developer", …)  → code
                   → delegate("tester", …)     → tests
                   → orchestrator synthesises & replies to you
```

## Usage

1. Copy the config into your PeakBot config directory:
   ```bash
   cp examples/pipeline-team/config.yaml ~/.config/peakbot/peakbot/config.yaml
   ```
2. Add your API key (or set `OPENROUTER_API_KEY`):
   ```yaml
   providers:
     - name: openrouter
       type: openrouter
       api_key: sk-or-v1-your-key-here
   ```
3. Run PeakBot and describe what you want to build:
   ```
   I want a CLI tool that converts markdown to HTML
   ```

## Notes on this design

- **A role is `(model alias, prompt)` + optional `env:`.** The `model:` names an
  alias from `providers:` (the same aliases `/model` uses). Omit it to fall back
  to `default_model`.
- **Sub-agents get the full built-in toolset MINUS `delegate`** (no nested
  delegation). They can read files, run bash, search the web, etc. — there is
  **no harness sandbox in v1**; scoping which tools a role gets is a deliberate
  operator responsibility (a top-level tool-disable feature will apply here once
  it lands).
- **`env:` is per-role** and merged only into that sub-agent's bash tool env — it
  never leaks into the orchestrator or other roles.
- **Cost and tokens roll up.** A delegation's usage lands in `/stats` just like
  the orchestrator's own turns.

## Customization

Edit `config.yaml` to change model aliases, tune prompts, or add/remove roles.
Keep the orchestrator in `default_model`; put only sub-agents under
`pipeline.agents`.
