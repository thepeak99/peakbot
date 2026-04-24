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
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
    KeyboardEnhancementFlags, KeyModifiers, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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
use crate::ui::repl::spinner;
use crate::ui::repl::todo_panel::{DEFAULT_PANEL_PERCENT, render_todo_panel, should_show_panel};
use crate::ui::ui_trait::{Ui, UiAction};

/// Minimum terminal height
const MIN_TERMINAL_HEIGHT: u16 = 10;

/// Minimum terminal width
const MIN_TERMINAL_WIDTH: u16 = 20;

/// Maximum number of *content* lines the input area will grow to before it
/// stops expanding and starts scrolling internally. Borders add +2 on top.
///
/// Picked as 4 on the "after 3–4 lines it should stop growing" UX brief
/// (chat 2026-04-24).
pub const MAX_INPUT_CONTENT_LINES: u16 = 4;

/// Pure scroll math for the input area.
///
/// Given the logical line the cursor sits on (`cursor_line`), the current
/// scroll offset (`current_scroll`, in lines from the top of the buffer),
/// and the number of visible content lines in the input viewport
/// (`visible_lines`), return the new scroll offset that keeps the cursor
/// visible with minimum movement.
///
/// - If the cursor is above the window, scroll up so the cursor sits on
///   the top visible line.
/// - If the cursor is below the window, scroll down so the cursor sits on
///   the bottom visible line.
/// - Otherwise, keep `current_scroll` unchanged.
///
/// Degenerate: `visible_lines == 0` returns `cursor_line` (safe no-op;
/// real layouts never hit this).
pub fn compute_input_scroll(cursor_line: u16, current_scroll: u16, visible_lines: u16) -> u16 {
    if visible_lines == 0 {
        return cursor_line;
    }
    if cursor_line < current_scroll {
        cursor_line
    } else if cursor_line >= current_scroll.saturating_add(visible_lines) {
        // cursor_line + 1 - visible_lines, saturating
        cursor_line.saturating_add(1).saturating_sub(visible_lines)
    } else {
        current_scroll
    }
}

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
    /// Vertical scroll offset *inside* the input area, measured in visual
    /// (wrapped) rows from the top of the buffer. Only non-zero when the
    /// buffer has more visual rows than `MAX_INPUT_CONTENT_LINES`.
    /// Recomputed every render in `ReplUi::render` based on
    /// `cursor_visual_row` so it naturally tracks both edits and terminal
    /// resizes.
    pub input_scroll: u16,
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
            input_scroll: 0,
            // Start dirty so the first frame always renders, even before any
            // mutation has happened on the StateManager.
            local_dirty: true,
        }
    }

    /// Maximum scroll position (content height minus viewport height)
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height) + 1
    }

    // ─── Input editor: single-char edits ──────────────────────────────────

    /// Insert a character at the cursor and advance the cursor by one char.
    pub fn insert_char(&mut self, c: char) {
        self.input_buffer.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Insert a literal `\n` at the cursor and advance the cursor by one.
    /// This is the "Shift+Enter / Alt+Enter" path.
    pub fn insert_newline(&mut self) {
        self.input_buffer.insert(self.cursor_pos, '\n');
        self.cursor_pos += 1;
    }

    /// Remove the character to the left of the cursor, if any.
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            // Walk back to the previous char boundary (UTF-8 safe).
            let mut new_pos = self.cursor_pos - 1;
            while new_pos > 0 && !self.input_buffer.is_char_boundary(new_pos) {
                new_pos -= 1;
            }
            self.input_buffer.drain(new_pos..self.cursor_pos);
            self.cursor_pos = new_pos;
        }
    }

    /// Remove the character at the cursor, if any.
    pub fn delete(&mut self) {
        if self.cursor_pos < self.input_buffer.len() {
            let mut end = self.cursor_pos + 1;
            while end < self.input_buffer.len() && !self.input_buffer.is_char_boundary(end) {
                end += 1;
            }
            self.input_buffer.drain(self.cursor_pos..end);
        }
    }

    /// Clear the input buffer, cursor, and scroll. Called after submitting.
    pub fn clear_input(&mut self) {
        self.input_buffer.clear();
        self.cursor_pos = 0;
        self.input_scroll = 0;
    }

    // ─── Input editor: cursor navigation ──────────────────────────────────

    /// Move cursor one char left (UTF-8 safe).
    pub fn move_left(&mut self) {
        if self.cursor_pos > 0 {
            let mut new_pos = self.cursor_pos - 1;
            while new_pos > 0 && !self.input_buffer.is_char_boundary(new_pos) {
                new_pos -= 1;
            }
            self.cursor_pos = new_pos;
        }
    }

    /// Move cursor one char right (UTF-8 safe).
    pub fn move_right(&mut self) {
        if self.cursor_pos < self.input_buffer.len() {
            let mut new_pos = self.cursor_pos + 1;
            while new_pos < self.input_buffer.len()
                && !self.input_buffer.is_char_boundary(new_pos)
            {
                new_pos += 1;
            }
            self.cursor_pos = new_pos;
        }
    }

    /// Move cursor to the start of the current logical line (just after the
    /// previous `\n`, or to buffer start on line 0).
    pub fn move_home(&mut self) {
        self.cursor_pos = self.current_line_start();
    }

    /// Move cursor to the end of the current logical line (just before the
    /// next `\n`, or to buffer end on the last line).
    pub fn move_end(&mut self) {
        self.cursor_pos = self.current_line_end();
    }

    /// Move cursor up one logical line, preserving the visual column when
    /// possible (clamped to target line length). At line 0, moves to buffer
    /// start.
    pub fn move_up(&mut self) {
        let line_start = self.current_line_start();
        if line_start == 0 {
            // Already on line 0 — go to buffer start.
            self.cursor_pos = 0;
            return;
        }
        let col = self.cursor_pos - line_start;
        // Previous line spans [prev_start .. line_start - 1] ('\n' at line_start - 1)
        let prev_line_end = line_start - 1; // index of the '\n'
        let prev_line_start = self.input_buffer[..prev_line_end]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_line_len = prev_line_end - prev_line_start;
        let new_col = col.min(prev_line_len);
        self.cursor_pos = prev_line_start + new_col;
    }

    /// Move cursor down one logical line, preserving the visual column when
    /// possible. At the last line, moves to buffer end.
    pub fn move_down(&mut self) {
        let line_start = self.current_line_start();
        let line_end = self.current_line_end();
        let col = self.cursor_pos - line_start;
        if line_end == self.input_buffer.len() {
            // Already on last line — go to buffer end.
            self.cursor_pos = self.input_buffer.len();
            return;
        }
        // Next line starts after the '\n' at `line_end`.
        let next_line_start = line_end + 1;
        let next_line_end = self.input_buffer[next_line_start..]
            .find('\n')
            .map(|i| next_line_start + i)
            .unwrap_or(self.input_buffer.len());
        let next_line_len = next_line_end - next_line_start;
        let new_col = col.min(next_line_len);
        self.cursor_pos = next_line_start + new_col;
    }

    // ─── Input editor: introspection ──────────────────────────────────────

    /// Byte offset of the start of the logical line the cursor sits on.
    fn current_line_start(&self) -> usize {
        self.input_buffer[..self.cursor_pos]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    /// Byte offset of the end of the logical line the cursor sits on
    /// (index of the next `\n`, or buffer length).
    fn current_line_end(&self) -> usize {
        self.input_buffer[self.cursor_pos..]
            .find('\n')
            .map(|i| self.cursor_pos + i)
            .unwrap_or(self.input_buffer.len())
    }

    /// Which 0-indexed logical line the cursor is on (count of `\n`s
    /// before the cursor).
    pub fn cursor_logical_line(&self) -> u16 {
        let count = self.input_buffer[..self.cursor_pos]
            .bytes()
            .filter(|&b| b == b'\n')
            .count();
        count.min(u16::MAX as usize) as u16
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

    /// The welcome banner shown when the chat transcript is empty.
    ///
    /// Single source of truth so the snapshot path
    /// ([`build_chat_history_paragraph`]) and the live cached path
    /// ([`Self::render`]) never drift apart.
    ///
    /// Keybinding hints (Ctrl+C / Ctrl+T) live on the chat frame's
    /// bottom border via [`Self::chat_block`], not here — they're
    /// permanently visible once the transcript fills.
    fn welcome_lines() -> Vec<Line<'static>> {
        vec![Line::from(Span::styled(
            "Welcome to PeakBot! Start a conversation or use /help for commands.",
            Style::default().fg(Color::DarkGray),
        ))]
    }

    /// The bordered block that wraps the chat transcript.
    ///
    /// Single source of truth for both the snapshot path and the live
    /// render path. The bottom border carries a persistent keybinding
    /// hint — right-aligned so the top-left `" Chat Messages "` title
    /// stays the dominant label, and clipped gracefully on narrow
    /// terminals (ratatui truncates titles that don't fit).
    fn chat_block() -> Block<'static> {
        Block::default()
            .title(" Chat Messages ")
            .title_bottom(Line::from(" Ctrl+C exit · Ctrl+T tasks ").right_aligned())
            .borders(Borders::ALL)
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
            message_lines.extend(Self::welcome_lines());
        } else {
            for msg in &chat.messages {
                message_lines.extend(Self::build_chat_message_lines(msg));
            }
        }

        Paragraph::new(Text::from(message_lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: true })
            .block(Self::chat_block())
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

    /// Build the input paragraph with an animated "working" title when the
    /// agent is running.
    ///
    /// When `run_started_at` is `Some`, the block title becomes
    /// `" ⠹ Working · 00:07 · <status> · esc to stop "` (see `workin-baby.md`
    /// §5.3). Otherwise it's the plain `" Input "` title.
    ///
    /// Multiline behaviour: the buffer is split on `\n` into logical lines.
    /// The prompt marker (`> ` or the placeholder) only appears on line 0.
    /// The cursor block (`█`) is rendered inline on whichever logical line
    /// the cursor sits on. See chat 2026-04-24.
    pub fn build_input_paragraph<'a>(
        input: &str,
        cursor_pos: usize,
        is_running: bool,
        run_started_at: Option<std::time::Instant>,
        status_message: Option<&str>,
    ) -> Paragraph<'a> {
        let (prompt_text, prompt_color) = if input.is_empty() {
            ("💬 Message...", Color::DarkGray)
        } else {
            ("> ", Color::Cyan)
        };
        let prompt_span = Span::styled(prompt_text, Style::default().fg(prompt_color));
        let cursor_span = Span::styled("█", Style::default().fg(Color::Yellow));

        // Clamp cursor to buffer length to be safe.
        let cursor = cursor_pos.min(input.len());

        let lines: Vec<Line<'a>> = if input.is_empty() {
            // Empty buffer — placeholder only, no cursor block.
            vec![Line::from(vec![prompt_span])]
        } else {
            // Split buffer into logical lines. Walk once, building:
            //   - the line index the cursor is on
            //   - the cursor's column within that line
            // then assemble one `Line` per logical line with the cursor
            // inserted on the right one.
            let cursor_line_idx: usize = input[..cursor].bytes().filter(|&b| b == b'\n').count();
            let line_start_byte: usize = input[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let col_bytes = cursor - line_start_byte;

            input
                .split('\n')
                .enumerate()
                .map(|(i, line_text)| {
                    let mut spans: Vec<Span<'a>> = Vec::new();
                    if i == 0 {
                        spans.push(prompt_span.clone());
                    }
                    if i == cursor_line_idx {
                        // Cursor lives on this line.
                        let (pre, post) = line_text.split_at(col_bytes.min(line_text.len()));
                        spans.push(Span::raw(pre.to_string()));
                        spans.push(cursor_span.clone());
                        spans.push(Span::raw(post.to_string()));
                    } else {
                        spans.push(Span::raw(line_text.to_string()));
                    }
                    Line::from(spans)
                })
                .collect()
        };

        let title = match (is_running, run_started_at) {
            (true, Some(t)) => {
                // Truncate the phase label to ~24 chars to keep the title
                // readable on narrow terminals.
                let phase_full = status_message.unwrap_or("thinking");
                let phase: String = if phase_full.chars().count() > 24 {
                    phase_full.chars().take(23).collect::<String>() + "…"
                } else {
                    phase_full.to_string()
                };
                format!(
                    " {} Working · {} · {} · esc to stop ",
                    spinner::frame_for(t),
                    spinner::fmt_elapsed(t),
                    phase,
                )
            }
            _ => " Input ".to_string(),
        };

        // Bottom-border hint about newline vs submit. Keybinding hints
        // belong on borders, not content (memory 2026-04-24).
        let hint = Line::from(vec![Span::styled(
            " Shift/Alt+Enter: newline · Enter: send ",
            Style::default().fg(Color::DarkGray),
        )])
        .right_aligned();

        Paragraph::new(Text::from(lines))
            // `trim: false` preserves leading whitespace on pasted/wrapped
            // lines — important for code pastes.
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(title)
                    .title_bottom(hint)
                    .borders(Borders::ALL),
            )
    }

    /// Build a ratatui `Text` from an input buffer the same way
    /// `build_input_paragraph` structures it — one `Line` per
    /// `\n`-separated segment, including trailing empties. Kept in one
    /// place so `input_content_lines` and `cursor_visual_row` stay in sync
    /// with the rendered paragraph.
    fn input_text_lines(buffer: &str) -> Vec<Line<'static>> {
        buffer
            .split('\n')
            .map(|s| Line::from(s.to_string()))
            .collect()
    }

    /// Number of *visual* rows the input buffer occupies at the given
    /// content `width` after ratatui's word-wrap. This is what the user
    /// actually sees — a single long logical line can produce many visual
    /// rows; an empty buffer still occupies 1.
    ///
    /// Uses the same `Wrap { trim: false }` ratatui applies to the real
    /// input paragraph so the counts stay consistent. Does not account for
    /// the 2-column `"> "` prompt on line 0 — callers that need exact parity
    /// with the rendered paragraph should prefer `input.line_count(width)`
    /// on the already-built `Paragraph`.
    pub fn input_content_lines(buffer: &str, width: u16) -> u16 {
        let w = width.max(1);
        let rows = Paragraph::new(Text::from(Self::input_text_lines(buffer)))
            .wrap(Wrap { trim: false })
            .line_count(w);
        rows.max(1).min(u16::MAX as usize) as u16
    }

    /// Total height of the input area in terminal rows (content + top/bottom
    /// borders), capped at `MAX_INPUT_CONTENT_LINES + 2`.
    pub fn input_area_height(buffer: &str, width: u16) -> u16 {
        let content = Self::input_content_lines(buffer, width).min(MAX_INPUT_CONTENT_LINES);
        content + 2
    }

    /// Which 0-indexed *visual* row the cursor sits on after wrapping at
    /// `width` — i.e. how many rendered rows the text *before* the cursor
    /// produces, minus one.
    ///
    /// When the cursor sits at the start of a row (pre-cursor text ends
    /// right at a wrap boundary), we return the previous row's index. The
    /// rendered cursor block is then drawn at col 0 of the next row by
    /// ratatui; scroll-follow keeps that row visible on the next tick if
    /// needed.
    pub fn cursor_visual_row(buffer: &str, cursor_pos: usize, width: u16) -> u16 {
        let end = cursor_pos.min(buffer.len());
        if end == 0 {
            return 0;
        }
        let before = &buffer[..end];
        let w = width.max(1);
        let rows = Paragraph::new(Text::from(Self::input_text_lines(before)))
            .wrap(Wrap { trim: false })
            .line_count(w);
        rows.saturating_sub(1).min(u16::MAX as usize) as u16
    }

    /// Render the input area (takes built paragraph and renders it).
    /// `scroll` is the number of logical lines to skip from the top — used
    /// when the buffer has more lines than `MAX_INPUT_CONTENT_LINES`.
    pub fn render_input_area<'a>(
        f: &mut ratatui::Frame,
        area: Rect,
        paragraph: Paragraph<'a>,
        scroll: u16,
    ) {
        let scrolled = if scroll == 0 {
            paragraph
        } else {
            paragraph.scroll((scroll, 0))
        };
        f.render_widget(scrolled, area);
    }

    /// Number of *content* rows the input paragraph needs at `width`,
    /// stripping the 2 block border rows that ratatui 0.30's
    /// `Paragraph::line_count(width)` folds into its return value when the
    /// paragraph has a block. This is what the layout `Constraint::Length`
    /// math wants: purely the text-carrying rows, not the bordered total.
    ///
    /// See `real_paragraph_line_count_includes_block_borders` for the
    /// invariant this compensates for.
    pub fn paragraph_content_rows(paragraph: &Paragraph<'_>, width: u16) -> u16 {
        let total = paragraph.line_count(width.max(1)) as u16;
        total.saturating_sub(2).max(1)
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
                    state.is_running,
                    state.run_started_at,
                    state.status_message.as_deref(),
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

                // Split main area vertically: chat, input, status.
                // Input height is capped at MAX_INPUT_CONTENT_LINES + 2
                // (borders). Beyond that, `input_scroll` takes over.
                //
                // NB: `paragraph.line_count(width)` on a paragraph with a
                // block includes the 2 border rows in its total. We strip
                // them via `paragraph_content_rows` so `content_rows` is
                // purely the text rows we want to cap at
                // `MAX_INPUT_CONTENT_LINES`. See regression test
                // `real_paragraph_line_count_includes_block_borders`.
                if let Some(main) = main_area {
                    let content_width = main.width.saturating_sub(2).max(1);
                    let wrapped_rows = Self::paragraph_content_rows(&input, content_width);
                    let content_rows = wrapped_rows.min(MAX_INPUT_CONTENT_LINES);
                    let input_height = content_rows + 2;

                    // Scroll-follow in *visual* rows (not logical lines) so
                    // soft-wrapped long lines scroll correctly. Computed at
                    // render time because width is only known here and
                    // resizes also need to re-adjust.
                    let cursor_row = Self::cursor_visual_row(
                        &self.ui_state.input_buffer,
                        self.ui_state.cursor_pos,
                        content_width,
                    );
                    self.ui_state.input_scroll = compute_input_scroll(
                        cursor_row,
                        self.ui_state.input_scroll,
                        content_rows,
                    );

                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Percentage(100),
                            Constraint::Length(input_height),
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
                        Paragraph::new(Text::from(Self::welcome_lines()))
                        .style(Style::default().fg(Color::White))
                        .wrap(Wrap { trim: true })
                        .block(Self::chat_block())
                    } else {
                        Paragraph::new(Text::from(view.lines))
                            .style(Style::default().fg(Color::White))
                            .wrap(Wrap { trim: true })
                            .block(Self::chat_block())
                    };

                    Self::render_chat_history(
                        f,
                        chunks[0],
                        view.inner_scroll,
                        chat_history,
                        self.ui_state.content_height,
                    );
                    Self::render_input_area(f, chunks[1], input, self.ui_state.input_scroll);
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
            // Quit with Ctrl+C (opens confirmation dialog)
            KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_quit_confirm = true;
                self.confirm_yes_selected = false; // Default to "No"
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
            // Default handlers — input buffer editing. `input_scroll` is
            // recomputed at render time using the real terminal width and
            // `cursor_visual_row`, so key handlers only mutate the buffer
            // and cursor.
            KeyCode::Char(c) => {
                self.ui_state.insert_char(c);
            }
            KeyCode::Backspace => {
                self.ui_state.backspace();
            }
            KeyCode::Delete => {
                self.ui_state.delete();
            }
            KeyCode::Left => {
                self.ui_state.move_left();
            }
            KeyCode::Right => {
                self.ui_state.move_right();
            }
            KeyCode::Home => {
                self.ui_state.move_home();
            }
            KeyCode::End => {
                self.ui_state.move_end();
            }
            // Shift+Enter or Alt+Enter → insert newline (multiline input).
            // Shift+Enter only works on terminals that report the Kitty
            // keyboard protocol (kitty, foot, wezterm, ghostty, recent
            // alacritty). Alt+Enter is the universal fallback.
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.ui_state.insert_newline();
            }
            KeyCode::Enter => {
                let msg = self.ui_state.input_buffer.clone();
                if !msg.trim().is_empty() {
                    let _ = self.action_sender.send(UiAction::SendMessage(msg));
                }
                self.ui_state.clear_input();
            }
            // Up/Down navigate between logical lines within the input.
            // Command history is not useful for an agent (each turn is a
            // one-shot instruction, not a repeatable command).
            KeyCode::Up => {
                self.ui_state.move_up();
            }
            KeyCode::Down => {
                self.ui_state.move_down();
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
                // Esc interrupts the agent when it's running.
                // When idle, Esc is a no-op — use Ctrl+C to quit.
                if self.state_manager.is_running() {
                    let _ = self.action_sender.send(UiAction::RequestStop);
                }
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

        // Try to enable the Kitty keyboard protocol so we can distinguish
        // Shift+Enter from plain Enter (for multiline input). This works on
        // kitty, foot, wezterm, ghostty, and recent alacritty. On terminals
        // that don't support it (xterm, Terminal.app, vanilla gnome-terminal)
        // the push fails silently — Alt+Enter still works there as a
        // universal fallback.
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );

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
                        || size != self.last_size
                        || self.state_manager.is_running(); // animate the spinner while working

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
        // Pop the kitty keyboard flags before leaving the alt screen. No-op
        // if they were never successfully pushed.
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        execute!(io::stdout(), LeaveAlternateScreen)?;
        execute!(std::io::stdout(), DisableMouseCapture)?;
        self.terminal = None;
        self.running = false;
        Ok(())
    }
}


