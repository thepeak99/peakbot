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
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseEventKind,
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

use crate::state::StateManager;
use crate::ui::ChatMessage;
use crate::ui::app_state::{AppState, ChatState};
use crate::ui::repl::message_renderer::{MessageRenderer, PlainRenderer};
use crate::ui::repl::render_cache::ChatRenderCache;
use crate::ui::repl::todo_panel::{DEFAULT_PANEL_PERCENT, render_todo_panel, should_show_panel};
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
    /// Scroll position in todo panel
    pub todo_scroll_position: u16,
    /// Set to true whenever local (view-only) state that affects rendering
    /// changes — e.g. input buffer, scroll, quit dialog. The render loop
    /// combines this with `StateManager::revision()` to decide whether a
    /// redraw is necessary. Reset to `false` after each successful render.
    pub local_dirty: bool,
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
            todo_scroll_position: 0,
            // Start dirty so the first frame always renders, even before any
            // mutation has happened on the StateManager.
            local_dirty: true,
        }
    }

    /// Maximum scroll position (content height minus viewport height)
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height) + 1
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
    /// Last state revision we successfully rendered. Used with `local_dirty`
    /// to skip idle-tick redraws. See `slow-messages.md` §4.4.
    last_rendered_revision: u64,
    /// Last terminal size we laid out for. A mismatch (resize) forces a
    /// render even when nothing else changed.
    last_size: (u16, u16),
    /// Per-message rendered-line cache. Holds pre-built `Line`s keyed by a
    /// cheap fingerprint, plus per-message wrapped heights and a prefix-sum
    /// for O(log N) viewport lookup. See `slow-messages.md` §4.1.
    chat_cache: ChatRenderCache,
}

