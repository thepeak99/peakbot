//! TUI Rendering Module
//!
//! This module provides the rendering functions for the TUI implementation.
//! It follows a React-like pattern where each component takes only the state slice it needs.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::ui::app_state::{AppState, ChatState, ContextState, InputState, MessageRole, SessionState, TodoState};

/// Maximum number of TODO items to display in the panel
const MAX_TODO_ITEMS_DISPLAY: usize = 7;

/// Maximum number of commands to show in popup
const MAX_POPUP_ITEMS: usize = 8;

/// Calculate the TODO panel height based on number of items
fn calculate_todo_panel_height(todo: &TodoState) -> usize {
    if !todo.visible {
        return 0;
    }
    let item_count = todo.items.len().min(MAX_TODO_ITEMS_DISPLAY);
    // Panel height = items + top border + bottom border
    item_count + 2
}

/// Render any active popup
pub fn render_popup(f: &mut Frame, app: &AppState) {
    // Render command popup if active
    if app.command_popup.is_some() {
        render_command_popup(f, app);
    }
}

/// Render the UI to a terminal
///
/// This is the "root component" that composes all sub-components together.
/// It passes the appropriate state slice to each child component.
pub fn ui(f: &mut Frame, app: &AppState) {
    let size = f.area();

    // Calculate dynamic TODO panel height
    let todo_height = calculate_todo_panel_height(&app.todo);
    let todo_constraint = if app.todo.visible {
        Constraint::Length(todo_height as u16)
    } else {
        Constraint::Length(0)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Title bar
            Constraint::Fill(1),   // Chat area
            todo_constraint,       // TODO panel (dynamic height)
            Constraint::Length(3), // Input area
            Constraint::Length(1), // Status bar
        ])
        .split(size);

    // Render each panel - passing only the state slice each component needs
    render_title_bar(f, chunks[0]);
    render_chat_area(f, chunks[1], &app.chat);

    if app.todo.visible {
        render_todo_panel(f, chunks[2], &app.todo);
        render_input_area(f, chunks[3], &app.input);
        render_status_bar(f, chunks[4], &app.stats, &app.context);

        // Render popup if active - positioned above input area
        render_popup(f, app);
    } else {
        render_input_area(f, chunks[3], &app.input);
        render_status_bar(f, chunks[4], &app.stats, &app.context);

        // Render popup if active - positioned above input area
        render_popup(f, app);
    }
}

/// Render the title bar
pub fn render_title_bar(f: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1), // Left corner
            Constraint::Fill(1),   // Title
            Constraint::Length(4), // Help button [?]
            Constraint::Length(3), // Close button [x]
            Constraint::Length(1), // Right corner
        ])
        .split(area);

    let title = Paragraph::new("🤖 PeakBot TUI")
        .style(Style::default().fg(Color::LightCyan))
        .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    f.render_widget(title, chunks[1]);

    let help = Paragraph::new("[?]")
        .style(Style::default().fg(Color::LightBlue))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(help, chunks[2]);

    let close = Paragraph::new("[x]")
        .style(Style::default().fg(Color::LightRed))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(close, chunks[3]);
}

/// Render the chat message area
///
/// Takes only ChatState - doesn't need or want access to todo or input state
/// Supports auto-scrolling when there are many messages
pub fn render_chat_area(f: &mut Frame, area: Rect, chat: &ChatState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1), // Left border
            Constraint::Fill(1),  // Chat content
            Constraint::Length(1), // Right border
        ])
        .split(area);

    let left_border = Paragraph::new("│")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(left_border, chunks[0]);

    let chat_area = chunks[1];

    // Build message text with proper word wrapping
    let mut message_lines: Vec<Line> = Vec::new();

    if chat.messages.is_empty() {
        message_lines.push(Line::from("Welcome to PeakBot! Start a conversation or use /help for commands."));
    } else {
        for msg in &chat.messages {
            let (prefix, _color) = match msg.role {
                MessageRole::User => ("👤 User", Color::LightGreen),
                MessageRole::Agent => ("🤖 Agent", Color::LightMagenta),
                MessageRole::System => ("⚙️ System", Color::LightYellow),
                MessageRole::ToolCall => ("🔧 Tool", Color::Cyan),
                MessageRole::ToolResult => ("📋 Result", Color::Cyan),
            };
            
            let timestamp = msg.timestamp.format("%H:%M:%S").to_string();
            
            // Add header line
            message_lines.push(Line::from(format!("[{}] {}:", timestamp, prefix)));
            
            // Add content with word wrapping (respecting terminal width)
            // We'll wrap at the available width minus some padding
            let wrap_width = (chat_area.width.saturating_sub(4)) as usize;
            let content = wrap_text(&msg.content, wrap_width);
            message_lines.extend(content);
            
            // Add empty line between messages
            message_lines.push(Line::from(""));
        }
    }

    // Count actual rendered lines (after wrapping)
    let content_lines = message_lines.len();
    let view_height = chat_area.height.saturating_sub(2) as usize; // Account for borders

    // Calculate scroll offset
    let scroll_offset = if chat.auto_scroll && content_lines > view_height {
        // Auto-scroll to bottom
        content_lines.saturating_sub(view_height)
    } else {
        // Clamp manual scroll offset
        chat.scroll_offset.min(content_lines.saturating_sub(view_height).saturating_sub(1))
    };

    // Get visible lines
    let visible_lines: Vec<Line> = message_lines
        .into_iter()
        .skip(scroll_offset)
        .take(view_height)
        .collect();

    let paragraph = Paragraph::new(Text::from(visible_lines))
        .style(Style::default().fg(Color::LightCyan))
        .block(
            Block::default()
                .title(" Chat Messages ")
                .borders(Borders::ALL),
        );
    f.render_widget(paragraph, chat_area);

    // Render scrollbar if content exceeds view
    if content_lines > view_height {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::DarkGray));
        let mut scroll_state = ScrollbarState::new(content_lines)
            .position(scroll_offset);
        f.render_stateful_widget(scrollbar, chat_area, &mut scroll_state);
    }

    let right_border = Paragraph::new("│")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(right_border, chunks[2]);
}

