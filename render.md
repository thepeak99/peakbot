# REPL Render Method Implementation Plan

## Overview

Replace the empty `render()` method in `src/ui/repl/repl_impl.rs` with a ratatui-based implementation featuring:
- **Top section**: Scrollable chat history (Paragraph widget)
- **Bottom section**: Single-line input area that grows horizontally (wraps to next line) as needed

## Layout Design

```
┌─────────────────────────────────────────────┐
│                                             │
│              Chat History                   │
│           (Scrollable Paragraph)            │
│                                             │
│                                             │
│                                             │
├─────────────────────────────────────────────┤
│ > user input here...                        │  ← Input area (1+ rows)
└─────────────────────────────────────────────┘
```

### Input Area Behavior
- Starts at 1 row height
- Grows by 1 row when input exceeds terminal width
- Maximum growth: capped to prevent taking over the screen
- Cursor position tracked for editing

## Implementation Steps

### 1. Add ratatui imports to `repl_impl.rs`

```rust
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    init,
    restore,
};
use crossterm::event::{self, Event as TuiEvent};
```

### 2. Use ratatui's init/restore helpers

ratatui provides `init()` and `restore()` which handle raw mode, alternate screen, and cleanup automatically:

```rust
use ratatui::Terminal;
use crossterm::event::{self, Event as TuiEvent};

// init() returns Terminal<impl Backend> - we need to own this
let mut terminal = init();

// On shutdown:
restore();
```

`init()` handles:
- `enable_raw_mode()`
- `EnterAlternateScreen`
- `EnableBracketedPaste`
- `DisableMouseCapture`

`restore()` handles the reverse cleanup.

See: https://docs.rs/ratatui/latest/ratatui/fn.init.html

### 3. Calculate input area height

Helper function to determine how many rows the input needs:

```rust
fn calculate_input_height(input: &str, width: u16) -> u16 {
    if input.is_empty() || width == 0 {
        return 1;
    }
    
    let char_width = input.chars().count();
    let wrapped_lines = (char_width / width as usize) + 1;
    
    // Cap at reasonable maximum (e.g., 5 rows)
    wrapped_lines.min(5) as u16
}
```

### 4. Create input widget helper

Convert input buffer + cursor to styled `Paragraph`:

```rust
fn render_input<'a>(input: &'a str, cursor_pos: usize) -> Paragraph<'a> {
    // Split at cursor, create spans with cursor "block" styling
    let before = &input[..input.char_indices().nth(cursor_pos).map(|(i, _)| i).unwrap_or(input.len())];
    let after = &input[before.len()..];
    
    let spans = vec![
        Span::raw(before),
        Span::styled("█", Style::new().fg(Color::Yellow)),  // cursor
        Span::raw(after),
    ];
    
    Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::TOP).title("Input"))
        .wrap(Wrap { trim: true })
}
```

### 5. Create chat history widget helper

Format messages into styled paragraphs:

```rust
fn render_chat_history(state: &ChatState) -> Paragraph<'_> {
    let lines: Vec<Line> = state.messages.iter().flat_map(|msg| {
        let role_style = match msg.role {
            MessageRole::User => Style::new().fg(Color::Cyan),
            MessageRole::Agent => Style::new().fg(Color::Green),
            MessageRole::System => Style::new().fg(Color::Yellow),
            MessageRole::ToolCall => Style::new().fg(Color::Magenta),
            MessageRole::ToolResult => Style::new().fg(Color::Blue),
        };
        
        vec![
            Line::from(Span::styled(format!("[{}]", msg.role), role_style)),
            Line::from(Span::raw(&msg.content)),
            Line::from(Span::raw("")),  // blank line between messages
        ]
    }).collect();
    
    Paragraph::new(lines)
        .block(Block::default().title("Chat History"))
        .scroll_offset(state.scroll_offset)
        .wrap(Wrap { trim: true })
}
```

### 6. Implement the main `render` method

The render method takes a mutable reference to the terminal:

```rust
fn render(&mut self, state: &AppState, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    terminal.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                // Input area: grows dynamically
                Constraint::Length(self.calculate_input_height(&state.input.buffer, f.size().width)),
                // Chat history: takes remaining space
                Constraint::Min(1),
            ])
            .split(f.size());
        
        // Bottom: Input area
        let input_para = self.render_input(&state.input.buffer, state.input.cursor_pos);
        f.render_widget(input_para, chunks[0]);
        
        // Top: Chat history
        let chat_para = self.render_chat_history(&state.chat);
        f.render_widget(chat_para, chunks[1]);
    })?;
    Ok(())
}
```

