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
   - `scripts`: HashMap<String, PathBuf> (paths to the scripts inside the skill's scripts diretory, indexed by name)
   - `references`: HashMap<String, PathBuf> (paths to the references inside the skill's references directory, indexed by name)
   - `assets`: HashMap<String, PathBuf> (paths to the assets inside the skill's assets directory, indexed by name)

2. Implement YAML frontmatter parser:
   - Extract `---` delimited YAML block from SKILL.md
   - Parse using serde_yaml
   - Validate required fields (name, description)
   - Validate name format (lowercase, hyphens, no leading/trailing hyphens)

3. Add validation functions (https://github.com/agentskills/agentskills/blob/main/skills-ref/src/skills_ref/validator.py)
   - `validate_skill_name(name: &str) -> Result<(), ValidationError>` 
   - `validate_description(desc: &str) -> Result<(), ValidationError>`

---

### Phase 2: Skills Discovery and Loading

**Goal**: Load skills from filesystem, maintain skill registry

**Files to modify**:
- `src/config.rs` - Add skills directory configuration
- `src/skills/discovery.rs` - New file for skill discovery

**Tasks**:

1. Load skills from the .agents/skills directory in Linux systems and the .agents/skill subdirectory in the current workdir

2. Implement skill discovery:
   - Scan skills directory for subdirectories
   - Each subdirectory must contain SKILL.md
   - Build registry of available skills with metadata only (not body)

3. Add startup behavior:
   - On boot, load all skill metadata
   - Print available skills in REPL startup message
   - Inject skill description to the system message for the Model


## Compatibility Notes

- Skills can include `allowed-tools` field, but tool enforcement depends on agent implementation
- MCP servers can still be used alongside skills
- Existing built-in tools (file_read, file_edit, bash, etc.) remain available
- Skills are additive - they enhance rather than replace existing functionality