#[cfg(test)]
mod multiline_input_tests {
    //! Test-first specs for multiline input support.
    //! See `multiline-input.md` and chat 2026-04-24.
    //!
    //! These tests pin down:
    //!   - `UiState` editor methods (insert_newline, move_up/down, home/end per-line)
    //!   - `compute_input_scroll` (pure scroll math)
    //!   - `ReplUi::input_content_lines` / `input_area_height` helpers
    //!   - `MAX_INPUT_CONTENT_LINES` constant = 4
    //!
    //! Rendering snapshots live in `tests/repl_tests.rs`.

    use super::*;

    // ─── Editor: insert_newline ────────────────────────────────────────────

    #[test]
    fn insert_newline_at_end_of_empty_buffer_adds_one_newline() {
        let mut s = UiState::new();
        s.insert_newline();
        assert_eq!(s.input_buffer, "\n");
        assert_eq!(s.cursor_pos, 1);
    }

    #[test]
    fn insert_newline_at_end_of_text_appends_newline() {
        let mut s = UiState::new();
        s.input_buffer = "abc".to_string();
        s.cursor_pos = 3;
        s.insert_newline();
        assert_eq!(s.input_buffer, "abc\n");
        assert_eq!(s.cursor_pos, 4);
    }

    #[test]
    fn insert_newline_in_middle_splits_line() {
        let mut s = UiState::new();
        s.input_buffer = "abcdef".to_string();
        s.cursor_pos = 3;
        s.insert_newline();
        assert_eq!(s.input_buffer, "abc\ndef");
        assert_eq!(s.cursor_pos, 4); // just past the newline, start of new line
    }

