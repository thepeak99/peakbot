//! REPL UI Implementation — a View in MVC
//!
//! The REPL View:
//! - Reads user input and sends UiActions to the Controller (AgentRunner)
//! - Subscribes to StateManager and renders state to stdout
//! - Never calls the agent directly
//!
//! Data flow:
//!   User input → UiAction → Controller → Model (StateManager) → broadcast → View (render)

use anyhow::Result;
use crossterm::event::{self, Event, EventStream, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use std::future::pending;
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::ui::app_state::{AppState, ChatState, MessageRole};
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction};

/// Maximum input height in lines
const MAX_INPUT_LINES: usize = 5;

/// Minimum terminal height
const MIN_TERMINAL_HEIGHT: u16 = 10;

/// Minimum terminal width
const MIN_TERMINAL_WIDTH: u16 = 20;

/// REPL View — subscribes to StateManager and renders to stdout
pub struct ReplUi {
    state_manager: Arc<StateManager>,
    /// Send user actions to the Controller
    action_sender: UnboundedSender<UiAction>,
    /// Whether the view is running
    running: bool,
    /// Terminal for TUI rendering
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    /// Local input buffer
    input_buffer: String,
    /// Cursor position in input buffer
    cursor_pos: usize,
}

impl ReplUi {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        Self {
            state_manager,
            action_sender,
            running: true,
            terminal: None,
            input_buffer: String::new(),
            cursor_pos: 0,
        }
    }

    /// Render the chat history area
    pub fn render_chat_history(f: &mut ratatui::Frame, area: Rect, scroll: u16, chat: &ChatState) {
        let mut message_lines: Vec<Line> = Vec::new();

        if chat.messages.is_empty() {
            message_lines.push(Line::from(Span::styled(
                "Welcome to PeakBot! Start a conversation or use /help for commands.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in &chat.messages {
                let (prefix, color) = match msg.role {
                    MessageRole::User => ("👤 User", Color::LightGreen),
                    MessageRole::Agent => ("🤖 Agent", Color::LightMagenta),
                    MessageRole::System => ("⚙️ System", Color::LightYellow),
                    MessageRole::ToolCall => ("🔧 Tool", Color::Cyan),
                    MessageRole::ToolResult => ("📋 Result", Color::Blue),
                };

                let timestamp_str = msg.timestamp.format("%H:%M:%S").to_string();

                message_lines.push(Line::from(vec![
                    Span::raw("["),
                    Span::styled(timestamp_str, Style::default().fg(Color::DarkGray)),
                    Span::raw("] "),
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::raw(": "),
                    Span::raw(&msg.content),
                ]));
            }
        }

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let mut scroll_state = ScrollbarState::new(message_lines.len()).position(scroll as usize);
        f.render_stateful_widget(scrollbar, area, &mut scroll_state);

        let paragraph = Paragraph::new(Text::from(message_lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0))
            .block(
                Block::default()
                    .title(" Chat Messages ")
                    .borders(Borders::ALL),
            );
        f.render_widget(paragraph, area);
    }

    /// Render the input area with cursor (returns Paragraph for testing)
    pub fn get_input_area<'a>(input: &str, cursor_pos: usize) -> Paragraph<'a> {
        let (prompt_text, prompt_color) = if input.is_empty() {
            ("💬 Message...", Color::DarkGray)
        } else {
            ("> ", Color::Cyan)
        };

        let mut spans = vec![Span::styled(prompt_text, Style::default().fg(prompt_color))];

        if !input.is_empty() {
            let before_cursor = input[..cursor_pos.min(input.len())].to_string();
            let after_cursor = input[cursor_pos.min(input.len())..].to_string();

            spans.push(Span::raw(before_cursor));
            spans.push(Span::styled("█", Style::default().fg(Color::Yellow)));
            spans.push(Span::raw(after_cursor));
        }

        let paragraph = Paragraph::new(Line::from(spans))
            .wrap(Wrap { trim: true })
            .block(Block::default().title(" Input ").borders(Borders::ALL));
        paragraph
    }

    /// Render the status bar
    pub fn render_status_bar(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
        let stats = &state.stats;
        let context = &state.context;

        let total_tokens = stats.total_tokens();
        let tokens_str = stats.format_tokens(total_tokens);
        let cost_str = stats.format_cost();
        let context_pct = context.usage_percentage();

        let status_text = format!(
            "Tokens: {} │ Calls: {} │ Cost: ${} │ Context: {:.1}% │ Model: {}",
            tokens_str, stats.total_api_calls, cost_str, context_pct, stats.model,
        );

        let paragraph = Paragraph::new(status_text)
            .style(Style::default().fg(Color::LightCyan))
            .block(Block::default().borders(Borders::NONE));

        f.render_widget(paragraph, area);
    }

    /// Main render function
    fn render(&mut self, state: &AppState) -> Result<()> {
        if let Some(ref mut terminal) = self.terminal {
            terminal.draw(|f| {
                let size = f.area();

                if size.height < MIN_TERMINAL_HEIGHT || size.width < MIN_TERMINAL_WIDTH {
                    let warning = Paragraph::new("Terminal too small. Please resize.");
                    f.render_widget(warning, size);
                    return;
                }

                let input = Self::get_input_area(&self.input_buffer, self.cursor_pos);

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(100),
                        Constraint::Min(input.line_count(size.width - 2) as u16),
                        Constraint::Length(1),
                    ])
                    .split(size);

                Self::render_chat_history(f, chunks[0], 0, &state.chat);
                Self::render_status_bar(f, chunks[2], state);
                f.render_widget(input, chunks[1]);
            })?;
        }
        Ok(())
    }

    fn handle_keyboard_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input_buffer.remove(self.cursor_pos);
                }
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.input_buffer.len() {
                    self.input_buffer.remove(self.cursor_pos);
                }
            }
            KeyCode::Left => {
                self.cursor_pos = self.cursor_pos.saturating_sub(1);
            }
            KeyCode::Right => {
                self.cursor_pos = (self.cursor_pos + 1).min(self.input_buffer.len());
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
            }
            KeyCode::End => {
                self.cursor_pos = self.input_buffer.len();
            }
            KeyCode::Enter => {
                let msg = self.input_buffer.clone();
                if !msg.trim().is_empty() {
                    let _ = self.action_sender.send(UiAction::SendMessage(msg));
                }
                self.input_buffer.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Up | KeyCode::Down => {
                // Command history navigation - placeholder
            }
            KeyCode::Esc => {
                panic!("At the disco");
            }
            _ => {}
        }
    }

    /// Handle input events
    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key_event) => self.handle_keyboard_input(key_event),
            _ => {}
        }
    }
}

impl Ui for ReplUi {
    async fn init(&mut self) -> Result<()> {
        self.terminal = Some(ratatui::init());

        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let mut events = EventStream::new();
        let mut state_rx = self.state_manager.subscribe();
        while self.running {
            tokio::select! {
                // Handle keyboard events via tokio::select!
                key = events.next() => {
                    if let Some(Ok(key)) = key {
                        self.handle_input(key);
                        let state = self.state_manager.get_state();
                        self.render(&state)?;
                    }
                }
                // Async stream subscription — receive state updates
                state = state_rx.recv() => {
                    if let Some(state) = state {
                        self.render(&state)?;
                    } else {
                        // Channel closed, subscriber was dropped
                        break;
                    }
                }
            }
        }

        self.shutdown().await?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        if let Some(ref mut terminal) = self.terminal {
            terminal.draw(|f| {
                let rect = f.area();
                f.render_widget(Clear, rect);
            })?;
        }
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        self.terminal = None;
        self.running = false;
        Ok(())
    }
}
