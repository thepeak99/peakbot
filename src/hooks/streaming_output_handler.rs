//! Streaming output handler for real-time agent message display.
//!
//! This handler prints the agent's thinking and messages as they happen,
//! providing visibility into the agent's reasoning process.

use crate::hooks::{AgentEvent, EventHandler};
use std::sync::atomic::{AtomicBool, Ordering};

/// Verbosity level for streaming output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerbosityLevel {
    /// Only show final output (no thinking, no tool calls)
    Quiet,
    /// Show thinking, messages, and tool calls (default)
    #[default]
    Normal,
    /// Show everything including tool results
    Verbose,
}

/// Handler that streams agent output to the console in real-time
#[derive(Debug)]
pub struct StreamingOutputHandler {
    /// Verbosity level controlling what to display
    verbosity: VerbosityLevel,
    /// Whether to show thinking/reasoning blocks
    show_thinking: bool,
    /// Whether to show tool call details
    show_tool_calls: bool,
    /// Whether to show tool results
    show_tool_results: bool,
    /// Flag to prevent duplicate prints (for tracking if we're in a tool call sequence)
    in_tool_sequence: AtomicBool,
}

impl StreamingOutputHandler {
    /// Create a new streaming output handler with default settings
    pub fn new() -> Self {
        Self {
            verbosity: VerbosityLevel::Normal,
            show_thinking: true,
            show_tool_calls: true,
            show_tool_results: false,
            in_tool_sequence: AtomicBool::new(false),
        }
    }

    /// Create a new handler with custom verbosity
    pub fn with_verbosity(verbosity: VerbosityLevel) -> Self {
        Self {
            verbosity,
            ..Self::new()
        }
    }

    /// Configure whether to show thinking blocks
    pub fn show_thinking(mut self, show: bool) -> Self {
        self.show_thinking = show;
        self
    }

    /// Configure whether to show tool calls
    pub fn show_tool_calls(mut self, show: bool) -> Self {
        self.show_tool_calls = show;
        self
    }

    /// Configure whether to show tool results
    pub fn show_tool_results(mut self, show: bool) -> Self {
        self.show_tool_results = show;
        self
    }
}

impl Default for StreamingOutputHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHandler for StreamingOutputHandler {
    fn handle_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::CompletionRequest { .. } => {
                // Could show a "thinking..." indicator here if desired
                // For now, we'll wait for the response to avoid flicker
            }

            AgentEvent::CompletionResponse {
                content,
                reasoning,
                ..
            } => {
                // Print reasoning/thinking if present and enabled
                if self.show_thinking {
                    if let Some(reason) = reasoning {
                        if !reason.trim().is_empty() {
                            println!("\n{}💭 Thinking:{}{}", COLOR_DIM, COLOR_RESET, COLOR_BOLD);
                            println!("{}", reason);
                            println!("{}", COLOR_RESET);
                        }
                    }
                }

                // Print content if non-empty
                if !content.trim().is_empty() {
                    // Check if this looks like a tool call announcement
                    if content.contains("I'll use") || content.contains("I will use") {
                        println!("{}📝 Agent:{} {}", COLOR_DIM, COLOR_RESET, content);
                    } else {
                        println!("{}", content);
                    }
                }
            }

            AgentEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                if self.show_tool_calls {
                    // Mark that we're in a tool sequence
                    self.in_tool_sequence.store(true, Ordering::SeqCst);

                    println!("\n{}🔧 Calling tool:{} {}", COLOR_CYAN, COLOR_RESET, tool_name);

                    // Show arguments in a formatted way (truncated if too long)
                    if self.verbosity == VerbosityLevel::Verbose {
                        let args = truncate_string(arguments, 200);
                        println!(
                            "   {}↪{} {}",
                            COLOR_DIM,
                            COLOR_RESET,
                            indent_text(&args, 2)
                        );
                    } else {
                        // Show a brief summary of arguments
                        if let Some(summary) = summarize_args(tool_name, arguments) {
                            println!("   {}↪{} {}", COLOR_DIM, COLOR_RESET, summary);
                        }
                    }
                }
            }

            AgentEvent::ToolResult {
                tool_name,
                result,
                success,
                ..
            } => {
                if self.show_tool_results || self.verbosity == VerbosityLevel::Verbose {
                    let icon = if *success { "✅" } else { "❌" };
                    let color = if *success { COLOR_GREEN } else { COLOR_RED };

                    println!(
                        "\n{}{} Tool completed:{} {}",
                        COLOR_DIM, icon, COLOR_RESET, tool_name
                    );

                    // Show result summary or error
                    if !*success {
                        let error_msg = truncate_string(result, 150);
                        println!(
                            "   {}{}{} {}",
                            color, "Error:", COLOR_RESET, indent_text(&error_msg, 2)
                        );
                    } else if self.verbosity == VerbosityLevel::Verbose {
                        let summary = summarize_result(tool_name, result);
                        println!("   {}↻{} {}", COLOR_DIM, COLOR_RESET, summary);
                    }
                } else if self.show_tool_calls && *success {
                    // Just a quick acknowledgment in normal mode
                    println!(
                        "   {}✅ Tool completed{}{}",
                        COLOR_GREEN, COLOR_RESET, COLOR_DIM
                    );
                }

                // Mark that we're done with the tool sequence
                self.in_tool_sequence.store(false, Ordering::SeqCst);
            }

            AgentEvent::SessionStart { model, .. } => {
                println!(
                    "\n{}🚀 Session started with model:{} {}{}",
                    COLOR_GREEN, COLOR_RESET, model, COLOR_RESET
                );
            }

            AgentEvent::SessionEnd {
                total_tokens,
                total_cost,
                ..
            } => {
                println!(
                    "\n{}🏁 Session ended:{} {} tokens, ${:.4}{}",
                    COLOR_GREEN,
                    COLOR_RESET,
                    total_tokens,
                    total_cost,
                    COLOR_RESET
                );
            }
        }
    }

    fn name(&self) -> &str {
        "StreamingOutputHandler"
    }
}

