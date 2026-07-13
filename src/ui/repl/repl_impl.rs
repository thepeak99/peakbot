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
    KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
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

use crate::PEAKBOOT_VERSION;
use crate::state::StateManager;
use crate::ui::ChatMessage;
use crate::ui::app_state::{AppState, ChatState};
use crate::ui::repl::bash_panel::{
    effective_panel_height as bash_effective_panel_height, render_bash_panel,
};
use crate::ui::repl::confirm_dialog::{ConfirmAction, ConfirmDialog, render_confirm_dialog};
use crate::ui::repl::markdown::MarkdownRenderer;
use crate::ui::repl::message_renderer::{MessageRenderer, PlainRenderer};
use crate::ui::repl::render_cache::ChatRenderCache;
use crate::ui::repl::spinner;
use crate::ui::repl::todo_panel::{DEFAULT_PANEL_PERCENT, render_todo_panel, should_show_panel};
use crate::ui::ui_trait::{CommandPopupState, CompletionItem, PopupMode, Ui, UiAction};

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

/// Display-cell width available for chat content, given the full chat
/// pane width and whether select mode is engaged.
///
/// Normal mode draws:
/// - the chat block (left + right border = 2 cells), and
/// - a vertical scrollbar in the rightmost column (1 cell, see
///   [`ReplUi::render_chat_history`]).
///
/// So `main.width - 3` is what's actually drawable.
///
/// Select mode strips ALL chrome — no block, no scrollbar — so the
/// full `main.width` is drawable.
///
/// This number is fed into [`crate::ui::repl::ChatRenderCache::sync`]
/// and through the renderer pipeline; width-sensitive renderers (e.g.
/// [`crate::ui::repl::MarkdownRenderer`] for tables and fenced code
/// rules) lay out to fit exactly. Pre-fix this returned `main.width - 2`
/// in normal mode, causing code-block bottom rules to overflow into the
/// scrollbar column — visible only as the closing `┘` glyph being eaten.
/// Pinned by `chat_pane_content_width_subtracts_borders_and_scrollbar`.
pub fn chat_pane_content_width(main_width: u16, select_mode: bool) -> u16 {
    if select_mode {
        main_width
    } else {
        main_width.saturating_sub(3)
    }
}