    // ─── Editor: move_up / move_down ───────────────────────────────────────

    #[test]
    fn move_up_from_single_line_goes_to_start() {
        let mut s = UiState::new();
        s.input_buffer = "hello".to_string();
        s.cursor_pos = 3;
        s.move_up();
        assert_eq!(s.cursor_pos, 0);
    }

    #[test]
    fn move_up_preserves_column() {
        let mut s = UiState::new();
        s.input_buffer = "hello\nworld".to_string();
        // cursor on 'r' of "world" -> pos 8, col 2 on line 1
        s.cursor_pos = 8;
        s.move_up();
        // now on line 0 at col 2 -> pos 2 (on 'l' of "hello")
        assert_eq!(s.cursor_pos, 2);
    }

    #[test]
    fn move_up_clamps_to_shorter_line_length() {
        let mut s = UiState::new();
        s.input_buffer = "hi\nworld".to_string();
        // cursor at col 4 on line 1 ("worl" + on 'd')
        s.cursor_pos = 7; // just before 'd', col 4
        s.move_up();
        // line 0 is "hi" (len 2), cursor clamps to end of line 0 -> pos 2
        assert_eq!(s.cursor_pos, 2);
    }

    #[test]
    fn move_down_from_single_line_goes_to_end() {
        let mut s = UiState::new();
        s.input_buffer = "hello".to_string();
        s.cursor_pos = 2;
        s.move_down();
        assert_eq!(s.cursor_pos, 5);
    }

