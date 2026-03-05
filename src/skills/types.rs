//! Skill data types matching the Agent Skills specification

use std::collections::HashMap;
use std::path::PathBuf;

/// The main Skill structure representing a loaded skill package
///
/// The fields name, description, license, compatibility, and allowed_tools
#[derive(Debug, Clone)]
pub struct Skill {
    /// Unique identifier for the skill (lowercase, hyphens allowed)
    pub name: String,
    /// Human-readable description of what the skill does
    pub description: String,
    /// Optional license string (e.g., "MIT", "Apache-2.0")
    pub license: Option<String>,
    /// Optional compatibility string (e.g., "PeakBot v0.1+")
    pub compatibility: Option<String>,
    /// Optional space-delimited list of allowed tools
    pub allowed_tools: Option<String>,
    /// The path to the SKILL.md file
    pub skill_md: PathBuf,
    /// Paths to scripts in the skill's scripts directory, indexed by name
    pub scripts: HashMap<String, PathBuf>,
    /// Paths to references in the skill's references directory, indexed by name
    pub references: HashMap<String, PathBuf>,
    /// Paths to assets in the skill's assets directory, indexed by name
    pub assets: HashMap<String, PathBuf>,
    /// Path to the skill directory (set during discovery)
    pub path: PathBuf,
}
