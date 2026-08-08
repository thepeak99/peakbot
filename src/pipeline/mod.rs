//! Multi-agent pipeline module for PeakBot.
//!
//! This module enables multiple agents to work together in pipelines,
//! with an entrypoint agent that can delegate tasks to specialized sub-agents.

mod delegate_tool;
mod handoff;
pub(crate) mod registry;
mod set;
mod sub_agent_messages;

use crate::hooks::SessionHook;
use std::sync::{Arc, Mutex};

pub use delegate_tool::{DelegateTool, SubAgentDeps, fire_stop};
pub use registry::SubAgentRegistry;
// Stage 1.1: `set` is now a real module. Production types live here; the
// test module at the bottom of `set.rs` is gated on `#[cfg(test)]`.
// Re-exports let `crate::pipeline::PipelineSet::build(...)` be called
// from `main.rs` / `session.rs` / wizard / tests without reaching into
// the leaf module.
//
// The `#[allow(unused_imports)]` is the bridge across the stage 1.1 /
// 1.2 boundary: no live caller yet in this commit (runtime wiring
// lands in commit 2/3), but the public surface is already part of the
// crate's API and the test module references it via `crate::pipeline`
// paths.
#[allow(unused_imports)]
pub use set::{PipelineInfo, PipelineSet, PipelineSetError, ResolvedPipeline};

/// The sub-agent hook that is *currently* running inside a `delegate` call,
/// if any. Owned by the session; set by [`DelegateTool`] for the duration of
/// a delegation and cleared when it returns. `/stop` fires `request_stop` on
/// this hook (in addition to the orchestrator's) so a stop lands on the
/// innermost running agent — the whole turn then unwinds out (D6).
pub type ActiveSubAgentHook = Arc<Mutex<Option<Arc<SessionHook>>>>;
