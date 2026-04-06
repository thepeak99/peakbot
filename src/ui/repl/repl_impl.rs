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
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
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
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time;

use crate::ui::ChatMessage;
use crate::ui::app_state::{AppState, ChatState, MessageRole};
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction};

/// Minimum terminal height
const MIN_TERMINAL_HEIGHT: u16 = 10;

/// Minimum terminal width
const MIN_TERMINAL_WIDTH: u16 = 20;

/// UI state for rendering — what the user sees and interacts with
/// Extracted from ReplUi to keep orchestration separate from rendering state
pub struct UiState {
    /// Local input buffer
    pub input_buffer: String,
    /// Cursor position in input buffer
    pub cursor_pos: usize,
    /// Current scroll position (line offset)
    pub scroll_position: u16,
    /// Total content height in lines
    pub content_height: u16,
    /// Visible area height
    pub viewport_height: u16,
    /// Whether to auto-scroll to bottom when new messages arrive
    pub auto_scroll: bool,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            input_buffer: String::new(),
            cursor_pos: 0,
            scroll_position: 0,
            content_height: 0,
            viewport_height: 0,
            auto_scroll: true,
        }
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

/// REPL View — subscribes to StateManager and renders to stdout
pub struct ReplUi {
    state_manager: Arc<StateManager>,
    /// Send user actions to the Controller
    action_sender: UnboundedSender<UiAction>,
    /// Whether the view is running
    running: bool,
    /// Terminal for TUI rendering
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    /// UI state for rendering (input, scroll, viewport)
    ui_state: UiState,
    /// Whether the quit confirmation dialog is visible
    show_quit_confirm: bool,
    /// Which button is selected: true = "Yes", false = "No" (default)
    confirm_yes_selected: bool,
}

