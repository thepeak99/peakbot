//! Foreground bash tool panel — bottom-strip widget.
//!
//! Renders the snapshot in [`crate::ui::app_state::BashPanelState`] as a
//! single bordered strip with:
//!
//! - **Header** (line 1, inside the top border via the block title):
//!   - `Running`  → `⏵ <cmd> · pid <n> · <elapsed>`
//!   - `Finished` → `✓ <cmd> · exit 0 · ran <dur>` (success) or
//!     `✗ <cmd> · exit <n> · ran <dur>` (failure)
//! - **Tail** (5 fixed rows): the last ≤ 5 lines of output, padded with
//!   blank rows so the strip's vertical footprint is stable across
//!   states. New lines arrive at the bottom; oldest fall off the top
//!   (`tail -f`-style).
//! - **stdin row** (only when `Running`): `stdin» _` — a placeholder
//!   for the slice 4 input field.
//!
//! Total vertical footprint:
//! - `Idle`     → 0 rows (panel hidden by the layout — never reaches
//!   this module).
//! - `Running`  → 8 rows: top border + 5 tail + stdin + bottom border.
//! - `Finished` → 7 rows: top border + 5 tail + bottom border.
//!
//! See `make-term-great-again.md` "Panel layout" for the locked design.
//! Slice 2 ships the renderer; slice 3 wires the foreground `bash` tool
//! to feed it, slice 4 wires real key forwarding into the stdin row.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::ui::app_state::BashPanelState;

/// Fixed number of output rows shown by the panel. Locked at 5 by the
/// design — *not* a config option. If a future need for scroll appears
/// it's a new feature, not a tweak to this constant.
pub const TAIL_ROWS: u16 = 5;

/// Total height of the panel for [`BashPanelState::Running`]:
/// top border + 5 tail rows + stdin row + bottom border.
///
/// The `Block::default().borders(Borders::ALL)` block contributes one
/// row each for the top and bottom borders — the title is rendered
/// *inside* the top border, not as its own row.
pub const RUNNING_HEIGHT: u16 = 1 + TAIL_ROWS + 1 + 1;

/// Total height of the panel for [`BashPanelState::Finished`]:
/// top border + 5 tail rows + bottom border. No stdin row — nothing is
/// reading.
pub const FINISHED_HEIGHT: u16 = 1 + TAIL_ROWS + 1;

/// Vertical footprint required to render the given panel state.
/// `Idle` is the only zero-height variant; the caller's layout uses
/// this to decide whether to allocate a strip at all.
pub fn panel_height(state: &BashPanelState) -> u16 {
    match state {
        BashPanelState::Idle => 0,
        BashPanelState::Running { .. } => RUNNING_HEIGHT,
        BashPanelState::Finished { .. } => FINISHED_HEIGHT,
    }
}

