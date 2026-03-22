//! UI Abstraction Layer
//!
//! This module provides a clean abstraction between the core PeakBot logic
//! and various UI implementations (TUI, Web, Mobile, REPL).
//!
//! Architecture:
//! ```
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                         UI Layer                                │
//! ├─────────────────┬─────────────────┬─────────────────────────────┤
//! │   TUI           │   Web UI        │   Mobile UI                 │
//! │   (ratatui)     │   (Axum/WS)     │   (Flutter/RN)              │
//! ├─────────────────┴─────────────────┴─────────────────────────────┤
//! │                    UI Abstraction Layer                        │
//! │              (ui_trait.rs, app_state.rs)                       │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                      State Layer                                │
//! │         (AppState, state updates via channels)                 │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                     Core Logic Layer                           │
//! │   AgentRunner, Tools, Providers, ContextManager, etc.          │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

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
