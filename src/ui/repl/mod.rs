//! REPL UI Implementation
//!
//! This module wraps the existing REPL as a Ui trait implementation
//! for backward compatibility.

pub mod repl_impl;

pub use repl_impl::ReplUi;
