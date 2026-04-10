//! Todo Panel Widget
//!
//! Renders the todo list as a side panel in the REPL TUI.
//! Display-only v1 — interactions happen via the todo tool in chat.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

use crate::ui::app_state::{TodoItem, TodoState};

/// Minimum width for the todo panel to be useful
pub const MIN_PANEL_WIDTH: u16 = 20;

/// Default panel width as percentage of terminal
pub const DEFAULT_PANEL_PERCENT: u16 = 30;

/// Minimum terminal width for panel to show
pub const MIN_TERMINAL_WIDTH: u16 = 60;

/// Render the todo panel widget
pub fn render_todo_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    state: &TodoState,
    scroll_position: u16,
) {
    if area.width < MIN_PANEL_WIDTH || area.height < 3 {
        return;
    }

    let items = &state.items;

    // Build content lines
    let content_lines = if items.is_empty() {
        vec![Line::from(vec![Span::styled(
            "No tasks",
            Style::default().fg(Color::DarkGray).italic(),
        )])]
    } else {
        items.iter().map(render_todo_item).collect::<Vec<_>>()
    };

    let content_height = content_lines.len();
    let viewport_height = area.height.saturating_sub(2) as usize; // minus block borders
    let max_scroll = content_height.saturating_sub(viewport_height);
    let scroll = scroll_position.min(max_scroll as u16) as usize;

    // Build paragraph with scrolling
    let paragraph = ratatui::widgets::Paragraph::new(Text::from(content_lines))
        .wrap(Wrap { trim: true })
        .scroll((scroll as u16, 0));

    // Render with block
    let block = Block::default()
        .title(" ✓ TODO ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(paragraph.block(block), area);

    // Render scrollbar if needed
    if content_height > viewport_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let scroll_state = ScrollbarState::new(content_height).position(scroll);

        // Render scrollbar on the right edge
        let scroll_area = Rect::new(area.right() - 1, area.top(), 1, area.height);
        f.render_stateful_widget(scrollbar, scroll_area, &mut scroll_state.clone());
    }
}

/// Render a single todo item as a styled line
fn render_todo_item(item: &TodoItem) -> Line<'static> {
    let (icon, color, strike) = match item.status {
        crate::TodoStatus::Pending => ("○", Color::DarkGray, false),
        crate::TodoStatus::InProgress => ("◐", Color::Yellow, false),
        crate::TodoStatus::Completed => ("●", Color::Green, true),
        crate::TodoStatus::Cancelled => ("✗", Color::Red, false),
    };

    let text_style = if strike {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(ratatui::style::Modifier::CROSSED_OUT)
    } else {
        Style::default().fg(Color::White)
    };

    let id_style = Style::default().fg(Color::DarkGray);
    let icon_style = Style::default().fg(color);

    // Truncate text if too long (panel is narrow)
    let max_text_len = 30;
    let text = truncate_text(&item.text, max_text_len);

    Line::from(vec![
        Span::styled(icon, icon_style),
        Span::raw(" "),
        Span::styled(format!("#{}", item.id), id_style),
        Span::raw(" "),
        Span::styled(text, text_style),
    ])
}

/// Truncate text to max_len, adding "..." if truncated
fn truncate_text(s: &str, max_len: usize) -> String {
    let width = s.chars().count();
    if width <= max_len {
        s.to_string()
    } else if max_len < 3 {
        "...".to_string()
    } else {
        let end = max_len - 3;
        s.chars().take(end).collect::<String>() + "..."
    }
}

/// Calculate how many lines the todo panel needs to display all items
#[allow(dead_code)]
pub fn calculate_content_height(state: &TodoState, _width: u16) -> usize {
    state.items.len()
}

/// Check if the todo panel should be shown based on terminal size
pub fn should_show_panel(terminal_width: u16) -> bool {
    terminal_width >= MIN_TERMINAL_WIDTH
}
