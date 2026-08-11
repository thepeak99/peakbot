//! State module
//!
//! Contains application state management. StateManager is the single source of truth
//! for all application state (stats, todo, chat, etc.).

pub mod state_manager;

// Re-export StateManager for convenience.
// #183: `StopTally` is the snapshot type `stop_turn_processes` returns; it
// needs to be reachable from `lib.rs::stop_message` (which is module-private
// but its tests in `mod tests` reference it via `crate::state::StopTally`).
pub use state_manager::{StateManager, StdinNotActive, StopTally};
