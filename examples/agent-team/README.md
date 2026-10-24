# PeakBot Software-Team Profile (example)

A multi-agent setup: you talk to **one** agent (`lead`), which delegates tasks
to specialised **subagents** — a software team of PM, architect, developer, and
tester. A second profile (`research`) shows a lighter team with its own persona.

## The mental model

- **A profile is a named working setup.** `config.yaml` declares two:
  `software-team` (first = the default) and `research`. Each lists its
  **agents** in order; an agent may declare **subagents**.
- **You talk to one agent: `lead`.** It is just an agent with a `model:` and
  an optional `prompt:` (rendered as the `# Instructions` section of its system
  prompt). It is **not** one of its subagents.
- `lead` has a **`delegate(role, task, parent_task_id)`** tool — present exactly
  because it declares `subagents:` (derived; there is no flag to set). It calls
  subagents **one at a time** (sequential — there is no parallel mode).
- Each delegation runs the subagent to completion on a **fresh** context
  (its prompt + that one task + its own tool loop) and returns **one string**.
- **You see every subagent's turns** in the transcript, tagged `🧩 <role>`. The
  **lead sees only the returned string** — never the subagent's internal steps.

## Subagents of `software-team › lead`

| Role | Model alias | Responsibility |
|------|-------------|----------------|
| **pm** | `sonnet-3.5` | Turns a request into a product spec |
| **architect** | `sonnet` | Designs the system |
| **developer** | `sonnet` | Implements the design |
| **tester** | `flash` | Writes tests (has a demo `env:` var) |

## Typical flow

The lead decides the order based on the request, e.g.:

```
you → lead → delegate("pm", …)         → spec
           → delegate("architect", …)  → design
           → delegate("developer", …)  → code
           → delegate("tester", …)     → tests
           → lead synthesises & replies to you
```

## Usage

1. Copy the config into your PeakBot config directory:
   ```bash
   cp examples/agent-team/config.yaml ~/.config/peakbot/config.yaml
   ```
   (macOS: `~/Library/Application Support/com.peakbot.peakbot/config.yaml`; Windows: `%APPDATA%\peakbot\peakbot\config\config.yaml`)
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
4. To use the research team instead, pick it before the first turn:
   ```
   /profile research
   ```
   (`/agent <name>` switches between a profile's agents; both choices lock once
   the conversation has started — `/new` for a fresh one.)

## Notes on this design

- **A subagent is `(model alias, prompt)` + optional `env:` / `skills:`.** The
  `model:` names an alias from `providers:` (the same aliases `/model` uses).
  Omit it to inherit the lead's model.
- **Subagents get the SAME tools as the lead, minus `delegate`** (no nested
  delegation) — the profile's `tools:` filter and its `mcps:` servers apply to
  them too. There is **no harness sandbox**; scoping which tools the team gets is
  a deliberate operator responsibility (`tools:` / `mcps:` on the profile).
- **`env:` is per-subagent** and overlaid on the profile's `env:` for that
  subagent's bash tool only — it never leaks into the lead or other subagents.
- **Cost and tokens roll up.** A delegation's usage lands in `/stats` just like
  the lead's own turns.

## Customization

Edit `config.yaml` to change model aliases, tune prompts, or add/remove
subagents. Put only specialists under `subagents:`; the agent you chat with is
the one that declares them.
