//! Parser for SKILL.md files with YAML frontmatter

use crate::skills::types::Skill;
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Validation errors for skill fields
#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Invalid skill name '{name}': {reason}")]
    InvalidName { name: String, reason: String },
    #[error("Invalid description: {0}")]
    InvalidDescription(String),
}

/// YAML frontmatter structure matching the Agent Skills spec
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(default)]
    allowed_tools: Option<String>,
}

/// Validate skill name: 1-64 chars, lowercase letters, numbers, and hyphens only,
/// must start and end with alphanumeric (no leading/trailing hyphens)
pub fn validate_skill_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::InvalidName {
            name: name.to_string(),
            reason: "name cannot be empty".to_string(),
        });
    }

    if name.len() > 64 {
        return Err(ValidationError::InvalidName {
            name: name.to_string(),
            reason: "name must be 64 characters or less".to_string(),
        });
    }

    // Check first and last characters are alphanumeric
    let first_char = name.chars().next().unwrap();
    let last_char = name.chars().last().unwrap();

    if !first_char.is_alphanumeric() {
        return Err(ValidationError::InvalidName {
            name: name.to_string(),
            reason: "name must start with an alphanumeric character".to_string(),
        });
    }

    if !last_char.is_alphanumeric() {
        return Err(ValidationError::InvalidName {
            name: name.to_string(),
            reason: "name must end with an alphanumeric character".to_string(),
        });
    }

    // Check all characters are lowercase alphanumeric or hyphen
    for (i, c) in name.chars().enumerate() {
        if c != '-' && !c.is_ascii_lowercase() && !c.is_ascii_digit() {
            return Err(ValidationError::InvalidName {
                name: name.to_string(),
                reason: format!(
                    "character '{}' at position {} is not allowed - use lowercase letters, numbers, or hyphens only",
                    c, i
                ),
            });
        }
    }

    Ok(())
}

/// Validate description: 1-1024 characters
pub fn validate_description(desc: &str) -> Result<(), ValidationError> {
    if desc.is_empty() {
        return Err(ValidationError::InvalidDescription(
            "description cannot be empty".to_string(),
        ));
    }

    if desc.len() > 1024 {
        return Err(ValidationError::InvalidDescription(
            "description must be 1024 characters or less".to_string(),
        ));
    }

    Ok(())
}

/// Parse YAML frontmatter from SKILL.md content
/// Returns (frontmatter_yaml, body_markdown)
fn parse_frontmatter(content: &str) -> Result<(String, String)> {
    let content = content.trim_start();

    if !content.starts_with("---") {
        return Err(anyhow!("Missing YAML frontmatter delimiter '---'"));
    }

    // Find the closing ---
    let after_first_dash = &content[3..];
    let end_pos = after_first_dash
        .find("---")
        .ok_or_else(|| anyhow!("Missing closing '---' for YAML frontmatter"))?;

    let yaml_str = after_first_dash[..end_pos].trim();
    let body = after_first_dash[end_pos + 3..].trim_start();

    Ok((yaml_str.to_string(), body.to_string()))
}

/// Parse a SKILL.md file and return a Skill struct
pub fn parse_skill_file(skill_dir: &Path) -> Result<Skill> {
    let skill_md_path = skill_dir.join("SKILL.md");

    if !skill_md_path.exists() {
        return Err(anyhow!("SKILL.md not found in {}", skill_dir.display()));
    }

    let content = fs::read_to_string(&skill_md_path)?;
    parse_skill_content(skill_dir, &content, &skill_md_path)
}

/// Parse SKILL.md content and return a Skill struct
/// Note: This does NOT load the body - the body will be loaded by the model when required
pub fn parse_skill_content(skill_dir: &Path, content: &str, skill_md_path: &Path) -> Result<Skill> {
    let (yaml_str, _body) = parse_frontmatter(content)?;

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(&yaml_str)
        .map_err(|e| anyhow!("Failed to parse YAML frontmatter: {}", e))?;

    // Validate required fields
    validate_skill_name(&frontmatter.name)?;
    validate_description(&frontmatter.description)?;

    // Build scripts, references, and assets maps
    let scripts = index_directory(skill_dir, "scripts");
    let references = index_directory(skill_dir, "references");
    let assets = index_directory(skill_dir, "assets");

    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        license: frontmatter.license,
        compatibility: frontmatter.compatibility,
        allowed_tools: frontmatter.allowed_tools,
        skill_md: skill_md_path.to_path_buf(),
        scripts,
        references,
        assets,
        path: skill_dir.to_path_buf(),
    })
}

/// Index all files in a subdirectory, returning a HashMap of filename -> full path
fn index_directory(skill_dir: &Path, subdir: &str) -> HashMap<String, PathBuf> {
    let mut result = HashMap::new();

    let subdir_path = skill_dir.join(subdir);
    if !subdir_path.is_dir() {
        return result;
    }

    if let Ok(entries) = fs::read_dir(&subdir_path) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
            {
                result.insert(name.to_string(), entry.path());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_temp_skill(name: &str, content: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(name);
        fs::create_dir_all(&skill_dir).unwrap();

        let skill_md = skill_dir.join("SKILL.md");
        fs::write(&skill_md, content).unwrap();

        tmp
    }

    #[test]
    fn test_parse_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
---
# Body content
"#;
        let (yaml, body) = parse_frontmatter(content).unwrap();
        assert!(yaml.contains("name: test-skill"));
        assert!(body.contains("Body content"));
    }

    #[test]
    fn test_validate_skill_name_valid() {
        assert!(validate_skill_name("test-skill").is_ok());
        assert!(validate_skill_name("skill123").is_ok());
        assert!(validate_skill_name("a").is_ok());
        assert!(validate_skill_name("test-skill-123").is_ok());
    }

    #[test]
    fn test_validate_skill_name_invalid() {
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("Test-Skill").is_err()); // uppercase
        assert!(validate_skill_name("-test").is_err()); // starts with hyphen
        assert!(validate_skill_name("test-").is_err()); // ends with hyphen
        assert!(validate_skill_name("test skill").is_err()); // space
    }

    #[test]
    fn test_validate_description() {
        assert!(validate_description("A valid description").is_ok());
        assert!(validate_description("").is_err());
    }

    #[test]
    fn test_parse_full_skill() {
        let tmp = create_temp_skill(
            "my-skill",
            r#"---
name: my-skill
description: A test skill for unit testing
license: MIT
---
# My Skill

This is the body content.
"#,
        );

        let skill = parse_skill_file(tmp.path().join("my-skill").as_path()).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "A test skill for unit testing");
        assert_eq!(skill.license, Some("MIT".to_string()));
        assert!(
            skill
                .skill_md
                .to_string_lossy()
                .contains("my-skill/SKILL.md")
        );
    }
}
