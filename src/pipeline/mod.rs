//! Multi-agent pipeline module for PeakBot.
//!
//! This module enables multiple agents to work together in pipelines,
//! with an entrypoint agent that can delegate tasks to specialized sub-agents.

mod delegate_tool;
mod handoff;
pub(crate) mod registry;
mod set;
mod sub_agent_messages;

pub use delegate_tool::{DelegateTool, SubAgentDeps};
pub use registry::SubAgentRegistry;
// `PipelineSet` is built once at boot (`main.rs`) and carried by
// `SessionDeps` / `RebuildContext`; the re-exports let callers say
// `crate::pipeline::PipelineSet` without reaching into the leaf module.
pub use set::{PipelineInfo, PipelineSet, PipelineSetError, ResolvedPipeline};
