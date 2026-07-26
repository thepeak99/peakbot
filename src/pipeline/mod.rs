//! Multi-agent pipeline module for PeakBot.
//!
//! This module enables multiple agents to work together in pipelines,
//! with an entrypoint agent that can delegate tasks to specialized sub-agents.

mod delegate_tool;
mod handoff;
pub(crate) mod registry;

use crate::hooks::SessionHook;
use std::sync::{Arc, Mutex};

pub use delegate_tool::{DelegateTool, SubAgentDeps, fire_stop};
pub use registry::SubAgentRegistry;

/// The sub-agent hook that is *currently* running inside a `delegate` call,
/// if any. Owned by the session; set by [`DelegateTool`] for the duration of
/// a delegation and cleared when it returns. `/stop` fires `request_stop` on
/// this hook (in addition to the orchestrator's) so a stop lands on the
/// innermost running agent — the whole turn then unwinds out (D6).
pub type ActiveSubAgentHook = Arc<Mutex<Option<Arc<SessionHook>>>>;
