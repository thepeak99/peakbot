//! Skill discovery and registry management

use crate::skills::parser::parse_skill_file;
use crate::skills::types::Skill;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const PROMPT: &str = r#"
# Skills
You can use skills.

Skills are organized in folders. Each folder contains a `SKILL.md` file describing the capability, when to use it, required inputs, and usage instructions. **You do not load skill documentation by default** — only read a `SKILL.md` when you've determined a skill is likely relevant.

# Workflow

1. Read the user's request and identify what kind of task it is.
2. Scan the available skill list and check if any skill plausibly applies.
3. If yes — load and read its `SKILL.md` before doing anything else.
4. After reading, decide: does this skill actually fit? If yes, follow its instructions precisely. If no, discard it and either try another skill or proceed without.
5. If multiple skills seem relevant, load all candidates before starting the task.
6. If no skill applies, complete the task using permanently available tools (e.g. MCPs, built-in functions) or your own reasoning.

# Guidelines

- Never assume how a skill works — always read its `SKILL.md` first.
- When in doubt whether a skill applies, load it. Reading is cheap; doing the wrong thing isn't.
- Follow `SKILL.md` instructions exactly — they encode hard-won best practices.
- If a task could chain multiple skills together, plan the full sequence before starting.

# Available skills
"#;

/// Registry of loaded skills
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    /// Full skill data, indexed by skill name
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    /// Create a new empty skill registry
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Get a skill by name
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Get all skills
    pub fn all(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Get the number of registered skills
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Load all skills from a directory, appending a user-facing line to
    /// `warnings` for every skill that fails to parse (so the UI can surface
    /// it) — a single bad skill never aborts the scan.
    pub fn load_from_directory(&mut self, dir: &Path, warnings: &mut Vec<String>) {
        if !dir.is_dir() {
            tracing::debug!("Skills directory does not exist: {}", dir.display());
            return;
        }

        tracing::info!("Loading skills from: {}", dir.display());

        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to read skills directory {}: {}", dir.display(), e);
                warnings.push(format!(
                    "⚠ Skill directory unreadable: {} — {e}",
                    dir.display()
                ));
                return;
            }
        };

        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(e) => {
                    warnings.push(format!(
                        "⚠ Skill entry unreadable in {}: {e}",
                        dir.display()
                    ));
                    continue;
                }
            };

            if !path.is_dir() {
                continue;
            }

            // Check if this directory contains a SKILL.md
            if !path.join("SKILL.md").exists() {
                tracing::debug!("Skipping {} - no SKILL.md found", path.display());
                continue;
            }

            match parse_skill_file(&path) {
                Ok(skill) => {
                    let name = skill.name.clone();
                    tracing::info!("Loaded skill: {}", name);
                    self.skills.insert(name, skill);
                }
                Err(e) => {
                    tracing::warn!("Failed to parse skill in {}: {}", path.display(), e);
                    warnings.push(format!("⚠ Skill load failed: {} — {e}", path.display()));
                }
            }
        }
    }

    /// Generate a system prompt section with available skills
    pub fn to_system_prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }

        let mut section = PROMPT.to_owned();

        for skill in self.skills.values() {
            section.push_str(&format!(
                "- {}: `{}` - {}\n",
                skill.name,
                skill.skill_md.to_string_lossy(),
                skill.description,
            ));
            if let Some(tools) = &skill.allowed_tools {
                section.push_str(&format!("  - Allowed tools: `{}`\n", tools));
            }
        }

        section
    }
}

/// Get the default skills directories to search
/// On Linux: ~/.agents/skills (global) and `<session_cwd>/.agents/skills` (local).
/// The local dir is resolved against the caller-supplied session cwd, not the
/// process cwd — after `/cd`/`/load` the process cwd never moves (see the
/// per-session cwd refactor), so the session value is the only correct base.
pub fn get_default_skills_dirs(session_cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Global skills directory: ~/.agents/skills
    if let Some(home) = dirs::home_dir() {
        let global_skills = home.join(".agents").join("skills");
        dirs.push(global_skills);
    }

    // 2. Local skills directory: <session_cwd>/.agents/skills
    dirs.push(session_cwd.join(".agents").join("skills"));

    dirs
}

/// Load skills from all default locations relative to `session_cwd`.
/// Returns the populated registry plus any user-facing load warnings (a bad
/// skill is skipped, never fatal) so callers can surface them in the UI.
pub fn load_default_skills(session_cwd: &Path) -> (SkillRegistry, Vec<String>) {
    let mut registry = SkillRegistry::new();
    let mut warnings = Vec::new();

    for dir in get_default_skills_dirs(session_cwd) {
        if dir.exists() {
            tracing::info!("Checking for skills in: {}", dir.display());
            registry.load_from_directory(&dir, &mut warnings);
        }
    }

    if registry.is_empty() {
        tracing::info!("No skills found in default locations");
    } else {
        tracing::info!("Loaded {} skill(s)", registry.len());
    }

    (registry, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, content: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn load_from_directory_warns_on_broken_skill_and_keeps_valid_ones() {
        let tmp = TempDir::new().unwrap();
        write_skill(
            tmp.path(),
            "good-skill",
            "---\nname: good-skill\ndescription: A valid skill\n---\n# Body\n",
        );
        // Missing frontmatter delimiter → parse failure.
        write_skill(tmp.path(), "broken-skill", "no frontmatter here\n");

        let mut registry = SkillRegistry::new();
        let mut warnings = Vec::new();
        registry.load_from_directory(tmp.path(), &mut warnings);

        assert!(registry.get("good-skill").is_some());
        assert_eq!(registry.len(), 1, "broken skill must be skipped");
        assert_eq!(warnings.len(), 1, "broken skill must produce one warning");
        assert!(warnings[0].contains("broken-skill"));
    }

    #[test]
    fn get_default_skills_dirs_uses_session_cwd_for_local() {
        let session_cwd = Path::new("/tmp/some/session/dir");
        let dirs = get_default_skills_dirs(session_cwd);
        assert!(
            dirs.iter()
                .any(|d| d == &session_cwd.join(".agents").join("skills")),
            "local skills dir must resolve against the session cwd, not the process cwd"
        );
    }
}
