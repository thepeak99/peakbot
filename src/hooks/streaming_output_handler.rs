//! Streaming output handler for real-time agent message display.
//!
//! This handler prints the agent's thinking and messages as they happen,
//! providing visibility into the agent's reasoning process.
//!
//! Features:
//! - Timestamps on all output lines
//! - Bright colors optimized for dark terminals
//! - Clear state indicators (THINKING vs TALKING)

use crate::hooks::{AgentEvent, EventHandler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

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

/// Text color options for streaming output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextColor {
    #[default]
    BrightWhite,
    White,
    Yellow,  // For high contrast
}

impl TextColor {
    fn ansi_code(&self) -> &'static str {
        match self {
            TextColor::BrightWhite => COLOR_BRIGHT_WHITE,
            TextColor::White => COLOR_WHITE,
            TextColor::Yellow => COLOR_BRIGHT_YELLOW,
        }
    }
}

/// Configuration for streaming output
#[derive(Debug, Clone)]
pub struct StreamingConfig {
    /// Whether to show timestamps on output lines
    pub show_timestamps: bool,
    /// Whether to show state headers (THINKING/TALKING)
    pub show_state_headers: bool,
    /// Whether to use color output
    pub use_color: bool,
    /// Text color for main content
    pub text_color: TextColor,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            show_timestamps: true,
            show_state_headers: true,
            use_color: true,
            text_color: TextColor::BrightWhite,
        }
    }
}

impl StreamingConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        let show_timestamps = std::env::var("PEAKBOT_STREAM_TIMESTAMPS")
            .map(|v| v.parse::<bool>().unwrap_or(true))
            .unwrap_or(true);
        
        let show_state_headers = std::env::var("PEAKBOT_STREAM_HEADERS")
            .map(|v| v.parse::<bool>().unwrap_or(true))
            .unwrap_or(true);
        
        let use_color = std::env::var("PEAKBOT_STREAM_COLOR")
            .map(|v| v.parse::<bool>().unwrap_or(true))
            .unwrap_or(true);
        
        let text_color = std::env::var("PEAKBOT_STREAM_TEXT_COLOR")
            .map(|v| match v.to_lowercase().as_str() {
                "yellow" => TextColor::Yellow,
                "white" => TextColor::White,
                _ => TextColor::BrightWhite,
            })
            .unwrap_or(TextColor::BrightWhite);
        
        Self {
            show_timestamps,
            show_state_headers,
            use_color,
            text_color,
        }
    }
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
    /// Streaming configuration
    config: StreamingConfig,
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
            config: StreamingConfig::default(),
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

    /// Create a new handler with custom config
    pub fn with_config(config: StreamingConfig) -> Self {
        Self {
            config,
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
                            if self.config.show_state_headers {
                                print_thinking_header();
                            }
                            for line in reason.lines() {
                                if !line.trim().is_empty() {
                                    print_thinking_line(line, &self.config);
                                }
                            }
                        }
                    }
                }

                // Print content if non-empty
                if !content.trim().is_empty() {
                    // Check if this looks like a tool call announcement
                    if content.contains("I'll use") || content.contains("I will use") {
                        if self.config.show_state_headers {
                            print_talking_header();
                        }
                        for line in content.lines() {
                            if !line.trim().is_empty() {
                                print_talking_line(line, &self.config);
                            }
                        }
                    } else {
                        // Regular content - print with timestamps but no header
                        for line in content.lines() {
                            if !line.trim().is_empty() {
                                print_talking_line(line, &self.config);
                            }
                        }
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

                    print_tool_header(tool_name, &self.config);

                    // Show arguments in a formatted way (truncated if too long)
                    if self.verbosity == VerbosityLevel::Verbose {
                        let args = truncate_string(arguments, 200);
                        for line in args.lines() {
                            print_tool_arg_line(line, &self.config);
                        }
                    } else {
                        // Show a brief summary of arguments
                        if let Some(summary) = summarize_args(tool_name, arguments) {
                            print_tool_arg_line(&summary, &self.config);
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
                    print_tool_complete(tool_name, *success, &self.config);
                    
                    // Show summarized result in verbose mode
                    if self.verbosity == VerbosityLevel::Verbose {
                        let summary = summarize_result(tool_name, result);
                        let indented_summary = indent_text(&summary, 4);
                        let ts = format_timestamp_with_color(&self.config);
                        println!("{}    {}↪ {}{}", ts, COLOR_WHITE, indented_summary, COLOR_RESET);
                    }
                }
            }

            AgentEvent::SessionStart { model, .. } => {
                let ts = format_timestamp_with_color(&self.config);
                println!(
                    "\n{}{}🚀 Session started with model:{} {}{}",
                    ts,
                    if self.config.use_color { COLOR_BRIGHT_GREEN } else { "" },
                    if self.config.use_color { COLOR_RESET } else { "" },
                    model,
                    if self.config.use_color { COLOR_RESET } else { "" }
                );
            }

            AgentEvent::SessionEnd {
                total_tokens,
                total_cost,
                ..
            } => {
                let ts = format_timestamp_with_color(&self.config);
                println!(
                    "\n{}{}🏁 Session ended:{} {} tokens, ${:.4}{}",
                    ts,
                    if self.config.use_color { COLOR_BRIGHT_GREEN } else { "" },
                    if self.config.use_color { COLOR_RESET } else { "" },
                    total_tokens,
                    total_cost,
                    if self.config.use_color { COLOR_RESET } else { "" }
                );
            }
        }
    }

    fn name(&self) -> &str {
        "StreamingOutputHandler"
    }
}

