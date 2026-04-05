# REPL UI Refactor: Symmetric Build/Render Functions

## TL;DR

Split REPL rendering into **build** functions (return data) and **render** functions (consume data). Current code has asymmetric patterns: `get_input_area` returns a Paragraph, while `render_chat_history` does everything inline.

## Current State

```
get_input_area(input, cursor_pos) → Paragraph  ✓ returns data
    └── render() calls f.render_widget(input, ...)

render_chat_history(f, area, scroll, chat)     ✗ does everything
    └── creates paragraph, creates scrollbar, renders both
```

No symmetry. Can't test build logic independently. Can't reuse paragraph building elsewhere.

## Proposed State

```
build_input_paragraph(input, cursor_pos) → Paragraph  ✓ build
    └── render_input_area(f, area, input, cursor_pos)  ✓ render

build_chat_history_paragraph(chat, scroll) → Paragraph  ✓ build
    └── render_chat_history(f, area, scroll, chat)      ✓ render (calls build + adds scrollbar)
```

Symmetric. Testable. Reusable.

## Changes

### 1. Rename `get_input_area` → `build_input_paragraph`

**File:** `src/ui/repl/repl_impl.rs`

```rust
// Line 134: rename function
pub fn build_input_paragraph<'a>(input: &str, cursor_pos: usize) -> Paragraph<'a> {
```

**File:** `src/ui/repl/repl_impl.rs`

Update call site (line 198):
```rust
let input = Self::build_input_paragraph(&self.input_buffer, self.cursor_pos);
```

### 2. Add `render_input_area`

**File:** `src/ui/repl/repl_impl.rs`

After `build_input_paragraph` (~line 157):

```rust
/// Render the input area (takes built paragraph and renders it)
pub fn render_input_area<'a>(f: &mut ratatui::Frame, area: Rect, paragraph: Paragraph<'a>) {
    f.render_widget(paragraph, area);
}
```

### 3. Add `build_chat_history_paragraph`

**File:** `src/ui/repl/repl_impl.rs`

Extract paragraph creation from `render_chat_history` into new function (~after line 131):

```rust
/// Build the chat history paragraph (returns paragraph, caller handles rendering)
pub fn build_chat_history_paragraph(chat: &ChatState, scroll: u16) -> Paragraph<'static> {
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

    Paragraph::new(Text::from(message_lines))
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .block(
            Block::default()
                .title(" Chat Messages ")
                .borders(Borders::ALL),
        )
}
```

### 4. Simplify `render_chat_history` to call build + render scrollbar

Replace the body of `render_chat_history` (lines 80-131) with:

```rust
pub fn render_chat_history(f: &mut ratatui::Frame, area: Rect, scroll: u16, chat: &ChatState) {
    let paragraph = Self::build_chat_history_paragraph(chat, scroll);
    let content_height = paragraph.line_count(area.width - 2) as u16;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(100), Constraint::Length(1)])
        .split(area);

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .style(Style::default().fg(Color::DarkGray));
    let mut scroll_state = ScrollbarState::new(content_height as usize).position(scroll as usize);
    f.render_stateful_widget(scrollbar, chunks[1], &mut scroll_state);

    f.render_widget(paragraph, chunks[0]);
}
```

### 5. Update call site in `render()`

Line 198 stays the same (uses `build_input_paragraph`), line 221 stays the same (`render_chat_history` unchanged signature).

Optional: use `render_input_area` in render():
```rust
let input = Self::build_input_paragraph(&self.input_buffer, self.cursor_pos);
Self::render_input_area(f, chunks[1], input);
```

## Verification

After changes:
- `cargo check` passes
- `cargo test` passes (if any)
- REPL renders correctly (manual test)

## What Was Left Out

- Not changing the scrollbar/scroll_state logic significantly
- Not adding new abstractions beyond what's requested
- Not modifying `render_status_bar` (already symmetric pattern)
