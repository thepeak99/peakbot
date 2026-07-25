# PeakBot Six-Role Software-Team Pipeline

A fuller multi-agent team than the basic `pipeline-team` example. You talk to
**one** orchestrator; it delegates to six specialised sub-agents that mirror a
real software team.

## The team

The orchestrator is your top-level `default_model` agent — **not** a role below.
These are the sub-agents it can `delegate` to:

| Role | Alias | Model (Jul 2026) | Does | Aborts / hands back when… |
|------|-------|------------------|------|----------------------------|
| **pm** | `sonnet` | Claude Sonnet 5 | Scope, deliverables, success criteria; surfaces open questions + assumptions | scope is fundamentally underspecified |
| **researcher** | `kimi` | Kimi K3 | Exhaustively scours web + codebase; cited findings + a justified recommendation | — (reports unknowns instead) |
| **architect** | `opus` | Claude Opus 5 | Clean, Zen-of-Engineering design: components, data model, contracts, task plan | handed too little to design responsibly |
| **junior** | `sonnet` | Claude Sonnet 5 | Executes a concrete plan faithfully; clears obvious small obstacles | the task stops being mechanical — clean abort with a handoff |
| **senior** | `opus` | Claude Opus 5 | Plans first, then builds anything to spec; quick-searches facts, flags deep unknowns | a deep unknown blocks — emits `DEEP RESEARCH NEEDED` |
| **tester** | `sonnet` | Claude Sonnet 5 | **TDD**: writes failing tests from the spec, before the code | a spec item can't be turned into a test — flags the gap |

The model mapping is the current recommendation (updated 2026-07-25) based on
public benchmarks and Anthropic's / Moonshot's own positioning. See the
`MODEL TIER RATIONALE` comment block at the top of `config.yaml` for the
per-role reasoning, and the "Not used" notes on Kimi K2 Thinking, Claude
Fable 5 / Mythos 5, and MiniMax M3.

## Why the prompts are written the way they are

Every sub-agent runs on a **fresh context**, returns **one string**, **can't
talk to the human**, and has **no `delegate` tool**. The prompts are built
around those facts:

- "PM asks clarifying questions" → the PM writes an **OPEN QUESTIONS** section
  with a default assumption per question, marked BLOCKING / NON-BLOCKING, for
  the orchestrator to relay. It can't interrogate you mid-run.
- "Senior asks for deep research" → the Senior can't summon the Researcher
  itself (no nested delegation). It does quick fact-checks inline, and for deep
  unknowns emits `DEEP RESEARCH NEEDED: <question>` for the **orchestrator** to
  route to the Researcher.
- "Junior aborts when it gets hard" → the Junior returns `STATUS: ABORTED` with
  the exact blocker and a handoff suggestion. An honest early abort is a success.
- "Tester does TDD" → tests are written from the **spec** (PM criteria +
  Architect interfaces), expected to be **RED** before implementation exists.

## Typical flow

The orchestrator sequences the team based on the request — e.g.:

```
you → orchestrator
        → delegate("pm", …)                 → scope + open questions
        → delegate("researcher", …)         → cited findings + recommendation   (if there's a real unknown)
        → delegate("architect", pm+research)→ locked design + task plan
        → delegate("tester", spec)          → failing tests (TDD, red)
        → delegate("senior" | "junior", …)  → implementation until green
        → orchestrator synthesises & replies to you
```

Junior for mechanical tasks the Architect flagged junior-safe; Senior for the
rest. The orchestrator relays the PM's blocking questions to you before letting
the build start.

## The orchestrator prompt

The orchestrator is your top-level `default_model` agent — its persona is **not**
a role under `pipeline.agents`. This example ships a tailored orchestrator prompt
**already wired into `config.yaml`** via `pipeline.orchestrator_prompt` — the
config field whose content PeakBot appends to the orchestrator's system prompt
whenever sub-agents are active. So the moment you `/subagents on`, the
orchestrator already knows how to lead this team: **delegate everything** (it
never codes, tests, or builds itself), research before acting when it isn't sure,
pack full context into each fresh-context delegation, relay the PM's blocking
questions to you, and route the Senior's `DEEP RESEARCH NEEDED` signals to the
Researcher.

The same text also lives standalone in
[`orchestrator-prompt.md`](orchestrator-prompt.md) — use that copy if you'd
rather install the prompt via your project's `AGENTS.md` (whose content is
injected into the system prompt) instead of the config field. The two are kept
in sync; pick one, not both.

## Usage

1. Copy the config into your PeakBot config directory:
   ```bash
   cp examples/pipeline-dev-team/config.yaml ~/.config/peakbot/peakbot/config.yaml
   ```
2. Add your API key (or set `OPENROUTER_API_KEY`).
3. Run PeakBot, enable sub-agents for this conversation (opt-in, before turn 1),
   then describe what you want:
   ```
   /subagents on
   I want a CLI tool that converts markdown to HTML with a --watch mode
   ```

## Customization

- **Model tiers** are cost knobs. `opus` on orchestrator/architect/senior buys
  judgment + self-verification; `sonnet` on pm/junior/tester is the cheapest
  disciplined execution tier; `kimi` on the researcher is the cheapest
  long-horizon agentic tier with strong MCP-Atlas / BrowseComp numbers.
  Downgrade the three `opus` roles to `sonnet` if you don't have Opus 5 access
  (cost ~halved, modest quality drop). Downgrade `senior` to `kimi` for a
  coding-tier swap (DeepSWE 67.5 vs Opus 5's 68.8 — within the margin).
  See the `MODEL TIER RATIONALE` block in `config.yaml` for full tradeoffs.
- **`pipeline:` is boot-only** — edit and restart to apply (it's in the
  "restart to apply" set of the live-reload rules).
- Keep the orchestrator in `default_model`; put only sub-agents under
  `pipeline.agents`. Never add an `orchestrator` role.
