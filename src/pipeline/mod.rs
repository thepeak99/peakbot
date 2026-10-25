//! Agents and subagents: the `delegate` tool lets an agent hand one task at
//! a time to one of its subagents (`ResolvedAgent.subagents`).

mod delegate_tool;
mod handoff;
mod sub_agent_messages;

pub use delegate_tool::{DelegateTool, SubAgentDeps};