    #[test]
    fn move_down_preserves_column() {
        let mut s = UiState::new();
        s.input_buffer = "hello\nworld".to_string();
        s.cursor_pos = 2; // col 2 on line 0, on 'l'
        s.move_down();
        // line 1 at col 2 -> pos 8 (hello\nwo| -> 'r')
        assert_eq!(s.cursor_pos, 8);
    }

    #[test]
    fn move_down_clamps_to_shorter_line_length() {
        let mut s = UiState::new();
        s.input_buffer = "world\nhi".to_string();
        s.cursor_pos = 4; // col 4 on line 0, on 'd'
        s.move_down();
        // line 1 is "hi" (len 2), col clamps to 2 -> pos 5+1+2 = 8
        assert_eq!(s.cursor_pos, 8);
    }

    // ─── Editor: per-line home/end ─────────────────────────────────────────

    #[test]
    fn move_home_goes_to_start_of_current_logical_line() {
        let mut s = UiState::new();
        s.input_buffer = "abc\ndef".to_string();
        s.cursor_pos = 6; // on 'f' of "def"
        s.move_home();
        // start of line 1 = pos 4 (just after the '\n' at index 3)
        assert_eq!(s.cursor_pos, 4);
    }

    #[test]
    fn move_home_at_first_line_goes_to_buffer_start() {
        let mut s = UiState::new();
        s.input_buffer = "abc\ndef".to_string();
        s.cursor_pos = 2;
        s.move_home();
        assert_eq!(s.cursor_pos, 0);
    }