impl ReplUi {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        Self::with_renderer(state_manager, action_sender, Box::new(PlainRenderer))
    }

    /// Construct a `ReplUi` with a custom [`MessageRenderer`] — the seam
    /// through which a future markdown renderer slots in without touching
    /// anything else in this file. See `slow-messages.md` §4.2.
    pub fn with_renderer(
        state_manager: Arc<StateManager>,
        action_sender: UnboundedSender<UiAction>,
        renderer: Box<dyn MessageRenderer>,
    ) -> Self {
        Self {
            state_manager,
            action_sender,
            running: true,
            terminal: None,
            ui_state: UiState::new(),
            show_quit_confirm: false,
            confirm_yes_selected: false,
            last_rendered_revision: 0,
            last_size: (0, 0),
            chat_cache: ChatRenderCache::new(renderer),
        }
    }

    /// Build the full chat-history paragraph from scratch.
    ///
    /// Kept for backwards compatibility (the snapshot-test suite in
    /// `tests/repl_tests.rs` uses this). The live render path no longer
    /// calls this — it goes through [`ChatRenderCache`] instead, which is
    /// what makes rendering independent of history size. See
    /// `slow-messages.md`.
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

    /// Render a single chat message into owned `Line`s.
    ///
    /// Thin wrapper over [`PlainRenderer`] preserved for the existing
    /// snapshot tests. Prefer calling the renderer (or the cache) in new
    /// code.
    pub fn build_chat_message_lines(msg: &ChatMessage) -> Vec<Line<'static>> {
        PlainRenderer.render(msg)
    }

    /// Render the chat history area with scrollbar.
    ///
    /// `content_height` is the total wrapped line count, passed in from the
    /// caller so we don't recompute word-wrap here. The same value was
    /// already computed by `render()` to drive scrolling/layout — previously
    /// we re-ran `paragraph.line_count(width)` in this function, duplicating
    /// O(N·M·W) work on every frame. See `slow-messages.md`.
    pub fn render_chat_history(
        f: &mut ratatui::Frame,
        area: Rect,
        scroll: u16,
        paragraph: Paragraph,
        content_height: u16,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100), Constraint::Length(1)])
            .split(area);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let mut scroll_state = ScrollbarState::new(content_height as usize)
            .position((scroll + area.height - 2) as usize);
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

        Paragraph::new(Line::from(spans))
            .wrap(Wrap { trim: true })
            .block(Block::default().title(" Input ").borders(Borders::ALL))
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
            Line::from(vec![Span::styled(
                warning,
                Style::default().fg(Color::LightRed),
            )]),
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
            Line::from(vec![Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )]),
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

                // Check if todo panel should be shown (based on terminal size and visibility state)
                let show_todo = state.todo.visible && should_show_panel(size.width);

                // Layout: either full width (no todo) or split horizontally (with todo)
                let (main_area, todo_area) = if show_todo {
                    let chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Percentage(100 - DEFAULT_PANEL_PERCENT),
                            Constraint::Percentage(DEFAULT_PANEL_PERCENT),
                        ])
                        .split(size);
                    (Some(chunks[0]), Some(chunks[1]))
                } else {
                    (Some(size), None)
                };

                // Split main area vertically: chat, input, status
                if let Some(main) = main_area {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Percentage(100),
                            Constraint::Min(input.line_count(main.width - 2) as u16),
                            Constraint::Length(1),
                        ])
                        .split(main);

                    // Width inside the chat block borders is (main.width - 2).
                    let chat_wrap_width = main.width.saturating_sub(2);

                    // Sync per-message cache against current messages at
                    // current width. Only mutated rows are re-rendered;
                    // only mutated/resized rows are re-wrapped. See
                    // `slow-messages.md` §4.1.
                    self.chat_cache
                        .sync(&state.chat.messages, chat_wrap_width);

                    self.ui_state.viewport_height = chunks[0].height;
                    self.ui_state.content_height =
                        self.chat_cache.total_height().min(u16::MAX as u32) as u16;

                    // Calculate scroll based on auto_scroll setting
                    let max_scroll = self.ui_state.max_scroll();
                    let scroll = if self.ui_state.auto_scroll {
                        // Scroll to bottom
                        max_scroll
                    } else {
                        // Use stored position (clamped to valid range)
                        self.ui_state.scroll_position.min(max_scroll)
                    };

                    // Build the viewport-sized paragraph from the cache.
                    // Work here is O(viewport), independent of history size.
                    // `window()` returns both the Lines covering the viewport
                    // AND the partial-line offset into the first visible
                    // message; we feed the offset straight into `Paragraph::scroll`.
                    let view = self.chat_cache.window(scroll as u32, chunks[0].height);
                    let chat_history = if view.lines.is_empty() && state.chat.messages.is_empty()
                    {
                        // Empty transcript — show the welcome banner.
                        Paragraph::new(Line::from(Span::styled(
                            "Welcome to PeakBot! Start a conversation or use /help for commands.",
                            Style::default().fg(Color::DarkGray),
                        )))
                        .style(Style::default().fg(Color::White))
                        .wrap(Wrap { trim: true })
                        .block(
                            Block::default()
                                .title(" Chat Messages ")
                                .borders(Borders::ALL),
                        )
                    } else {
                        Paragraph::new(Text::from(view.lines))
                            .style(Style::default().fg(Color::White))
                            .wrap(Wrap { trim: true })
                            .block(
                                Block::default()
                                    .title(" Chat Messages ")
                                    .borders(Borders::ALL),
                            )
                    };

                    Self::render_chat_history(
                        f,
                        chunks[0],
                        view.inner_scroll,
                        chat_history,
                        self.ui_state.content_height,
                    );
                    Self::render_input_area(f, chunks[1], input);
                    Self::render_status_bar(f, chunks[2], state);
                }

                // Render todo panel if visible
                if let Some(todo_rect) = todo_area {
                    render_todo_panel(
                        f,
                        todo_rect,
                        &state.todo,
                        self.ui_state.todo_scroll_position,
                    );
                }

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
            // Toggle todo panel with Ctrl+T
            KeyCode::Char('t' | 'T') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state_manager.toggle_todo_panel();
                // Reset scroll position when toggling
                self.ui_state.todo_scroll_position = 0;
            }
            // Scroll todo panel with Ctrl+Up/Down when panel is visible
            KeyCode::Up
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.state_manager.is_todo_panel_visible() =>
            {
                self.ui_state.todo_scroll_position =
                    self.ui_state.todo_scroll_position.saturating_sub(1);
            }
            KeyCode::Down
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.state_manager.is_todo_panel_visible() =>
            {
                // Simple increment - will be clamped during render
                self.ui_state.todo_scroll_position += 1;
            }
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
                // Command history navigation
            }
            // Scroll handling
            KeyCode::PageUp => {
                let max_scroll = self.ui_state.max_scroll();
                self.ui_state.scroll_position = self
                    .ui_state
                    .scroll_position
                    .saturating_sub(10)
                    .min(max_scroll);
                self.ui_state.auto_scroll = false;
            }
            KeyCode::PageDown => {
                let max_scroll = self.ui_state.max_scroll();
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
                    let max_scroll = self.ui_state.max_scroll();
                    self.ui_state.scroll_position = self
                        .ui_state
                        .scroll_position
                        .saturating_sub(3)
                        .min(max_scroll);
                    self.ui_state.auto_scroll = false;
                }
                MouseEventKind::ScrollDown => {
                    let max_scroll = self.ui_state.max_scroll();
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
                        // Any input event may have changed local view state
                        // (input buffer, cursor, scroll, dialogs). Mark
                        // dirty so the next tick renders.
                        self.ui_state.local_dirty = true;
                        self.handle_input(e);
                    }
                }
                _ = ticks.tick() => {
                    // Skip-idle-tick: redraw only when something meaningful
                    // has changed. `revision()` covers all StateManager
                    // mutations; `local_dirty` covers view-only changes;
                    // `last_size` catches terminal resize.
                    let revision = self.state_manager.revision();
                    let size = self.terminal
                        .as_ref()
                        .and_then(|t| t.size().ok())
                        .map(|s| (s.width, s.height))
                        .unwrap_or((0, 0));

                    let needs_render = revision != self.last_rendered_revision
                        || self.ui_state.local_dirty
                        || size != self.last_size;

                    if !needs_render {
                        continue;
                    }

                    let state = self.state_manager.get_state();
                    self.render(&state)?;
                    self.last_rendered_revision = revision;
                    self.last_size = size;
                    self.ui_state.local_dirty = false;
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
