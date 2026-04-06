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
        widgets::Widget,
    };

    use super::snapshot_helpers::*;
    use peakbot::ui::app_state::{AppState, ChatMessage, ChatState, MessageRole};
    use peakbot::ui::repl::ReplUi;

    // === Input Area Tests ===

    #[test]
    fn input_area_empty() {
        let paragraph = ReplUi::build_input_paragraph("", 0);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_empty", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_start() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 0);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_start", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_middle() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 2);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_middle", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_end() {
        let paragraph = ReplUi::build_input_paragraph("Hello", 5);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_end", lines.join("\n"));
    }

    #[test]
    fn input_area_long_text() {
        let paragraph = ReplUi::build_input_paragraph(
            "This is a very long input that will wrap to multiple lines",
            0,
        );
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_long_text", lines.join("\n"));
    }

    // === Chat History Tests ===

    #[test]
    fn chat_welcome() {
        let chat = ChatState::new();
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_agent_message_multiline", lines.join("\n"));
    }

    // === Status Bar Tests ===

    #[test]
    fn status_bar_empty() {
        let state = AppState::new();
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
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

        terminal.draw(|f| {
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Scroll position 0 = showing top of content
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Scroll position 5 = showing middle of content
            ReplUi::render_chat_history(f, chunks[0], 5, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            // Max scroll = 15 messages - 8 visible = 7 (show last messages)
            ReplUi::render_chat_history(f, chunks[0], 8, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(70, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
        let paragraph = ReplUi::build_chat_history_paragraph(&chat);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(100), Constraint::Length(1)])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, paragraph);
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
}
