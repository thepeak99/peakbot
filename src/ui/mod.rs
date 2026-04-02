//! UI Abstraction Layer
//!
//! This module provides a clean abstraction between the core PeakBot logic
//! and various UI implementations (TUI, Web, Mobile, REPL).
//!
//! ## MVC Architecture
//!
//! - **Model** (`StateManager`): single source of truth for UI state. Broadcasts to subscribers.
//! - **View** (`Ui` impls): read state from Model, render to screen, send user input to Controller.
//! - **Controller** (`AgentRunner`): receive input from View, call agent, write to Model.
//!
//! Data flows one way only:
//!   View ──UiAction──► Controller ──writes──► Model ──broadcasts──► View

pub mod app_state;
pub mod state_manager;
pub mod ui_trait;

// REPL UI (always available for backward compatibility)
pub mod repl;

// TUI (feature-gated)
#[cfg(feature = "tui")]
pub mod tui;

// Re-export commonly used types
pub use app_state::*;
pub use state_manager::*;
pub use ui_trait::*;

// Re-export UI implementations
pub use repl::*;

#[cfg(feature = "tui")]
pub use tui::*;