    #[test]
    fn move_end_goes_to_end_of_current_logical_line() {
        let mut s = UiState::new();
        s.input_buffer = "abc\ndef".to_string();
        s.cursor_pos = 4; // start of line 1
        s.move_end();
        // end of line 1 = buffer.len() = 7
        assert_eq!(s.cursor_pos, 7);
    }

    #[test]
    fn move_end_on_non_last_line_stops_before_newline() {
        let mut s = UiState::new();
        s.input_buffer = "abc\ndef".to_string();
        s.cursor_pos = 1; // on 'b' of line 0
        s.move_end();
        // end of line 0 = pos 3 (before '\n')
        assert_eq!(s.cursor_pos, 3);
    }

    // ─── Editor: clear resets scroll too ───────────────────────────────────

    #[test]
    fn clear_input_resets_buffer_cursor_and_scroll() {
        let mut s = UiState::new();
        s.input_buffer = "abc\ndef\nghi".to_string();
        s.cursor_pos = 8;
        s.input_scroll = 5;
        s.clear_input();
        assert_eq!(s.input_buffer, "");
        assert_eq!(s.cursor_pos, 0);
        assert_eq!(s.input_scroll, 0);
    }

    // ─── Editor: visual cursor line ────────────────────────────────────────

    #[test]
    fn cursor_logical_line_counts_newlines_before_cursor() {
        let mut s = UiState::new();
        s.input_buffer = "a\nb\nc\nd".to_string();
        s.cursor_pos = 0;
        assert_eq!(s.cursor_logical_line(), 0);
        s.cursor_pos = 2; // on 'b'
        assert_eq!(s.cursor_logical_line(), 1);
        s.cursor_pos = 4; // on 'c'
        assert_eq!(s.cursor_logical_line(), 2);
        s.cursor_pos = 7; // after 'd'
        assert_eq!(s.cursor_logical_line(), 3);
    }