impl ReplUi {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        Self {
            state_manager,
            action_sender,
            running: true,
            terminal: None,
            ui_state: UiState::new(),
            show_quit_confirm: false,
            confirm_yes_selected: false,
        }
    }

    /// Build the chat history paragraph (returns Paragraph, caller handles rendering)
    pub fn build_chat_history_paragraph<'a>(chat: &'a ChatState) -> Paragraph<'a> {
        let mut message_lines: Vec<Line> = Vec::new();

        if chat.messages.is_empty() {
            message_lines.push(Line::from(Span::styled(
                "Welcome to PeakBot! Start a conversation or use /help for commands.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in &chat.messages {
                message_lines.extend(Self::build_chat_message_lines(msg));
            }
        }

        Paragraph::new(Text::from(message_lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Chat Messages ")
                    .borders(Borders::ALL),
            )
    }

    pub fn build_chat_message_lines<'a>(msg: &'a ChatMessage) -> Vec<Line<'a>> {
        let (prefix, color) = match msg.role {
            MessageRole::User => ("👤 User", Color::LightGreen),
            MessageRole::Agent => ("🤖 Agent", Color::LightMagenta),
            MessageRole::System => ("⚙️ System", Color::LightYellow),
            MessageRole::ToolCall => ("🔧 Tool", Color::Cyan),
            MessageRole::ToolResult => ("📋 Result", Color::Blue),
        };

        // Split content by newlines to handle multiline messages
        let content_lines: Vec<&str> = msg.content.split('\n').collect();

        let mut out = Vec::new();

        for (i, content_line) in content_lines.iter().enumerate() {
            // First line gets the full header (timestamp + role), subsequent lines get indentation
            let line_content = if i == 0 {
                vec![
                    Span::raw("["),
                    Span::styled(
                        format!("{}", msg.timestamp.format("%H:%M:%S")),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw("] "),
                    Span::styled(prefix, Style::default().fg(color)),
                    Span::raw(": "),
                    Span::raw(*content_line),
                ]
            } else {
                vec![Span::raw(*content_line)]
            };

            out.push(Line::from(line_content));
        }
        out
    }

    /// Render the chat history area with scrollbar
    pub fn render_chat_history(
        f: &mut ratatui::Frame,
        area: Rect,
        scroll: u16,
        paragraph: Paragraph,
    ) {
        let content_height = paragraph.line_count(area.width.saturating_sub(2)) as usize;

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100), Constraint::Length(1)])
            .split(area);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let mut scroll_state =
            ScrollbarState::new(content_height).position((scroll + area.height - 2) as usize);
        f.render_stateful_widget(scrollbar, chunks[1], &mut scroll_state);

        let scrolled = paragraph.scroll((scroll, 0));
        f.render_widget(scrolled, chunks[0]);
    }

    /// Build the input area paragraph (returns Paragraph for rendering)
    pub fn build_input_paragraph<'a>(input: &str, cursor_pos: usize) -> Paragraph<'a> {
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

    /// Render the input area (takes built paragraph and renders it)
    pub fn render_input_area<'a>(f: &mut ratatui::Frame, area: Rect, paragraph: Paragraph<'a>) {
        f.render_widget(paragraph, area);
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

    /// Render the quit confirmation dialog overlay
    pub fn render_quit_confirm(f: &mut ratatui::Frame, area: Rect, yes_selected: bool) {
        // Calculate centered popup dimensions
        let popup_width = 50;
        let popup_height = 9;
        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        // Clear the popup area
        f.render_widget(Clear, popup_area);

        // Fixed-width button labels for proper centering
        // Both buttons are padded to 14 characters
        const YES_BTN: &str = "  Yes, leave   ";
        const NO_BTN: &str = "  No, stay     ";
        const BTN_SEPARATOR: &str = "   ";
        const BTN_TOTAL_WIDTH: usize = 14 + 3 + 14; // yes + separator + no

        // Calculate centering offset for buttons
        let btn_left_padding = (popup_width as usize).saturating_sub(BTN_TOTAL_WIDTH) / 2;

        // Style for selected vs unselected
        let selected_style = Style::default().fg(Color::Yellow).bg(Color::DarkGray);
        let unselected_style = Style::default().fg(Color::White);

        let (yes_btn, yes_style) = if yes_selected {
            ("[ Yes, leave ]", selected_style)
        } else {
            (YES_BTN, unselected_style)
        };
        let (no_btn, no_style) = if !yes_selected {
            ("[ No, stay ]", selected_style)
        } else {
            (NO_BTN, unselected_style)
        };

        // Centered warning text (⚠️ = 2 visual chars, total = 30 visual)
        let warning = "              ⚠️  WAIT! DON'T LEAVE!  ⚠️";
        // Centered question (36 chars, centered = 7 spaces)
        let question = "       Are you sure you want to quit PeakBot?";
        // Centered hint text
        let hint = "      ←/→ to switch  ·  Enter to confirm  ·  Esc to cancel";

        // Build padding strings
        let btn_padding = " ".repeat(btn_left_padding);

        let content = vec![
            Line::from(vec![Span::raw("")]),
            Line::from(vec![Span::styled(warning, Style::default().fg(Color::LightRed))]),
            Line::from(vec![Span::raw("")]),
            Line::from(vec![Span::raw(question)]),
            Line::from(vec![Span::raw("")]),
            Line::from(vec![
                Span::raw(btn_padding),
                Span::styled(yes_btn, yes_style),
                Span::raw(BTN_SEPARATOR),
                Span::styled(no_btn, no_style),
            ]),
            Line::from(vec![Span::raw("")]),
            Line::from(vec![Span::styled(hint, Style::default().fg(Color::DarkGray))]),
            Line::from(vec![Span::raw("")]),
        ];

        let paragraph = Paragraph::new(content)
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::LightCyan)),
            );

        f.render_widget(paragraph, popup_area);
    }

    /// Main render function
    fn render(&mut self, state: &AppState) -> Result<()> {
        // Calculate content height and extract scroll state before borrowing terminal
        if let Some(ref mut terminal) = self.terminal {
            terminal.draw(|f| {
                let size = f.area();

                if size.height < MIN_TERMINAL_HEIGHT || size.width < MIN_TERMINAL_WIDTH {
                    let warning = Paragraph::new("Terminal too small. Please resize.");
                    f.render_widget(warning, size);
                    return;
                }

                let input = Self::build_input_paragraph(
                    &self.ui_state.input_buffer,
                    self.ui_state.cursor_pos,
                );

                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(100),
                        Constraint::Min(input.line_count(size.width - 2) as u16),
                        Constraint::Length(1),
                    ])
                    .split(size);

                let chat_history = Self::build_chat_history_paragraph(&state.chat);
                self.ui_state.viewport_height = chunks[0].height;
                self.ui_state.content_height =
                    chat_history.line_count(size.width.saturating_sub(2)) as u16;

                // Calculate scroll based on auto_scroll setting
                let max_scroll = self
                    .ui_state
                    .content_height
                    .saturating_sub(self.ui_state.viewport_height);
                let scroll = if self.ui_state.auto_scroll {
                    // Scroll to bottom
                    max_scroll
                } else {
                    // Use stored position (clamped to valid range)
                    self.ui_state.scroll_position.min(max_scroll)
                };

                Self::render_chat_history(f, chunks[0], scroll, chat_history);
                Self::render_input_area(f, chunks[1], input);
                Self::render_status_bar(f, chunks[2], state);

                // Render quit confirmation dialog if visible
                if self.show_quit_confirm {
                    Self::render_quit_confirm(f, size, self.confirm_yes_selected);
                }
            })?;
        }
        Ok(())
    }

    fn handle_keyboard_input(&mut self, key: KeyEvent) {
        match key.code {
            // Quit confirmation dialog handlers (must come before general handlers)
            KeyCode::Esc if self.show_quit_confirm => {
                // ESC while dialog is open = cancel (close dialog)
                self.show_quit_confirm = false;
            }
            KeyCode::Enter if self.show_quit_confirm => {
                if self.confirm_yes_selected {
                    self.running = false;
                }
                self.show_quit_confirm = false;
            }
            KeyCode::Char('y' | 'Y') if self.show_quit_confirm => {
                self.confirm_yes_selected = true;
            }
            KeyCode::Char('n' | 'N') if self.show_quit_confirm => {
                self.confirm_yes_selected = false;
            }
            KeyCode::Left | KeyCode::Right if self.show_quit_confirm => {
                self.confirm_yes_selected = !self.confirm_yes_selected;
            }
            // Default handlers
            KeyCode::Char(c) => {
                self.ui_state
                    .input_buffer
                    .insert(self.ui_state.cursor_pos, c);
                self.ui_state.cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if self.ui_state.cursor_pos > 0 {
                    self.ui_state.cursor_pos -= 1;
                    self.ui_state.input_buffer.remove(self.ui_state.cursor_pos);
                }
            }
            KeyCode::Delete => {
                if self.ui_state.cursor_pos < self.ui_state.input_buffer.len() {
                    self.ui_state.input_buffer.remove(self.ui_state.cursor_pos);
                }
            }
            KeyCode::Left => {
                self.ui_state.cursor_pos = self.ui_state.cursor_pos.saturating_sub(1);
            }
            KeyCode::Right => {
                self.ui_state.cursor_pos =
                    (self.ui_state.cursor_pos + 1).min(self.ui_state.input_buffer.len());
            }
            KeyCode::Home => {
                self.ui_state.cursor_pos = 0;
            }
            KeyCode::End => {
                self.ui_state.cursor_pos = self.ui_state.input_buffer.len();
            }
            KeyCode::Enter => {
                let msg = self.ui_state.input_buffer.clone();
                if !msg.trim().is_empty() {
                    let _ = self.action_sender.send(UiAction::SendMessage(msg));
                }
                self.ui_state.input_buffer.clear();
                self.ui_state.cursor_pos = 0;
            }
            KeyCode::Up | KeyCode::Down => {
                // Command history navigation - placeholder
            }
            // Scroll handling
            KeyCode::PageUp => {
                let max_scroll = self
                    .ui_state
                    .content_height
                    .saturating_sub(self.ui_state.viewport_height);
                self.ui_state.scroll_position = self
                    .ui_state
                    .scroll_position
                    .saturating_sub(10)
                    .min(max_scroll);
                self.ui_state.auto_scroll = false;
            }
            KeyCode::PageDown => {
                let max_scroll = self
                    .ui_state
                    .content_height
                    .saturating_sub(self.ui_state.viewport_height);
                self.ui_state.scroll_position =
                    (self.ui_state.scroll_position + 10).min(max_scroll);
                // Re-enable auto-scroll when reaching bottom
                if self.ui_state.scroll_position >= max_scroll {
                    self.ui_state.auto_scroll = true;
                }
            }
            KeyCode::Esc => {
                // First ESC = show confirmation dialog
                self.show_quit_confirm = true;
                self.confirm_yes_selected = false; // Default to "No"
            }
            _ => {}
        }
    }

    /// Handle input events
    fn handle_input(&mut self, event: Event) {
        match event {
            Event::Key(key_event) => self.handle_keyboard_input(key_event),
            // Mouse wheel scrolling
            Event::Mouse(mouse_event) => match mouse_event.kind {
                MouseEventKind::ScrollUp => {
                    let max_scroll = self
                        .ui_state
                        .content_height
                        .saturating_sub(self.ui_state.viewport_height);
                    self.ui_state.scroll_position = self
                        .ui_state
                        .scroll_position
                        .saturating_sub(3)
                        .min(max_scroll);
                    self.ui_state.auto_scroll = false;
                }
                MouseEventKind::ScrollDown => {
                    let max_scroll = self
                        .ui_state
                        .content_height
                        .saturating_sub(self.ui_state.viewport_height);
                    self.ui_state.scroll_position =
                        (self.ui_state.scroll_position + 3).min(max_scroll);
                    // Re-enable auto-scroll when reaching bottom
                    if self.ui_state.scroll_position >= max_scroll {
                        self.ui_state.auto_scroll = true;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl Ui for ReplUi {
    async fn init(&mut self) -> Result<()> {
        self.terminal = Some(ratatui::init());
        execute!(std::io::stdout(), EnableMouseCapture)?;

        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        let mut events = EventStream::new();
        let mut ticks = time::interval(Duration::from_millis(50));
        while self.running {
            tokio::select! {
                // Handle keyboard events via tokio::select!
                e = events.next() => {
                    if let Some(Ok(e)) = e {
                        self.handle_input(e);
                    }
                }
                _ = ticks.tick() => {
                    let state = self.state_manager.get_state();
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
        execute!(std::io::stdout(), DisableMouseCapture)?;
        self.terminal = None;
        self.running = false;
        Ok(())
    }
}