/// Resolve a `/cd` argument to a canonical, absolute directory path.
///
/// Expands a leading `~`/`~/` to `$HOME`, resolves relative paths against
/// the current cwd, then canonicalises. Returns a human-readable error
/// (no filesystem mutation) when the path doesn't exist or isn't a
/// directory — the caller surfaces it as a system message and never
/// dispatches the switch. Pure enough to unit-test against a temp dir.
pub fn resolve_cd_path(arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    let expanded: std::path::PathBuf = if arg == "~" {
        std::path::PathBuf::from(std::env::var("HOME").map_err(|_| "$HOME is not set".to_string())?)
    } else if let Some(rest) = arg.strip_prefix("~/") {
        let home = std::env::var("HOME").map_err(|_| "$HOME is not set".to_string())?;
        std::path::PathBuf::from(home).join(rest)
    } else {
        std::path::PathBuf::from(arg)
    };
    // canonicalize() resolves relative paths against the process cwd and
    // fails on a non-existent path — exactly the "clear error before any
    // mutation" guarantee we want.
    let canonical = expanded
        .canonicalize()
        .map_err(|e| format!("cannot resolve `{arg}`: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!("not a directory: {}", canonical.display()));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

/// Render a directory path for a tight, single-line surface (the status
/// bar). Abbreviates a `$HOME` prefix to `~`, then — if still wider than
/// `max_cells` characters — keeps the trailing segment (the part the user
/// cares about) behind a leading `…`. Never widens; a short path is
/// returned unchanged.
pub fn abbreviate_path(path: &std::path::Path, max_cells: usize) -> String {
    let full = path.to_string_lossy();
    let shortened = match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => {
            format!("~{}", &full[home.len()..])
        }
        _ => full.into_owned(),
    };
    let count = shortened.chars().count();
    if count <= max_cells {
        return shortened;
    }
    // Keep the tail (leaf dir) — a leading `…` costs one cell.
    let keep = max_cells.saturating_sub(1);
    let tail: String = shortened.chars().skip(count - keep).collect();
    format!("…{tail}")
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
    /// "Select mode": when `true`, mouse capture is *off* and the user can
    /// drag-select chat text using their terminal's native selection UI
    /// (and copy with the terminal's native shortcut — Ctrl+Shift+C,
    /// Cmd+C, right-click → Copy, etc.). Toggled with F4. While true,
    /// mouse-wheel scroll stops working — keyboard scroll (PgUp/PgDn,
    /// arrows) keeps working. See `copy-and-paste-me-baby.md`.
    pub select_mode: bool,
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
            select_mode: false,
        }
    }

    /// Maximum scroll position — the offset that bottom-aligns the last
    /// content line with the last inner viewport row.
    ///
    /// `viewport_height` MUST be the inner content area height (outer
    /// bordered chat block minus its 2 border rows — see the assignment
    /// in `ReplUi::render`). The formula is then exact:
    /// `content_height - viewport_height`.
    ///
    /// Pre-fix (2026-04-24 bug) this returned `content - viewport + 1`,
    /// which bottom-aligned the *penultimate* line and left the last
    /// chat line clipped under the block's bottom border forever.
    /// See `chat_scroll_tests` below.
    pub fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// Resolve the line offset to render this frame AND persist it back
    /// into `scroll_position`.
    ///
    /// When pinned to the bottom (`auto_scroll`), the offset is
    /// `max_scroll`; otherwise it's the stored position clamped to range.
    /// Writing it back keeps `scroll_position` truthful so the scroll
    /// handlers always start from where the viewport actually is — without
    /// this, the first scroll-up off the bottom computed from a stale
    /// `scroll_position` and teleported the view (issue #31).
    pub fn effective_scroll(&mut self) -> u16 {
        let max_scroll = self.max_scroll();
        let scroll = if self.auto_scroll {
            max_scroll
        } else {
            self.scroll_position.min(max_scroll)
        };
        self.scroll_position = scroll;
        scroll
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
            while new_pos < self.input_buffer.len() && !self.input_buffer.is_char_boundary(new_pos)
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
    /// Active modal confirmation dialog. `None` when closed.
    /// Generalised from the original `show_quit_confirm` +
    /// `confirm_yes_selected` bool pair when `/model` became the
    /// second consumer. See `confirm_dialog.rs`.
    pub(crate) confirm_dialog: Option<ConfirmDialog>,
    /// Optional model registry — when present, `/model <alias>`
    /// submissions are intercepted and validated before any action is
    /// dispatched. Tests + the legacy single-provider boot path leave
    /// this `None` and `/model` falls through unchanged.
    pub(crate) model_registry: Option<Arc<crate::config::ModelRegistry>>,
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
    /// Slash-command autocomplete popup — `None` when closed, `Some` when
    /// open. Local to the REPL view (see `allehailmenu.md` §3: popup state
    /// is view-only, mirrors the `show_quit_confirm` precedent rather than
    /// going through `StateManager`).
    pub(crate) command_popup: Option<CommandPopupState>,
    /// **Sticky dismissal flag for the slash-command popup.** Set to
    /// `true` when the user *explicitly* closes the popup (Esc,
    /// accept_command via Tab/Enter, Shift/Alt+Enter). While `true`,
    /// `sync_popup` refuses to auto-reopen the popup even when the
    /// buffer matches a valid command-prefix pattern — respecting the
    /// "user said no" semantic.
    ///
    /// Reset to `false` when:
    /// - The buffer becomes empty (fresh slate), or
    /// - The explicit `/` open arm fires (user-driven reopen).
    ///
    /// Distinct from sync_popup's *reactive* closes (whitespace inside
    /// the prefix) — those leave the flag at `false` so that backspacing
    /// out the offending whitespace restores the popup. See
    /// `allehailmenu.md` §5.2 and the procedural rule "sync_popup must
    /// not auto-open what accept_command just closed".
    pub(crate) popup_dismissed: bool,
    /// **Multiline compose mode.** When `true`, plain `Enter` inserts a
    /// newline instead of submitting; `Ctrl+G` toggles the mode (or
    /// submits + exits when already on). View-only ephemeral state —
    /// see `multiline-mode.md`.
    pub(crate) multiline_mode: bool,

    /// **Foreground bash stdin buffer (slice 4 of #11).** Characters
    /// typed by the user while [`Self::stdin_focused`] is true,
    /// accumulating until `Enter` forwards them to the running PTY
    /// child via [`StateManager::try_forward_bash_stdin`]. View-only
    /// ephemeral state — same lifecycle as `input_buffer` and
    /// `multiline_mode` (lost on REPL restart, intentional: a fresh
    /// REPL has no live bash to receive the bytes).
    ///
    /// **NOT persisted** because:
    /// - A `/new` / `/load` / `/model` clears the bash panel and the
    ///   live PTY child it was feeding. A buffer surviving across that
    ///   transition would be addressed to nobody.
    /// - The buffer commonly contains passwords (`sudo`, `git`
    ///   credential prompts). Persisting bytes typed in a focused
    ///   secret-input UI is the wrong default.
    pub(crate) stdin_buffer: String,

    /// **Foreground bash stdin focus flag (slice 4 of #11).** When
    /// `true`, raw key events (`Char`, `Backspace`, `Enter`) route to
    /// the stdin buffer instead of the chat input. Set by `Ctrl+S`
    /// while the bash panel is `Running`; cleared by `Esc` (preserves
    /// the buffer for retry) or automatically when the panel
    /// transitions away from `Running` (no live reader → nothing to
    /// type at).
    pub(crate) stdin_focused: bool,
}

impl ReplUi {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        // Live REPL gets the markdown renderer; agent replies are
        // formatted, user/system/tool roles fall through to PlainRenderer
        // verbatim. See `markdown-render.md`.
        Self::with_renderer(
            state_manager,
            action_sender,
            Box::new(MarkdownRenderer::default()),
        )
    }

    /// Construct a `ReplUi` and attach a model registry. Production
    /// boot uses this so `/model <alias>` is intercepted and validated
    /// in the View before any action is sent. Tests / harnesses that
    /// don't care about model switching can keep using
    /// [`ReplUi::new`].
    pub fn new_with_registry(
        state_manager: Arc<StateManager>,
        action_sender: UnboundedSender<UiAction>,
        registry: Arc<crate::config::ModelRegistry>,
    ) -> Self {
        let mut ui = Self::new(state_manager, action_sender);
        ui.model_registry = Some(registry);
        ui
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
            confirm_dialog: None,
            model_registry: None,
            last_rendered_revision: 0,
            last_size: (0, 0),
            chat_cache: ChatRenderCache::new(renderer),
            command_popup: None,
            popup_dismissed: false,
            multiline_mode: false,
            stdin_buffer: String::new(),
            stdin_focused: false,
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
    ///
    /// The version string reads [`crate::PEAKBOOT_VERSION`] — the same
    /// constant the system prompt and the `WelcomeState` wire payload
    /// pull from. Keeping all three on one identifier is the point: a
    /// patched binary always agrees with itself about what is running.
    fn welcome_lines() -> Vec<Line<'static>> {
        vec![
            Line::from(vec![
                Span::styled(
                    "✨ PeakBot ",
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("v{PEAKBOOT_VERSION}"),
                    Style::default().fg(Color::LightYellow),
                ),
            ]),
            Line::from(Span::styled(
                "Start a conversation or use /help for commands.",
                Style::default().fg(Color::Gray),
            )),
        ]
    }

    /// The bordered block that wraps the chat transcript.
    ///
    /// Single source of truth for both the snapshot path and the live
    /// render path. The bottom border carries a persistent keybinding
    /// hint — right-aligned so the top-left `" Chat Messages "` title
    /// stays the dominant label, and clipped gracefully on narrow
    /// terminals (ratatui truncates titles that don't fit).
    ///
    /// Hint ordering reflects keybind likelihood of use:
    /// `Ctrl+C exit` → universal escape, always first;
    /// `Ctrl+T tasks` / `Ctrl+B bash` → panel toggles, grouped;
    /// `F4 select` → mode switch, less frequent;
    /// `Ctrl+G multi` → input mode, least frequent.
    ///
    /// When `select_mode` is `true`, the block returns *naked* — no
    /// borders, no titles, no hint. The point is that with the
    /// terminal's mouse capture disabled (see `toggle_select_mode`)
    /// the user is about to drag-select chat text with their terminal's
    /// own selection UI, and any `│`/`─`/title characters in the visible
    /// buffer would contaminate the clipboard. Content (timestamps,
    /// role prefixes) stays — it's content, not chrome.
    fn chat_block(select_mode: bool) -> Block<'static> {
        if select_mode {
            return Block::default().borders(Borders::NONE);
        }
        Block::default()
            .title(" Chat Messages ")
            .title_bottom(
                Line::from(" Ctrl+C exit · Ctrl+T tasks · Ctrl+B bash · F4 select · Ctrl+G multi ")
                    .right_aligned(),
            )
            .borders(Borders::ALL)
    }

    /// Build the full chat-history paragraph from scratch.
    ///
    /// Kept for backwards compatibility (the snapshot-test suite in
    /// `tests/repl_tests.rs` uses this). The live render path no longer
    /// calls this — it goes through [`ChatRenderCache`] instead, which is
    /// what makes rendering independent of history size. See
    /// `slow-messages.md`.
    ///
    /// `select_mode` is forwarded to [`chat_block`]; pass `false` from
    /// snapshot tests that want the chrome.
    pub fn build_chat_history_paragraph<'a>(
        chat: &'a ChatState,
        select_mode: bool,
    ) -> Paragraph<'a> {
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
            .wrap(Wrap { trim: false })
            .block(Self::chat_block(select_mode))
    }

    /// Render a single chat message into owned `Line`s.
    ///
    /// Thin wrapper over [`PlainRenderer`] preserved for the existing
    /// snapshot tests. Prefer calling the renderer (or the cache) in new
    /// code.
    ///
    /// Width is passed as `0` because [`PlainRenderer`] is width-
    /// oblivious. If the snapshot path is ever pointed at a width-
    /// sensitive renderer, this needs a real value.
    pub fn build_chat_message_lines(msg: &ChatMessage) -> Vec<Line<'static>> {
        PlainRenderer.render(msg, 0)
    }

    /// Render the chat history area with scrollbar.
    ///
    /// **Two distinct scroll values are needed** because the transcript
    /// cache only builds a viewport-sized paragraph:
    ///
    /// - `global_scroll` — offset into the FULL transcript, in wrapped
    ///   lines. Drives the scrollbar thumb only. Range `0..=max_scroll`.
    /// - `paragraph_scroll` — offset into the local `paragraph`, which
    ///   starts at the first visible message's boundary. Range
    ///   `0..wrapped_counts[first_visible_message]`. Fed into
    ///   `Paragraph::scroll`.
    ///
    /// Pre-fix (2026-04-24 bug): this function took a single `scroll`
    /// parameter used for both jobs. After commit `3b3149e` landed the
    /// O(viewport) cache, callers passed `view.inner_scroll` —
    /// correct for `Paragraph::scroll` but tiny (bounded by one
    /// message's wrapped height), so the scrollbar thumb got stuck near
    /// the top in long conversations.
    ///
    /// `content_height` is the total wrapped line count — same value
    /// the render loop feeds into `UiState.content_height`. Passed in
    /// so we don't re-run word-wrap here (see `slow-messages.md`).
    ///
    /// `select_mode` skips the scrollbar entirely and gives the
    /// paragraph the full width; the `█`/`▼` glyphs would otherwise
    /// land in the user's clipboard during a terminal-native drag-select.
    pub fn render_chat_history(
        f: &mut ratatui::Frame,
        area: Rect,
        global_scroll: u16,
        paragraph_scroll: u16,
        paragraph: Paragraph,
        content_height: u16,
        select_mode: bool,
    ) {
        if select_mode {
            let scrolled = paragraph.scroll((paragraph_scroll, 0));
            f.render_widget(scrolled, area);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100), Constraint::Length(1)])
            .split(area);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let mut scroll_state =
            ScrollbarState::new(content_height as usize).position(global_scroll as usize);
        f.render_stateful_widget(scrollbar, chunks[1], &mut scroll_state);

        let scrolled = paragraph.scroll((paragraph_scroll, 0));
        f.render_widget(scrolled, chunks[0]);
    }

    /// Build the input paragraph with an animated "working" title when the
    /// agent is running.
    ///
    /// When `run_started_at` is `Some`, the block title becomes
    /// `" ⠹ Working · 00:07 · <status> · ⏳ N queued · esc to stop "` —
    /// the queued segment only appears when `pending_input > 0`. The hint
    /// sits in the working title (not the status bar) because that's where
    /// the user's eye is during a busy turn.
    ///
    /// Multiline behaviour: the buffer is split on `\n` into logical lines.
    /// The prompt marker (`> ` or the placeholder) only appears on line 0.
    /// The cursor block (`█`) is rendered inline on whichever logical line
    /// the cursor sits on. See chat 2026-04-24.
    #[allow(clippy::too_many_arguments)] // status-bar segments are unrelated; bundling them into a struct gains nothing
    pub fn build_input_paragraph<'a>(
        input: &str,
        cursor_pos: usize,
        is_running: bool,
        run_started_at: Option<std::time::Instant>,
        status_message: Option<&str>,
        pending_input: usize,
        bg_running: usize,
        multiline: bool,
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
                    // Normalise per-logical-line: strips VS16 / ZWJ from
                    // user-typed or pasted input so the input area
                    // doesn't drift the cursor on Linux/kitty.
                    // See `garbled.md` Class A bypass.
                    let safe_line = crate::ui::emoji_normalize::normalize_for_terminal(line_text);
                    // Recompute split position in the *normalised* string
                    // when the cursor lives on this line. Since stripped
                    // codepoints are zero-width, the byte offset only
                    // shifts when stripped bytes precede the cursor —
                    // which is rare, but worth handling correctly.
                    let mut spans: Vec<Span<'a>> = Vec::new();
                    if i == 0 {
                        spans.push(prompt_span.clone());
                    }
                    if i == cursor_line_idx {
                        // Cursor lives on this logical line. Compute the
                        // pre/post split in the *original* line_text bytes
                        // (which the cursor was tracking), then normalise
                        // each half independently. This keeps the cursor
                        // visually positioned where the user expects, even
                        // if VS16 was sitting between cursor and content.
                        let split_at = col_bytes.min(line_text.len());
                        let (pre_raw, post_raw) = line_text.split_at(split_at);
                        let pre = crate::ui::emoji_normalize::normalize_for_terminal(pre_raw)
                            .into_owned();
                        let post = crate::ui::emoji_normalize::normalize_for_terminal(post_raw)
                            .into_owned();
                        spans.push(Span::raw(pre));
                        spans.push(cursor_span.clone());
                        spans.push(Span::raw(post));
                    } else {
                        spans.push(Span::raw(safe_line.into_owned()));
                    }
                    Line::from(spans)
                })
                .collect()
        };

        let bg_segment = if bg_running > 0 {
            format!(" · 🛰 {bg_running} bg")
        } else {
            String::new()
        };
        let title = match (is_running, run_started_at) {
            (true, Some(t)) => {
                // Truncate the phase label to ~24 chars to keep the title
                // readable on narrow terminals. Normalise first so any
                // VS16 / ZWJ in agent-controlled status text doesn't
                // drift the title border. See `garbled.md` Class A.
                let phase_full = crate::ui::emoji_normalize::normalize_for_terminal(
                    status_message.unwrap_or("thinking"),
                );
                let phase: String = if phase_full.chars().count() > 24 {
                    phase_full.chars().take(23).collect::<String>() + "…"
                } else {
                    phase_full.into_owned()
                };
                let queued = if pending_input > 0 {
                    format!(" · ⏳ {pending_input} queued")
                } else {
                    String::new()
                };
                format!(
                    " {} Working · {} · {}{}{} · esc to stop ",
                    spinner::frame_for(t),
                    spinner::fmt_elapsed(t),
                    phase,
                    queued,
                    bg_segment,
                )
            }
            // Multiline compose mode (idle): the title doubles as the
            // affordance — what to press to send, what to press to cancel.
            // No emoji with VS16 — `✎` (U+270E) is a single-cell glyph
            // safe across kitty/vte/iTerm. See memory.md Class-B policy.
            _ if multiline => {
                format!(" ✎ Multiline · Ctrl+G to send · Esc to cancel{bg_segment} ")
            }
            _ => format!(" Input{bg_segment} "),
        };

        // Bottom-border hint about newline vs submit. Keybinding hints
        // belong on borders, not content (memory 2026-04-24). Discoverability
        // for Ctrl+G lives on the *chat* block's bottom hint (alongside
        // Ctrl+C / Ctrl+T / Ctrl+B / F4) so the input hint stays short
        // enough to fit on narrow terminals (snapshot tests use width=60).
        let hint_text = if multiline {
            " Enter: newline · Ctrl+G: send "
        } else {
            " Shift/Alt+Enter: newline · Enter: send "
        };
        let hint = Line::from(vec![Span::styled(
            hint_text,
            Style::default().fg(Color::White),
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

    /// Render the "select mode" banner. Replaces the status bar while
    /// select mode is active. Single row, bright reverse-video style so
    /// the user can't miss it — the whole point of this row is to remind
    /// them they're in a non-default modal state and how to get out.
    pub fn render_select_mode_banner(f: &mut ratatui::Frame, area: Rect) {
        let text =
            " 📋 SELECT MODE — drag to select · use your terminal's copy keys · F4 to resume ";
        let paragraph = Paragraph::new(text).style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightYellow)
                .add_modifier(Modifier::BOLD),
        );
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

        // The `⏳ N queued` hint lives in the working title of the input
        // block (see `build_input_paragraph`), not here — that's where the
        // user's eye is during a busy turn. Status bar stays for tokens /
        // cost / model / context / cwd only.
        let mut status_text = format!(
            "Tokens: {} │ Calls: {} │ Cost: ${} │ Context: {:.1}% │ Model: {}",
            tokens_str, stats.total_api_calls, cost_str, context_pct, stats.model,
        );
        // cwd lives on the welcome banner (stamped at boot, refreshed on
        // /cd). Absent only before the banner is set — omit rather than
        // guess.
        if let Some(w) = &state.welcome {
            status_text.push_str(&format!(" │ 📁 {}", abbreviate_path(&w.cwd, 48)));
        }

        let paragraph = Paragraph::new(status_text)
            .style(Style::default().fg(Color::LightCyan))
            .block(Block::default().borders(Borders::NONE));

        f.render_widget(paragraph, area);
    }

    /// Backward-compat shim for the original `render_quit_confirm`
    /// helper. Delegates to the new generic
    /// [`render_confirm_dialog`] using a freshly-built
    /// [`ConfirmDialog::quit`]. Kept on `ReplUi` so existing snapshot
    /// tests (`tests/repl_tests.rs::quit_confirm_*`) call it
    /// unchanged.
    pub fn render_quit_confirm(f: &mut ratatui::Frame, area: Rect, yes_selected: bool) {
        let mut dialog = ConfirmDialog::quit();
        dialog.yes_selected = yes_selected;
        render_confirm_dialog(f, area, &dialog);
    }

    /// Main render function
    fn render(&mut self, state: &AppState) -> Result<()> {
        // Auto-reset stdin focus when the bash panel is no longer
        // accepting input. Triggered by the producer-side transition
        // `Running → Finished` (child exited) or `Running → Idle`
        // (cleared on /new, /load, /model). Without this, a user who
        // was typing into stdin would keep seeing the focused cursor
        // pointing at nothing. Buffer is dropped too — there is no
        // way to retry against a different child, and a stale buffer
        // surviving into the chat-input flow would be surprising.
        if self.stdin_focused && !state.bash_panel.is_running() {
            self.stdin_focused = false;
            self.stdin_buffer.clear();
            self.ui_state.local_dirty = true;
        }

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
                    state.pending_input_count,
                    state.bg.running_count,
                    self.multiline_mode,
                );

                // Check if todo panel should be shown (based on terminal size and visibility state)
                // In select mode we forcibly hide the side panel so the
                // chat takes the full width — selection across the panel
                // boundary would otherwise glue todo items into the copy.
                let show_todo = !self.ui_state.select_mode
                    && state.todo.visible
                    && should_show_panel(size.width);

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

                    // Bash panel strip height — 0 when Idle ⇒ collapses
                    // out of the layout entirely. Slice 2 of #11.
                    // `Constraint::Length(0)` is well-defined in ratatui:
                    // the chunk exists but has zero rows and renders
                    // nothing. The `effective_*` form combines the
                    // producer state with the user's `Ctrl+B` visibility
                    // override (`bash-panel-as-real-panel.md`): Idle +
                    // OpenedByUser renders a small empty frame;
                    // ClosedByUser collapses regardless of state.
                    let bash_height =
                        bash_effective_panel_height(&state.bash_panel, state.bash_panel_visibility);

                    // Scroll-follow in *visual* rows (not logical lines) so
                    // soft-wrapped long lines scroll correctly. Computed at
                    // render time because width is only known here and
                    // resizes also need to re-adjust.
                    let cursor_row = Self::cursor_visual_row(
                        &self.ui_state.input_buffer,
                        self.ui_state.cursor_pos,
                        content_width,
                    );
                    self.ui_state.input_scroll =
                        compute_input_scroll(cursor_row, self.ui_state.input_scroll, content_rows);

                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Percentage(100),
                            Constraint::Length(bash_height),
                            Constraint::Length(input_height),
                            Constraint::Length(1),
                        ])
                        .split(main);

                    // Layout indices: chunks[0]=chat, chunks[1]=bash strip
                    // (0 rows when Idle ⇒ invisible), chunks[2]=input,
                    // chunks[3]=status. The bash strip lives in the chat
                    // column only (not full terminal width) — see slice 2
                    // notes in `make-term-great-again.md`.
                    let chat_chunk = chunks[0];
                    let bash_strip_chunk = chunks[1];
                    let input_chunk = chunks[2];
                    let status_chunk = chunks[3];

                    // Width available for chat content depends on the
                    // chrome the chat pane currently draws. In normal
                    // mode the block has left+right borders (2 cells)
                    // AND `render_chat_history` carves a 1-cell scrollbar
                    // off the right edge — so true content width is
                    // `main.width - 3`. In select mode there's no block
                    // *and* no scrollbar (early-return in
                    // `render_chat_history`) so we get `main.width`.
                    //
                    // The renderer is told this exact number; width-
                    // sensitive content (markdown tables, fenced code
                    // rules) sizes itself to fit inside the visible area
                    // instead of overflowing the right edge.
                    //
                    // Pinned by `chat_pane_content_width` below; do not
                    // inline this math without updating that test.
                    let select_mode = self.ui_state.select_mode;
                    let chat_wrap_width = chat_pane_content_width(main.width, select_mode);

                    // Sync per-message cache against current messages at
                    // current width. Only mutated rows are re-rendered;
                    // only mutated/resized rows are re-wrapped. See
                    // `slow-messages.md` §4.1.
                    self.chat_cache.sync(&state.chat.messages, chat_wrap_width);

                    self.ui_state.viewport_height = chat_chunk.height.saturating_sub(2);
                    self.ui_state.content_height =
                        self.chat_cache.total_height().min(u16::MAX as u32) as u16;

                    // Calculate scroll based on auto_scroll setting, and
                    // persist it so the scroll handlers start from the real
                    // viewport position (issue #31).
                    let scroll = self.ui_state.effective_scroll();

                    // Build the viewport-sized paragraph from the cache.
                    // Work here is O(viewport), independent of history size.
                    // `window()` returns both the Lines covering the viewport
                    // AND the partial-line offset into the first visible
                    // message; we feed the offset straight into `Paragraph::scroll`.
                    // In select mode we strip every piece of chrome from
                    // the chat surface so the user's terminal-native copy
                    // captures only message content. Chrome includes:
                    // borders, the bordered block's titles + bottom-hint,
                    // and the right-edge scrollbar column. `select_mode`
                    // is already in scope (used above for
                    // `chat_pane_content_width`); reuse it here.
                    let view = self.chat_cache.window(scroll as u32, chat_chunk.height);
                    let chat_history = if view.lines.is_empty() && state.chat.messages.is_empty() {
                        // Empty transcript — show the welcome banner.
                        Paragraph::new(Text::from(Self::welcome_lines()))
                            .style(Style::default().fg(Color::White))
                            .wrap(Wrap { trim: false })
                            .block(Self::chat_block(select_mode))
                    } else {
                        Paragraph::new(Text::from(view.lines))
                            .style(Style::default().fg(Color::White))
                            .wrap(Wrap { trim: false })
                            .block(Self::chat_block(select_mode))
                    };

                    Self::render_chat_history(
                        f,
                        chat_chunk,
                        /* global_scroll    */ scroll,
                        /* paragraph_scroll */ view.inner_scroll,
                        chat_history,
                        self.ui_state.content_height,
                        select_mode,
                    );

                    // Render the foreground bash-tool panel between chat
                    // and input. `Idle` ⇒ `bash_height == 0` ⇒ the chunk
                    // is zero-rows and the renderer no-ops. Slice 2 of
                    // #11; producer wiring lands in slice 3; slice 4
                    // wires the real stdin buffer + focus state below.
                    if bash_height > 0 {
                        render_bash_panel(
                            f,
                            bash_strip_chunk,
                            &state.bash_panel,
                            &self.stdin_buffer,
                            self.stdin_focused,
                        );
                    }

                    // Input area: in select mode, swap its bordered block
                    // for a borderless one so a vertical selection
                    // through the input box doesn't pick up box chars
                    // or the working-spinner title. The prompt marker
                    // (`> `) and cursor block stay — they're content.
                    let input = if select_mode {
                        input.block(Block::default().borders(Borders::NONE))
                    } else {
                        input
                    };
                    Self::render_input_area(f, input_chunk, input, self.ui_state.input_scroll);
                    if select_mode {
                        // Modal: replace the status bar with a banner that
                        // tells the user (1) what's happening and (2) how
                        // to leave. Mouse-wheel scroll is off; PgUp/PgDn
                        // still work, and so does typing into the input.
                        Self::render_select_mode_banner(f, status_chunk);
                    } else {
                        Self::render_status_bar(f, status_chunk, state);
                    }

                    // Render command popup ABOVE the input area if open.
                    // Drawn after the chat + input so it sits on top (via
                    // `Clear` inside the renderer). See `allehailmenu.md` §6.
                    if let Some(popup) = self.command_popup.as_ref() {
                        crate::ui::repl::command_popup::render_command_popup(f, input_chunk, popup);
                    }
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

                // Render confirmation dialog overlay if any is open.
                if let Some(ref dialog) = self.confirm_dialog {
                    render_confirm_dialog(f, size, dialog);
                }
            })?;
        }
        Ok(())
    }

    /// Flip "select mode" on/off and emit the matching mouse-capture
    /// escape sequence to the terminal. When `select_mode` becomes `true`,
    /// we send `DisableMouseCapture` so the terminal can do its own
    /// click-and-drag selection; when `false`, we re-enable capture so
    /// mouse-wheel scroll works again. Errors writing to stdout are
    /// swallowed — at worst the indicator and the underlying capture
    /// state would briefly disagree, and the next toggle would resync.
    ///
    /// The IO is gated on `self.terminal.is_some()` so unit tests (which
    /// build a `ReplUi` via the test harness without calling `init()`)
    /// can exercise the F4 binding without scribbling raw escape bytes
    /// onto the developer's live terminal under `cargo test`. Pinned by
    /// `f4_does_not_emit_mouse_escapes_without_a_terminal`.
    fn toggle_select_mode(&mut self) {
        self.ui_state.select_mode = !self.ui_state.select_mode;
        self.ui_state.local_dirty = true;
        if self.terminal.is_none() {
            return;
        }
        let mut out = std::io::stdout();
        let _ = if self.ui_state.select_mode {
            execute!(out, DisableMouseCapture)
        } else {
            execute!(out, EnableMouseCapture)
        };
    }

    /// Resolve the open confirm dialog: dispatch its `ConfirmAction` if
    /// the user picked Yes, then close the dialog regardless. Called
    /// from the `Enter` arm in `handle_keyboard_input`.
    ///
    /// This is the *only* place `ConfirmAction` variants get matched —
    /// any future variant added to the enum lights up here as a
    /// non-exhaustive match (clippy: `match_same_arms` is OK because
    /// each branch may grow). Add a new arm whenever you add a new
    /// confirm-gated operation.
    fn commit_confirm_dialog(&mut self) {
        let Some(dialog) = self.confirm_dialog.take() else {
            return;
        };
        if !dialog.yes_selected {
            return;
        }
        match dialog.action {
            ConfirmAction::Quit => {
                self.running = false;
            }
            ConfirmAction::SwitchModel { alias, .. } => {
                let _ = self.action_sender.send(UiAction::SwitchModel(alias));
            }
            ConfirmAction::ChangeCwd { path } => {
                let _ = self.action_sender.send(UiAction::ChangeCwd(path));
            }
        }
    }

    /// Pre-submission interceptor for `/model <alias>`. Production
    /// behaviour, gated on a non-`None` `model_registry`:
    ///
    /// - Bare `/model` (no arg) — falls through to the controller as a
    ///   `Command`; `process_command_internal` lists the available
    ///   models.
    /// - `/model <alias>` with **invalid** alias — emits the canonical
    ///   error system message; nothing further.
    /// - `/model <alias>` matching the **current** alias — emits a
    ///   "Already on X." system message; no destructive reset.
    /// - Valid + new alias, **empty** chat — sends
    ///   `UiAction::SwitchModel(alias)` directly. No confirmation
    ///   needed (no content to lose).
    /// - Valid + new alias, **non-empty** chat — opens a
    ///   [`ConfirmDialog::switch_model`]. The eventual `UiAction` is
    ///   dispatched by `commit_confirm_dialog` if the user confirms.
    ///
    /// Returns `true` if the submission was fully consumed (do not
    /// dispatch `UiAction::SendMessage`); `false` if the caller should
    /// fall through to the standard send path.
    fn try_intercept_model_command(&mut self, msg: &str) -> bool {
        let trimmed = msg.trim();
        if trimmed.eq_ignore_ascii_case("/model") {
            // No arg — defer to the controller's listing handler.
            return false;
        }
        let Some(rest) = trimmed
            .strip_prefix("/model ")
            .or_else(|| trimmed.strip_prefix("/MODEL "))
        else {
            return false;
        };
        let alias = rest.trim();
        if alias.is_empty() {
            return false; // treat as bare `/model`
        }
        let Some(registry) = self.model_registry.clone() else {
            // No registry attached — let the controller handle it.
            return false;
        };
        let Some(resolved) = registry.resolve(alias) else {
            // Unknown alias. Emit the canonical error system message and
            // do not dispatch anything further.
            let available = registry.aliases_sorted().join(", ");
            self.state_manager.add_system_message(format!(
                "❌ /model: unknown alias `{alias}`. Available: {available}",
            ));
            return true;
        };
        // Same as current?
        let current = self.state_manager.get_model_alias();
        if current == resolved.alias {
            self.state_manager
                .add_system_message(format!("Already on {}.", resolved.alias));
            return true;
        }
        // Decide whether to confirm. Empty chat = skip confirm.
        let chat_empty = self.state_manager.get_state().chat.messages.is_empty();
        let descriptor = format!("{} · {}", resolved.provider_name, resolved.model_name);
        if chat_empty {
            let _ = self
                .action_sender
                .send(UiAction::SwitchModel(resolved.alias.clone()));
        } else {
            self.confirm_dialog = Some(ConfirmDialog::switch_model(&resolved.alias, &descriptor));
        }
        true
    }

    /// Pre-submission interceptor for `/cd <path>`. Mirrors
    /// [`Self::try_intercept_model_command`]:
    ///
    /// - Bare `/cd` (no arg) — falls through to the controller, which
    ///   prints the current working directory (orientation).
    /// - `/cd <path>` that doesn't resolve to an existing directory —
    ///   emits an error system message; nothing further. **No state is
    ///   mutated** (the chdir happens later, in the agent loop).
    /// - `/cd <path>` resolving to the **current** cwd — emits an
    ///   "Already in X." system message; no destructive reset.
    /// - Valid + different, **empty** chat — sends
    ///   `UiAction::ChangeCwd(path)` directly (no confirmation).
    /// - Valid + different, **non-empty** chat — opens a
    ///   [`ConfirmDialog::change_cwd`]; the `UiAction` is dispatched by
    ///   `commit_confirm_dialog` on confirm.
    ///
    /// Returns `true` if the submission was fully consumed.
    fn try_intercept_cd_command(&mut self, msg: &str) -> bool {
        let trimmed = msg.trim();
        if trimmed.eq_ignore_ascii_case("/cd") {
            // No arg — defer to the controller's "print cwd" handler.
            return false;
        }
        let Some(rest) = trimmed
            .strip_prefix("/cd ")
            .or_else(|| trimmed.strip_prefix("/CD "))
        else {
            return false;
        };
        let arg = rest.trim();
        if arg.is_empty() {
            return false; // treat as bare `/cd`
        }
        let resolved = match resolve_cd_path(arg) {
            Ok(p) => p,
            Err(e) => {
                self.state_manager
                    .add_system_message(format!("❌ /cd: {e}"));
                return true;
            }
        };
        // Same as the current working directory?
        let current = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if resolved == current {
            self.state_manager
                .add_system_message(format!("Already in {resolved}."));
            return true;
        }
        let chat_empty = self.state_manager.get_state().chat.messages.is_empty();
        if chat_empty {
            let _ = self.action_sender.send(UiAction::ChangeCwd(resolved));
        } else {
            self.confirm_dialog = Some(ConfirmDialog::change_cwd(&resolved));
        }
        true
    }

    /// Combined pre-submission interceptor for the session-switch
    /// commands (`/model`, `/cd`). Each is a `/new` + rebuild on a
    /// different axis; they share the confirm-on-non-empty-chat shape.
    /// Returns `true` if either consumed the submission.
    fn try_intercept_switch_command(&mut self, msg: &str) -> bool {
        self.try_intercept_model_command(msg) || self.try_intercept_cd_command(msg)
    }

    fn handle_keyboard_input(&mut self, key: KeyEvent) {
        match key.code {
            // Toggle todo panel with Ctrl+T
            KeyCode::Char('t' | 'T') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state_manager.toggle_todo_panel();
                // Reset scroll position when toggling
                self.ui_state.todo_scroll_position = 0;
            }
            // Toggle visibility of the foreground bash panel with Ctrl+B.
            //
            // **View-only** — never touches the underlying
            // [`BashPanelState`]. Running bashes keep streaming under
            // the hood; the panel just stops drawing.
            //
            // Works in every state including Idle (where it opens a
            // small empty frame) — that's the "open it anytime, like
            // the tasks panel" contract. See
            // `bash-panel-as-real-panel.md` for the design.
            //
            // The visibility flag persists across user messages — a
            // hide carries until either another Ctrl+B or until the
            // agent starts a new bash invocation (which clears the
            // override; see `StateManager::start_bash_panel`).
            // Conversation resets (`/new`, `/load`, `/model`) restore
            // visibility to `Auto`.
            //
            // Why Ctrl+B (not Esc): Esc routes to the agent-interrupt
            // catch-all at the bottom of this handler. Any Esc-based
            // dismiss would steal the agent-stop keystroke the moment
            // a panel showed. The cardinal regression-pin
            // `esc_still_interrupts_agent_with_finished_panel_visible`
            // guards this; original design doc lives in
            // `close-bash-panel-v2.md`.
            KeyCode::Char('b' | 'B') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state_manager.toggle_bash_panel_visibility();
            }
            // F4: toggle "select mode" — disables our mouse capture so the
            // terminal's own click-and-drag selection (and native copy
            // shortcut) work. F4 again restores mouse-wheel scroll.
            // See `copy-and-paste-me-baby.md`.
            KeyCode::F(4) => {
                self.toggle_select_mode();
            }
            // Quit with Ctrl+C (opens confirmation dialog)
            KeyCode::Char('c' | 'C') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.confirm_dialog = Some(ConfirmDialog::quit());
            }
            // Toggle multiline compose mode with Ctrl+G ("Go multi"). When
            // OFF, first tap turns it ON and Enter starts inserting newlines
            // instead of submitting. When ON, second tap submits the buffer
            // (if non-empty) and exits the mode. Gated off while a confirm
            // dialog or command popup is open so those modal surfaces win.
            //
            // Why Ctrl+G specifically: byte 0x07 (BEL) is detectable on every
            // terminal as `Char('g') + CONTROL`, has no Enter/Tab/Backspace
            // collision, and unlike Ctrl+L carries no decades-old "clear
            // screen" muscle memory from shells. See `multiline-mode.md`.
            KeyCode::Char('g' | 'G')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.confirm_dialog.is_none()
                    && self.command_popup.is_none() =>
            {
                if self.multiline_mode {
                    // Second tap → submit + exit (mirrors plain-Enter submit).
                    let msg = self.ui_state.input_buffer.clone();
                    self.multiline_mode = false;
                    if !msg.trim().is_empty() && !self.try_intercept_switch_command(&msg) {
                        let _ = self.action_sender.send(UiAction::SendMessage(msg));
                    }
                    self.ui_state.clear_input();
                } else {
                    // First tap → enter mode. Buffer untouched.
                    self.multiline_mode = true;
                }
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
            // Confirmation dialog handlers (must come before general handlers).
            // Routes to whatever ConfirmAction the open dialog carries.
            KeyCode::Esc if self.confirm_dialog.is_some() => {
                // ESC while dialog is open = cancel (close dialog)
                self.confirm_dialog = None;
            }
            // Esc while in multiline compose mode = cancel mode without
            // submitting. Buffer is preserved (the user may still want
            // their draft); they can tap Ctrl+G again to re-enter, or
            // Backspace it down. See `multiline-mode.md` §5.3.
            KeyCode::Esc if self.multiline_mode => {
                self.multiline_mode = false;
            }
            // ── Foreground bash stdin (slice 4 of #11) ───────────────────
            //
            // Routing rule: stdin claims keys only while focused AND the
            // bash panel is Running. The Ctrl+S → focus arm is gated on
            // the panel being Running so an idle Ctrl+S falls through to
            // any future binding. All stdin arms sit **after** modal
            // dialogs (so they don't steal Esc/Enter from a confirm
            // popup) and **before** the generic Char/Enter/Backspace
            // arms (so a focused stdin row claims the keystroke instead
            // of editing the chat input).
            //
            // Why Ctrl+S: raw mode disables XON/XOFF (the historical
            // "stop output" meaning of Ctrl+S in cooked mode is gone),
            // and we already use other Ctrl combos (Ctrl+T, Ctrl+G)
            // without trouble. See `make-term-great-again.md` slice 4.
            KeyCode::Char('s' | 'S')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.stdin_focused
                    && self.state_manager.get_state().bash_panel.is_running()
                    && self.confirm_dialog.is_none() =>
            {
                self.stdin_focused = true;
                self.ui_state.local_dirty = true;
            }
            // Esc while stdin-focused: unfocus, but **preserve the
            // buffer**. A user who started typing a password and
            // realised they're in the wrong panel should not lose
            // their bytes — they can Ctrl+S back into focus and Enter
            // to send.
            KeyCode::Esc if self.stdin_focused => {
                self.stdin_focused = false;
                self.ui_state.local_dirty = true;
            }
            // Enter while stdin-focused: send the buffered line. On
            // success clear the buffer; on `StdinNotActive` keep it
            // (the user retries — see the `try_forward_bash_stdin`
            // recovery contract).
            KeyCode::Enter if self.stdin_focused => {
                let line = self.stdin_buffer.clone();
                match self.state_manager.try_forward_bash_stdin(line) {
                    Ok(()) => {
                        self.stdin_buffer.clear();
                    }
                    Err(_) => {
                        // No live foreground bash. Drop focus so the
                        // next Enter doesn't keep trying; preserve the
                        // buffer so the user sees what they had typed.
                        self.stdin_focused = false;
                    }
                }
                self.ui_state.local_dirty = true;
            }
            // Backspace while stdin-focused: pop the last char, UTF-8
            // boundary-safe (chars().last() + remove the byte range,
            // mirroring the chat-input editing helpers).
            KeyCode::Backspace if self.stdin_focused => {
                if let Some(c) = self.stdin_buffer.chars().last() {
                    let new_len = self.stdin_buffer.len() - c.len_utf8();
                    self.stdin_buffer.truncate(new_len);
                    self.ui_state.local_dirty = true;
                }
            }
            // Char while stdin-focused: append. The Ctrl-modified
            // arms above (Ctrl+T, Ctrl+G, Ctrl+S, Ctrl+C) already won
            // their cases via more-specific guards, so this only sees
            // plain characters.
            KeyCode::Char(c)
                if self.stdin_focused && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.stdin_buffer.push(c);
                self.ui_state.local_dirty = true;
            }
            KeyCode::Enter if self.confirm_dialog.is_some() => {
                self.commit_confirm_dialog();
            }
            KeyCode::Char('y' | 'Y') if self.confirm_dialog.is_some() => {
                if let Some(ref mut d) = self.confirm_dialog {
                    d.yes_selected = true;
                }
            }
            KeyCode::Char('n' | 'N') if self.confirm_dialog.is_some() => {
                if let Some(ref mut d) = self.confirm_dialog {
                    d.yes_selected = false;
                }
            }
            KeyCode::Left | KeyCode::Right if self.confirm_dialog.is_some() => {
                if let Some(ref mut d) = self.confirm_dialog {
                    d.toggle_selection();
                }
            }
            // ── Command popup handlers ─────────────────────────────────────
            // When the popup is open, Up/Down/Tab/Enter/Esc are diverted to
            // popup semantics. Everything else (Char, Backspace, Left/Right,
            // Home/End) falls through to the normal editing arms — the
            // popup's `prefix` is re-synced from the buffer at the end of
            // the handler (see `sync_popup`). See `allehailmenu.md` §5.2.
            KeyCode::Esc if self.command_popup.is_some() => {
                self.command_popup = None;
                // Explicit user dismissal — sync_popup must not
                // auto-reopen until the buffer is cleared.
                self.popup_dismissed = true;
            }
            KeyCode::Up if self.command_popup.is_some() => {
                if let Some(p) = self.command_popup.as_mut() {
                    p.navigate_up();
                }
            }
            KeyCode::Down if self.command_popup.is_some() => {
                if let Some(p) = self.command_popup.as_mut() {
                    p.navigate_down();
                }
            }
            KeyCode::Tab if self.command_popup.is_some() => {
                self.accept_command(false);
            }
            KeyCode::Enter
                if self.command_popup.is_some()
                    && (key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)) =>
            {
                // Shift/Alt+Enter closes popup + inserts newline (commands
                // are one-liners — a newline means "I'm done autocompleting").
                self.command_popup = None;
                // Explicit user dismissal.
                self.popup_dismissed = true;
                self.ui_state.insert_newline();
            }
            KeyCode::Enter if self.command_popup.is_some() => {
                self.accept_command(true);
            }
            // Open the popup on a bare `/` into an empty buffer. This arm
            // is ORDER-CRITICAL: it must come before the generic
            // `KeyCode::Char(c)` arm below, and it must come AFTER the
            // popup-is-open arms above (so a second `/` while the popup
            // is open is treated as a literal char, not a re-open).
            KeyCode::Char('/')
                if self.ui_state.input_buffer.is_empty()
                    && self.command_popup.is_none()
                    && self.confirm_dialog.is_none()
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.ui_state.insert_char('/');
                self.command_popup = Some(CommandPopupState::new(String::new()));
                // Explicit user-driven open — clear any prior dismissal.
                self.popup_dismissed = false;
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
            // While in multiline compose mode, plain Enter inserts a
            // newline. Submit lives on Ctrl+G (the same key that opened
            // the mode). See `multiline-mode.md` §5.2. This arm MUST
            // come before the bare `KeyCode::Enter` submit arm below.
            KeyCode::Enter if self.multiline_mode => {
                self.ui_state.insert_newline();
            }
            KeyCode::Enter => {
                let msg = self.ui_state.input_buffer.clone();
                if !msg.trim().is_empty() && !self.try_intercept_switch_command(&msg) {
                    let _ = self.action_sender.send(UiAction::SendMessage(msg));
                }
                self.ui_state.clear_input();
                // A composition just ended; ensure multiline mode is off
                // so the next message follows the default contract.
                self.multiline_mode = false;
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
                #[allow(clippy::collapsible_match)]
                if self.state_manager.is_running() {
                    let _ = self.action_sender.send(UiAction::RequestStop);
                }
            }
            _ => {}
        }

        // After every key event, keep the popup's `prefix` in sync with
        // the buffer and close it when the user has left command-name
        // territory (buffer empty, no leading `/`, or entered args).
        // Single source of truth: the buffer. See `allehailmenu.md` §5.2.
        self.sync_popup();
    }

    /// Sync the command popup with the buffer. **Refreshes / transitions
    /// / closes only — never auto-opens from `None`.** Opening is the job
    /// of explicit triggers:
    /// - `KeyCode::Char('/')` arm → opens `SlashCommand` mode.
    /// - `accept_command` Tab on a takes-args command → opens
    ///   `Argument` mode (via `open_or_sync_argument_popup`).
    /// - SlashCommand→Argument *transition* when the user types past the
    ///   command name into a known arg-completing command (handled here).
    ///
    /// This split is what keeps `Tab → fill buffer → close popup`
    /// behaviour consistent across both modes (sync_popup never reopens
    /// what `accept_command` just closed).
    fn sync_popup(&mut self) {
        let buf = self.ui_state.input_buffer.clone();

        // Empty buffer is a fresh slate — clear any sticky dismissal so
        // the next explicit `/` (or any future opener) works cleanly,
        // and close any lingering popup (the SlashCommand branch below
        // would also do this, but we want the empty-buffer behaviour to
        // be consolidated in one place).
        if buf.is_empty() {
            self.popup_dismissed = false;
            self.command_popup = None;
            return;
        }

        if self.command_popup.is_none() {
            // Popup is closed. If the user explicitly dismissed (Esc,
            // accept_command, Shift+Enter), respect that until the
            // buffer clears. Otherwise, attempt a *re-open* when the
            // buffer matches a valid command-prefix pattern — this
            // restores the popup after a reactive auto-close (e.g. the
            // user typed an extra space and then backspaced it).
            if !self.popup_dismissed {
                self.try_reopen_popup(&buf);
            }
            return;
        }

        let Some(popup) = self.command_popup.as_mut() else {
            return;
        };

        match popup.mode.clone() {
            PopupMode::Argument { command } => {
                let trigger = format!("/{} ", command);
                if let Some(rest) = buf.strip_prefix(&trigger) {
                    // Trim leading whitespace so `/model  ` (extra spaces)
                    // is treated the same as `/model ` — the popup stays
                    // open with an empty prefix.
                    let trimmed = rest.trim_start();
                    if !trimmed.contains(char::is_whitespace) {
                        // Still in `/<command> <prefix>` — refresh prefix
                        // and is_current flags through the helper.
                        let cmd = command.clone();
                        let rest_owned = trimmed.to_string();
                        self.open_or_sync_argument_popup(&cmd, &rest_owned);
                    } else {
                        // Whitespace inside the prefix (e.g., `/model x y`)
                        // — close the popup.
                        self.command_popup = None;
                    }
                } else if buf.starts_with('/') && !buf.contains(char::is_whitespace) {
                    // Backspaced out into the command-name region.
                    // Transition back to a SlashCommand-mode popup.
                    *popup = CommandPopupState::new(buf[1..].to_string());
                } else {
                    self.command_popup = None;
                }
            }
            PopupMode::SlashCommand => {
                // Transition into Argument mode if buffer matches a known
                // arg-completing command (currently only `/model `).
                if let Some(rest) = buf.strip_prefix("/model ")
                    && !rest.contains(char::is_whitespace)
                {
                    self.open_or_sync_argument_popup("model", rest);
                    return;
                }
                // Close conditions: buffer no longer represents a
                // command-name prefix.
                if !buf.starts_with('/') || buf.chars().any(|c| c.is_whitespace()) {
                    self.command_popup = None;
                    return;
                }
                // Plain prefix sync.
                popup.prefix = buf[1..].to_string();
                let count = popup.filtered_items().len();
                if count == 0 {
                    popup.selected_index = 0;
                } else if popup.selected_index >= count {
                    popup.selected_index = count - 1;
                }
            }
        }
    }

    /// Attempt to re-open a previously auto-closed popup based on the
    /// buffer's current shape. Called from `sync_popup` *only* when
    /// `popup_dismissed == false` (so explicit dismissals stay
    /// dismissed). Mirrors the open patterns that originally created
    /// the popup, so the user experience after a reactive close ≡ the
    /// experience after the original trigger:
    ///
    /// - `/<name>` (no whitespace) → SlashCommand mode with prefix `<name>`.
    /// - `/model ` or `/model <prefix>` (no whitespace inside `<prefix>`)
    ///   → Argument mode for `model`.
    ///
    /// Anything else is a no-op (popup stays closed).
    fn try_reopen_popup(&mut self, buf: &str) {
        // Argument-mode trigger takes precedence: `/model ` or
        // `/model <something>` (where `<something>` has no internal
        // whitespace) should restore the Argument popup, not the
        // SlashCommand one.
        if let Some(rest) = buf.strip_prefix("/model ") {
            let trimmed = rest.trim_start();
            if !trimmed.contains(char::is_whitespace) {
                self.open_or_sync_argument_popup("model", trimmed);
                return;
            }
        }
        // SlashCommand trigger: bare `/` or `/<name>` with no whitespace.
        if let Some(rest) = buf.strip_prefix('/')
            && !rest.contains(char::is_whitespace)
        {
            self.command_popup = Some(CommandPopupState::new(rest.to_string()));
        }
    }

    /// Open or refresh the argument-completion popup for `command`.
    /// Currently `model` is the only command with structured arg
    /// completion; pass through any other command name as a no-op.
    ///
    /// Items are rebuilt from the [`ModelRegistry`] each call (cheap —
    /// handful of models), with `is_current` set on the active alias so
    /// the renderer can mark it. If no registry is attached (legacy
    /// single-provider boot or test harness without
    /// `new_with_registry`), the popup stays closed.
    fn open_or_sync_argument_popup(&mut self, command: &str, prefix: &str) {
        if command != "model" {
            return;
        }
        let Some(registry) = self.model_registry.clone() else {
            // No registry — legacy single-provider boot. Don't open the
            // popup; bare `/model` still falls through to the
            // controller's listing handler.
            self.command_popup = None;
            return;
        };
        let current_alias = self.state_manager.get_model_alias();
        let items: Vec<CompletionItem> = registry
            .iter_sorted()
            .into_iter()
            .map(|(alias, resolved)| CompletionItem {
                value: alias.to_string(),
                description: format!("{} · {}", resolved.provider_name, resolved.model_name),
                is_current: alias == current_alias,
                takes_args: false,
            })
            .collect();

        // Are we already in the right Argument-mode popup? If so just
        // refresh prefix (and is_current flags, defensive against
        // mid-popup model swaps which don't happen today but might).
        let already_arg_mode = matches!(
            self.command_popup.as_ref().map(|p| &p.mode),
            Some(PopupMode::Argument { command: c }) if c == command
        );

        if already_arg_mode {
            if let Some(popup) = self.command_popup.as_mut() {
                popup.prefix = prefix.to_string();
                for it in popup.all_items.iter_mut() {
                    it.is_current = it.value == current_alias;
                }
                let count = popup.filtered_items().len();
                if count == 0 {
                    popup.selected_index = 0;
                } else if popup.selected_index >= count {
                    popup.selected_index = count - 1;
                }
            }
        } else {
            self.command_popup = Some(CommandPopupState::new_argument(
                command.to_string(),
                prefix.to_string(),
                items,
            ));
        }
    }

    /// Accept the currently-selected popup item.
    ///
    /// - **SlashCommand mode**: writes `/<name>` (plus a trailing space
    ///   for arg-taking commands) into the buffer; if `submit && !takes_args`,
    ///   sends `UiAction::SendMessage("/<name>")` and clears the input.
    /// - **Argument mode**: writes `/<command> <value>` into the buffer;
    ///   if `submit`, routes through `try_intercept_model_command` (so
    ///   the confirm-dialog flow + canonical diagnostics keep firing
    ///   from a single place) and clears the input.
    /// - Closes the popup in every case.
    /// - No-op when the popup's filter is empty (no selected item).
    ///
    /// See `allehailmenu.md` §5.2 (Tab/Enter semantics) and the
    /// glorious-popup proposal for the Argument-mode shape.
    fn accept_command(&mut self, submit: bool) {
        // Capture the item + mode without holding a borrow on self that
        // would block the `command_popup = None` write below.
        let captured: Option<(CompletionItem, PopupMode)> = self
            .command_popup
            .as_ref()
            .and_then(|p| p.selected_item().cloned().map(|i| (i, p.mode.clone())));
        self.command_popup = None;
        let Some((item, mode)) = captured else {
            return;
        };

        match mode {
            PopupMode::SlashCommand => {
                let completed = if item.takes_args {
                    format!("/{} ", item.value)
                } else {
                    format!("/{}", item.value)
                };
                if submit && !item.takes_args {
                    let _ = self
                        .action_sender
                        .send(UiAction::SendMessage(completed.clone()));
                    self.ui_state.clear_input();
                    // Slash command submitted = composition ended; exit
                    // multiline mode. See test
                    // `accept_command_clears_multiline_mode`.
                    self.multiline_mode = false;
                } else {
                    self.ui_state.input_buffer = completed.clone();
                    self.ui_state.cursor_pos = completed.len();
                    // If the command we just completed has structured
                    // argument completion (currently only `/model`),
                    // open the Argument popup straight away — saves the
                    // user one round-trip ("type the space, see the
                    // list"). The helper bails on commands without
                    // arg completion, so this is a safe blanket call.
                    if item.takes_args {
                        let cmd_name = item.value.clone();
                        self.open_or_sync_argument_popup(&cmd_name, "");
                    }
                }
            }
            PopupMode::Argument { command } => {
                let completed = format!("/{} {}", command, item.value);
                if submit {
                    // Route through the View-side interceptor — single
                    // source of truth for `/model <alias>` / `/cd <path>`
                    // semantics (confirm dialog on non-empty chat,
                    // "Already on X" on same target, etc).
                    let intercepted = self.try_intercept_switch_command(&completed);
                    if !intercepted {
                        // Defensive fallthrough: registry was attached
                        // when the popup was built, so this path isn't
                        // expected to fire. Still, be conservative.
                        let _ = self
                            .action_sender
                            .send(UiAction::SendMessage(completed.clone()));
                    }
                    self.ui_state.clear_input();
                    self.multiline_mode = false;
                } else {
                    // Tab in arg mode: fill the buffer with the chosen
                    // value and place cursor at the end. The user can
                    // then review or hit Enter.
                    self.ui_state.input_buffer = completed.clone();
                    self.ui_state.cursor_pos = completed.len();
                }
            }
        }

        // If accept_command ended with the popup still closed (e.g. Tab
        // on a non-arg command, or Tab on an Argument-mode item), mark
        // dismissed so sync_popup won't re-open it via the
        // "no-explicit-dismissal" heuristic. When accept_command
        // transitioned into Argument mode (e.g. Tab on `/model`), the
        // popup is open again and dismissed stays at its prior value.
        if self.command_popup.is_none() {
            self.popup_dismissed = true;
        }
    }

    /// Handle input events
    fn handle_input(&mut self, event: Event) {
        match event {
            // Filter on `KeyEventKind` BEFORE dispatching. On Windows,
            // crossterm's `ReadConsoleInputW` always reports both press
            // AND release records, so an unfiltered dispatcher fires
            // `handle_keyboard_input` twice per keystroke (typing `a`
            // becomes `aa`, one Backspace deletes two cells). On Linux/
            // macOS only `Press` is delivered (we never push
            // `KeyboardEnhancementFlags::REPORT_EVENT_TYPES`), so this
            // filter is a no-op there. `Repeat` is the autorepeat
            // firing — it represents a real keystroke from the user's
            // POV, so we forward it. See ratatui FAQ "Why am I getting
            // duplicate key events on Windows?" and crossterm 0.29
            // `event.rs::KeyEvent::kind` doc.
            Event::Key(key_event)
                if key_event.kind == crossterm::event::KeyEventKind::Press
                    || key_event.kind == crossterm::event::KeyEventKind::Repeat =>
            {
                self.handle_keyboard_input(key_event)
            }
            Event::Key(_) => { /* Release: ignore */ }
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
            // /exit path: the command handler sets `AppState.exit_requested`
            // via `StateManager::request_exit()`. We observe it on each
            // iteration and break out — no confirmation dialog, no banner,
            // just stop. Checked BEFORE `select!` so a pending keystroke
            // can't stall the shutdown.
            if self.state_manager.exit_requested() {
                self.running = false;
                break;
            }
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

/// Best-effort terminal-state restorer. The async `shutdown()` is the
/// happy-path teardown; this Drop is the safety net for *unhappy* paths:
/// a panic mid-render, an `Err(?)` short-circuit before `shutdown()` runs,
/// or any future caller that forgets to await `shutdown()`. Without this,
/// a panic strands the user's terminal in raw mode + alt screen + mouse
/// capture, producing the `35;43;18M`-style spam on every cursor move
/// until they `reset`.
///
/// We gate on `self.terminal.is_some()` so the guard is a no-op when:
///   - `init()` was never called (unit tests), or
///   - `shutdown()` already ran successfully (it sets `terminal = None`).
///
/// All errors are swallowed — Drop must not panic, and there is nothing
/// useful to do with an `Err` here anyway. The escape sequences are
/// emitted in the *reverse* order of `init()` to mirror the well-formed
/// teardown in `shutdown()`.
impl Drop for ReplUi {
    fn drop(&mut self) {
        if self.terminal.is_none() {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
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
        let empty = ReplUi::build_input_paragraph("", 0, false, None, None, 0, 0, false);
        // 1 content line (placeholder) + 2 border rows = 3.
        assert_eq!(empty.line_count(120), 3);

        let one_newline = ReplUi::build_input_paragraph("\n", 1, false, None, None, 0, 0, false);
        // 2 content lines + 2 border rows = 4.
        assert_eq!(one_newline.line_count(120), 4);

        let two_newlines = ReplUi::build_input_paragraph("\n\n", 2, false, None, None, 0, 0, false);
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
        let empty = ReplUi::build_input_paragraph("", 0, false, None, None, 0, 0, false);
        assert_eq!(ReplUi::paragraph_content_rows(&empty, 120), 1);

        let one_newline = ReplUi::build_input_paragraph("\n", 1, false, None, None, 0, 0, false);
        assert_eq!(ReplUi::paragraph_content_rows(&one_newline, 120), 2);

        let two_newlines = ReplUi::build_input_paragraph("\n\n", 2, false, None, None, 0, 0, false);
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

    /// The `⏳ N queued` segment appears in the working title only when the
    /// agent is busy AND there are queued user messages. This is the visible
    /// surface for `make-flow-great-again.md`'s pending-input counter — if a
    /// future change drops it from `build_input_paragraph`, the user loses
    /// the only signal that their typed-during-busy keystrokes landed.
    #[test]
    fn working_title_shows_queued_count_when_pending_and_busy() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let started = std::time::Instant::now();
        let para = ReplUi::build_input_paragraph(
            "hello",
            5,
            true,
            Some(started),
            Some("thinking"),
            3,
            0,
            false,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        terminal.draw(|f| f.render_widget(para, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered: String = (0..buffer.area().height)
            .flat_map(|y| {
                (0..buffer.area().width)
                    .map(move |x| buffer[(x, y)].symbol().to_string())
                    .chain(std::iter::once("\n".to_string()))
            })
            .collect();
        // The hourglass `⏳` is a double-width glyph; ratatui pads it to two
        // display cells, so the rendered substring is `⏳  3 queued` (two
        // spaces between glyph and count). Match the count + label so we
        // don't pin the spacing the terminal chose.
        assert!(
            rendered.contains("3 queued") && rendered.contains('⏳'),
            "working title must show the queued count; rendered:\n{rendered}",
        );
    }

    /// Negative pin: when the agent is busy but no input is queued, no
    /// queued segment should appear (avoids `⏳ 0 queued` noise).
    #[test]
    fn working_title_omits_queued_segment_when_count_is_zero() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let started = std::time::Instant::now();
        let para = ReplUi::build_input_paragraph(
            "hello",
            5,
            true,
            Some(started),
            Some("thinking"),
            0,
            0,
            false,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        terminal.draw(|f| f.render_widget(para, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered: String = (0..buffer.area().height)
            .flat_map(|y| {
                (0..buffer.area().width)
                    .map(move |x| buffer[(x, y)].symbol().to_string())
                    .chain(std::iter::once("\n".to_string()))
            })
            .collect();
        assert!(
            !rendered.contains("queued"),
            "working title must not show queued hint when count == 0; rendered:\n{rendered}",
        );
    }
}

#[cfg(test)]
mod chat_scroll_tests {
    //! Test-first specs for the chat-viewport scroll math.
    //!
    //! Pins down two post-fix contracts (see memory.md entry
    //! 2026-04-24 "scrollbar + last-line-hidden diagnosis"):
    //!
    //!   1. `UiState::viewport_height` stores the INNER chat content
    //!      area (outer bordered block height minus the 2 border rows).
    //!   2. `UiState::max_scroll()` returns exactly
    //!      `content_height - viewport_height` — no `+1`, no off-by-one.
    //!
    //! Pre-fix behaviour: `viewport_height` was the outer bordered area
    //! and `max_scroll` returned `content - viewport + 1`, so the last
    //! chat line was clipped under the bottom border forever.
    //!
    //! Scrollbar-position contract (the sibling regression) is pinned
    //! by an integration test in `tests/repl_tests.rs` since it needs a
    //! `TestBackend` frame.
    use super::*;

    #[test]
    fn max_scroll_is_content_minus_inner_viewport_no_plus_one() {
        let mut s = UiState::new();
        s.content_height = 100;
        s.viewport_height = 28; // inner content area (outer bordered block was 30)
        assert_eq!(
            s.max_scroll(),
            72,
            "max_scroll must be C - V (no +1). Pre-fix returned 73."
        );
    }

    #[test]
    fn max_scroll_saturates_to_zero_when_content_fits_viewport() {
        // When all content fits inside the inner viewport, there is
        // nothing to scroll and max_scroll must be 0. Pre-fix the `+1`
        // returned 1 here, which let `auto_scroll` push the content
        // down by one row and hide the top line for free.
        let mut s = UiState::new();
        s.content_height = 10;
        s.viewport_height = 30;
        assert_eq!(s.max_scroll(), 0, "content fits → nothing to scroll");
    }

    #[test]
    fn max_scroll_plus_viewport_covers_content_exactly() {
        // Invariant: scrolling to `max_scroll` lands the last content
        // line on the last inner viewport row. That requires
        // `max_scroll + viewport_height == content_height` whenever
        // content overflows. Pre-fix this equalled `content + 1`,
        // overshooting — which also hid the last line because
        // Paragraph::scroll skipped one extra row past where content
        // ends.
        let mut s = UiState::new();
        s.content_height = 50;
        s.viewport_height = 10;
        assert_eq!(
            s.max_scroll() + s.viewport_height,
            s.content_height,
            "max_scroll must bottom-align the final line exactly"
        );
    }

    #[test]
    fn effective_scroll_syncs_position_to_bottom_while_auto_scrolling() {
        // The issue #31 bug: while pinned to the bottom, `scroll_position`
        // stayed stale (initial 0), so the first scroll-up off the bottom
        // teleported the view to the top. `effective_scroll` must write the
        // computed bottom offset back so the next handler starts from there.
        let mut s = UiState::new();
        s.content_height = 100;
        s.viewport_height = 30;
        s.auto_scroll = true;
        s.scroll_position = 0; // stale

        let scroll = s.effective_scroll();

        assert_eq!(scroll, s.max_scroll(), "auto_scroll renders at the bottom");
        assert_eq!(
            s.scroll_position,
            s.max_scroll(),
            "scroll_position must be synced to the bottom, not left stale"
        );
    }

    #[test]
    fn effective_scroll_clamps_and_syncs_when_not_auto_scrolling() {
        // When the user has scrolled up, a stored position past the new
        // max_scroll (content shrank) must be clamped — and the clamp must
        // persist, not just be used for this one frame.
        let mut s = UiState::new();
        s.content_height = 40;
        s.viewport_height = 30; // max_scroll == 10
        s.auto_scroll = false;
        s.scroll_position = 999; // beyond range

        let scroll = s.effective_scroll();

        assert_eq!(scroll, 10, "stored position clamped to max_scroll");
        assert_eq!(s.scroll_position, 10, "clamp persisted back to state");
    }
}

#[cfg(test)]
mod command_popup_tests {
    //! Test-first specs for the slash-command autocomplete popup.
    //! See `allehailmenu.md` §5 (interaction model) and §7 (test plan).
    //!
    //! These tests pin down:
    //!   - when the popup opens (only on `/` into an empty buffer)
    //!   - prefix sync from buffer to popup on every edit
    //!   - close conditions (backspace past `/`, whitespace, newline, esc)
    //!   - navigation (up/down)
    //!   - completion (Tab) and run (Enter)
    //!
    //! Rendering snapshots live in `tests/repl_tests.rs`.
    use super::*;
    use crate::state::StateManager;
    use crate::ui::ui_trait::{CommandPopupState, UiAction};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    /// Build a ReplUi + UiAction receiver for testing keyboard handling.
    /// No terminal is attached — `handle_keyboard_input` only mutates
    /// local state and sends on the action channel.
    fn harness() -> (ReplUi, UnboundedReceiver<UiAction>) {
        let sm = StateManager::new_arc();
        let (tx, rx) = unbounded_channel();
        (ReplUi::new(sm, tx), rx)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press_with(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    // ─── Opening ─────────────────────────────────────────────────────────

    #[test]
    fn slash_on_empty_buffer_opens_popup() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert!(ui.command_popup.is_some(), "popup should be open");
        assert_eq!(ui.ui_state.input_buffer, "/");
        assert_eq!(ui.ui_state.cursor_pos, 1);
        assert_eq!(
            ui.command_popup.as_ref().unwrap().prefix,
            "",
            "prefix is empty right after opening (only the slash)"
        );
    }

    #[test]
    fn slash_on_nonempty_buffer_stays_literal() {
        let (mut ui, _rx) = harness();
        ui.ui_state.insert_char('h');
        ui.ui_state.insert_char('i');
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert!(
            ui.command_popup.is_none(),
            "popup must NOT open mid-sentence"
        );
        assert_eq!(ui.ui_state.input_buffer, "hi/");
    }

    #[test]
    fn slash_while_popup_open_is_just_a_char() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Char('t')));
        // Another '/' now — popup stays open, slash inserted literally
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert!(ui.command_popup.is_some());
        assert_eq!(ui.ui_state.input_buffer, "/st/");
        assert_eq!(ui.command_popup.as_ref().unwrap().prefix, "st/");
    }

    // ─── Prefix sync + close conditions ─────────────────────────────────

    #[test]
    fn typing_letters_updates_prefix() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Char('t')));
        assert_eq!(ui.command_popup.as_ref().unwrap().prefix, "st");
    }

    #[test]
    fn backspace_past_slash_closes_popup() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        assert!(ui.command_popup.is_some());
        // Backspace 's'
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(ui.command_popup.is_some(), "still open with just '/'");
        // Backspace '/'
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(
            ui.command_popup.is_none(),
            "popup closes when '/' is removed"
        );
        assert_eq!(ui.ui_state.input_buffer, "");
    }

    #[test]
    fn typing_space_closes_popup() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('l')));
        ui.handle_keyboard_input(press(KeyCode::Char('o')));
        ui.handle_keyboard_input(press(KeyCode::Char('a')));
        ui.handle_keyboard_input(press(KeyCode::Char('d')));
        // Space → enters args region → popup closes
        ui.handle_keyboard_input(press(KeyCode::Char(' ')));
        assert!(
            ui.command_popup.is_none(),
            "popup closes when user types space (args region)"
        );
        assert_eq!(ui.ui_state.input_buffer, "/load ");
    }

    #[test]
    fn shift_enter_closes_popup_and_inserts_newline() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press_with(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(ui.command_popup.is_none());
        assert_eq!(ui.ui_state.input_buffer, "/s\n");
    }

    // ─── Navigation ──────────────────────────────────────────────────────

    #[test]
    fn down_arrow_advances_selection_when_popup_open() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert_eq!(ui.command_popup.as_ref().unwrap().selected_index, 0);
        ui.handle_keyboard_input(press(KeyCode::Down));
        assert_eq!(ui.command_popup.as_ref().unwrap().selected_index, 1);
    }

    #[test]
    fn up_arrow_clamps_at_zero_when_popup_open() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Up)); // clamps at 0
        assert_eq!(ui.command_popup.as_ref().unwrap().selected_index, 0);
    }

    #[test]
    fn arrows_when_popup_closed_navigate_input_lines() {
        // Up/Down should NOT be intercepted when popup is closed —
        // they must still act as the existing line-nav keys.
        let (mut ui, _rx) = harness();
        // Buffer: "ab\ncd" with cursor at end (pos=5 on "d")
        ui.ui_state.insert_char('a');
        ui.ui_state.insert_char('b');
        ui.ui_state.insert_newline();
        ui.ui_state.insert_char('c');
        ui.ui_state.insert_char('d');
        let before = ui.ui_state.cursor_pos;
        ui.handle_keyboard_input(press(KeyCode::Up));
        assert!(
            ui.ui_state.cursor_pos < before,
            "Up should move cursor up a line when popup closed"
        );
    }

    // ─── Completion (Tab) and Run (Enter) ────────────────────────────────

    #[test]
    fn tab_completes_no_args_command() {
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Char('t'))); // prefix "st" → /stats
        ui.handle_keyboard_input(press(KeyCode::Tab));
        assert!(ui.command_popup.is_none(), "tab closes popup");
        assert_eq!(ui.ui_state.input_buffer, "/stats");
        assert_eq!(
            ui.ui_state.cursor_pos, 6,
            "cursor moved to end of completion"
        );
        assert!(
            rx.try_recv().is_err(),
            "tab must NOT send a UiAction — it only completes"
        );
    }

    #[test]
    fn tab_completes_takes_args_command_adds_space() {
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('l')));
        ui.handle_keyboard_input(press(KeyCode::Char('o'))); // /load
        ui.handle_keyboard_input(press(KeyCode::Tab));
        assert!(ui.command_popup.is_none());
        assert_eq!(ui.ui_state.input_buffer, "/load ");
        assert_eq!(ui.ui_state.cursor_pos, 6);
        assert!(rx.try_recv().is_err(), "tab never submits");
    }

    #[test]
    fn enter_runs_no_args_command() {
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Char('t'))); // /stats
        ui.handle_keyboard_input(press(KeyCode::Enter));
        assert!(ui.command_popup.is_none());
        // Buffer cleared after submit (same as regular Enter behaviour)
        assert_eq!(ui.ui_state.input_buffer, "");
        match rx.try_recv() {
            Ok(UiAction::SendMessage(m)) => assert_eq!(m, "/stats"),
            other => panic!("expected SendMessage(/stats), got {:?}", other),
        }
    }

    #[test]
    fn enter_on_takes_args_command_does_not_send() {
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('l')));
        ui.handle_keyboard_input(press(KeyCode::Char('o'))); // /load
        ui.handle_keyboard_input(press(KeyCode::Enter));
        assert!(ui.command_popup.is_none());
        assert_eq!(ui.ui_state.input_buffer, "/load ");
        assert_eq!(ui.ui_state.cursor_pos, 6);
        assert!(
            rx.try_recv().is_err(),
            "enter on takes_args command must fill, not submit"
        );
    }

    #[test]
    fn esc_closes_popup_preserves_buffer() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Esc));
        assert!(ui.command_popup.is_none());
        assert_eq!(
            ui.ui_state.input_buffer, "/s",
            "Esc dismisses popup but keeps what user typed"
        );
    }

    // ─── F4 select-mode toggle ───────────────────────────────────────────
    //
    // F4 lets the user step out of our mouse-capture mode so the terminal's
    // own click-and-drag selection works. The toggle lives entirely in
    // `UiState.select_mode`; the IO that emits `Disable/EnableMouseCapture`
    // happens in a separate fn that requires a real terminal, so these tests
    // pin only the bool flip and the keyboard binding.

    #[test]
    fn select_mode_default_is_false() {
        let (ui, _rx) = harness();
        assert!(
            !ui.ui_state.select_mode,
            "Mouse capture is on by default — select_mode must start false"
        );
    }

    #[test]
    fn f4_toggles_select_mode() {
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::F(4)));
        assert!(ui.ui_state.select_mode, "F4 enters select mode");
        ui.handle_keyboard_input(press(KeyCode::F(4)));
        assert!(!ui.ui_state.select_mode, "F4 again exits select mode");
    }

    #[test]
    fn f4_does_not_send_action() {
        // F4 is purely view-local — no UiAction should hit the controller.
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(press(KeyCode::F(4)));
        assert!(
            rx.try_recv().is_err(),
            "F4 must not emit a UiAction — it's view-local"
        );
    }

    #[test]
    fn f4_does_not_emit_mouse_escapes_without_a_terminal() {
        // Regression: previously `toggle_select_mode` unconditionally ran
        // `execute!(stdout(), EnableMouseCapture)`. Under `cargo test` the
        // harness never calls `init()` (no alt screen, no shutdown), so
        // those bytes leaked onto the developer's live terminal, leaving
        // mouse-reporting enabled and spamming the prompt with sequences
        // like `35;43;18M`. The contract: when no real terminal is
        // attached (`self.terminal.is_none()`), the toggle must flip
        // state but emit zero IO. The precondition assertion below is
        // the structural guard — if the harness ever starts attaching a
        // terminal, this test must be revisited.
        let (mut ui, _rx) = harness();
        assert!(
            ui.terminal.is_none(),
            "harness must not attach a real terminal"
        );
        ui.handle_keyboard_input(press(KeyCode::F(4)));
        assert!(ui.ui_state.select_mode);
        ui.handle_keyboard_input(press(KeyCode::F(4)));
        assert!(!ui.ui_state.select_mode, "two F4 presses cancel out");
    }

    // ─── Windows duplicate-key regression ──────────────────────────────
    //
    // On Windows, crossterm's underlying `ReadConsoleInputW` always
    // reports BOTH press and release records, so `KeyEvent.kind` cycles
    // through `Press`, then `Release` for every keystroke. On Linux/macOS
    // (without `KeyboardEnhancementFlags::REPORT_EVENT_TYPES` pushed —
    // which we don't push) only `Press` is delivered. If `handle_input`
    // does not filter on `kind`, every keystroke fires the handler twice
    // on Windows: typing `a` produces `aa`, Backspace deletes two chars,
    // and so on. See crossterm 0.29 `event.rs::KeyEvent::kind` doc and
    // ratatui FAQ "Why am I getting duplicate key events on Windows?".
    //
    // Contract pinned by the tests below: `handle_input` must dispatch
    // ONLY for `Press` and `Repeat` (autorepeat); `Release` is a no-op.

    fn key_with_kind(code: KeyCode, kind: crossterm::event::KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind)
    }

    #[test]
    fn key_release_is_dropped_at_dispatch_boundary() {
        // Windows-shape sequence: Press 'a', Release 'a'. If the
        // dispatcher passes Release through to `handle_keyboard_input`,
        // the buffer ends up "aa". Correct behaviour: "a".
        let (mut ui, _rx) = harness();
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Char('a'),
            crossterm::event::KeyEventKind::Press,
        )));
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Char('a'),
            crossterm::event::KeyEventKind::Release,
        )));
        assert_eq!(
            ui.ui_state.input_buffer, "a",
            "Release event must not re-fire the keystroke"
        );
    }

    #[test]
    fn key_repeat_is_treated_as_a_press() {
        // Autorepeat (holding a key) arrives as `Repeat` on Windows /
        // kitty-protocol terminals. Each Repeat event is a real
        // keystroke from the user's POV.
        let (mut ui, _rx) = harness();
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Char('a'),
            crossterm::event::KeyEventKind::Press,
        )));
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Char('a'),
            crossterm::event::KeyEventKind::Repeat,
        )));
        assert_eq!(ui.ui_state.input_buffer, "aa");
    }

    #[test]
    fn backspace_release_does_not_double_delete() {
        // Reported symptom on Windows: pressing Backspace once removed
        // two characters because the Release event hit
        // `handle_keyboard_input` a second time.
        let (mut ui, _rx) = harness();
        ui.ui_state.input_buffer = "ab".to_string();
        ui.ui_state.cursor_pos = 2;
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Backspace,
            crossterm::event::KeyEventKind::Press,
        )));
        ui.handle_input(Event::Key(key_with_kind(
            KeyCode::Backspace,
            crossterm::event::KeyEventKind::Release,
        )));
        assert_eq!(
            ui.ui_state.input_buffer, "a",
            "one Backspace press must delete exactly one char"
        );
    }

    #[test]
    fn drop_without_init_is_a_safe_noop() {
        // Drop guard contract: when `init()` was never called
        // (`self.terminal.is_none()`), Drop must do nothing and must not
        // panic. The harness builds a ReplUi via `ReplUi::new` without
        // calling `init()`, which is exactly the "agent crashed before
        // attaching a terminal" / "test fixture goes out of scope" path.
        //
        // We cannot directly test the *attached* Drop branch in unit
        // tests because emitting `DisableMouseCapture` to real stdout
        // is precisely the leak we're guarding against — the test
        // itself would graffiti the developer's tty. The structural
        // floor pinned here is: dropping any test-built ReplUi never
        // explodes. If Drop ever starts panicking (eg. someone adds an
        // `unwrap`), this test will fail with `panic in Drop` and
        // surface the regression. The remaining "attached" branch is
        // four lines of `let _ = ...` (all errors swallowed) and is
        // verified by inspection; if it grows, extract a pure helper.
        let (ui, _rx) = harness();
        assert!(
            ui.terminal.is_none(),
            "harness must not attach a real terminal — Drop branch under test is the unattached one"
        );
        drop(ui);
    }

    // The `chat_block(select_mode)` contract: when `select_mode` is true,
    // the block must produce no glyphs at all (so a terminal-native copy
    // doesn't pick up borders/titles/hints). Pinned by rendering the
    // returned block into a buffer and asserting every cell is empty.

    #[test]
    fn chat_block_select_mode_has_no_borders() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::Widget;
        let b = ReplUi::chat_block(true);
        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        let _ = term.draw(|f| {
            b.render(f.area(), f.buffer_mut());
        });
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                assert_eq!(
                    cell.symbol(),
                    " ",
                    "select_mode chat block must have no glyphs (got {:?} at ({}, {}))",
                    cell.symbol(),
                    x,
                    y
                );
            }
        }
    }

    // ─── Chat-pane width accounting (chrome subtraction) ──────────────
    //
    // Pre-fix bug: `chat_wrap_width = main.width - 2` accounted for the
    // chat block's two borders but missed the right-edge scrollbar, so
    // the renderer thought it had 1 more cell than it actually did. In
    // select mode there's no scrollbar AND no block, so the bug was
    // invisible there — exactly the symptom the user reported ("code
    // blocks render properly only in F4"). The fix lives in
    // `chat_pane_content_width`; this test pins it.

    #[test]
    fn chat_pane_content_width_subtracts_borders_and_scrollbar() {
        // Normal mode: 2 (block borders) + 1 (scrollbar) = 3 cells of
        // chrome. Anything else and code-block rules overflow.
        assert_eq!(chat_pane_content_width(80, false), 77);
        assert_eq!(chat_pane_content_width(120, false), 117);
    }

    #[test]
    fn chat_pane_content_width_select_mode_uses_full_width() {
        // Select mode strips chrome entirely (no block, no scrollbar);
        // markdown content can use the full pane.
        assert_eq!(chat_pane_content_width(80, true), 80);
        assert_eq!(chat_pane_content_width(120, true), 120);
    }

    #[test]
    fn chat_pane_content_width_saturates_on_tiny_terminal() {
        // Pathologically narrow terminal (≤3 cells in normal mode):
        // the helper must not underflow. Renderer downstream handles
        // 0-width as "no usable space".
        assert_eq!(chat_pane_content_width(3, false), 0);
        assert_eq!(chat_pane_content_width(2, false), 0);
        assert_eq!(chat_pane_content_width(0, false), 0);
    }

    // ─── Direct CommandPopupState hooks (sanity) ─────────────────────────

    #[test]
    fn popup_state_starts_at_index_zero() {
        let p = CommandPopupState::new(String::new());
        assert_eq!(p.selected_index, 0);
        assert_eq!(p.scroll_offset, 0);
    }

    // ─── Foreground bash stdin (slice 4 of #11) ──────────────────────────

    /// Helper: put the bash panel into Running so the Ctrl+S gate opens.
    fn make_running_panel(ui: &ReplUi) {
        ui.state_manager
            .start_bash_panel("read x; echo got: $x".into(), 1234);
    }

    #[test]
    fn ctrl_s_focuses_stdin_only_when_bash_running() {
        let (mut ui, _rx) = harness();
        assert!(!ui.stdin_focused, "starts unfocused");

        // No panel yet — Ctrl+S falls through (it's gated).
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            !ui.stdin_focused,
            "Ctrl+S must not focus stdin when no bash is running"
        );

        // Now make the panel Running and try again.
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(ui.stdin_focused, "Ctrl+S should focus stdin while Running");
    }

    #[test]
    fn esc_unfocuses_stdin_and_preserves_buffer() {
        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        // Type a few characters into stdin.
        ui.handle_keyboard_input(press(KeyCode::Char('h')));
        ui.handle_keyboard_input(press(KeyCode::Char('i')));
        assert_eq!(ui.stdin_buffer, "hi");
        assert!(ui.stdin_focused);

        // Esc unfocuses but preserves the buffer.
        ui.handle_keyboard_input(press(KeyCode::Esc));
        assert!(!ui.stdin_focused, "Esc should unfocus");
        assert_eq!(
            ui.stdin_buffer, "hi",
            "Esc must preserve buffer (recovery contract)"
        );
    }

    #[test]
    fn enter_while_stdin_focused_sends_via_state_manager() {
        use tokio::sync::mpsc::unbounded_channel;

        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        // Register a real stdin sender on the state manager so the
        // send succeeds — without one, try_forward_bash_stdin returns
        // Err and we'd be testing the wrong path.
        let (tx, mut stdin_rx) = unbounded_channel::<String>();
        ui.state_manager.set_bash_stdin_tx(tx);

        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        ui.handle_keyboard_input(press(KeyCode::Char('o')));
        ui.handle_keyboard_input(press(KeyCode::Char('k')));
        ui.handle_keyboard_input(press(KeyCode::Enter));

        // On success the buffer is cleared and the line lands on rx.
        assert_eq!(ui.stdin_buffer, "");
        let got = stdin_rx.try_recv().expect("send should have queued a line");
        assert_eq!(got, "ok");
        // Focus stays — the user can keep typing follow-ups (e.g. a
        // second sudo prompt) without re-pressing Ctrl+S.
        assert!(ui.stdin_focused);
    }

    #[test]
    fn enter_with_no_active_stdin_preserves_buffer_and_drops_focus() {
        let (mut ui, _rx) = harness();
        // Bash panel is Running (so Ctrl+S focuses) but we deliberately
        // do NOT register a stdin tx — try_forward_bash_stdin must
        // return Err(StdinNotActive). The handler should preserve the
        // buffer (recovery contract) and drop focus.
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        ui.handle_keyboard_input(press(KeyCode::Char('p')));
        ui.handle_keyboard_input(press(KeyCode::Char('w')));
        assert_eq!(ui.stdin_buffer, "pw");
        ui.handle_keyboard_input(press(KeyCode::Enter));
        assert_eq!(ui.stdin_buffer, "pw", "buffer must survive a failed send");
        assert!(
            !ui.stdin_focused,
            "focus should drop so next Enter doesn't keep retrying blindly"
        );
    }

    #[test]
    fn backspace_pops_utf8_chars_from_stdin_buffer() {
        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        // Multi-byte char to make sure we pop by char, not by byte.
        ui.stdin_buffer.push_str("aé");
        assert_eq!(ui.stdin_buffer.len(), 3); // 'a' is 1 byte, 'é' is 2
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert_eq!(
            ui.stdin_buffer, "a",
            "should pop the multi-byte char cleanly"
        );
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert_eq!(ui.stdin_buffer, "");
        // Backspace on empty is a safe no-op.
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert_eq!(ui.stdin_buffer, "");
    }

    #[test]
    fn stdin_focus_resets_when_bash_panel_leaves_running() {
        // Renders trigger the auto-reset; we simulate by calling the
        // logic inline. The full render path needs a Terminal which
        // tests don't have, but the auto-reset block runs before the
        // terminal section.
        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('s'), KeyModifiers::CONTROL));
        ui.handle_keyboard_input(press(KeyCode::Char('x')));
        assert!(ui.stdin_focused);
        assert_eq!(ui.stdin_buffer, "x");

        // Producer-side: finish the panel. Snapshot the resulting
        // state and pass it through the auto-reset path that lives at
        // the top of render().
        ui.state_manager.finish_bash_panel(0, vec![]);
        let snap = ui.state_manager.get_state();
        // Replicate the exact auto-reset block from render():
        if ui.stdin_focused && !snap.bash_panel.is_running() {
            ui.stdin_focused = false;
            ui.stdin_buffer.clear();
        }
        assert!(
            !ui.stdin_focused,
            "focus must reset when panel leaves Running"
        );
        assert_eq!(
            ui.stdin_buffer, "",
            "buffer must clear when there's nothing to type at"
        );
    }

    // ── Bash panel visibility (bash-panel-as-real-panel.md) ──────────────

    fn make_finished_panel(ui: &ReplUi) {
        ui.state_manager.start_bash_panel("make".into(), 99);
        ui.state_manager.finish_bash_panel(0, vec!["done".into()]);
    }

    #[test]
    fn ctrl_b_with_running_panel_closes_it() {
        // Running + Ctrl+B ⇒ ClosedByUser (panel collapses).
        // Underlying state stays Running so bash output keeps streaming
        // under the hood.
        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        assert!(matches!(
            ui.state_manager.get_state().bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::Auto
        ));
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        let snap = ui.state_manager.get_state();
        assert!(matches!(
            snap.bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::ClosedByUser
        ));
        assert!(
            snap.bash_panel.is_running(),
            "underlying state must NOT change — bash keeps running"
        );
    }

    #[test]
    fn ctrl_b_with_finished_panel_closes_it() {
        // Finished + Ctrl+B ⇒ ClosedByUser; underlying state stays
        // Finished. Visibility persists across user messages (see
        // user_close_survives_new_user_message in state_manager tests).
        let (mut ui, _rx) = harness();
        make_finished_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        let snap = ui.state_manager.get_state();
        assert!(matches!(
            snap.bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::ClosedByUser
        ));
        assert!(matches!(
            snap.bash_panel,
            crate::ui::app_state::BashPanelState::Finished { .. }
        ));
    }

    #[test]
    fn ctrl_b_with_idle_panel_opens_empty_frame() {
        // The "open it anytime, like the tasks panel" contract:
        // Ctrl+B on Idle is NO LONGER a no-op — it opens the panel
        // (OpenedByUser). The renderer will draw the 3-row empty
        // frame.
        let (mut ui, _rx) = harness();
        assert!(ui.state_manager.get_state().bash_panel.is_idle());
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(matches!(
            ui.state_manager.get_state().bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::OpenedByUser
        ));
    }

    #[test]
    fn ctrl_b_toggles_when_already_closed() {
        // Press once → ClosedByUser. Press again → OpenedByUser
        // (toggle inverts effective visibility).
        let (mut ui, _rx) = harness();
        make_running_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(matches!(
            ui.state_manager.get_state().bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::ClosedByUser
        ));
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(matches!(
            ui.state_manager.get_state().bash_panel_visibility,
            crate::ui::app_state::BashPanelVisibility::OpenedByUser
        ));
    }

    #[test]
    fn ctrl_b_does_not_request_agent_stop() {
        // The cardinal pin behind this whole feature: Ctrl+B must NOT
        // emit `UiAction::RequestStop`. Esc (the v1 proposal) would
        // have routed through the agent-interrupt catch-all the moment
        // a Finished panel showed. The user's original pushback
        // ("esc is used to stop the agent") created this test. See
        // close-bash-panel-v2.md.
        let (mut ui, mut rx) = harness();
        make_finished_panel(&ui);
        ui.handle_keyboard_input(press_with(KeyCode::Char('b'), KeyModifiers::CONTROL));
        // No action should land on the channel — Ctrl+B is view-only.
        assert!(rx.try_recv().is_err(), "Ctrl+B must not emit any UiAction");
    }

    #[test]
    fn esc_still_interrupts_agent_with_finished_panel_visible() {
        // Regression guard: pin the existing Esc semantic that the v1
        // Esc-based proposal would have broken. With a Finished panel
        // visible and the agent running, Esc must still route to
        // RequestStop, not be swallowed by some bash-panel arm.
        let (mut ui, mut rx) = harness();
        make_finished_panel(&ui);
        ui.state_manager.set_running(true);
        ui.handle_keyboard_input(press(KeyCode::Esc));
        match rx.try_recv() {
            Ok(UiAction::RequestStop) => {}
            other => panic!("expected UiAction::RequestStop, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod multiline_mode_tests {
    //! Test-first specs for **multiline compose mode** (Ctrl+G toggle).
    //! See `multiline-mode.md`.
    //!
    //! Contract:
    //!   - Ctrl+G with mode=off → enter mode, buffer untouched, no send.
    //!   - Ctrl+G with mode=on  → submit (if non-empty) + clear buffer + exit mode.
    //!   - Enter while mode=on  → insert `\n` (NOT submit).
    //!   - Esc  while mode=on   → exit mode, buffer preserved (cancel verb).
    //!   - Mode is gated off while quit-confirm or command popup is open.
    //!   - Shift/Alt+Enter still inserts newline regardless of mode.
    //!   - After accept_command (popup → SendMessage), mode is reset.
    use super::*;
    use crate::state::StateManager;
    use crate::ui::ui_trait::UiAction;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn harness() -> (ReplUi, UnboundedReceiver<UiAction>) {
        let sm = StateManager::new_arc();
        let (tx, rx) = unbounded_channel();
        (ReplUi::new(sm, tx), rx)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn shift_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    }

    // ─── Initial state ────────────────────────────────────────────────────

    #[test]
    fn multiline_mode_starts_off() {
        let (ui, _rx) = harness();
        assert!(!ui.multiline_mode, "fresh ReplUi must default mode=off");
    }

    // ─── Toggle on / toggle off ───────────────────────────────────────────

    #[test]
    fn ctrl_g_with_empty_buffer_enters_mode_no_send() {
        let (mut ui, mut rx) = harness();
        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));
        assert!(ui.multiline_mode, "first Ctrl+G must turn mode on");
        assert_eq!(
            ui.ui_state.input_buffer, "",
            "buffer must remain untouched on toggle-on"
        );
        assert!(
            rx.try_recv().is_err(),
            "no UiAction must be sent on toggle-on"
        );
    }

    #[test]
    fn ctrl_g_uppercase_also_toggles() {
        // Some terminals emit the shifted form for Ctrl+letter; both
        // 'g' and 'G' must work or the binding will mysteriously fail
        // when CapsLock is on.
        let (mut ui, _rx) = harness();
        ui.handle_keyboard_input(ctrl(KeyCode::Char('G')));
        assert!(ui.multiline_mode);
    }

    #[test]
    fn ctrl_g_in_mode_with_text_submits_and_exits() {
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;
        ui.ui_state.input_buffer = "hello\nworld".to_string();
        ui.ui_state.cursor_pos = 11;

        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));

        assert!(!ui.multiline_mode, "second Ctrl+G must turn mode off");
        assert_eq!(ui.ui_state.input_buffer, "", "buffer must be cleared");
        match rx.try_recv() {
            Ok(UiAction::SendMessage(m)) => assert_eq!(m, "hello\nworld"),
            other => panic!("expected SendMessage(\"hello\\nworld\"), got {other:?}"),
        }
    }

    #[test]
    fn ctrl_g_in_mode_with_empty_buffer_just_exits_no_send() {
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;

        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));

        assert!(!ui.multiline_mode);
        assert_eq!(ui.ui_state.input_buffer, "");
        assert!(rx.try_recv().is_err(), "empty submit must not send");
    }

    #[test]
    fn ctrl_g_in_mode_with_whitespace_only_does_not_send() {
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;
        ui.ui_state.input_buffer = "   \n  ".to_string();
        ui.ui_state.cursor_pos = 6;

        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));

        assert!(!ui.multiline_mode, "mode must still exit on whitespace");
        assert_eq!(ui.ui_state.input_buffer, "");
        assert!(
            rx.try_recv().is_err(),
            "whitespace-only buffer must not send"
        );
    }

    // ─── Enter behaviour while in mode ────────────────────────────────────

    #[test]
    fn enter_in_mode_inserts_newline_not_submit() {
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;
        ui.ui_state.input_buffer = "abc".to_string();
        ui.ui_state.cursor_pos = 3;

        ui.handle_keyboard_input(press(KeyCode::Enter));

        assert!(ui.multiline_mode, "Enter must not exit the mode");
        assert_eq!(
            ui.ui_state.input_buffer, "abc\n",
            "Enter in mode must insert \\n at cursor"
        );
        assert!(rx.try_recv().is_err(), "Enter in mode must not send");
    }

    #[test]
    fn enter_outside_mode_still_submits_as_before() {
        let (mut ui, mut rx) = harness();
        // mode=off (default)
        ui.ui_state.input_buffer = "hello".to_string();
        ui.ui_state.cursor_pos = 5;

        ui.handle_keyboard_input(press(KeyCode::Enter));

        match rx.try_recv() {
            Ok(UiAction::SendMessage(m)) => assert_eq!(m, "hello"),
            other => panic!("expected SendMessage, got {other:?}"),
        }
        assert_eq!(ui.ui_state.input_buffer, "");
    }

    #[test]
    fn shift_enter_still_inserts_newline_in_mode() {
        // Muscle-memory path stays alive — Shift+Enter is a portable
        // newline regardless of mode.
        let (mut ui, _rx) = harness();
        ui.multiline_mode = true;
        ui.ui_state.input_buffer = "abc".to_string();
        ui.ui_state.cursor_pos = 3;

        ui.handle_keyboard_input(shift_enter());

        assert_eq!(ui.ui_state.input_buffer, "abc\n");
        assert!(ui.multiline_mode);
    }

    // ─── Esc cancel ───────────────────────────────────────────────────────

    #[test]
    fn esc_in_mode_exits_without_submitting() {
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;
        ui.ui_state.input_buffer = "draft".to_string();
        ui.ui_state.cursor_pos = 5;

        ui.handle_keyboard_input(press(KeyCode::Esc));

        assert!(!ui.multiline_mode, "Esc must exit multiline mode");
        assert_eq!(
            ui.ui_state.input_buffer, "draft",
            "Esc must preserve the buffer"
        );
        assert!(rx.try_recv().is_err(), "Esc must not send");
    }

    // ─── Gating ───────────────────────────────────────────────────────────

    #[test]
    fn ctrl_g_is_gated_off_while_quit_confirm_is_open() {
        let (mut ui, _rx) = harness();
        ui.confirm_dialog = Some(ConfirmDialog::quit());

        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));

        assert!(
            !ui.multiline_mode,
            "Ctrl+G must be a no-op while a confirm dialog is open"
        );
    }

    #[test]
    fn ctrl_g_is_gated_off_while_command_popup_is_open() {
        let (mut ui, _rx) = harness();
        // Open the popup the natural way (slash on empty buffer).
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert!(ui.command_popup.is_some(), "precondition: popup is open");

        ui.handle_keyboard_input(ctrl(KeyCode::Char('g')));

        assert!(
            !ui.multiline_mode,
            "Ctrl+G must not toggle mode while popup is open"
        );
    }

    // ─── Reset on slash-command submission ────────────────────────────────

    #[test]
    fn accept_command_clears_multiline_mode() {
        // Edge case: user hits Ctrl+G on an empty buffer, then types `/`,
        // then runs a command via popup Enter. The composition is over —
        // mode must auto-exit so the next message isn't surprising.
        let (mut ui, mut rx) = harness();
        ui.multiline_mode = true;

        // Open popup, navigate to a no-args command, run it.
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Char('t')));
        ui.handle_keyboard_input(press(KeyCode::Char('a')));
        ui.handle_keyboard_input(press(KeyCode::Char('t')));
        ui.handle_keyboard_input(press(KeyCode::Char('s')));
        ui.handle_keyboard_input(press(KeyCode::Enter));

        // Sanity: the slash command was submitted.
        match rx.try_recv() {
            Ok(UiAction::SendMessage(m)) => assert_eq!(m, "/stats"),
            other => panic!("expected SendMessage(\"/stats\"), got {other:?}"),
        }

        assert!(
            !ui.multiline_mode,
            "running a slash command must clear multiline_mode"
        );
    }

    // ─── Title / chrome wiring ────────────────────────────────────────────

    #[test]
    fn build_input_paragraph_accepts_multiline_flag() {
        // Just a compile-time pin: the signature must accept the new
        // bool. The visual diff is owned by snapshot tests.
        let _p = ReplUi::build_input_paragraph("hi", 2, false, None, None, 0, 0, true);
        let _p = ReplUi::build_input_paragraph("hi", 2, false, None, None, 0, 0, false);
    }
}

