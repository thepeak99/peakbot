# PeakBot Skills Implementation Plan

This document outlines the implementation plan to add Agent Skills support to PeakBot, based on the [Agent Skills specification](https://raw.githubusercontent.com/agentskills/agentskills/refs/heads/main/docs/specification.mdx).

## Overview

The Agent Skills format defines a standardized way to package agent capabilities as skill directories containing `SKILL.md` with YAML frontmatter. PeakBot currently operates as a general-purpose coding agent. Adding Skills support will allow PeakBot to:

1. Discover and load skills from a configured skills directory
2. Use skill metadata (`name`, `description`) at startup for skill selection
3. Activate skills on-demand based on task context
4. Execute skill instructions and leverage referenced files (scripts, references, assets)

---

## Current Architecture

### Existing Components
- **lib.rs**: Core library with agent building, MCP server connections, REPL loop
- **config.rs**: Configuration from YAML files and environment variables
- **tools/**: Built-in tools (file_read, file_edit, bash, list_directory, fetch_url)
- **system_prompt.txt**: Static system prompt baked into the binary

### Tool Definitions
| Tool | Purpose |
|------|---------|
| `file_read` | Read files with line numbers, optional line range |
| `file_edit` | View, create, str_replace, insert file operations |
| `bash` | Execute shell commands with timeout |
| `list_directory` | List directory contents (recursive optional) |
| `fetch_url` | HTTP GET requests |

---

## Implementation Phases

### Phase 1: Skills Data Structures and Parsing

**Goal**: Define core data types and parsing logic for SKILL.md files

**Files to create/modify**:
- `src/skills/mod.rs` - New module for skills functionality
- `src/skills/types.rs` - Skill, SkillMetadata, SkillConfig structs
- `src/skills/parser.rs` - YAML frontmatter and SKILL.md parsing

**Tasks**:
1. Create `Skill` struct with fields matching spec:
   - `name`: String (1-64 chars, lowercase + hyphens, validated)
   - `description`: String (1-1024 chars)
   - `license`: Option<String>
   - `compatibility`: Option<String> (max 500 chars)
   - `metadata`: Option<HashMap<String, String>>
   - `allowed_tools`: Option<String> (space-delimited)
   - `body`: String (markdown content after frontmatter)

2. Create skill directory structure types:
   - `SkillDirectory` - represents a skill folder
   - `SkillPath` - enum for skill-relative paths (scripts/, references/, assets/)

3. Implement YAML frontmatter parser:
   - Extract `---` delimited YAML block from SKILL.md
   - Parse using serde_yaml
   - Validate required fields (name, description)
   - Validate name format (lowercase, hyphens, no leading/trailing hyphens)

4. Add validation functions:
   - `validate_skill_name(name: &str) -> Result<(), ValidationError>`
   - `validate_description(desc: &str) -> Result<(), ValidationError>`

**Estimated effort**: 2-3 hours

---

### Phase 2: Skills Discovery and Loading

**Goal**: Load skills from filesystem, maintain skill registry

**Files to modify**:
- `src/config.rs` - Add skills directory configuration
- `src/skills/discovery.rs` - New file for skill discovery

**Tasks**:
1. Extend `Config` with skills settings:
   ```rust
   pub struct Config {
       // ... existing fields
       pub skills_directory: Option<PathBuf>,  // Where to find skills
       pub active_skill: Option<String>,       // Pre-activated skill
   }
   ```

2. Add config loading for `SKILLS_DIR` environment variable (or config.yaml)

3. Implement skill discovery:
   - Scan skills directory for subdirectories
   - Each subdirectory must contain SKILL.md
   - Build registry of available skills with metadata only (not body)

4. Add startup behavior:
   - On boot, load all skill metadata
   - Print available skills in REPL startup message

**Estimated effort**: 2 hours

---

### Phase 3: Skill Activation and Context Injection

**Goal**: Activate skills on-demand and inject skill content into agent context

**Files to modify**:
- `src/lib.rs` - Integrate skills with agent
- `src/skills/activation.rs` - New file for activation logic

**Tasks**:
1. Create `SkillActivation` struct to track active skill state:
   ```rust
   pub struct SkillActivation {
       pub skill: Skill,
       pub scripts: HashMap<String, PathBuf>,  // name -> path
       pub references: HashMap<String, PathBuf>,
       pub assets: HashMap<String, PathBuf>,
   }
   ```

2. Implement skill activation:
   - `activate_skill(name: &str) -> Result<SkillActivation, Error>`
   - Load full SKILL.md body when activated
   - Index available files in scripts/, references/, assets/ directories

3. Modify agent building to support skill context injection:
   - Add method to inject skill preamble into agent prompts
   - Skills can modify the system prompt or provide supplemental context

4. Add tool for skill management:
   - `list_skills` - Show available skills
   - `activate_skill` - Activate a skill by name
   - `deactivate_skill` - Return to general mode
   - `get_skill_info` - Show skill details

**Estimated effort**: 3-4 hours

---

### Phase 4: Progressive Disclosure and File References

**Goal**: Implement efficient loading as specified (metadata → instructions → resources)

**Files to modify**:
- `src/skills/loader.rs` - New file for lazy loading
- Update existing tools to handle skill-relative paths

**Tasks**:
1. Implement three-tier loading:
   - **Tier 1 (startup)**: Load only name + description for all skills (~100 tokens)
   - **Tier 2 (activation)**: Load full SKILL.md body when skill activated (<5000 tokens recommended)
   - **Tier 3 (on-demand)**: Load scripts/, references/, assets/ only when requested

2. Handle file references in skill markdown:
   - Parse relative paths like `scripts/extract.py` or `references/finance.md`
   - Provide mechanism for agent to load these files
   - Support `skills-ref validate` integration (optional)

3. Add skill-relative path resolution:
   - When skill is active, resolve paths relative to skill root
   - Agent can ask to "load script X" and get its contents

**Estimated effort**: 2-3 hours

---

### Phase 5: Skill Selection Intelligence

**Goal**: Help the agent select appropriate skills based on user input

**Files to modify**:
- `src/skills/selection.rs` - New file for skill matching

**Tasks**:
1. Implement simple keyword matching:
   - Extract keywords from user input
   - Match against skill descriptions
   - Suggest relevant skills to the agent

2. Create skill recommendation tool:
   - `suggest_skills(task_description: &str) -> Vec<SkillSummary>`
   - Returns ranked list of potentially relevant skills

3. Integrate with REPL:
   - When user mentions skill-related keywords, agent can auto-suggest activation

**Estimated effort**: 1-2 hours

---

### Phase 6: Testing and Validation

**Goal**: Ensure implementation is correct and robust

**Files to create**:
- `tests/skills/` - Integration tests

**Tasks**:
1. Create sample skills for testing:
   - `test-skills/pdf-processing/SKILL.md`
   - `test-skills/data-analysis/SKILL.md`

2. Write unit tests:
   - YAML frontmatter parsing
   - Name validation
   - Skill discovery

3. Write integration tests:
   - Full skill activation flow
   - File reference resolution
   - Progressive loading behavior

**Estimated effort**: 2 hours

---

## Implementation Order Summary

| Phase | Effort | Description |
|-------|--------|-------------|
| Phase 1 | 2-3h | Data structures and parsing |
| Phase 2 | 2h | Discovery and loading |
| Phase 3 | 3-4h | Activation and context injection |
| Phase 4 | 2-3h | Progressive disclosure |
| Phase 5 | 1-2h | Skill selection |
| Phase 6 | 2h | Testing |
| **Total** | **~13-17h** | Complete skills implementation |

---

## Configuration Changes

### Environment Variables to Add
- `SKILLS_DIR` - Path to skills root directory (default: disabled)
- `ACTIVE_SKILL` - Pre-activate a skill on startup (optional)

### Config File Format (config.yaml)
```yaml
skills:
  directory: /path/to/skills
  active_skill: pdf-processing  # optional
```

---

## Compatibility Notes

- Skills can include `allowed-tools` field, but tool enforcement depends on agent implementation
- MCP servers can still be used alongside skills
- Existing built-in tools (file_read, file_edit, bash, etc.) remain available
- Skills are additive - they enhance rather than replace existing functionality

---

## Future Enhancements (Out of Scope)

- `skills-ref` CLI integration for validation
- Skill versioning and dependency management
- Remote skill registries
- Skill sharing/publishing workflows