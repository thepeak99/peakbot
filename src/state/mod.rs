//! State module
//!
//! Contains application state management. StateManager is the single source of truth
//! for all application state (stats, todo, chat, etc.).

pub mod state_manager;

// Re-export StateManager for convenience
pub use state_manager::{StateManager, StdinNotActive};