/// Wrap text to fit within a maximum width, breaking on word boundaries
fn wrap_text(text: &str, max_width: usize) -> Vec<Line> {
    if max_width == 0 {
        return vec![Line::from(text)];
    }

    let mut lines = Vec::new();
    
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        let mut current_line = String::new();
        
        for word in paragraph.split_whitespace() {
            let word_len = word.chars().count();
            
            if current_line.is_empty() {
                if word_len > max_width {
                    // Word is longer than max width, need to break it
                    let mut chars_remaining = word_len;
                    let mut chars_on_line = 0;
                    for c in word.chars() {
                        if chars_on_line >= max_width {
                            lines.push(Line::from(current_line.clone()));
                            current_line.clear();
                            chars_on_line = 0;
                        }
                        current_line.push(c);
                        chars_on_line += 1;
                        chars_remaining -= 1;
                    }
                } else {
                    current_line.push_str(word);
                }
            } else if current_line.chars().count() + 1 + word_len <= max_width {
                // Word fits on current line
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                // Word doesn't fit, start new line
                lines.push(Line::from(current_line.clone()));
                if word_len > max_width {
                    // Word is longer than max width, need to break it
                    let mut chars_on_line = 0;
                    for c in word.chars() {
                        if chars_on_line >= max_width {
                            lines.push(Line::from(current_line.clone()));
                            current_line.clear();
                            chars_on_line = 0;
                        }
                        current_line.push(c);
                        chars_on_line += 1;
                    }
                } else {
                    current_line.clear();
                    current_line.push_str(word);
                }
            }
        }
        
        if !current_line.is_empty() {
            lines.push(Line::from(current_line));
        }
    }
    
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    
    lines
}

