//! TUI Implementation using ratatui
//!
//! This module provides a Terminal User Interface implementation for PeakBot
//! using the ratatui library. It implements the `Ui` trait to provide a
//! consistent interface with other UI backends (Web, Mobile, REPL).

pub mod input_handler;
pub mod renderer;
pub mod runner;
pub mod tui_impl;

pub use runner::{TuiAgentRunner, RunnerEvent};
pub use tui_impl::Tui;
