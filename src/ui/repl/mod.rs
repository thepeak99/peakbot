//! REPL UI Implementation
//!
//! This module wraps the existing REPL as a Ui trait implementation (View in MVC).

pub mod repl_impl;
pub mod todo_panel;

pub use repl_impl::{ReplUi, UiState};