    // ─── Scroll math: compute_input_scroll ─────────────────────────────────

    #[test]
    fn compute_input_scroll_cursor_within_window_keeps_scroll() {
        // window: lines 2..2+4 = [2,3,4,5]; cursor at line 3 -> no change
        assert_eq!(compute_input_scroll(3, 2, 4), 2);
        assert_eq!(compute_input_scroll(2, 2, 4), 2); // at top edge
        assert_eq!(compute_input_scroll(5, 2, 4), 2); // at bottom edge
    }

    #[test]
    fn compute_input_scroll_cursor_above_window_scrolls_up() {
        // cursor at line 1, current scroll 3 -> new scroll = 1
        assert_eq!(compute_input_scroll(1, 3, 4), 1);
        assert_eq!(compute_input_scroll(0, 5, 4), 0);
    }

    #[test]
    fn compute_input_scroll_cursor_below_window_scrolls_down() {
        // window height 4, cursor at line 6, scroll 0 -> new scroll = 6 + 1 - 4 = 3
        assert_eq!(compute_input_scroll(6, 0, 4), 3);
        assert_eq!(compute_input_scroll(10, 0, 4), 7);
    }

    #[test]
    fn compute_input_scroll_zero_visible_lines_is_safe() {
        // Degenerate: avoid underflow.
        assert_eq!(compute_input_scroll(5, 2, 0), 5);
    }

