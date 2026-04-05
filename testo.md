# PeakBot REPL UI Tests

## Status: ✅ IMPLEMENTED

All REPL UI tests are implemented and passing (14 tests).

---

## Test Files

| File | Purpose |
|------|---------|
| `tests/repl_tests.rs` | Main integration tests |
| `tests/snapshot_helpers.rs` | Helper functions for TestBackend rendering |

---

## Running Tests

```bash
cargo test --test repl_tests   # Run integration tests
cargo test --lib                # Run unit tests
cargo test                      # Run all tests
```

---

## Test Coverage

### Input Area Tests (5 tests)
- `input_area_empty` - Empty input shows placeholder
- `input_area_cursor_start` - Cursor at start of text
- `input_area_cursor_middle` - Cursor in middle of text
- `input_area_cursor_end` - Cursor at end of text
- `input_area_line_count` - Text wrapping calculation

### Chat History Tests (3 tests)
- `chat_welcome` - Welcome message displayed
- `chat_single_user_message` - User message rendered with prefix
- `chat_single_agent_message` - Agent message rendered with prefix

### Status Bar Tests (2 tests)
- `status_bar_empty` - Empty state shows tokens/context
- `status_bar_with_stats` - Stats formatting (1.5k, etc.)

### State Unit Tests (4 tests)
- `test_chat_message_roles` - ChatMessage role enum
- `test_chat_state_auto_scroll` - auto_scroll toggles on message add
- `test_session_state_formatting` - Token/cost formatting
- `test_context_state_percentage` - Context usage calculation

---

## Implementation Notes

### TestBackend Pattern
Uses Ratatui's `TestBackend` for in-memory rendering:

```rust
use ratatui::{backend::TestBackend, Terminal, widgets::Widget};
use peakbot::ui::repl::ReplUi;

#[test]
fn example() {
    let backend = TestBackend::new(60, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    
    terminal.draw(|f| {
        let paragraph = ReplUi::get_input_area("Hello", 2);
        f.render_widget(paragraph, f.area());
    });
    
    // Use buffer_to_lines helper for content inspection
    let lines = buffer_to_lines(terminal.backend());
    assert!(lines[1].contains("Hello"));
}
```

### Helper Functions

`tests/snapshot_helpers.rs`:
```rust
pub fn render_widget<W: Widget>(widget: W, width: u16, height: u16) -> Terminal<TestBackend>
pub fn buffer_to_lines(backend: &TestBackend) -> Vec<String>
```

### Source Code Changes

Functions in `src/ui/repl/repl_impl.rs` were made `pub` (from `pub(crate)`) for integration test access:
- `get_input_area()`
- `render_chat_history()`
- `render_status_bar()`

### Test Assertions

Due to style differences in Ratatui's buffer (colors), tests use content-based assertions rather than exact buffer matches:

```rust
// ✅ Good: content inspection
assert!(lines[1].contains("Hello"))

// ⚠️ Avoid: exact buffer matching (styles differ)
terminal.backend().assert_buffer_lines([...])  // Use only for border content
```

---

## API

| Function | Returns | Testable |
|----------|---------|----------|
| `get_input_area(input, cursor_pos)` | `Paragraph<'a>` | ✅ |
| `render_chat_history(f, area, scroll, chat)` | renders to Frame | ✅ |
| `render_status_bar(f, area, state)` | renders to Frame | ✅ |

---

## References

- [Ratatui TestBackend](https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html)
- [Ratatui Testing Recipe](https://ratatui.rs/recipes/testing/snapshots/)
