# Configuration

This page documents `config.yaml` features that go beyond the quick-start
example in the README. See `README.md` for how to locate your platform
config directory and the base provider/model shape.

## Profiles

A **profile** is a named, boot-selected overlay over a small set of
agent-behaviour fields. It lets one binary + one config file serve multiple
deployments (e.g. "my own CLI" vs. "a locked-down web deployment") without
maintaining separate config files or forking the binary.

```yaml
# ~/.config/peakbot/config.yaml  (MASTER ONLY — see below)
providers:
  - name: openrouter
    type: openrouter
    api_key: ...
    models:
      - name: anthropic/claude-sonnet-4.6
        alias: sonnet
default_model: sonnet

profiles:
  locked:
    tools:
      disabled: [bash]
    memory:
      enabled: false
```

Select a profile at boot with `--profile <name>`:

```sh
peakbot --profile locked
```

`--profile` has no clap conflicts — it works with `--tui`, `--stdio`, and the
default web UI alike. There is no runtime `/profile` switch and no
config-file default: the flag is the one, visible selection channel.

### Fields (PR 1)

A profile currently supports two fields, each `Option`-typed — `None` (i.e.
omitted) leaves the resolved value untouched:

| Field | Replaces | Notes |
|---|---|---|
| `tools:` | the effective `tools:` block wholesale | same blocklist `disabled:` XOR allowlist `only:` shape and validation as the top-level `tools:` key |
| `memory:` | the effective `memory:` block wholesale | same shape as the top-level `memory:` key |

A profile with neither field set is legal (it's a no-op placeholder); a
profile is not required to set every field, and any field it omits falls
through to whatever master/per-repo resolved to.

### Precedence — the profile is a ceiling, not a suggestion

```
1. profile overlay (from --profile, master config only)   ← wins
2. per-repo .peakbot/config.yaml
3. master config.yaml
4. built-in defaults
```

The profile is applied **last**, after the normal master+per-repo merge, and
it is **re-applied on every reload** (`/cd`, `/model`, `/new`, `/load`,
`/pipeline`). This means a profile that disables `bash` stays disabled even
if a repo you `/cd` into ships a `.peakbot/config.yaml` that tries to
re-enable every tool — the profile always wins. There is no runtime way to
escape an active profile from inside a session.

### Master-only

Profiles are read from the **master** config only
(`~/.config/peakbot/config.yaml` or platform equivalent). A `profiles:`
block in a per-repo `.peakbot/config.yaml` is silently ignored for merge
purposes — a checked-out repo must not be able to redefine the deployment's
guarantees — but PeakBot does not drop it silently: boot emits one warning
when a per-repo config declares `profiles:`:

```
⚠ .peakbot/config.yaml declares `profiles:` — ignored (profiles are master-config only).
```

### Unknown profile name

`--profile ghost` when no profile named `ghost` is configured is a boot
error that names the profile you asked for and lists every profile the
master config actually declares.