// ============================================================================
// ANSI Color Codes
// ============================================================================

const COLOR_RESET: &str = "\x1b[0m";
const COLOR_GREEN: &str = "\x1b[32m";
const COLOR_RED: &str = "\x1b[31m";
const COLOR_CYAN: &str = "\x1b[36m";
const COLOR_DIM: &str = "\x1b[90m";
const COLOR_BOLD: &str = "\x1b[1m";

// ============================================================================
// Helper Functions
// ============================================================================

/// Truncate a string to max_length, adding "..." if truncated
fn truncate_string(s: &str, max_length: usize) -> String {
    if s.len() <= max_length {
        s.to_string()
    } else {
        format!("{}...", &s[..max_length.min(s.len())])
    }
}

/// Indent each line of text by the specified number of spaces
fn indent_text(text: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{}{}", indent, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Summarize tool arguments for display
fn summarize_args(tool_name: &str, arguments: &str) -> Option<String> {
    match tool_name {
        "file_read" => {
            // Extract path and line range
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let path = json.get("path").and_then(|v| v.as_str()).unwrap_or("file");
                let start = json.get("start_line").and_then(|v| v.as_u64());
                let end = json.get("end_line").and_then(|v| v.as_u64());

                match (start, end) {
                    (Some(s), Some(e)) => Some(format!("Read lines {}-{} from: {}", s, e, path)),
                    (Some(s), None) => Some(format!("Read from line {} in: {}", s, path)),
                    (None, Some(e)) => Some(format!("Read up to line {} from: {}", e, path)),
                    _ => Some(format!("Read: {}", path)),
                }
            } else {
                None
            }
        }
        "file_edit" => {
            // Extract operation and path
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let command = json.get("command").and_then(|v| v.as_str()).unwrap_or("edit");
                let path = json.get("path").and_then(|v| v.as_str()).unwrap_or("file");
                Some(format!("{}: {}", command, path))
            } else {
                None
            }
        }
        "list_directory" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let path = json.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let recursive = json.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);
                if recursive {
                    Some(format!("List (recursive): {}", path))
                } else {
                    Some(format!("List: {}", path))
                }
            } else {
                None
            }
        }
        "bash" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let command = json.get("command").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("Run: {}", truncate_string(command, 50)))
            } else {
                None
            }
        }
        "fetch_url" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("URL");
                Some(format!("Fetch: {}", truncate_string(url, 60)))
            } else {
                None
            }
        }
        "web_search" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let query = json.get("query").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("Search: {}", truncate_string(query, 50)))
            } else {
                None
            }
        }
        "think" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let thought = json.get("thought").and_then(|v| v.as_str()).unwrap_or("");
                Some(format!("Reasoning about: {}", truncate_string(thought, 50)))
            } else {
                None
            }
        }
        "todo" => {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(arguments) {
                let action = json.get("action").and_then(|v| v.as_str()).unwrap_or("list");
                Some(format!("Todo: {}", action))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Summarize tool results for display
fn summarize_result(tool_name: &str, result: &str) -> String {
    match tool_name {
        "file_read" => {
            // Count lines and show a preview
            let line_count = result.lines().count();
            let preview = truncate_string(&result.lines().take(3).collect::<Vec<_>>().join("\n"), 100);
            format!("Read {} lines\n{}", line_count, preview)
        }
        "list_directory" => {
            // Count items
            let item_count = result.lines().count();
            format!("Found {} items", item_count)
        }
        "bash" => {
            // Show first few lines of output
            let line_count = result.lines().count();
            if line_count > 5 {
                format!(
                    "Output: {} lines (showing first 5)\n{}",
                    line_count,
                    result.lines().take(5).collect::<Vec<_>>().join("\n")
                )
            } else {
                format!("Output:\n{}", result)
            }
        }
        "fetch_url" => {
            let byte_count = result.len();
            format!("Fetched {} bytes", byte_count)
        }
        _ => truncate_string(result, 200),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_string_no_truncation() {
        let input = "Short text";
        assert_eq!(truncate_string(input, 100), "Short text");
    }

    #[test]
    fn test_truncate_string_with_truncation() {
        let input = "This is a longer text that should be truncated";
        let result = truncate_string(input, 20);
        assert_eq!(result, "This is a longer te...");
    }

    #[test]
    fn test_indent_text() {
        let input = "line1\nline2\nline3";
        let result = indent_text(input, 4);
        assert_eq!(result, "    line1\n    line2\n    line3");
    }

    #[test]
    fn test_summarize_args_file_read() {
        let args = r#"{"path": "/test/file.txt", "start_line": 1, "end_line": 10}"#;
        let summary = summarize_args("file_read", args);
        assert_eq!(summary, Some("Read lines 1-10 from: /test/file.txt".to_string()));
    }

    #[test]
    fn test_summarize_args_bash() {
        let args = r#"{"command": "ls -la /home"}"#;
        let summary = summarize_args("bash", args);
        assert_eq!(summary, Some("Run: ls -la /home".to_string()));
    }
}
