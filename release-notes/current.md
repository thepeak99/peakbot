# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Pipeline overhaul phase 1 (lane plumbing, latent — no behaviour change yet): added `MessageSource::SubAgent { role }` and an `is_orchestrator_lane()` predicate. `get_agent_history` now filters to the orchestrator lane, so a sub-agent's turns will never leak into the orchestrator's wire context (Background turns still counted as orchestrator input). Renderer and web UI label sub-agent turns with a `🧩 <role>` badge; the REPL render-cache fingerprint now includes the message source lane.