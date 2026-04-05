//! REPL UI Tests using TestBackend with insta snapshots
//!
//! These tests use Ratatui's TestBackend to render widgets and store
//! snapshots for regression testing.

mod snapshot_helpers;

#[cfg(test)]
mod tests {
    use ratatui::{
        backend::TestBackend,
        layout::{Constraint, Direction, Layout},
        Terminal,
        widgets::Widget,
    };
    use insta::assert_snapshot;

    use peakbot::ui::app_state::{AppState, ChatMessage, ChatState, MessageRole};
    use peakbot::ui::repl::ReplUi;
    use super::snapshot_helpers::*;

    // === Input Area Tests ===

    #[test]
    fn input_area_empty() {
        let paragraph = ReplUi::get_input_area("", 0);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_empty", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_start() {
        let paragraph = ReplUi::get_input_area("Hello", 0);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_start", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_middle() {
        let paragraph = ReplUi::get_input_area("Hello", 2);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_middle", lines.join("\n"));
    }

    #[test]
    fn input_area_cursor_end() {
        let paragraph = ReplUi::get_input_area("Hello", 5);
        let terminal = render_widget(paragraph, 60, 3);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_cursor_end", lines.join("\n"));
    }

    #[test]
    fn input_area_long_text() {
        let paragraph = ReplUi::get_input_area("This is a very long input that will wrap to multiple lines", 0);
        let terminal = render_widget(paragraph, 60, 5);
        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("input_area_long_text", lines.join("\n"));
    }

    // === Chat History Tests ===

    #[test]
    fn chat_welcome() {
        let chat = ChatState::new();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(100),
                    Constraint::Length(1),
                ])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, &chat);
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
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(100),
                    Constraint::Length(1),
                ])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, &chat);
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
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(100),
                    Constraint::Length(1),
                ])
                .split(f.area());
            ReplUi::render_chat_history(f, chunks[0], 0, &chat);
        });

        let lines = buffer_to_lines(terminal.backend());
        assert_snapshot!("chat_single_agent_message", lines.join("\n"));
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
        assert!(chat.auto_scroll, "auto_scroll should be true after adding message");
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
}
