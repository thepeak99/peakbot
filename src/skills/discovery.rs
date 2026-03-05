//! Skill discovery and registry management

use crate::skills::parser::parse_skill_file;
use crate::skills::types::Skill;
use anyhow::Result;
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
#[derive(Debug, Default)]
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

    /// Load all skills from a directory
    pub fn load_from_directory(&mut self, dir: &Path) -> Result<()> {
        if !dir.is_dir() {
            tracing::debug!("Skills directory does not exist: {}", dir.display());
            return Ok(());
        }

        tracing::info!("Loading skills from: {}", dir.display());

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

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
                }
            }
        }

        Ok(())
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
/// On Linux: ~/.agents/skills and ./.agents/skills (in current workdir)
pub fn get_default_skills_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. Global skills directory: ~/.agents/skills
    if let Some(home) = dirs::home_dir() {
        let global_skills = home.join(".agents").join("skills");
        dirs.push(global_skills);
    }

    // 2. Local skills directory: ./.agents/skills (in current workdir)
    if let Ok(cwd) = std::env::current_dir() {
        let local_skills = cwd.join(".agents").join("skills");
        dirs.push(local_skills);
    }

    dirs
}

/// Load skills from all default locations
pub fn load_default_skills() -> Result<SkillRegistry> {
    let mut registry = SkillRegistry::new();

    for dir in get_default_skills_dirs() {
        if dir.exists() {
            tracing::info!("Checking for skills in: {}", dir.display());
            if let Err(e) = registry.load_from_directory(&dir) {
                tracing::warn!("Error loading skills from {}: {}", dir.display(), e);
            }
        }
    }

    if registry.is_empty() {
        tracing::info!("No skills found in default locations");
    } else {
        tracing::info!("Loaded {} skill(s)", registry.len());
    }

    Ok(registry)
}