// ============================================================================
// ANSI Color Codes (Optimized for Dark Terminal)
// ============================================================================

const COLOR_RESET:      &str = "\x1b[0m";   // Reset
const COLOR_BRIGHT_CYAN:&str = "\x1b[96m";  // Timestamps
const COLOR_BRIGHT_YELLOW:&str = "\x1b[93m"; // Thinking header
const COLOR_BRIGHT_GREEN: &str = "\x1b[92m"; // Talking header, success
const COLOR_BRIGHT_BLUE:  &str = "\x1b[94m"; // Tool header
const COLOR_BRIGHT_WHITE: &str = "\x1b[97m"; // Main text (was gray/dim)
const COLOR_WHITE:        &str = "\x1b[37m"; // Indicators/arrows
const COLOR_BRIGHT_RED:   &str = "\x1b[91m"; // Errors

// ============================================================================
// Output Helper Functions
// ============================================================================

/// Generate current timestamp in [HH:MM:SS] format
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let hours = (now.as_secs() / 3600) % 24;
    let mins = (now.as_secs() / 60) % 60;
    let secs = now.as_secs() % 60;
    format!("[{}:{}:{}]", hours, mins, secs)
}

/// Format timestamp with cyan color based on config
fn format_timestamp_with_color(config: &StreamingConfig) -> String {
    if config.show_timestamps {
        if config.use_color {
            format!("{}{}{}", COLOR_BRIGHT_CYAN, timestamp(), COLOR_RESET)
        } else {
            timestamp()
        }
    } else {
        String::new()
    }
}

/// Print thinking header with timestamp
fn print_thinking_header() {
    println!("\n{}{} 🤔 THINKING{}", timestamp(), COLOR_BRIGHT_YELLOW, COLOR_RESET);
}

/// Print a line of thinking content
fn print_thinking_line(line: &str, config: &StreamingConfig) {
    let ts = format_timestamp_with_color(config);
    let text_color = if config.use_color { config.text_color.ansi_code() } else { "" };
    let reset = if config.use_color { COLOR_RESET } else { "" };
    println!("{} {}{}{}", ts, text_color, line, reset);
}

/// Print talking header with timestamp
fn print_talking_header() {
    println!("{}{} 💬 TALKING{}", timestamp(), COLOR_BRIGHT_GREEN, COLOR_RESET);
}

/// Print a line of talking content
fn print_talking_line(line: &str, config: &StreamingConfig) {
    let ts = format_timestamp_with_color(config);
    let text_color = if config.use_color { config.text_color.ansi_code() } else { "" };
    let reset = if config.use_color { COLOR_RESET } else { "" };
    println!("{} {}{}{}", ts, text_color, line, reset);
}

/// Print tool call header
fn print_tool_header(tool_name: &str, config: &StreamingConfig) {
    let ts = format_timestamp_with_color(config);
    println!("\n{}{} 🔧 TOOL: {}{}", ts, COLOR_BRIGHT_BLUE, tool_name, COLOR_RESET);
}

/// Print tool argument line
fn print_tool_arg_line(args: &str, config: &StreamingConfig) {
    let ts = format_timestamp_with_color(config);
    println!("{}    {}↪ {}{}", ts, COLOR_WHITE, args, COLOR_RESET);
}

/// Print tool complete message
fn print_tool_complete(tool_name: &str, is_error: bool, config: &StreamingConfig) {
    let ts = format_timestamp_with_color(config);
    if is_error {
        println!("{}{} ❌ TOOL ERROR: {}{}", ts, COLOR_BRIGHT_RED, tool_name, COLOR_RESET);
    } else {
        println!("{}{} ✅ TOOL COMPLETE: {}{}", ts, COLOR_BRIGHT_GREEN, tool_name, COLOR_RESET);
    }
}

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
        assert_eq!(result, "This is a longer tex...");
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

    #[test]
    fn test_timestamp_format() {
        let ts = timestamp();
        assert!(ts.starts_with("["));
        assert!(ts.ends_with("]"));
        assert!(ts.len() >= 8 && ts.len() <= 10); // [H:MM:SS] to [HH:MM:SS]
    }

    #[test]
    fn test_streaming_config_from_env() {
        let config = StreamingConfig::from_env();
        assert!(config.show_timestamps); // default
        assert!(config.show_state_headers); // default
        assert!(config.use_color); // default
    }
}