### 7. Update the `run` method

Store terminal in the struct or manage it in the run loop:

```rust
async fn run(&mut self) -> Result<()> {
    use ratatui::Terminal;
    use crossterm::terminal::CrosstermBackend;
    use std::io;
    
    // Setup terminal (handles raw mode, alternate screen)
    let mut terminal: Terminal<CrosstermBackend<io::Stdout>> = init();
    
    // Subscribe to state
    let mut state_receiver = self.state_manager.subscribe();
    let initial_state = self.state_manager.get_state();
    self.render(&initial_state, &mut terminal)?;
    
    loop {
        tokio::select! {
            state_event = state_receiver.recv() => {
                match state_event {
                    Ok(state) => self.render(&state, &mut terminal)?,
                    Err(e) => eprintln!("State error: {}", e),
                }
            }
            tui_event = event::read() => {
                // Handle input events via crossterm directly
                // Route to input buffer management
                self.handle_tui_event(tui_event?);
            }
        }
        
        if !self.running {
            break;
        }
    }
    
    // Restore terminal on exit
    restore();
    Ok(())
}
```

**Alternative**: Store `terminal` in the struct `ReplUi` so it's accessible across renders:

```rust
pub struct ReplUi {
    state_manager: Arc<StateManager>,
    action_sender: UnboundedSender<UiAction>,
    text_input: String,
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,  // Add this
    welcome_printed: bool,
    // ...rest
}
```

### 8. Input event handling

Complete key handling for the input area:

```rust
fn handle_tui_event(&mut self, event: TuiEvent) {
    match event {
        TuiEvent::Key(key) => match key.code {
            KeyCode::Char(c) => {
                // Insert character at cursor
                self.text_input.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.text_input.remove(self.cursor_pos);
                }
            }
            KeyCode::Delete => {
                if self.cursor_pos < self.text_input.len() {
                    self.text_input.remove(self.cursor_pos);
                }
            }
            KeyCode::Left => {
                self.cursor_pos = self.cursor_pos.saturating_sub(1);
            }
            KeyCode::Right => {
                self.cursor_pos = (self.cursor_pos + 1).min(self.text_input.len());
            }
            KeyCode::Home => {
                self.cursor_pos = 0;
            }
            KeyCode::End => {
                self.cursor_pos = self.text_input.len();
            }
            KeyCode::Enter => {
                // Send message via UiAction
                let msg = self.text_input.clone();
                if !msg.trim().is_empty() {
                    self.action_sender.send(UiAction::SendMessage(msg)).ok();
                }
                self.text_input.clear();
                self.cursor_pos = 0;
            }
            KeyCode::Esc => {
                self.text_input.clear();
                self.cursor_pos = 0;
            }
            _ => {}
        },
        _ => {}
    }
}
```

## Data Structures to Add

### InputState enhancements (in `app_state.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputState {
    pub buffer: String,
    #[serde(default)]
    pub cursor_pos: usize,
    #[serde(default)]
    pub in_command_mode: bool,
    #[serde(default)]
    pub wrapped_lines: usize,  // For tracking multi-line input
}
```

## Constraints & Edge Cases

1. **Minimum terminal size**: Enforce minimum 10 rows x 20 cols
2. **Empty chat**: Show placeholder text like "No messages yet..."
3. **Long messages**: Word-wrap in chat history
4. **Cursor in wrapped input**: Position cursor correctly across wrapped lines
5. **Resize handling**: Recalculate input height on terminal resize event
6. **Scroll in chat**: Support Up/Down arrows or mouse wheel for chat history

## Files to Modify

1. `src/ui/repl/repl_impl.rs` - Main render implementation
2. `src/ui/app_state.rs` - Add `wrapped_lines` to `InputState` (optional)

## Ratatui Version Notes

Using `ratatui = "0.30"` (currently in Cargo.toml):
- `init()` returns `Terminal<CrosstermBackend<Stdout>>` (backend inferred from context)
- `Layout` constraints use `Constraint::Length` and `Constraint::Min`
- `Paragraph` with `wrap()` for text wrapping
- `Block` with `borders()` for borders
- `terminal.draw()` takes a closure with `Frame`

See: https://docs.rs/ratatui/latest/ratatui/fn.init.html
