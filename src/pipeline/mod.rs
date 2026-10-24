//! Agents and subagents: the `delegate` tool lets an agent hand one task at
//! a time to one of its subagents (`ResolvedAgent.subagents`).

mod delegate_tool;
mod handoff;
mod sub_agent_messages;

pub use delegate_tool::{DelegateTool, SubAgentDeps};
// Re-exported for tests/old_behaviour_snapshots.rs (pre-0.24 capture); hidden from docs.
#[doc(hidden)]
pub use delegate_tool::build_sub_agent_preamble;