#[cfg(test)]
mod model_popup_tests {
    //! Test-first specs for the `/model <alias>` argument-completion
    //! popup.
    //!
    //! These pin down:
    //!   - typing `/model ` opens the popup in `Argument` mode with the
    //!     registry's aliases as items
    //!   - typing more characters filters by alias prefix
    //!   - the active alias is marked `is_current` so the renderer can
    //!     prefix it with `→`
    //!   - Enter on a chosen alias routes through
    //!     `try_intercept_model_command` (so the confirm-dialog flow
    //!     fires for non-empty chats)
    //!   - Tab on a chosen alias fills the buffer to `/model <alias>`
    //!     and closes the popup
    //!   - without a registry (legacy single-provider boot) the popup
    //!     never opens in `Argument` mode
    //!
    //! See proposal `glorious-popup.md` and the architecture note in
    //! memory.md (2026-05-07).
    use super::*;
    use crate::ProviderType;
    use crate::config::ModelRegistry;
    use crate::config::model_registry::{ModelEntry, ProviderEntry};
    use crate::state::StateManager;
    use crate::ui::ui_trait::{PopupMode, UiAction};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Build a two-alias registry: `sonnet` (default) and `opus`. Same
    /// shape used by the multi-model tests in `model_registry.rs`.
    fn two_alias_registry() -> Arc<ModelRegistry> {
        let prov = ProviderEntry {
            name: "openrouter".into(),
            kind: ProviderType::OpenRouter,
            api_key: Some("sk-or-test".into()),
            base_url: None,
            models: vec![
                ModelEntry {
                    name: "anthropic/claude-3.7-sonnet".into(),
                    alias: Some("sonnet".into()),
                    max_tokens: Some(8192),
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                },
                ModelEntry {
                    name: "anthropic/claude-opus-4".into(),
                    alias: Some("opus".into()),
                    max_tokens: None,
                    temperature: None,
                    extra_params: None,
                    prompt_caching: None,
                    vision: None,
                    context_size: None,
                },
            ],
        };
        Arc::new(ModelRegistry::build(&[prov], Some("sonnet")).expect("registry should build"))
    }

