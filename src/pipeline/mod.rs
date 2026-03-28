//! Multi-agent pipeline module for PeakBot.
//!
//! This module enables multiple agents to work together in pipelines,
//! with an entrypoint agent that can delegate tasks to specialized sub-agents.

mod delegate_tool;
mod registry;

pub use delegate_tool::DelegateTool;
pub use registry::SubAgentRegistry;
