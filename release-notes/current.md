# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

### Multi-pipeline — named teams with per-conversation selection

**BREAKING: legacy `pipeline:` config block no longer boots.** PeakBot now refuses to start if the old `pipeline:` block (with `enabled:` and `orchestrator_prompt:`) is present — including `enabled: false`. A hard boot error prints a migration hint.

**Migration recipe:** wrap your existing `agents:` under a named `pipelines:` entry, move `orchestrator_prompt:` to `orchestrator.prompt`, and drop `enabled:`:

```yaml
# Before (legacy — no longer boots)
pipeline:
  enabled: true
  orchestrator_prompt: "You lead the team."
  agents:
    reviewer:
      model: sonnet
      prompt: "You review diffs."

# After (new shape)
pipelines:
  - name: my-team
    orchestrator:
      prompt: "You lead the team."
    agents:
      reviewer:
        model: sonnet
        prompt: "You review diffs."
```

**What's new:**

- **`pipelines:` list** — declare one or more named teams. Each entry has `name`, a required `orchestrator:` (model + optional prompt/persona), and an `agents:` map of sub-agent roles. Declaration order is UI order.
- **Orchestrator as team member** — the orchestrator is configured alongside sub-agents inside each pipeline entry. `orchestrator.model` sets the orchestrator's model (falls back to `default_model`), `orchestrator.prompt` adds an addendum to the orchestrator recipe, and `orchestrator.persona` replaces the global persona for that pipeline's orchestrator only.
- **Per-conversation pipeline selection** — pick a team with `/pipeline <name>` (TUI) or the pipeline selector in the Agents tab (web). Locked after the first turn. `/pipeline none` = single agent mode. `/new` keeps the current selection.
- **Locked orchestrator model** — when a pipeline is selected, the orchestrator's model is derived from the pipeline config. `/model <alias>` refuses with an explanatory message; the top model selector is read-only.
- **`/subagents` removed** — the command is replaced by `/pipeline`. A tombstone message points to the new command.
- **Setup wizard emits the new shape** — the Multi-agent step now writes a `pipelines:` entry (name, orchestrator model/prompt/persona, member roles) instead of the legacy block, and the config endpoint validates it with the same checks the binary runs at boot. A config imported with several `pipelines:` entries is shown read-only and written back verbatim — the wizard never rewrites your teams.
- **Orchestrator-scoped context meter** — the Session tab context meter now shows orchestrator context only (sub-agent turns don't move it), labelled "Orchestrator context". This is the same signal the compaction gate reads.