    fn harness_with_registry() -> (ReplUi, UnboundedReceiver<UiAction>) {
        let sm = StateManager::new_arc();
        // Mark the active alias so `is_current` flags can light up.
        sm.set_model_alias("sonnet".to_string());
        let (tx, rx) = unbounded_channel();
        let ui = ReplUi::new_with_registry(sm, tx, two_alias_registry());
        (ui, rx)
    }

    fn harness_no_registry() -> (ReplUi, UnboundedReceiver<UiAction>) {
        let sm = StateManager::new_arc();
        let (tx, rx) = unbounded_channel();
        (ReplUi::new(sm, tx), rx)
    }

    /// Type a string literally via `handle_keyboard_input` so each
    /// keystroke goes through the full sync_popup path.
    fn type_str(ui: &mut ReplUi, s: &str) {
        for c in s.chars() {
            ui.handle_keyboard_input(press(KeyCode::Char(c)));
        }
    }

    #[test]
    fn model_space_opens_arg_popup_with_aliases() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup should open after `/model <space>`");
        assert!(matches!(
            &popup.mode,
            PopupMode::Argument { command } if command == "model"
        ));
        // Both aliases present; alphabetical (opus, sonnet).
        let values: Vec<&str> = popup.all_items.iter().map(|i| i.value.as_str()).collect();
        assert_eq!(values, vec!["opus", "sonnet"]);
        // Active alias gets the marker.
        let sonnet = popup
            .all_items
            .iter()
            .find(|i| i.value == "sonnet")
            .unwrap();
        assert!(sonnet.is_current, "active alias should be marked");
        let opus = popup.all_items.iter().find(|i| i.value == "opus").unwrap();
        assert!(!opus.is_current);
    }

    #[test]
    fn model_arg_popup_filters_by_alias_prefix() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model son");
        let popup = ui.command_popup.as_ref().unwrap();
        let filtered: Vec<&str> = popup
            .filtered_items()
            .iter()
            .map(|i| i.value.as_str())
            .collect();
        assert_eq!(filtered, vec!["sonnet"], "prefix `son` only matches sonnet");
    }

    #[test]
    fn enter_on_model_alias_routes_through_interceptor() {
        // Non-empty chat → Enter on a chosen alias should open the
        // confirm dialog rather than dispatch a raw SwitchModel action.
        let (mut ui, mut rx) = harness_with_registry();
        // Make chat non-empty so the interceptor routes via the dialog.
        ui.state_manager
            .add_user_message("a previous turn".to_string());
        type_str(&mut ui, "/model opus");
        // Make sure the popup is on the `opus` row.
        let popup = ui.command_popup.as_ref().unwrap();
        assert_eq!(popup.selected_item().unwrap().value, "opus");
        // Enter while popup open → accept_command(submit=true).
        ui.handle_keyboard_input(press(KeyCode::Enter));
        // Expect: confirm dialog open, NO SwitchModel action emitted.
        assert!(
            ui.confirm_dialog.is_some(),
            "confirm dialog must open when chat is non-empty"
        );
        assert!(
            rx.try_recv().is_err(),
            "no UiAction should be sent yet — must wait for confirmation"
        );
        // And the input buffer is cleared.
        assert_eq!(ui.ui_state.input_buffer, "");
        assert!(ui.command_popup.is_none(), "popup closed after accept");
    }

    #[test]
    fn enter_on_model_alias_with_empty_chat_dispatches_switch_directly() {
        let (mut ui, mut rx) = harness_with_registry();
        // Empty chat — interceptor sends SwitchModel directly, no dialog.
        type_str(&mut ui, "/model opus");
        ui.handle_keyboard_input(press(KeyCode::Enter));
        assert!(
            ui.confirm_dialog.is_none(),
            "no dialog expected on empty chat"
        );
        let action = rx.try_recv().expect("a UiAction should be sent");
        assert_eq!(action, UiAction::SwitchModel("opus".to_string()));
    }

    // === /cd interceptor ==============================================

    /// A real, distinct directory the tests can `/cd` into. Uses the
    /// system temp dir canonicalised (macOS symlinks /tmp → /private/tmp,
    /// so we compare against the canonical form the interceptor produces).
    fn a_real_other_dir() -> String {
        let tmp = std::env::temp_dir();
        tmp.canonicalize()
            .unwrap_or(tmp)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn resolve_cd_path_rejects_nonexistent() {
        assert!(resolve_cd_path("/no/such/dir/anywhere/xyz").is_err());
    }

    #[test]
    fn resolve_cd_path_canonicalises_existing_dir() {
        let tmp = std::env::temp_dir();
        let expected = tmp.canonicalize().unwrap().to_string_lossy().into_owned();
        let got = resolve_cd_path(&tmp.to_string_lossy()).expect("temp dir resolves");
        assert_eq!(got, expected);
    }

    #[test]
    fn resolve_cd_path_expands_tilde() {
        // `~` should expand to $HOME and resolve, when HOME is a real dir.
        if let Ok(home) = std::env::var("HOME")
            && std::path::Path::new(&home).is_dir()
        {
            let got = resolve_cd_path("~").expect("~ resolves");
            let expected = std::path::Path::new(&home)
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn abbreviate_path_returns_short_path_unchanged() {
        let p = std::path::Path::new("/tmp/proj");
        assert_eq!(abbreviate_path(p, 48), "/tmp/proj");
    }

    #[test]
    fn abbreviate_path_tail_truncates_long_path() {
        // A path longer than the budget keeps the tail behind a leading `…`.
        let p = std::path::Path::new("/aaaa/bbbb/cccc/dddd/eeee/ffff/leaf");
        let got = abbreviate_path(p, 12);
        assert_eq!(got.chars().count(), 12, "abbreviated to the budget: {got}");
        assert!(got.starts_with('…'), "leading ellipsis: {got}");
        assert!(got.ends_with("leaf"), "keeps the leaf dir: {got}");
    }

    #[test]
    fn abbreviate_path_home_becomes_tilde() {
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            let p = std::path::PathBuf::from(&home).join("workspace");
            assert_eq!(abbreviate_path(&p, 48), "~/workspace");
        }
    }

    #[test]
    fn status_bar_shows_cwd_when_welcome_set() {
        use crate::ui::app_state::{AppState, WelcomeState};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let state = AppState {
            welcome: Some(WelcomeState {
                provider_name: "p".into(),
                model: "m".into(),
                max_tokens: 1,
                builtin_tools_count: 0,
                mcp_tools_count: 0,
                skills_count: 0,
                searxng_enabled: false,
                searxng_url: None,
                cost_tracking_enabled: false,
                compaction_enabled: false,
                compaction_threshold: 0.0,
                compaction_keep_recent: 0,
                conversation_persistence_enabled: false,
                cwd: std::path::PathBuf::from("/tmp/peakbot-cwd-test"),
                peakbot_version: String::new(),
            }),
            ..Default::default()
        };

        let mut terminal = Terminal::new(TestBackend::new(120, 3)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                ReplUi::render_status_bar(f, area, &state);
            })
            .unwrap();
        let rendered = format!("{:?}", terminal.backend().buffer());
        assert!(
            rendered.contains("peakbot-cwd-test"),
            "status bar must show the cwd; got:\n{rendered}"
        );
    }

    #[test]
    fn cd_bare_falls_through_to_controller() {
        // Bare `/cd` is NOT consumed by the interceptor — the controller
        // prints the cwd. `try_intercept_cd_command` returns false.
        let (mut ui, _rx) = harness_no_registry();
        assert!(!ui.try_intercept_cd_command("/cd"));
    }

    #[test]
    fn cd_nonexistent_path_errors_without_dispatch() {
        let (mut ui, mut rx) = harness_no_registry();
        let consumed = ui.try_intercept_cd_command("/cd /no/such/dir/anywhere/xyz");
        assert!(consumed, "an invalid /cd is still consumed (error shown)");
        assert!(
            rx.try_recv().is_err(),
            "no action dispatched for an invalid path"
        );
        assert!(ui.confirm_dialog.is_none(), "no dialog for an invalid path");
    }

    #[test]
    fn cd_same_dir_is_noop() {
        let (mut ui, mut rx) = harness_no_registry();
        let here = std::env::current_dir().unwrap();
        let consumed = ui.try_intercept_cd_command(&format!("/cd {}", here.display()));
        assert!(consumed);
        assert!(
            rx.try_recv().is_err(),
            "/cd into the current dir dispatches nothing"
        );
        assert!(ui.confirm_dialog.is_none());
    }

    #[test]
    fn cd_valid_empty_chat_dispatches_directly() {
        let (mut ui, mut rx) = harness_no_registry();
        let target = a_real_other_dir();
        // Skip if temp_dir happens to be the cwd (won't be in CI, but be safe).
        let here = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if target == here {
            return;
        }
        let consumed = ui.try_intercept_cd_command(&format!("/cd {target}"));
        assert!(consumed);
        assert!(ui.confirm_dialog.is_none(), "empty chat → no dialog");
        match rx.try_recv().expect("a ChangeCwd action should be sent") {
            UiAction::ChangeCwd(p) => assert_eq!(p, target),
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn cd_valid_nonempty_chat_opens_confirm_dialog() {
        let (mut ui, mut rx) = harness_no_registry();
        let target = a_real_other_dir();
        let here = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if target == here {
            return;
        }
        ui.state_manager.add_user_message("a previous turn".into());
        let consumed = ui.try_intercept_cd_command(&format!("/cd {target}"));
        assert!(consumed);
        assert!(
            rx.try_recv().is_err(),
            "non-empty chat → wait for confirmation, no action yet"
        );
        match ui.confirm_dialog.as_ref().map(|d| &d.action) {
            Some(ConfirmAction::ChangeCwd { path }) => assert_eq!(path, &target),
            other => panic!("expected ChangeCwd confirm dialog, got {other:?}"),
        }
    }

    #[test]
    fn tab_on_model_alias_fills_buffer_and_closes_popup() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model op");
        // Sanity: opus is the only filtered item.
        assert_eq!(
            ui.command_popup
                .as_ref()
                .unwrap()
                .selected_item()
                .unwrap()
                .value,
            "opus"
        );
        ui.handle_keyboard_input(press(KeyCode::Tab));
        assert_eq!(ui.ui_state.input_buffer, "/model opus");
        assert_eq!(ui.ui_state.cursor_pos, "/model opus".len());
        // Tab in arg mode closes the popup (consistent with SlashCommand
        // mode Tab semantics).
        assert!(ui.command_popup.is_none(), "Tab closes the popup");
    }

    #[test]
    fn arg_popup_does_not_open_without_registry() {
        let (mut ui, _rx) = harness_no_registry();
        type_str(&mut ui, "/model ");
        // Buffer ends in a whitespace and no registry to populate Arg
        // items → popup must stay closed (legacy single-provider boot
        // behaviour).
        assert!(
            ui.command_popup.is_none(),
            "no registry → no arg-mode popup"
        );
    }

    #[test]
    fn backspace_from_argument_mode_returns_to_slashcommand_mode() {
        // Type `/model ` → Argument mode. Backspace once → buffer
        // becomes `/model` → should transition back to SlashCommand
        // mode (so the user sees the slash-command list again).
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        assert!(matches!(
            ui.command_popup.as_ref().unwrap().mode,
            PopupMode::Argument { .. }
        ));
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup should remain open after backspace into command name");
        assert!(matches!(popup.mode, PopupMode::SlashCommand));
        assert_eq!(popup.prefix, "model");
    }

    /// Regression pin for issue #52: backspacing from Argument mode past
    /// the `/` should keep the popup open in SlashCommand mode all the
    /// way down to an empty prefix, then close only when the `/` itself
    /// is removed.
    #[test]
    fn backspace_from_argument_mode_past_slash_keeps_popup_open() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        assert!(
            matches!(
                ui.command_popup.as_ref().unwrap().mode,
                PopupMode::Argument { .. }
            ),
            "precondition: popup is in Argument mode"
        );

        // 1 backspace: `/model ` → `/model` → transition to SlashCommand
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup must stay open after backspacing space");
        assert!(matches!(popup.mode, PopupMode::SlashCommand));
        assert_eq!(popup.prefix, "model");

        // 2 backspaces: `/model` → `/mode`
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().expect("popup must stay open");
        assert_eq!(popup.prefix, "mode");

        // 3 backspaces: `/mode` → `/mod`
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().expect("popup must stay open");
        assert_eq!(popup.prefix, "mod");

        // 4 backspaces: `/mod` → `/mo`
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().expect("popup must stay open");
        assert_eq!(popup.prefix, "mo");

        // 5 backspaces: `/mo` → `/m`
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().expect("popup must stay open");
        assert_eq!(popup.prefix, "m");

        // 6 backspaces: `/m` → `/` → empty prefix, still open
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().expect("popup must stay open");
        assert_eq!(popup.prefix, "");

        // 7 backspaces: `/` → `` → popup closes
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(
            ui.command_popup.is_none(),
            "popup must close when buffer is empty"
        );
        assert_eq!(ui.ui_state.input_buffer, "");
    }

    /// Issue #52: after transitioning from Argument to SlashCommand mode,
    /// the popup's filtered items should include the `/model` command
    /// (since "model" is a valid slash command). This ensures the popup
    /// is useful immediately after the transition.
    #[test]
    fn backspace_from_argument_mode_shows_model_command() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        assert!(matches!(
            ui.command_popup.as_ref().unwrap().mode,
            PopupMode::Argument { .. }
        ));

        // Backspace once: transition to SlashCommand
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui.command_popup.as_ref().unwrap();
        assert!(matches!(popup.mode, PopupMode::SlashCommand));
        assert_eq!(popup.prefix, "model");

        // The "model" command should be visible after transition
        let filtered: Vec<&str> = popup
            .filtered_items()
            .into_iter()
            .map(|i| i.value.as_str())
            .collect();
        assert_eq!(
            filtered,
            vec!["model"],
            "transitioned popup should show the /model command"
        );
    }

    /// Issue #52 follow-up: after backspacing from `/model ` to `/model`
    /// (SlashCommand mode), typing space again should transition back to
    /// Argument mode. This is the "correct then space" path.
    #[test]
    fn space_after_backspace_from_argument_reopens_arg_popup() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        assert!(
            matches!(
                ui.command_popup.as_ref().unwrap().mode,
                PopupMode::Argument { .. }
            ),
            "precondition: popup is in Argument mode"
        );

        // Backspace: transition to SlashCommand
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(
            matches!(
                ui.command_popup.as_ref().unwrap().mode,
                PopupMode::SlashCommand
            ),
            "after backspace: popup should be in SlashCommand mode"
        );

        // Type space again: should transition back to Argument mode
        ui.handle_keyboard_input(press(KeyCode::Char(' ')));
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup must stay open after typing space");
        assert!(
            matches!(&popup.mode, PopupMode::Argument { command } if command == "model"),
            "after typing space: popup should be back in Argument mode, got {:?}",
            popup.mode
        );
        // Both aliases should be present
        let values: Vec<&str> = popup.all_items.iter().map(|i| i.value.as_str()).collect();
        assert_eq!(values, vec!["opus", "sonnet"]);
    }

    /// Issue #52 follow-up #2: typing `/mod`, then a space (which
    /// auto-closes the popup because the buffer leaves command-name
    /// territory), then backspacing the space MUST reopen the popup.
    /// The user did not explicitly dismiss — the popup was reactively
    /// closed by sync_popup, so removing the offending whitespace
    /// should restore the popup. Same applies to Argument mode.
    #[test]
    fn backspace_after_auto_close_in_slash_mode_reopens_popup() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/mod");
        assert!(
            ui.command_popup.is_some(),
            "precondition: popup is open after typing /mod"
        );

        // Type space: buffer becomes `/mod `, popup auto-closes.
        ui.handle_keyboard_input(press(KeyCode::Char(' ')));
        assert!(
            ui.command_popup.is_none(),
            "popup should close when whitespace enters command-name territory"
        );
        assert_eq!(ui.ui_state.input_buffer, "/mod ");

        // Backspace the space: buffer becomes `/mod` again — popup
        // must reappear (user didn't explicitly dismiss).
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup must reopen after backspacing the offending space");
        assert!(matches!(popup.mode, PopupMode::SlashCommand));
        assert_eq!(popup.prefix, "mod");
    }

    /// Same regression in Argument mode: typing `/model `, then a space
    /// (which auto-closes — `/model  ` is two spaces, whitespace inside
    /// the prefix is allowed at the start, but typing a real word and
    /// then more would close). The canonical user flow: type `/model
    /// sonn`, accidentally hit space, type a character, realise, then
    /// backspace through.
    #[test]
    fn backspace_after_auto_close_in_argument_mode_reopens_popup() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model sonn");
        assert!(
            matches!(
                ui.command_popup.as_ref().unwrap().mode,
                PopupMode::Argument { .. }
            ),
            "precondition: popup is in Argument mode"
        );

        // Type ` x` to force whitespace inside the prefix → auto-close.
        type_str(&mut ui, " x");
        assert!(
            ui.command_popup.is_none(),
            "popup should close when whitespace appears inside arg prefix"
        );

        // Backspace twice to remove ` x` and land back at `/model sonn`.
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert_eq!(ui.ui_state.input_buffer, "/model sonn");
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup must reopen once the offending whitespace is gone");
        assert!(
            matches!(&popup.mode, PopupMode::Argument { command } if command == "model"),
            "popup should be in Argument mode for `model`, got {:?}",
            popup.mode
        );
        assert_eq!(popup.prefix, "sonn");
    }

    /// Esc explicitly dismisses the popup. After Esc, mutating the buffer
    /// (e.g., backspacing) must NOT auto-reopen — user said no.
    /// Re-opening only happens after the buffer is fully cleared and the
    /// user types `/` again.
    #[test]
    fn esc_dismissal_persists_across_buffer_mutations() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/mod");
        assert!(ui.command_popup.is_some());

        // Esc: dismiss explicitly.
        ui.handle_keyboard_input(press(KeyCode::Esc));
        assert!(ui.command_popup.is_none(), "Esc closes the popup");

        // Backspace: buffer becomes `/mo` — popup must stay closed.
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(
            ui.command_popup.is_none(),
            "popup must NOT reopen after Esc dismissal"
        );

        // Backspace down to empty.
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert_eq!(ui.ui_state.input_buffer, "");
        assert!(ui.command_popup.is_none());

        // Now typing `/` again should reopen (explicit trigger).
        ui.handle_keyboard_input(press(KeyCode::Char('/')));
        assert!(
            ui.command_popup.is_some(),
            "popup reopens via explicit `/` trigger after buffer clears"
        );
    }

    /// Tab-accepting a non-arg-taking command (e.g. `/stats`) closes the
    /// popup. Backspacing afterwards must NOT auto-reopen the popup —
    /// this is the original procedural-rule case (sync_popup must not
    /// auto-open what accept_command just closed).
    #[test]
    fn tab_accept_dismissal_persists_across_backspace() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/stat");
        assert!(ui.command_popup.is_some());

        // Tab accepts `/stats`.
        ui.handle_keyboard_input(press(KeyCode::Tab));
        assert!(ui.command_popup.is_none(), "Tab closes the popup");
        assert_eq!(ui.ui_state.input_buffer, "/stats");

        // Backspace one char: `/stats` → `/stat`. Popup must stay closed.
        ui.handle_keyboard_input(press(KeyCode::Backspace));
        assert!(
            ui.command_popup.is_none(),
            "popup must NOT reopen after Tab-accept dismissal"
        );
    }

    /// Issue #52 follow-up: "add a space and then correct" — typing a second
    /// space in Argument mode should NOT close the popup. The user might
    /// accidentally type an extra space while correcting their input.
    #[test]
    fn double_space_in_argument_mode_should_not_close_popup() {
        let (mut ui, _rx) = harness_with_registry();
        type_str(&mut ui, "/model ");
        assert!(
            matches!(
                ui.command_popup.as_ref().unwrap().mode,
                PopupMode::Argument { .. }
            ),
            "precondition: popup is in Argument mode"
        );

        // Type a second space: `/model  ` — popup should stay open
        ui.handle_keyboard_input(press(KeyCode::Char(' ')));
        let popup = ui
            .command_popup
            .as_ref()
            .expect("popup must stay open after second space");
        assert!(
            matches!(&popup.mode, PopupMode::Argument { command } if command == "model"),
            "after second space: popup should still be in Argument mode, got {:?}",
            popup.mode
        );
    }
}
