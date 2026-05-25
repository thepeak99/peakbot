//! REPL UI Tests using TestBackend with insta snapshots
//!
//! These tests use Ratatui's TestBackend to render widgets and store
//! snapshots for regression testing.

mod snapshot_helpers;

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        layout::{Constraint, Direction, Layout},
    };

    use super::snapshot_helpers::*;
    use peakbot::ui::app_state::{AppState, ChatMessage, ChatState, MessageRole};
    use peakbot::ui::repl::ReplUi;

    // === Input Area Tests ===

    #[test]
    fn input_area_empty() {
        let paragraph = ReplUi::build_input_paragraph("", 0, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_empty", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_start() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 0, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_start", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_middle() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 2, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_middle", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_end() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 5, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_end", lines.join("\n"));
    }

    #[test]
    fn input_area_long_text() {
        let paragraph = ReplUi::build_input_paragraph(
            "This is a very long input that will wrap to multiple lines",
            0,
            false,
            None,
            None,
            0,
            0,
            false,
        );
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_long_text", lines.join("\n"));
    }

    // === Multiline Input Snapshot Tests ===
    //
    // Pin down the contract for multiline editing rendering. See
    // chat 2026-04-24 and `repl_impl.rs::multiline_input_tests`.

    /// Two logical lines with cursor on the second line.
    /// Expected: "> abc" on line 0, "d█ef" on line 1.
    #[test]
    fn input_area_multiline_cursor_on_second_line() {
        let paragraph =
            ReplUi::build_input_paragraph("abc\ndef", 5, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!(
            "input_area_multiline_cursor_on_second_line",
            lines.join("\n")
        );
    }

    /// Cursor on first line of a two-line buffer.
    /// Expected: "> a█bc" on line 0, "def" on line 1.
    #[test]
    fn input_area_multiline_cursor_on_first_line() {
        let paragraph =
            ReplUi::build_input_paragraph("abc\ndef", 1, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!(
            "input_area_multiline_cursor_on_first_line",
            lines.join("\n")
        );
    }

    /// Cursor at the end of the first logical line (right before '\n').
    /// Expected: "> abc█" on line 0, "def" on line 1.
    #[test]
    fn input_area_multiline_cursor_at_end_of_first_line() {
        let paragraph =
            ReplUi::build_input_paragraph("abc\ndef", 3, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!(
            "input_area_multiline_cursor_at_end_of_first_line",
            lines.join("\n")
        );
    }

    /// Buffer ending with a trailing newline; cursor at buffer end = empty
    /// line 1 with lonely cursor.
    /// Expected: "> abc" on line 0, "█" on line 1.
    #[test]
    fn input_area_multiline_cursor_on_empty_trailing_line() {
        let paragraph = ReplUi::build_input_paragraph("abc\n", 4, false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!(
            "input_area_multiline_cursor_on_empty_trailing_line",
            lines.join("\n")
        );
    }

    /// Three lines, cursor in the middle line.
    #[test]
    fn input_area_multiline_three_lines() {
        let paragraph = ReplUi::build_input_paragraph(
            "first\nsecond\nthird",
            8,
            false,
            None,
            None,
            0,
            0,
            false,
        );
        let terminal = render_widget(paragraph, 60, 6);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_multiline_three_lines", lines.join("\n"));
    }

    // === Chat History Tests ===

    #[test]
    fn chat_welcome() {
        let chat = ChatState::new();
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_welcome", lines.join("\n"));
    }

    #[test]
    fn chat_single_user_message() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "Hello".to_string(),
            "2024-01-01 12:00:00",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_user_message", lines.join("\n"));
    }

    #[test]
    fn chat_single_agent_message() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "Hi there!".to_string(),
            "2024-01-01 12:00:00",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_agent_message", lines.join("\n"));
    }

    #[test]
    fn chat_single_user_message_multiline() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "This is a message\nwith a newline\nin it.".to_string(),
            "2024-01-01 12:00:00",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_user_message_multiline", lines.join("\n"));
    }

    #[test]
    fn chat_single_agent_message_multiline() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "Line one\nLine two\nLine three".to_string(),
            "2024-01-01 12:00:00",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_agent_message_multiline", lines.join("\n"));
    }

    /// Regression pin for issue #5: leading and inner whitespace in agent
    /// replies must be preserved, not stripped by Wrap { trim: true }.
    /// YAML is the canonical test case — indentation is semantically meaningful.
    #[test]
    fn chat_agent_yaml_preserves_whitespace() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "```yaml\nname: peakbot\nversion: 0.4.3\nfeatures:\n  - search\n  - files\n    - bash\n```"
                .to_string(),
            "2024-01-01 12:00:00",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 15);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        let joined = lines.join("\n");

        // The "  - " line must start with two spaces before the dash.
        // If trimming is on, leading spaces are stripped and the line starts
        // with the dash directly. This assertion catches that regression.
        assert!(
            joined.contains("  - search"),
            "leading spaces on YAML list items must be preserved"
        );
        assert!(
            joined.contains("    - bash"),
            "deeper indentation (4 spaces) must also be preserved"
        );
    }

    // === Status Bar Tests ===

    #[test]
    fn status_bar_empty() {
        let state = AppState::new();
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_status_bar(f, f.area(), &state);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("status_bar_empty", lines.join("\n"));
    }

    #[test]
    fn status_bar_with_stats() {
        let mut state = AppState::new();
        state.stats.total_input_tokens = 1000;
        state.stats.total_output_tokens = 500;
        state.stats.total_api_calls = 10;
        state.stats.total_cost = 0.025;
        state.stats.model = "claude-3.7-sonnet".to_string();

        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_status_bar(f, f.area(), &state);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("status_bar_with_stats", lines.join("\n"));
    }

    // === State Unit Tests (these don't need snapshots) ===

    #[test]
    fn test_chat_message_roles() {
        let user_msg = ChatMessage::user("test".to_string());
        assert_eq!(user_msg.role, MessageRole::User);

        let agent_msg = ChatMessage::agent("response".to_string());
        assert_eq!(agent_msg.role, MessageRole::Agent);

        let system_msg = ChatMessage::system("system".to_string());
        assert_eq!(system_msg.role, MessageRole::System);
    }

    #[test]
    fn test_chat_state_auto_scroll() {
        let mut chat = ChatState::new();
        assert!(!chat.auto_scroll, "Initial auto_scroll should be false");

        chat.add_message(ChatMessage::user("test".to_string()));
        assert!(
            chat.auto_scroll,
            "auto_scroll should be true after adding message"
        );
    }

    #[test]
    fn test_session_state_formatting() {
        let mut state = AppState::new();
        state.stats.total_input_tokens = 1500;
        state.stats.total_output_tokens = 750;

        assert_eq!(state.stats.total_tokens(), 2250);
        assert_eq!(state.stats.format_tokens(1500), "1.5k");
        assert_eq!(state.stats.format_tokens(1_500_000), "1.5M");
        assert_eq!(state.stats.format_cost(), "0.0000");
    }

    #[test]
    fn test_context_state_percentage() {
        let mut state = AppState::new();
        state.context.current_usage = 50_000;
        state.context.window_size = 200_000;
        state.context.compaction_enabled = true;
        state.context.compaction_threshold = 0.8;

        assert!((state.context.usage_percentage() - 25.0).abs() < f64::EPSILON);
        assert!(!state.context.should_compact());

        state.context.current_usage = 180_000;
        assert!(state.context.should_compact());
    }

    // === Scrolling Snapshot Tests ===

    /// Test chat history at scroll position 0 (top)
    #[test]
    fn chat_scroll_top() {
        let mut chat = ChatState::new();
        for i in 1..=15 {
            chat.add_message(ChatMessage::with_timestamp(
                MessageRole::User,
                format!("Message {}", i),
                "2024-01-01 12:00:00",
            ));
        }
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Scroll position 0 = showing top of content
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_scroll_top", lines.join("\n"));
    }

    /// Test chat history at scroll position 5 (middle)
    #[test]
    fn chat_scroll_middle() {
        let mut chat = ChatState::new();
        for i in 1..=15 {
            chat.add_message(ChatMessage::with_timestamp(
                MessageRole::User,
                format!("Message {}", i),
                "2024-01-01 12:00:00",
            ));
        }
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Scroll position 5 = showing middle of content
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 5, 5, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_scroll_middle", lines.join("\n"));
    }

    /// Test chat history at max scroll position (bottom)
    #[test]
    fn chat_scroll_bottom() {
        let mut chat = ChatState::new();
        for i in 1..=15 {
            chat.add_message(ChatMessage::with_timestamp(
                MessageRole::User,
                format!("Message {}", i),
                "2024-01-01 12:00:00",
            ));
        }
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Max scroll = 15 messages - 8 visible = 7 (show last messages)
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 8, 8, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_scroll_bottom", lines.join("\n"));
    }

    /// Test chat history with mixed message roles at scroll position 0
    #[test]
    fn chat_mixed_roles_scroll() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "Hello, how are you?".to_string(),
            "2024-01-01 12:00:00",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "I'm doing well, thank you!".to_string(),
            "2024-01-01 12:00:01",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "Can you help me with coding?".to_string(),
            "2024-01-01 12:00:02",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::ToolCall,
            "bash(\"ls -la\")".to_string(),
            "2024-01-01 12:00:03",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::ToolResult,
            "file1.txt file2.txt".to_string(),
            "2024-01-01 12:00:04",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "I found some files in the directory.".to_string(),
            "2024-01-01 12:00:05",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(70, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_mixed_roles_scroll", lines.join("\n"));
    }

    /// Test chat with long messages that require wrapping
    #[test]
    fn chat_long_messages_scroll() {
        let mut chat = ChatState::new();
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "This is a very long message that should wrap to multiple lines when displayed in the terminal. It contains lots of text that will need to be wrapped.".to_string(),
            "2024-01-01 12:00:00",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::Agent,
            "Here is another lengthy response that will definitely require multiple lines to display properly. It has detailed explanations and lots of information.".to_string(),
            "2024-01-01 12:00:01",
        ));
        chat.add_message(ChatMessage::with_timestamp(
            MessageRole::User,
            "Short".to_string(),
            "2024-01-01 12:00:02",
        ));
        let paragraph = ReplUi::build_chat_history_paragraph(&chat, false);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            let content_height = paragraph.line_count(chunks[0].width.saturating_sub(2)) as u16;
            ReplUi::render_chat_history(f, chunks[0], 0, 0, paragraph, content_height, false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_long_messages_scroll", lines.join("\n"));
    }

    // === Scrolling Unit Tests (no snapshots needed) ===

    #[test]
    fn test_scroll_position_clamping() {
        let mut chat = ChatState::new();
        // Add 20 messages
        for i in 1..=20 {
            chat.add_message(ChatMessage::user(format!("Message {}", i)));
        }

        // Calculate content height (20 messages)
        let content_height: u16 = 20;
        let viewport_height: u16 = 10;
        let max_scroll = content_height.saturating_sub(viewport_height);
        assert_eq!(max_scroll, 10); // Can scroll 10 lines down

        // Test that scroll position is clamped
        let scroll_above_max = 15u16;
        assert!(scroll_above_max > max_scroll);
        let clamped = scroll_above_max.min(max_scroll);
        assert_eq!(clamped, 10);
    }

    #[test]
    fn test_scroll_with_fewer_messages_than_viewport() {
        let mut chat = ChatState::new();
        // Add only 3 messages but viewport is 10 lines
        for i in 1..=3 {
            chat.add_message(ChatMessage::user(format!("Message {}", i)));
        }

        let content_height: u16 = 3;
        let viewport_height: u16 = 10;
        let max_scroll = content_height.saturating_sub(viewport_height);
        assert_eq!(max_scroll, 0); // No scrolling needed

        // Any scroll position should be clamped to 0
        assert_eq!(5u16.min(max_scroll), 0);
    }

    #[test]
    fn test_auto_scroll_toggle() {
        let mut chat = ChatState::new();

        // Initially no messages, auto_scroll should be false
        assert!(!chat.auto_scroll);

        // Add first message - auto_scroll should be set to true
        chat.add_message(ChatMessage::user("Hello".to_string()));
        assert!(chat.auto_scroll);

        // Manually disable auto_scroll
        chat.auto_scroll = false;
        assert!(!chat.auto_scroll);

        // Add another message
        chat.add_message(ChatMessage::agent("Hi!".to_string()));
        // add_message sets auto_scroll to true
        assert!(chat.auto_scroll);
    }

    #[test]
    fn test_page_up_clamping() {
        let content_height: u16 = 20;
        let viewport_height: u16 = 10;
        let max_scroll = content_height.saturating_sub(viewport_height);
        let initial_scroll = 5u16;

        // PageUp decrements by 10
        let new_scroll = initial_scroll.saturating_sub(10);
        let clamped = new_scroll.min(max_scroll);
        assert_eq!(clamped, 0); // Should not go below 0
    }

    #[test]
    fn test_page_down_clamping() {
        let content_height: u16 = 20;
        let viewport_height: u16 = 10;
        let max_scroll = content_height.saturating_sub(viewport_height);
        let initial_scroll = 8u16;

        // PageDown increments by 10
        let new_scroll = initial_scroll + 10;
        let clamped = new_scroll.min(max_scroll);
        assert_eq!(clamped, 10); // Should clamp to max_scroll
    }

    #[test]
    fn test_mouse_scroll_clamping() {
        let content_height: u16 = 20;
        let viewport_height: u16 = 10;
        let max_scroll = content_height.saturating_sub(viewport_height);

        // ScrollUp by 3 from position 1
        let scroll_up = 1u16.saturating_sub(3).min(max_scroll);
        assert_eq!(scroll_up, 0);

        // ScrollDown by 3 from position 9
        let scroll_down = (9u16 + 3).min(max_scroll);
        assert_eq!(scroll_down, 10);
    }

    // === Quit Confirmation Dialog Tests ===

    /// Test quit confirmation dialog with "No" selected (default state)
    #[test]
    fn quit_confirm_no_selected() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_quit_confirm(f, f.area(), false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("quit_confirm_no_selected", lines.join("\n"));
    }

    /// Test quit confirmation dialog with "Yes" selected
    #[test]
    fn quit_confirm_yes_selected() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_quit_confirm(f, f.area(), true);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("quit_confirm_yes_selected", lines.join("\n"));
    }

    /// Test quit confirmation dialog centered on a larger terminal
    #[test]
    fn quit_confirm_large_terminal() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_quit_confirm(f, f.area(), false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("quit_confirm_large_terminal", lines.join("\n"));
    }

    /// Test quit confirmation dialog on a minimal-sized terminal
    #[test]
    fn quit_confirm_minimal_terminal() {
        let backend = TestBackend::new(60, 15);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_quit_confirm(f, f.area(), false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("quit_confirm_minimal_terminal", lines.join("\n"));
    }

    /// Test quit confirmation dialog with Yes selected on large terminal
    #[test]
    fn quit_confirm_yes_selected_large() {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            ReplUi::render_quit_confirm(f, f.area(), true);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("quit_confirm_yes_selected_large", lines.join("\n"));
    }

    // === Todo Panel Snapshot Tests ===

    /// Test todo panel with no items (empty state)
    #[test]
    fn todo_panel_empty() {
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let state = TodoState::default();
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_empty", lines.join("\n"));
    }

    /// Test todo panel with a single pending item
    #[test]
    fn todo_panel_single_pending() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 1,
            text: "Write documentation".to_string(),
            completed: false,
            status: TodoStatus::Pending,
            selected: false,
        });

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_single_pending", lines.join("\n"));
    }

    /// Test todo panel with a single in-progress item
    #[test]
    fn todo_panel_single_in_progress() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 2,
            text: "Implement feature".to_string(),
            completed: false,
            status: TodoStatus::InProgress,
            selected: false,
        });

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_single_in_progress", lines.join("\n"));
    }

    /// Test todo panel with a single completed item (should show strikethrough)
    #[test]
    fn todo_panel_single_completed() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 3,
            text: "Fix bug".to_string(),
            completed: true,
            status: TodoStatus::Completed,
            selected: false,
        });

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_single_completed", lines.join("\n"));
    }

    /// Test todo panel with a single cancelled item
    #[test]
    fn todo_panel_single_cancelled() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 4,
            text: "Deprecated feature".to_string(),
            completed: false,
            status: TodoStatus::Cancelled,
            selected: false,
        });

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_single_cancelled", lines.join("\n"));
    }

    /// Test todo panel with multiple items
    #[test]
    fn todo_panel_multiple_items() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 1,
            text: "First task".to_string(),
            completed: false,
            status: TodoStatus::Completed,
            selected: false,
        });
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 2,
            text: "Second task".to_string(),
            completed: false,
            status: TodoStatus::InProgress,
            selected: false,
        });
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 3,
            text: "Third task".to_string(),
            completed: false,
            status: TodoStatus::Pending,
            selected: false,
        });

        let backend = TestBackend::new(30, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_multiple_items", lines.join("\n"));
    }

    /// Test todo panel with many items (tests scroll behavior)
    #[test]
    fn todo_panel_many_items() {
        use peakbot::TodoStatus;
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        for i in 1..=15 {
            let status = match i % 4 {
                1 => TodoStatus::Completed,
                2 => TodoStatus::InProgress,
                3 => TodoStatus::Pending,
                _ => TodoStatus::Cancelled,
            };
            state.items.push(peakbot::ui::app_state::TodoItem {
                id: i,
                text: format!("Task number {}", i),
                completed: matches!(status, TodoStatus::Completed),
                status,
                selected: false,
            });
        }

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_many_items", lines.join("\n"));
    }

    /// Test todo panel with long task text (truncation)
    #[test]
    fn todo_panel_long_text() {
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 1,
            text: "This is a very long task description that should definitely be truncated when displayed".to_string(),
            completed: false,
            status: peakbot::TodoStatus::Pending,
            selected: false,
        });

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_long_text", lines.join("\n"));
    }

    /// Test todo panel with narrow width
    #[test]
    fn todo_panel_narrow() {
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 1,
            text: "Task".to_string(),
            completed: false,
            status: peakbot::TodoStatus::Pending,
            selected: false,
        });

        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_narrow", lines.join("\n"));
    }

    /// Test todo panel on very small area (should not render)
    #[test]
    fn todo_panel_too_small() {
        use peakbot::ui::app_state::TodoState;
        use peakbot::ui::repl::todo_panel::render_todo_panel;

        let mut state = TodoState::default();
        state.items.push(peakbot::ui::app_state::TodoItem {
            id: 1,
            text: "Task".to_string(),
            completed: false,
            status: peakbot::TodoStatus::Pending,
            selected: false,
        });

        // Area too small (2x2)
        let backend = TestBackend::new(2, 2);
        let mut terminal = Terminal::new(backend).unwrap();

        let _ = terminal.draw(|f| {
            render_todo_panel(f, f.area(), &state, 0);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("todo_panel_too_small", lines.join("\n"));
    }

    // === Todo Panel Unit Tests (no snapshots needed) ===

    #[test]
    fn test_todo_panel_should_show() {
        use peakbot::ui::repl::todo_panel::should_show_panel;

        // Should show on wide terminals
        assert!(should_show_panel(80));
        assert!(should_show_panel(60));
        assert!(should_show_panel(61));

        // Should not show on narrow terminals
        assert!(!should_show_panel(59));
        assert!(!should_show_panel(40));
        assert!(!should_show_panel(20));
    }

    #[test]
    fn test_todo_item_status_icons() {
        use peakbot::TodoStatus;

        // Pending
        let pending = TodoStatus::Pending;
        assert_eq!(pending.to_string(), "pending");

        // InProgress
        let in_progress = TodoStatus::InProgress;
        assert_eq!(in_progress.to_string(), "in_progress");

        // Completed
        let completed = TodoStatus::Completed;
        assert_eq!(completed.to_string(), "completed");

        // Cancelled
        let cancelled = TodoStatus::Cancelled;
        assert_eq!(cancelled.to_string(), "cancelled");
    }

    // === Command Popup Snapshot Tests ===
    //
    // See `allehailmenu.md` §6 (rendering contract).
    //
    // The popup is rendered via `render_command_popup(f, input_area, popup)`.
    // We anchor a "pseudo input area" low on the test terminal so the popup
    // sits in a predictable place.

    use peakbot::ui::repl::command_popup::render_command_popup;
    use peakbot::ui::ui_trait::CommandPopupState;
    use ratatui::layout::Rect;

    /// Helper: render the popup into a test terminal and return the buffer
    /// lines. The `input_area` is anchored at y=20 so the popup sits above
    /// it, fully visible on a 24-row terminal.
    fn snapshot_popup(popup: &CommandPopupState) -> Vec<String> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            let input_area = Rect::new(0, 20, 80, 3);
            render_command_popup(f, input_area, popup);
        });
        buffer_to_lines(terminal.backend())
    }

    #[test]
    fn command_popup_just_opened() {
        let popup = CommandPopupState::new(String::new());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_just_opened", lines.join("\n"));
    }

    #[test]
    fn command_popup_filtered_single_match() {
        let popup = CommandPopupState::new("stat".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_filtered_single_match", lines.join("\n"));
    }

    #[test]
    fn command_popup_filtered_multi_match() {
        // "c" matches: context, compact, conversations
        let popup = CommandPopupState::new("c".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_filtered_multi_match", lines.join("\n"));
    }

    #[test]
    fn command_popup_no_matches() {
        let popup = CommandPopupState::new("xyz".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_no_matches", lines.join("\n"));
    }

    #[test]
    fn command_popup_selected_second() {
        let mut popup = CommandPopupState::new(String::new());
        popup.navigate_down();
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_selected_second", lines.join("\n"));
    }

    #[test]
    fn command_popup_takes_args_hint() {
        // `l` filters to /load (takes_args=true); popup should show the
        // "<args>" hint on that row.
        let popup = CommandPopupState::new("l".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_takes_args_hint", lines.join("\n"));
    }

    /// Issue #52: after transitioning from Argument to SlashCommand mode,
    /// the popup should show the `/model` command (prefix "model" matches
    /// the "model" slash command).
    #[test]
    fn command_popup_model_prefix_after_arg_transition() {
        let popup = CommandPopupState::new("model".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!(
            "command_popup_model_prefix_after_arg_transition",
            lines.join("\n")
        );
    }

    /// Issue #52: when the prefix doesn't match any command (e.g., "mode"
    /// after backspacing from "model"), the popup should show the
    /// "no matching commands" placeholder.
    #[test]
    fn command_popup_mode_prefix_no_matches() {
        let popup = CommandPopupState::new("mode".to_string());
        let lines = snapshot_popup(&popup);
        assert_snapshot!("command_popup_mode_prefix_no_matches", lines.join("\n"));
    }

    // === Chat Scrollbar Tests ===

    /// The scrollbar thumb position must be driven by the GLOBAL scroll
    /// offset into the full transcript, not by the paragraph-local
    /// `inner_scroll` (which is an offset into the first visible message
    /// and is bounded by one message's wrapped height).
    ///
    /// Bug being pinned (2026-04-24): since commit `3b3149e` landed the
    /// viewport render cache, `render_chat_history` took a single `scroll`
    /// parameter that was passed `view.inner_scroll` and then used both
    /// for `paragraph.scroll(...)` AND for `ScrollbarState::position(...)`.
    /// With `inner_scroll` always small, the thumb was stuck near the
    /// top in long conversations.
    ///
    /// Post-fix contract: `render_chat_history` takes TWO scroll args —
    /// `global_scroll` (for the scrollbar thumb) and `paragraph_scroll`
    /// (for the Paragraph inside the bordered block). This test exercises
    /// the new signature; with the old one, it fails to compile (which
    /// is a valid red state).
    #[test]
    fn render_chat_history_scrollbar_tracks_global_not_inner_scroll() {
        use ratatui::{
            text::Text,
            widgets::{Block, Borders, Paragraph, Wrap},
        };

        const HEIGHT: u16 = 20;
        const CONTENT_HEIGHT: u16 = 1000;
        let global_scroll: u16 = 500; // halfway through a 1000-line transcript
        let paragraph_scroll: u16 = 3; // tiny offset inside first visible msg

        let backend = TestBackend::new(40, HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();

        // A minimal paragraph stand-in: the scrollbar column doesn't depend
        // on what's rendered to the left of it. Block mimics the chat block.
        let paragraph = Paragraph::new(Text::from("line".repeat(10)))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL));

        terminal
            .draw(|f| {
                ReplUi::render_chat_history(
                    f,
                    f.area(),
                    global_scroll,
                    paragraph_scroll,
                    paragraph,
                    CONTENT_HEIGHT,
                    false,
                );
            })
            .unwrap();

        // Inspect the rightmost column (the scrollbar). Find the thumb row
        // — ratatui 0.30's default thumb symbol is `█`.
        let backend = terminal.backend();
        let buf = backend.buffer();
        let scrollbar_col = buf.area.width - 1;
        let mut thumb_row: Option<u16> = None;
        for row in 0..buf.area.height {
            let cell = buf.cell((scrollbar_col, row)).unwrap();
            if cell.symbol() == "█" {
                thumb_row = Some(row);
                break;
            }
        }
        let thumb_row = thumb_row
            .expect("scrollbar thumb (█) must be rendered somewhere in the scrollbar column");

        // global_scroll = 500 / content_length = 1000 → thumb belongs in
        // the middle third of the 20-row column. Pre-fix the thumb sat
        // at row 0–2 because `ScrollbarState.position = inner_scroll +
        // area.height - 2` = 3 + 18 = 21 out of 1000, pinning it at the
        // top.
        assert!(
            (5..=14).contains(&thumb_row),
            "thumb at row {thumb_row} (global_scroll=500/1000); \
             expected middle of scrollbar column (rows 5..=14). \
             Pre-fix this lived at rows 0–2."
        );
    }

    /// Complements the prior test: at `global_scroll == 0` the thumb
    /// must sit near the top of the scrollbar column. Pre-fix this
    /// happened to pass (wrong reasons: the buggy formula yielded
    /// `0 + area.height - 2 = 18 / 1000 ≈ top`). Post-fix it passes for
    /// the RIGHT reason — `position = 0`.
    #[test]
    fn render_chat_history_scrollbar_at_top_when_global_scroll_zero() {
        use ratatui::{
            text::Text,
            widgets::{Block, Borders, Paragraph, Wrap},
        };

        const HEIGHT: u16 = 20;
        const CONTENT_HEIGHT: u16 = 1000;

        let backend = TestBackend::new(40, HEIGHT);
        let mut terminal = Terminal::new(backend).unwrap();

        let paragraph = Paragraph::new(Text::from("line"))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL));

        terminal
            .draw(|f| {
                ReplUi::render_chat_history(
                    f,
                    f.area(),
                    /* global_scroll   */ 0,
                    /* paragraph_scroll*/ 0,
                    paragraph,
                    CONTENT_HEIGHT,
                    false,
                );
            })
            .unwrap();

        let backend = terminal.backend();
        let buf = backend.buffer();
        let scrollbar_col = buf.area.width - 1;
        let mut thumb_row: Option<u16> = None;
        for row in 0..buf.area.height {
            let cell = buf.cell((scrollbar_col, row)).unwrap();
            if cell.symbol() == "█" {
                thumb_row = Some(row);
                break;
            }
        }
        let thumb_row =
            thumb_row.expect("scrollbar thumb (█) must be rendered in the scrollbar column");

        assert!(
            thumb_row <= 3,
            "at global_scroll=0 thumb must sit near top (row ≤ 3), got row {thumb_row}"
        );
    }

    // === Input-area VS16 stripping ===
    //
    // When the user types or pastes VS16-bearing emoji into the input
    // box, `build_input_paragraph` constructs `Span::raw` directly
    // from the buffer contents — bypassing the renderer normaliser.
    // Every byte the user typed flows through to the terminal, including
    // U+FE0F. On Linux/kitty this drifts the cursor by 1 column. Pin
    // the contract: VS16 must be stripped from rendered input cells.
    // See `garbled.md` (Class A bypass).

    #[test]
    fn input_area_strips_vs16_from_user_input() {
        // Simulate user typing/pasting "⚠️ help me" (warn emoji + VS16).
        let input = "\u{26A0}\u{FE0F} help me";
        let paragraph =
            ReplUi::build_input_paragraph(input, input.len(), false, None, None, 0, 0, false);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        let joined = lines.join("\n");
        assert!(
            !joined.contains('\u{FE0F}'),
            "VS16 must not survive into rendered input cells: {joined:?}"
        );
        // The base symbol should still be there.
        assert!(
            joined.contains('\u{26A0}'),
            "base symbol must survive: {joined:?}"
        );
    }

    // === Bash panel snapshot tests (slice 2 of #11) ===
    //
    // Pin the rendered output of the foreground `bash` tool panel in
    // each lifecycle state. The renderer is the slice 2 deliverable;
    // these snapshots lock the visual contract so slice 3 (PTY wiring)
    // and slice 4 (stdin field) can't silently regress the surface.
    //
    // Helper imports are kept local to each test to mirror the style
    // already used by the todo-panel tests above.

    #[test]
    fn bash_panel_running_short_tail() {
        use chrono::{Local, TimeZone};
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        // Fixed start time so the elapsed clock in the header is
        // deterministic across machines. Pick something many seconds in
        // the past; the snapshot only locks the duration *format*, not
        // a specific value, by sanitising the digits below.
        let started_at = Local
            .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
            .single()
            .unwrap();
        let state = BashPanelState::Running {
            command: "psql -f migrate.sql".into(),
            pid: 4821,
            started_at,
            tail: vec![
                "NOTICE: table \"users\" does not exist, skipping".into(),
                "CREATE TABLE".into(),
                "CREATE INDEX".into(),
            ],
        };

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });

        let lines = buffer_to_lines(terminal.backend());
        // Replace the live-elapsed digits in the title so the snapshot
        // is reproducible. Header has shape `... · MM:SS ─...`.
        let sanitised = sanitise_elapsed(lines.join("\n"));
        assert_snapshot!("bash_panel_running_short_tail", sanitised);
    }

    #[test]
    fn bash_panel_running_overflow_keeps_last_5_lines() {
        use chrono::{Local, TimeZone};
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        let started_at = Local
            .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
            .single()
            .unwrap();
        let tail: Vec<String> = (0..10).map(|i| format!("line {}", i)).collect();
        let state = BashPanelState::Running {
            command: "yes | head".into(),
            pid: 100,
            started_at,
            tail,
        };

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });

        let lines = buffer_to_lines(terminal.backend());
        let sanitised = sanitise_elapsed(lines.join("\n"));
        assert_snapshot!("bash_panel_running_overflow_keeps_last_5_lines", sanitised);
    }

    #[test]
    fn bash_panel_finished_success() {
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        let state = BashPanelState::Finished {
            command: "make build".into(),
            exit_code: 0,
            duration_secs: 42,
            tail: vec![
                "Compiling peakbot v0.5.2".into(),
                "Finished `dev` profile [unoptimized + debuginfo]".into(),
            ],
        };

        let backend = TestBackend::new(80, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("bash_panel_finished_success", lines.join("\n"));
    }

    #[test]
    fn bash_panel_finished_failure() {
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        let state = BashPanelState::Finished {
            command: "cargo test --doc".into(),
            exit_code: 101,
            duration_secs: 7,
            tail: vec!["error[E0277]: trait bound not satisfied".into()],
        };

        let backend = TestBackend::new(80, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("bash_panel_finished_failure", lines.join("\n"));
    }

    /// Slice 4: stdin row when unfocused — shows label + buffer + the
    /// `[Ctrl+S]` hint. Locks the visual the user sees while typing a
    /// chat message with a bash panel open.
    #[test]
    fn bash_panel_running_stdin_unfocused_shows_hint() {
        use chrono::{Local, TimeZone};
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        let started_at = Local
            .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
            .single()
            .unwrap();
        let state = BashPanelState::Running {
            command: "sudo apt update".into(),
            pid: 4242,
            started_at,
            tail: vec!["[sudo] password for user:".into()],
        };

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });

        let lines = buffer_to_lines(terminal.backend());
        let sanitised = sanitise_elapsed(lines.join("\n"));
        assert_snapshot!("bash_panel_running_stdin_unfocused_shows_hint", sanitised);
    }

    /// Slice 4: stdin row when focused — shows buffer + block cursor,
    /// no hint. The cursor IS the focus signal. Also exercises a
    /// non-empty buffer so the wrapping/clipping behaviour is pinned.
    #[test]
    fn bash_panel_running_stdin_focused_with_buffer_and_cursor() {
        use chrono::{Local, TimeZone};
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::render_bash_panel;

        let started_at = Local
            .with_ymd_and_hms(2026, 1, 1, 12, 0, 0)
            .single()
            .unwrap();
        let state = BashPanelState::Running {
            command: "sudo apt update".into(),
            pid: 4242,
            started_at,
            tail: vec!["[sudo] password for user:".into()],
        };

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            // Buffer shown as typed; the renderer doesn't mask it
            // (echo suppression is the PTY's job — see the
            // `bash_stdin_no_echo_suppressed_under_pty` integration
            // pin).
            render_bash_panel(f, f.area(), &state, "hunter2", true);
        });

        let lines = buffer_to_lines(terminal.backend());
        let sanitised = sanitise_elapsed(lines.join("\n"));
        assert_snapshot!(
            "bash_panel_running_stdin_focused_with_buffer_and_cursor",
            sanitised
        );
    }

    #[test]
    fn bash_panel_idle_renders_nothing() {
        use peakbot::ui::app_state::BashPanelState;
        use peakbot::ui::repl::bash_panel::{panel_height, render_bash_panel};

        // Idle has zero height — the layout would not allocate any
        // area. Sanity-check both that the height function returns 0
        // *and* that calling the renderer on a zero-height area is a
        // safe no-op (defensive: callers shouldn't, but renderers
        // shouldn't panic if they do).
        let state = BashPanelState::Idle;
        assert_eq!(panel_height(&state), 0);

        // A nonzero-height surface; the renderer must still no-op for
        // `Idle` because the *state* says hidden.
        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let _ = terminal.draw(|f| {
            render_bash_panel(f, f.area(), &state, "", false);
        });
        let lines = buffer_to_lines(terminal.backend());
        // All cells should be blank — the renderer drew nothing.
        let joined: String = lines.join("");
        assert!(
            joined.chars().all(|c| c == ' '),
            "Idle render should leave the surface blank, got: {lines:?}"
        );
    }

    /// Replace the live `MM:SS` elapsed timer in the Running header so
    /// snapshots are reproducible. The header is shaped like
    /// `┌─ > psql ... · pid 4821 · 03:42 ─...┐`; we sweep any
    /// `<one-or-more digits>:<exactly 2 digits>` pattern to `MM:SS`.
    /// One-or-more on the left handles wide elapsed values like
    /// `34567890:42` that show up when a Running snapshot test pins a
    /// `started_at` years in the past.
    ///
    /// Cheap, scoped, and only touches the bash-panel snapshots.
    fn sanitise_elapsed(s: String) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Look ahead for `\d+:\d{2}` starting at `i`.
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i
                && j + 2 < bytes.len()
                && bytes[j] == b':'
                && bytes[j + 1].is_ascii_digit()
                && bytes[j + 2].is_ascii_digit()
            {
                out.push_str("MM:SS");
                i = j + 3;
            } else {
                // Push the next char (must respect UTF-8 boundaries).
                let ch_end = utf8_char_end(bytes, i);
                out.push_str(std::str::from_utf8(&bytes[i..ch_end]).unwrap());
                i = ch_end;
            }
        }
        out
    }

    fn utf8_char_end(bytes: &[u8], start: usize) -> usize {
        let b = bytes[start];
        // UTF-8 lead-byte → continuation-length table. A continuation
        // byte (0x80..0xC0) is *not* a valid start, but if we somehow
        // land on one we advance 1 to make progress instead of looping.
        let len = match b {
            0..=0x7F => 1,
            0x80..=0xBF => 1, // defensive: malformed boundary, step over
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        };
        (start + len).min(bytes.len())
    }
}