    // ─── Pure helpers on ReplUi ────────────────────────────────────────────

    #[test]
    fn input_content_lines_single_line() {
        assert_eq!(ReplUi::input_content_lines("hello", 60), 1);
    }

    #[test]
    fn input_content_lines_counts_newlines() {
        assert_eq!(ReplUi::input_content_lines("a\nb\nc", 60), 3);
        assert_eq!(ReplUi::input_content_lines("a\nb\nc\n", 60), 4); // trailing newline = empty final line
    }

    #[test]
    fn input_content_lines_empty_is_one() {
        // Empty buffer still shows the placeholder on one line.
        assert_eq!(ReplUi::input_content_lines("", 60), 1);
    }

    #[test]
    fn input_area_height_caps_at_max_plus_borders() {
        // 8 logical lines should cap at MAX_INPUT_CONTENT_LINES (4) + 2 borders = 6.
        let tall_buffer = "a\nb\nc\nd\ne\nf\ng\nh";
        assert_eq!(
            ReplUi::input_area_height(tall_buffer, 60),
            MAX_INPUT_CONTENT_LINES + 2
        );
    }

    #[test]
    fn input_area_height_small_buffers_stay_small() {
        assert_eq!(ReplUi::input_area_height("", 60), 3); // 1 content + 2 borders
        assert_eq!(ReplUi::input_area_height("a\nb", 60), 4); // 2 content + 2 borders
    }

