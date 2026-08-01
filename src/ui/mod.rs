//! UI Abstraction Layer
//!
//! This module provides a clean abstraction between the core PeakBot logic
//! and various UI implementations (REPL).
//!
//! ## MVC Architecture
//!
//! - **Model** (`StateManager` in `crate::state`): single source of truth for state.
//! - **View** (`Ui` impls): read state from Model, render to screen, send user input to Controller.
//! - **Controller** (`AgentRunner`): receive input from View, call agent, write to Model.
//!
//! Data flows one way only:
//!   View ──UiAction──► Controller ──writes──► Model ──broadcasts──► View

pub mod app_state;
pub mod emoji_normalize;
pub mod ui_trait;

// Shared duplex wire protocol (stdio + web Views).
pub mod wire;

// REPL UI
pub mod repl;

// NDJSON stdio UI (`peakbot --stdio`)
pub mod stdio;

// Web UI (default `peakbot` mode) — embedded shell + live WebSocket. See module docs.
pub mod web;

// Re-export commonly used types
pub use app_state::*;
pub use ui_trait::*;

// Re-export UI implementations
pub use repl::*;
pub use stdio::StdioUi;
pub use web::{DEFAULT_WEB_ADDR, WebUi};
pub use wire::build_models_snapshot;
