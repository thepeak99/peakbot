//! REPL UI Implementation
//!
//! This module wraps the existing REPL as a Ui trait implementation (View in MVC).

pub mod command_popup;
pub mod confirm_dialog;
pub mod markdown;
pub mod message_renderer;
pub mod render_cache;
pub mod repl_impl;
pub mod spinner;
pub mod todo_panel;

pub use confirm_dialog::{ConfirmAction, ConfirmDialog, render_confirm_dialog};
pub use markdown::MarkdownRenderer;
pub use message_renderer::{MessageRenderer, PlainRenderer};
pub use render_cache::{ChatRenderCache, WindowView};
pub use repl_impl::{ReplUi, UiState};
