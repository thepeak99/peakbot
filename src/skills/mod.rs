//! Skills module for PeakBot - Agent Skills support
//!
//! This module implements the Agent Skills specification for loading and managing
//! skill packages from the filesystem.

pub mod discovery;
pub mod parser;
pub mod types;

pub use discovery::{SkillRegistry, load_default_skills};
