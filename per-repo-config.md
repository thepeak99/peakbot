# Per-Repository Configuration — Feature Design

> **TL;DR**: Add `.peakbot/config.yaml` support for project-specific config overrides. Top-level keys present in the per-repo config replace those in the master config entirely. Simple, predictable, no magic.

---

## Motivation

PeakBot currently reads its master config from `~/.config/peakbot/peakbot/config.yaml`. This is great for global defaults, but teams working across multiple projects often need per-project overrides:

- Use a faster/cheaper model for prototyping repos
- Enable different MCP servers per project
- Configure project-specific bash environment variables
- Adjust context compaction settings for very large codebases

Currently, users work around this by:
1. Maintaining separate config files and manually switching
2. Using environment variables (which are global, not per-project)
3. Changing the master config (which affects all projects)

None of these are satisfying.

---

## Design Principles (Zen of Engineering)

**Simplicity is the key.**  
One file, one concept. No new formats, no nested directories with multiple config files, no magic.

**Principle of Least Astonishment.**  
The merge order is exactly what you'd expect:
1. Defaults (built-in)
2. Master config (`~/.config/peakbot/peakbot/config.yaml`)
3. Per-repo config (`.peakbot/config.yaml`)
4. Environment variables (highest priority)

**Fewer pieces → fewer things that can go wrong.**  
Only add `.peakbot/config.yaml` for now. No per-repo skills, no per-repo agents, no per-repo hooks. YAGNI.

**Don't be too clever.**  
Top-level key override only. If you specify `provider` in the per-repo config, the entire `provider` object is replaced. No deep nested merging.

---

## File Location

```
project-root/
├── .peakbot/
│   └── config.yaml    # Per-repo overrides (optional)
└── ...other files...
```

**Why a directory, not `.peakbot.yaml`?**
- Room to grow later (skills, hooks, etc.) without breaking backward compat
- Cleaner namespace (`.peakbot/config.yaml` vs `.peakbot.yaml`)
- Easy to `.gitignore` the entire directory

**Why not `.peakbot/` + multiple files?**
- YAGNI. Only `config.yaml` for now.

---

## Merge Semantics

### Top-Level Key Override

Each top-level field in `Config` is treated independently:

| Master Config | Per-Repo Config | Result |
|--------------|-----------------|--------|
| `provider: {...}` | `provider: {...}` | Per-repo wins (entire object replaced) |
| `provider: {...}` | *(not specified)* | Master wins |
| *(not specified)* | `provider: {...}` | Per-repo wins |
| `mcp_servers: [...]` | `mcp_servers: [...]` | Per-repo wins (entire array replaced) |

### What This Means Practically

```yaml
# .peakbot/config.yaml
# Master uses anthropic/claude-3.7-sonnet with api_key and max_tokens
# To use a different model, you must specify ALL provider fields

provider:
  type: openrouter
  config:
    api_key: sk-or-v1-xxx    # Must repeat if you want to keep from master
    model: google/gemini-2.0-flash-001
    max_tokens: 4096         # Must repeat if you want to keep from master
```

**Result:** Uses `gemini-2.0-flash-001` with the values you specified. The entire `provider` object is replaced.

> **Important:** With shallow merge, if you specify `provider` in per-repo config, you must include ALL fields you want to keep from master. Shallow merge doesn't do deep/nested merging.

**Alternative: Use environment variables for single-field overrides**

```bash
# Override only the model via environment (keeps master config otherwise)
export OPENROUTER_MODEL=google/gemini-2.0-flash-001
```


---

## Loading Order (Priority: Low → High)

1. **Defaults** — Built-in Rust defaults (e.g., `claude-3.7-sonnet`, max 4096 tokens)

2. **Master config** — `~/.config/peakbot/peakbot/config.yaml`
   - Loaded via `load_yaml_config()` in `config.rs`

3. **Per-repo config** — `.peakbot/config.yaml` (if exists)
   - Loaded after master config
   - Merged with `merge_with()` method
   - Only keys present in per-repo config are replaced

4. **Environment variables** (highest priority)
   - Already implemented in `Config::load()`
   - `OPENROUTER_API_KEY`, `OPENROUTER_MODEL`, `PROVIDER`, etc.
   - Unchanged behavior

---

## Edge Cases

### 1. Per-repo config doesn't exist
→ Silently skip. Master config used unchanged.

### 2. Per-repo config is malformed YAML
→ Log warning, skip the file entirely. Master config unchanged.
```rust
tracing::warn!("Failed to parse .peakbot/config.yaml: {}. Ignoring.", error);
```

### 3. Relative paths (e.g., `conversation.storage_dir`)
→ Resolve relative to the `.peakbot/` directory location, not `cwd`.
```rust
// If .peakbot/config.yaml has: storage_dir: ./conversations
// It becomes: /path/to/project/.peakbot/conversations
```

### 4. Secrets in per-repo config
→ Allowed (for private repos), but documented. Recommend `.peakbot/` in `.gitignore` for public repos.

### 5. Conflicting MCP servers
→ Per-repo config's `mcp_servers` array replaces master config's entirely. No merging of individual servers.

---

## Implementation Plan


## Implementation Status

### ✅ Phase 1: Core Merge Logic — IMPLEMENTED

**1. `merge_with` method** — Added to `Config` in `src/config.rs`
   - Shallow/top-level key override for all config fields
   - Fields absent in per-repo config are preserved from master

**2. Per-repo config loading** — Added `load_per_repo_config()` in `src/config.rs`
   - Looks for `.peakbot/config.yaml` in current working directory
   - Silently skips if not found
   - Logs warning if malformed

**3. Updated `Config::load()`** — Modified to merge configs in priority order:
   - Defaults → Master config → Per-repo config → Environment variables

**4. Unit tests** — Added 4 tests in `src/config.rs`:
   - `test_merge_with_partial_provider_override`
   - `test_merge_preserves_master_when_repo_doesnt_override`
   - `test_merge_mcp_servers_replacement`
   - `test_merge_context_config`

---

### Not Implemented (YAGNI)

- **Relative path resolution** — Storage directories always use absolute paths or defaults
  - This keeps the implementation simple and predictable
  - If needed later, add a post-merge path resolution step