    #[test]
    fn max_input_content_lines_constant_is_four() {
        assert_eq!(MAX_INPUT_CONTENT_LINES, 4);
    }

    /// Regression probe for the bug reported 2026-04-24 (late): the real
    /// input paragraph has `Borders::ALL`, and ratatui 0.30's
    /// `Paragraph::line_count(width)` on a paragraph-with-a-block returns
    /// `content_rows + border_rows` (i.e. includes the 2 border rows of the
    /// block in the count). Callers that need just the *content* row count
    /// must subtract those block rows themselves. If this invariant ever
    /// changes upstream, this test catches it.
    #[test]
    fn real_paragraph_line_count_includes_block_borders() {
        let empty = ReplUi::build_input_paragraph("", 0, false, None, None);
        // 1 content line (placeholder) + 2 border rows = 3.
        assert_eq!(empty.line_count(120), 3);

        let one_newline = ReplUi::build_input_paragraph("\n", 1, false, None, None);
        // 2 content lines + 2 border rows = 4.
        assert_eq!(one_newline.line_count(120), 4);

        let two_newlines = ReplUi::build_input_paragraph("\n\n", 2, false, None, None);
        // 3 content lines + 2 border rows = 5.
        assert_eq!(two_newlines.line_count(120), 5);
    }

    /// What the render path actually needs: the number of *content* rows
    /// (i.e. how many rows of the input box are for user text, excluding
    /// the 2 border rows). Expressed as a tiny helper on top of
    /// `paragraph.line_count()`, this is what must feed into the
    /// `Constraint::Length(input_height)` math in `ReplUi::render`.
    #[test]
    fn paragraph_content_rows_strips_block_borders() {
        let empty = ReplUi::build_input_paragraph("", 0, false, None, None);
        assert_eq!(ReplUi::paragraph_content_rows(&empty, 120), 1);

        let one_newline = ReplUi::build_input_paragraph("\n", 1, false, None, None);
        assert_eq!(ReplUi::paragraph_content_rows(&one_newline, 120), 2);

        let two_newlines = ReplUi::build_input_paragraph("\n\n", 2, false, None, None);
        assert_eq!(ReplUi::paragraph_content_rows(&two_newlines, 120), 3);
    }

    // ─── Wrapped (visual) line behaviour ──────────────────────────────────
    //
    // These pin down the fix for the bug reported 2026-04-24 (multiline
    // input scroll breaks on soft-wrap). Long logical lines must grow the
    // input area up to the cap, and the cursor must scroll-follow in visual
    // rows, not logical lines.

    #[test]
    fn input_content_lines_counts_wrapped_rows_not_logical_lines() {
        // 50 'a's at width 10 → wraps into 5 visual rows even though it's
        // only one logical line.
        let long = "a".repeat(50);
        assert_eq!(ReplUi::input_content_lines(&long, 10), 5);
    }

    #[test]
    fn input_area_height_caps_long_wrapped_line() {
        // 80 'a's at width 10 → 8 visual rows, capped at MAX + 2 = 6.
        let long = "a".repeat(80);
        assert_eq!(
            ReplUi::input_area_height(&long, 10),
            MAX_INPUT_CONTENT_LINES + 2
        );
    }

    #[test]
    fn cursor_visual_row_on_unwrapped_text_is_zero() {
        assert_eq!(ReplUi::cursor_visual_row("hello", 3, 60), 0);
        assert_eq!(ReplUi::cursor_visual_row("", 0, 60), 0);
    }

    #[test]
    fn cursor_visual_row_tracks_logical_newlines() {
        // "a\nb\nc" — cursor at pos 4 (start of "c") → row 2.
        assert_eq!(ReplUi::cursor_visual_row("a\nb\nc", 4, 60), 2);
    }

    #[test]
    fn cursor_visual_row_tracks_soft_wrap() {
        // 25 'a's at width 10 → cursor at the end sits on visual row 2
        // (rows 0, 1, 2 hold 10+10+5 chars).
        let long = "a".repeat(25);
        assert_eq!(ReplUi::cursor_visual_row(&long, 25, 10), 2);
        // Cursor at position 10 is at the end of row 0 / start of row 1 —
        // either 0 or 1 is acceptable; pin down the "end of previous row"
        // reading which is what ratatui's line_count returns.
        assert_eq!(ReplUi::cursor_visual_row(&long, 10, 10), 0);
    }
}