/// Render the bash panel into `area`. No-op for [`BashPanelState::Idle`]
/// — the layout shouldn't have given us a non-zero area in that case,
/// but the guard keeps the renderer safe under surprise.
///
/// `stdin_buffer` and `stdin_focused` shape the stdin row when the
/// panel is `Running` (no effect on `Finished` — nothing is reading).
/// When `stdin_focused` is true, the buffer is rendered bright with a
/// trailing `█` block-cursor; when false, the buffer is dim italic
/// with a `[Ctrl+S]` hint right-aligned so the user knows how to
/// claim focus.
pub fn render_bash_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    state: &BashPanelState,
    stdin_buffer: &str,
    stdin_focused: bool,
) {
    if matches!(state, BashPanelState::Idle) || area.height == 0 {
        return;
    }

    let (title, border_color, tail, show_stdin) = match state {
        BashPanelState::Idle => unreachable!("guarded above"),
        BashPanelState::Running {
            command,
            pid,
            started_at,
            tail,
        } => {
            let elapsed = (chrono::Local::now() - *started_at).num_seconds().max(0) as u64;
            let title = format!(
                " > {} · pid {} · {} ",
                short_command(command),
                pid,
                format_duration(elapsed),
            );
            (title, Color::Yellow, tail.as_slice(), true)
        }
        BashPanelState::Finished {
            command,
            exit_code,
            duration_secs,
            tail,
        } => {
            // Glyphs kept ASCII for terminal robustness (see todo_panel
            // for the kitty-emoji-presentation rationale): `+` for
            // success, `x` for failure.
            let (glyph, color) = if *exit_code == 0 {
                ("+", Color::Green)
            } else {
                ("x", Color::Red)
            };
            let title = format!(
                " {} {} · exit {} · ran {} ",
                glyph,
                short_command(command),
                exit_code,
                format_duration(*duration_secs),
            );
            (title, color, tail.as_slice(), false)
        }
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let content_lines = build_content_lines(tail, show_stdin, stdin_buffer, stdin_focused);
    let paragraph = Paragraph::new(Text::from(content_lines)).block(block);
    f.render_widget(paragraph, area);
}

/// Build the body lines (everything inside the bordered block):
///
/// - The last `TAIL_ROWS` lines of `tail`, padded at the top with
///   blanks if fewer (so new output appears at the *bottom*, like
///   `tail -f`).
/// - When `show_stdin` is true, a stdin row underneath. Its shape
///   depends on `stdin_focused`: unfocused renders `stdin> <buffer>`
///   dim italic with a right-aligned `[Ctrl+S]` hint; focused renders
///   `stdin> <buffer>█` bright with a block cursor and no hint
///   (cursor presence IS the focus signal).
///
/// Long lines are *not* truncated here — ratatui's `Paragraph` will
/// clip at the right edge. Wrapping is intentionally disabled in v1
/// to keep the 5-row tail height stable; a future iteration can move
/// to `Wrap { trim: false }` if we accept a variable footprint.
fn build_content_lines(
    tail: &[String],
    show_stdin: bool,
    stdin_buffer: &str,
    stdin_focused: bool,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(TAIL_ROWS as usize + 1);

    let take = TAIL_ROWS as usize;
    let visible: Vec<&str> = tail.iter().rev().take(take).map(|s| s.as_str()).collect();
    let pad = take.saturating_sub(visible.len());
    for _ in 0..pad {
        lines.push(Line::from(""));
    }
    for s in visible.into_iter().rev() {
        lines.push(Line::from(Span::raw(s.to_string())));
    }

    if show_stdin {
        lines.push(build_stdin_row(stdin_buffer, stdin_focused));
    }

    lines
}

/// Render the single stdin row. Extracted so unit tests can exercise
/// it in isolation — the visuals are the slice's most user-visible
/// surface and the snapshot pins lock the variants.
fn build_stdin_row(stdin_buffer: &str, stdin_focused: bool) -> Line<'static> {
    if stdin_focused {
        // Bright label + buffer + block cursor. No hint — the cursor
        // makes "you are typing here" unambiguous.
        Line::from(vec![
            Span::styled(
                "stdin> ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(stdin_buffer.to_string(), Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
        ])
    } else {
        // Dim italic label + buffer (or empty), trailing hint.
        // We don't right-align the hint by computing column widths
        // (that needs the area width which lives in the renderer);
        // a `· [Ctrl+S]` middle-dot separator keeps the row legible
        // without layout math.
        Line::from(vec![
            Span::styled(
                "stdin> ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                stdin_buffer.to_string(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(" · [Ctrl+S]", Style::default().fg(Color::DarkGray)),
        ])
    }
}

/// Format a command string for the header — keeps the first ~60 chars
/// and adds an ellipsis if longer. The full command lives in the
/// transcript; the header is signage, not documentation.
fn short_command(cmd: &str) -> String {
    const MAX: usize = 60;
    let cleaned = cmd.replace('\n', " ");
    if cleaned.chars().count() <= MAX {
        cleaned
    } else {
        cleaned.chars().take(MAX - 1).collect::<String>() + "…"
    }
}

/// Format a whole-second duration as `MM:SS` for the panel header.
/// Hours roll into minutes (`75:42` for 1h15m42s) — the panel is for
/// in-progress bash calls, not for displaying multi-hour daemons (use
/// `bash_bg` for those).
fn format_duration(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{:02}:{:02}", m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    #[test]
    fn idle_has_zero_height() {
        assert_eq!(panel_height(&BashPanelState::Idle), 0);
    }

    #[test]
    fn running_height_matches_constant() {
        let state = BashPanelState::Running {
            command: "ls".into(),
            pid: 1,
            started_at: Local::now(),
            tail: Vec::new(),
        };
        assert_eq!(panel_height(&state), RUNNING_HEIGHT);
        assert_eq!(RUNNING_HEIGHT, 8);
    }

    #[test]
    fn finished_height_matches_constant() {
        let state = BashPanelState::Finished {
            command: "ls".into(),
            exit_code: 0,
            duration_secs: 5,
            tail: Vec::new(),
        };
        assert_eq!(panel_height(&state), FINISHED_HEIGHT);
        assert_eq!(FINISHED_HEIGHT, 7);
    }

    #[test]
    fn format_duration_pads() {
        assert_eq!(format_duration(0), "00:00");
        assert_eq!(format_duration(7), "00:07");
        assert_eq!(format_duration(65), "01:05");
        assert_eq!(format_duration(3725), "62:05"); // hours roll into minutes
    }

    #[test]
    fn short_command_keeps_short() {
        assert_eq!(short_command("ls -la"), "ls -la");
    }

    #[test]
    fn short_command_truncates_long() {
        let long = "echo ".to_string() + &"x".repeat(200);
        let s = short_command(&long);
        assert_eq!(s.chars().count(), 60);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn short_command_flattens_newlines() {
        assert_eq!(short_command("echo hi\nls"), "echo hi ls");
    }

    #[test]
    fn build_content_lines_pads_top_for_short_tail() {
        let tail = vec!["one".to_string(), "two".to_string()];
        let lines = build_content_lines(&tail, false, "", false);
        // 5 tail rows, no stdin
        assert_eq!(lines.len(), 5);
        // First 3 are blank padding; last 2 are content
        assert!(lines[0].spans.iter().all(|s| s.content.is_empty()));
        assert!(lines[2].spans.iter().all(|s| s.content.is_empty()));
    }

    #[test]
    fn build_content_lines_takes_last_5_when_overflow() {
        let tail: Vec<String> = (0..10).map(|i| format!("line {}", i)).collect();
        let lines = build_content_lines(&tail, false, "", false);
        assert_eq!(lines.len(), 5);
        // Should be lines 5..=9 (the last 5)
        let first_text = lines[0].spans[0].content.to_string();
        assert_eq!(first_text, "line 5");
        let last_text = lines[4].spans[0].content.to_string();
        assert_eq!(last_text, "line 9");
    }

    #[test]
    fn build_content_lines_appends_stdin_row_when_requested() {
        let lines = build_content_lines(&[], true, "", false);
        assert_eq!(lines.len(), 6);
        // Last line should be the stdin prompt
        let txt: String = lines[5].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.starts_with("stdin>"));
    }

    // ── Slice 4: stdin row variants ──────────────────────────────────────

    #[test]
    fn stdin_row_unfocused_shows_label_and_hint() {
        let line = build_stdin_row("", false);
        let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.starts_with("stdin>"));
        assert!(txt.contains("[Ctrl+S]"));
        // No block cursor when unfocused.
        assert!(!txt.contains('█'));
    }

    #[test]
    fn stdin_row_unfocused_with_buffer_shows_buffer() {
        let line = build_stdin_row("password", false);
        let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.contains("password"));
        assert!(txt.contains("[Ctrl+S]"));
    }

    #[test]
    fn stdin_row_focused_shows_cursor_no_hint() {
        let line = build_stdin_row("hello", true);
        let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.starts_with("stdin>"));
        assert!(txt.contains("hello"));
        // Block cursor present, hint absent — cursor IS the focus signal.
        assert!(txt.contains('█'));
        assert!(!txt.contains("[Ctrl+S]"));
    }

    #[test]
    fn stdin_row_focused_empty_buffer_still_has_cursor() {
        let line = build_stdin_row("", true);
        let txt: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(txt.contains('█'));
        assert!(!txt.contains("[Ctrl+S]"));
    }
}
