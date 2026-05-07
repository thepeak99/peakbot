//! Slash-command autocomplete popup renderer.
//!
//! Anchored **above** the input area. Pure view: takes a
//! [`CommandPopupState`] and an `input_area` rect, paints the popup into
//! the frame. All state is owned by the caller; this module never mutates.
//!
//! See `allehailmenu.md` §6 for the visual contract and §5 for when it's
//! shown.

use crate::ui::ui_trait::{CommandPopupState, CompletionItem, PopupMode};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Maximum number of command rows visible at once before the popup stops
/// growing vertically. Beyond this, `CommandPopupState::scroll_offset`
/// handles paging.
const MAX_VISIBLE_ROWS: u16 = 8;

/// Maximum popup width. Keeps the popup readable on ultra-wide terminals
/// and stops it from drowning the chat behind it.
const MAX_POPUP_WIDTH: u16 = 60;

/// Render the command popup anchored immediately above `input_area`.
///
/// If the popup would clip off the top of the screen (insufficient rows
/// above the input), it shrinks to fit. If it's given zero rows to work
/// with, it renders nothing.
///
/// # Layout
/// - Width: `min(input_area.width, MAX_POPUP_WIDTH)`.
/// - Height: `min(filtered.len(), MAX_VISIBLE_ROWS) + 2` (borders), clamped
///   to available space above the input.
/// - Horizontal position: left-aligned with `input_area.x`.
/// - Vertical: sits with its bottom edge at `input_area.y`.
pub fn render_command_popup(f: &mut Frame, input_area: Rect, popup: &CommandPopupState) {
    let filtered = popup.filtered_items();
    let row_count = filtered.len().max(1) as u16; // at least 1 for "no matches"
    let desired_content_rows = row_count.min(MAX_VISIBLE_ROWS);

    // Space available above the input for the popup (including borders).
    let available_above = input_area.y;
    if available_above < 3 {
        // No room for a bordered popup at all — skip gracefully.
        return;
    }

    let desired_height = desired_content_rows + 2;
    let height = desired_height.min(available_above);
    let width = input_area.width.min(MAX_POPUP_WIDTH);

    let x = input_area.x;
    let y = input_area.y.saturating_sub(height);
    let popup_area = Rect::new(x, y, width, height);

    // Clear whatever was underneath (chat transcript) first.
    f.render_widget(Clear, popup_area);

    // Build content lines.
    let content_rows = height.saturating_sub(2); // subtract borders
    let visible_from = popup.scroll_offset.min(filtered.len().saturating_sub(1));
    let visible_to = (visible_from + content_rows as usize).min(filtered.len());

    let no_match_text = match &popup.mode {
        PopupMode::SlashCommand => "  (no matching commands)",
        PopupMode::Argument { .. } => "  (no matching values)",
    };

    let content: Vec<Line<'static>> = if filtered.is_empty() {
        vec![Line::from(Span::styled(
            no_match_text,
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        filtered[visible_from..visible_to]
            .iter()
            .enumerate()
            .map(|(offset, item)| {
                let absolute_idx = visible_from + offset;
                let is_selected = absolute_idx == popup.selected_index;
                render_item_row(item, is_selected, width, &popup.mode)
            })
            .collect()
    };

    // Title flips by mode so the user knows what they're picking. Keep
    // the bottom hint identical — same keybindings across modes.
    let title = match &popup.mode {
        PopupMode::SlashCommand => " Commands ".to_string(),
        PopupMode::Argument { command } => format!(" {} ", title_for_argument(command)),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::LightCyan))
        .title(title)
        .title_bottom(Line::from(" ↑/↓ · Tab complete · Enter run · Esc cancel ").right_aligned());

    let paragraph = Paragraph::new(content)
        .style(Style::default().fg(Color::White))
        .block(block);

    f.render_widget(paragraph, popup_area);
}

/// Map an `Argument { command }` mode to a human title for the popup.
/// Centralised here so adding a new arg-completing command later is a
/// one-arm match-extension, not a renderer rewrite.
fn title_for_argument(command: &str) -> &'static str {
    match command {
        "model" => "Models",
        _ => "Values",
    }
}

/// Render one popup row.
///
/// **SlashCommand mode**: `  /<name>[ <args>]   <description>`
/// **Argument mode**:     `  <value>           <description>` (with the
/// active selection prefixed by `→ ` in the description column when
/// `is_current` is set, e.g. the currently-active model alias).
///
/// The selected row fills the interior width with a highlight background
/// so it reads as a solid bar rather than a short strip. `popup_width`
/// includes the block borders; interior is `popup_width - 2`.
fn render_item_row(
    item: &CompletionItem,
    is_selected: bool,
    popup_width: u16,
    mode: &PopupMode,
) -> Line<'static> {
    let interior_width = popup_width.saturating_sub(2) as usize;

    // Left column: command name (with optional <args> hint) or bare value.
    let left = match mode {
        PopupMode::SlashCommand => {
            let args_hint = if item.takes_args { " <args>" } else { "" };
            format!("  /{}{}", item.value, args_hint)
        }
        PopupMode::Argument { .. } => format!("  {}", item.value),
    };

    // Right column: description, prefixed with → for the active item in
    // Argument mode (e.g. the currently-active model alias).
    let desc_prefix = if item.is_current { "→ " } else { "" };
    let desc = format!("{}{}", desc_prefix, item.description);

    // Column-layout: command on the left, description after a gap, padded
    // to fill the interior so the selection bar extends fully.
    let gap = 2;
    let left_len = left.chars().count();

    let available_for_desc = interior_width.saturating_sub(left_len + gap);
    let desc_text = if available_for_desc == 0 {
        String::new()
    } else if desc.chars().count() <= available_for_desc {
        format!("{:<w$}", desc, w = available_for_desc)
    } else {
        // Truncate with ellipsis.
        let take = available_for_desc.saturating_sub(1);
        let trunc: String = desc.chars().take(take).collect();
        format!("{}…", trunc)
    };

    let (name_style, desc_style) = if is_selected {
        (
            Style::default().fg(Color::Yellow).bg(Color::DarkGray),
            Style::default().fg(Color::White).bg(Color::DarkGray),
        )
    } else {
        (
            Style::default().fg(Color::LightCyan),
            Style::default().fg(Color::Gray),
        )
    };

    let gap_style = if is_selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    Line::from(vec![
        Span::styled(left, name_style),
        Span::styled(" ".repeat(gap), gap_style),
        Span::styled(desc_text, desc_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// Smoke test: popup renders without panicking on the tiniest usable
    /// frame. Regression guard for layout math (saturating arithmetic).
    #[test]
    fn popup_renders_on_minimum_terminal() {
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let popup = CommandPopupState::new(String::new());
        let _ = terminal.draw(|f| {
            let input_area = Rect::new(0, 7, 20, 3);
            render_command_popup(f, input_area, &popup);
        });
    }

    /// If there's no room above the input (input at y=0), the renderer
    /// bails out gracefully instead of overflowing into the input area.
    #[test]
    fn popup_skips_when_no_room_above_input() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let popup = CommandPopupState::new(String::new());
        let _ = terminal.draw(|f| {
            let input_area = Rect::new(0, 0, 80, 3);
            render_command_popup(f, input_area, &popup);
        });
    }

    #[test]
    fn popup_handles_empty_filter_with_placeholder_row() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let popup = CommandPopupState::new("zzz".to_string());
        let _ = terminal.draw(|f| {
            let input_area = Rect::new(0, 20, 80, 3);
            render_command_popup(f, input_area, &popup);
        });
    }
}