/// Render the TODO panel
///
/// Takes only TodoState - doesn't need or want access to chat or input state
pub fn render_todo_panel(f: &mut Frame, area: Rect, todo: &TodoState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(12), // "TODO (3)" title
            Constraint::Fill(1),    // Task list
            Constraint::Length(25), // Status column
        ])
        .split(area);

    let todo_title = Paragraph::new(format!("TODO ({})", todo.items.len()))
        .style(Style::default().fg(Color::LightYellow))
        .block(Block::default().borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM));
    f.render_widget(todo_title, chunks[0]);

    // Build todo text from state
    let todo_text: String = todo
        .items
        .iter()
        .map(|item| {
            let checkbox = if item.completed { "■" } else { "☐" };
            let marker = if item.selected { "→ " } else { "  " };
            format!("{}{} {}", marker, checkbox, item.text)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let todo_content = Paragraph::new(if todo_text.is_empty() {
        "No tasks yet. Use /todo add to create one."
    } else {
        &todo_text
    })
    .style(Style::default().fg(Color::White))
    .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
    f.render_widget(todo_content, chunks[1]);

    // Build status text from state
    let status_text: String = todo
        .items
        .iter()
        .map(|item| {
            let (indicator, label, color) = match item.status {
                crate::tools::todo::TodoStatus::InProgress => ("◉", "In Progress", Color::Yellow),
                crate::tools::todo::TodoStatus::Pending => ("○", "Pending", Color::DarkGray),
                crate::tools::todo::TodoStatus::Completed => ("✓", "Completed", Color::Green),
                crate::tools::todo::TodoStatus::Cancelled => ("✗", "Cancelled", Color::Red),
            };
            format!("{} {} [{}]", indicator, label, color.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");

    let status = Paragraph::new(if status_text.is_empty() {
        "No status"
    } else {
        &status_text
    })
    .style(Style::default().fg(Color::LightYellow))
    .block(Block::default().borders(Borders::RIGHT | Borders::TOP | Borders::BOTTOM));
    f.render_widget(status, chunks[2]);
}

/// Render the input area
///
/// Takes only InputState - doesn't need or want access to todo or chat state
pub fn render_input_area(f: &mut Frame, area: Rect, input: &InputState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),    // Input field
            Constraint::Length(30), // Send button
        ])
        .split(area);

    let input_text = if input.buffer.is_empty() {
        "💬 Type your message... (use / for commands)"
    } else {
        &input.buffer
    };

    let input_style = if input.buffer.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else if input.in_command_mode {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };

    let input_title = if input.in_command_mode {
        " Command "
    } else {
        " Message "
    };

    // Calculate wrap width for input (accounting for block borders and padding)
    let wrap_width = (chunks[0].width.saturating_sub(2)) as usize;
    
    // Wrap input text to fit in the available width
    let wrapped_lines = wrap_text(input_text, wrap_width);
    
    // Take only the first few lines that fit in the input area
    let max_input_lines = area.height.saturating_sub(2) as usize;
    let visible_lines: Vec<Line> = wrapped_lines
        .into_iter()
        .take(max_input_lines)
        .collect();
    
    let input_paragraph = if visible_lines.is_empty() {
        Paragraph::new("")
    } else {
        Paragraph::new(Text::from(visible_lines))
    }
        .style(input_style)
        .block(Block::default().title(input_title).borders(Borders::ALL));
    f.render_widget(input_paragraph, chunks[0]);

    let send_button = Paragraph::new("↵ Send")
        .style(Style::default().fg(Color::LightGreen))
        .block(Block::default().title(" Actions ").borders(Borders::ALL));
    f.render_widget(send_button, chunks[1]);
}

/// Render the status bar
pub fn render_status_bar(f: &mut Frame, area: Rect, stats: &SessionState, context: &ContextState) {
    let total_tokens = stats.total_tokens();
    let tokens_str = stats.format_tokens(total_tokens);
    let cost_str = stats.format_cost();
    let context_pct = context.usage_percentage();
    
    let status_text = format!(
        "Tokens: {} │ Calls: {} │ Cost: ${} │ Model: {} │ Context: {:.1}%",
        tokens_str,
        stats.total_api_calls,
        cost_str,
        stats.model,
        context_pct
    );

    let paragraph = Paragraph::new(status_text)
        .style(Style::default().fg(Color::LightCyan))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(paragraph, area);
}

/// Render the slash command popup
///
/// This shows a list of matching commands when the user types '/'
/// The popup is positioned in the chat area, at the bottom, above the input area
pub fn render_command_popup(f: &mut Frame, app: &AppState) {
    // Get the command popup state
    let popup = match &app.command_popup {
        Some(p) => p,
        None => return,
    };

    // Get the filtered commands - filtering is done in the popup state
    let commands = popup.filtered_commands();
    if commands.is_empty() {
        return;
    }

    let area = f.area();
    let popup_y = area.height.saturating_sub(10);
    let visible_height = (popup_y - 3).min(8) as usize;
    let popup_width = 50;

    let popup_area = Rect::new(
        2,
        popup_y,
        popup_width.min(area.width.saturating_sub(4)),
        visible_height as u16 + 2, // +2 for border
    );

    // Get the selected and scroll offset from popup state
    let selected_index = popup.selected_index;
    let scroll_offset = popup.scroll_offset;

    // Get visible slice of commands
    let visible_slice: Vec<_> = commands
        .iter()
        .skip(scroll_offset)
        .take(visible_height)
        .collect();

    // Build list items with selection highlighting
    let mut items: Vec<ListItem> = visible_slice
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let actual_index = scroll_offset + i;
            let style = if actual_index == selected_index {
                Style::default().fg(Color::White).bg(Color::Blue)
            } else {
                Style::default().fg(Color::White).bg(Color::Black)
            };
            
            let takes_arg_str = if cmd.takes_args { " <args>" } else { "" };
            let cmd_str = format!("/{}{} - {}", cmd.name, takes_arg_str, cmd.description);
            ListItem::new(cmd_str).style(style)
        })
        .collect();

    // Add help text at the bottom
    let help_text = "↑↓ Navigate │ Tab Select │ Enter Confirm │ Esc Cancel";
    let help_item = ListItem::new(help_text)
        .style(Style::default().fg(Color::DarkGray).bg(Color::Black));
    items.push(help_item);

    let list = List::new(items).block(
        Block::default()
            .title(" Commands ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::LightBlue))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(Clear, popup_area);
    f.render_widget(list, popup_area);
}
