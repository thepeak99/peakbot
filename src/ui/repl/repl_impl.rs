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
use crossterm::event::{self, Event as TuiEvent, KeyEvent};
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
            welcome_printed: false,
            event_receiver: None,
        }
    }

    /// Render the chat history area
    fn render_chat_history(f: &mut ratatui::Frame, area: Rect, chat: &ChatState) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .split(area);

        let left_border = Paragraph::new("│").style(Style::default().fg(Color::DarkGray));
        f.render_widget(left_border, chunks[0]);

        let chat_area = chunks[1];

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
                    Span::raw(":"),
                ]));

                // Use Paragraph's native wrapping instead of manual word wrapping
                let wrap_width = (chat_area.width.saturating_sub(4)) as usize;
                let wrapped_lines = Self::wrap_text(&msg.content, wrap_width);
                message_lines.extend(wrapped_lines);

                message_lines.push(Line::from(""));
            }
        }

        let content_lines = message_lines.len();
        let view_height = chat_area.height.saturating_sub(2) as usize;

        let scroll_offset = if chat.auto_scroll && content_lines > view_height {
            content_lines.saturating_sub(view_height)
        } else {
            chat.scroll_offset
                .min(content_lines.saturating_sub(view_height).saturating_sub(1))
        };

        let visible_lines: Vec<Line> = message_lines
            .into_iter()
            .skip(scroll_offset)
            .take(view_height)
            .collect();

        let paragraph = Paragraph::new(Text::from(visible_lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Chat Messages ")
                    .borders(Borders::ALL),
            );
        f.render_widget(paragraph, chat_area);

        if content_lines > view_height {
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(Style::default().fg(Color::DarkGray));
            let mut scroll_state = ScrollbarState::new(content_lines).position(scroll_offset);
            f.render_stateful_widget(scrollbar, chat_area, &mut scroll_state);
        }

        let right_border = Paragraph::new("│").style(Style::default().fg(Color::DarkGray));
        f.render_widget(right_border, chunks[2]);
    }

    /// Render the input area with cursor
    fn render_input_area(f: &mut ratatui::Frame, area: Rect, input: &str, cursor_pos: usize) {
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
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" Input ").borders(Borders::ALL));

        f.render_widget(paragraph, area);
    }

    /// Render the status bar
    fn render_status_bar(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
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
    fn render(&mut self, state: &AppState) -> io::Result<()> {
        if let Some(ref mut terminal) = self.terminal {
            terminal.draw(|f| {
                let size = f.area();

                if size.height < MIN_TERMINAL_HEIGHT || size.width < MIN_TERMINAL_WIDTH {
                    let warning = Paragraph::new("Terminal too small. Please resize.");
                    f.render_widget(warning, size);
                    return;
                }

                let input_height =
                    Self::calculate_input_height(&self.input_buffer, size.width) as u16;

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Fill(1),
                        Constraint::Length(input_height + 4),
                        Constraint::Length(1),
                    ])
                    .split(size);

                Self::render_chat_history(f, chunks[0], &state.chat);
                Self::render_input_area(f, chunks[1], &self.input_buffer, self.cursor_pos);
                Self::render_status_bar(f, chunks[2], state);
            })?;
        }
        Ok(())
    }

    /// Handle input events
    fn handle_input(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

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
        let events = crossterm::event::EventStream::new();
        let state_events = self.state_manager.subscribe();
        while self.running {
            tokio::select! {
                // Handle keyboard events via tokio::select!
                key = events.next() => {
                    if let Some(key) = key {
                        self.handle_input(key);
                        let state = self.state_manager.get_state();
                        self.render(&state)?;
                    }
                }
                // Periodic render to keep UI responsive to state changes
                state = state_events.recv() => {
                    self.render(&state)?;
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
