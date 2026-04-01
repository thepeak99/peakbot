# TUI Fix Plan

## TL;DR

Three bugs rooted in **input routing and state handling**, not complex architecture issues. The fixes are surgical: remove `Enter` from capture logic, fix Tab popup selection, and add scrolling to the command popup.

---

## Bug Analysis (Zen Engineering Lens)

### What's Working
- The separation between TUI rendering (`renderer.rs`) and input handling (`tui_impl.rs`) is clean
- State management via `StateManager` is well-structured
- The action/channel architecture between Tui and TuiAgentRunner is sound

### Bug 1: Enter Doesn't Send Messages

**Root Cause**: `should_capture()` returns `true` for `Enter`, routing it to the "special keys" branch instead of character input handling.

```rust
// input_handler.rs line 93
pub fn should_capture(&self, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Esc | KeyCode::Tab | KeyCode::Up | KeyCode::Down => true,
        // ...
    }
}
```

**Flow in `tui_impl.rs` run() loop**:
```
poll_event() → Enter key
    ↓
should_capture(Enter) = true
    ↓
Goes to "special keys" branch (line 180)
    ↓
Checks: if app.command_popup.is_some() → Exit popup
Else → _ → nothing happens
    ↓
Never reaches "character input" branch (line 263) where Enter handling lives
```

The actual Enter handling (line 265-277) is in an `else if` chain AFTER the character handling, guarded by `else if key.code == crossterm::event::KeyCode::Enter`. But because `should_capture` routes Enter to the special keys branch first, this code is **dead code for Enter**.

**Fix**: Move Enter handling into the `should_capture` branch, or restructure the flow.

### Bug 2: Tab Doesn't Select Popup Items

**Root Cause**: Same as Bug 1 - `should_capture()` returns `true` for `Tab`, but the handling in the special keys branch has a subtle issue.

```rust
// tui_impl.rs line 189-196
crossterm::event::KeyCode::Tab => {
    if app.command_popup.is_some() {  // ← Uses OLD state snapshot
        if let Some(command) = self.handle_popup_select() {  // ← Returns None?
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(command)));
            self.cancel_popup();
        }
    }
}
```

The variable `app` is captured at the start of the loop iteration (line 169):
```rust
let app = self.get_state();  // Snapshot for this iteration
```

The popup navigation updates state via `self.state_manager.update_state()`, which modifies the StateManager. But `app` is a snapshot, so after navigation:
1. User presses Up/Down → `handle_popup_navigation()` updates state
2. User presses Tab → `app.command_popup` is the **old snapshot**, not the updated one

**Fix**: Re-fetch state inside Tab handler, or ensure navigation updates propagate before Tab is processed.

### Bug 3: Popup Doesn't Scroll When Selection Exceeds View

**Root Cause**: `selected_index` is not constrained to visible items, and there's no scrolling offset calculation.

```rust
// renderer.rs line 352
let selected_index = popup.selected_index;  // Can be 10, 15, etc.

// renderer.rs line 355-369
let mut items: Vec<ListItem> = commands
    .into_iter()
    .take(MAX_POPUP_ITEMS)  // Only takes 8 items
    .enumerate()  // i = 0 to 7
    .map(|(i, cmd)| {
        let style = if i == selected_index {  // ← selected_index=10, i=0..7, NEVER matches!
            Style::default().fg(Color::White).bg(Color::Blue)
        } else { ... }
    })
```

If `selected_index = 10`:
- `commands.into_iter().take(8)` gives items 0-7
- `i == selected_index` is `0..7 == 10` → **never true**
- **No item is highlighted**
- User is "selecting" item 10 but can't see it

**Fix**: Calculate scroll offset to keep `selected_index` within the visible window `[scroll_offset, scroll_offset + MAX_POPUP_ITEMS)`.

### Additional Issue: `filtered_commands()` Called 4+ Times Per Frame

```rust
// ui_trait.rs - Each call creates NEW Vec, different pointer
pub fn filtered_commands(&self) -> Vec<SlashCommand> { ... }

// Called in:
// 1. render_command_popup() - for list
// 2. handle_popup_select() - for selected_command
// 3. selected_command() - calls filtered_commands() AGAIN
// 4. navigate_up/navigate_down - calls filtered_commands() for count
```

This is inefficient and creates subtle bugs (different instances of the same data). **Not critical but worth fixing** - cache the result or pass it through.

---

## Fix Plan

### Fix 1: Move Enter to Character Handling

**File**: `src/ui/tui/input_handler.rs`
**Change**: Remove `KeyCode::Enter` from `should_capture()`

```rust
pub fn should_capture(&self, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Tab | KeyCode::Up | KeyCode::Down => true,
        // Remove Enter - it should be handled in character input branch
        // ...
    }
}
```

**File**: `src/ui/tui/tui_impl.rs`
**Change**: Add Enter handling in the character branch

```rust
} else if let crossterm::event::KeyCode::Char(c) = key.code {
    // Handle character input
    if c == '/' {
        self.in_command_mode = true;
        self.handle_char_input(c);
    } else if self.in_command_mode {
        match c {
            '\n' | '\r' => {  // Enter now comes here as '\n' from Char('c')
                // ...
            }
            // ...
        }
    }
}
// ADD THIS:
} else if key.code == crossterm::event::KeyCode::Enter {
    if !self.input_buffer.is_empty() {
        if self.in_command_mode {
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(self.input_buffer.clone())));
        } else {
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::SendMessage(self.input_buffer.clone())));
        }
        self.input_buffer.clear();
        self.in_command_mode = false;
        self.update_input_state();
    }
}
```

Actually, a cleaner approach: **Move Enter handling to the special keys branch at line 180** since it's a special key:

```rust
crossterm::event::KeyCode::Enter => {
    if !self.input_buffer.is_empty() {
        if self.in_command_mode {
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(self.input_buffer.clone())));
        } else {
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::SendMessage(self.input_buffer.clone())));
        }
        self.input_buffer.clear();
        self.in_command_mode = false;
        self.update_input_state();
    }
}
```

And remove it from `should_capture()` to avoid the dead code at line 265-277.

### Fix 2: Fix Tab Popup Selection

**File**: `src/ui/tui/tui_impl.rs`
**Change**: Re-fetch state inside Tab handler

```rust
crossterm::event::KeyCode::Tab => {
    // Re-fetch state to get latest popup state after navigation
    let current_state = self.get_state();
    if current_state.command_popup.is_some() {
        if let Some(command) = self.handle_popup_select() {
            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(command)));
            self.cancel_popup();
        }
    }
}
```

But wait - `handle_popup_select()` reads from `self.state_manager.get_state().command_popup`, so it should get fresh state. The issue is `app.command_popup` at line 190 is the OLD snapshot.

Actually, `handle_popup_select()` calls:
```rust
let state = self.state_manager.get_state();  // Fresh state
if let Some(ref popup) = state.command_popup {
    if let Some(cmd) = popup.selected_command() {  // Uses fresh popup
```

So `handle_popup_select()` IS getting fresh state. The bug might be in `selected_command()`:

```rust
pub fn selected_command(&self) -> Option<SlashCommand> {
    self.filtered_commands().get(self.selected_index).cloned()
}
```

If `filtered_commands()` returns a filtered list (e.g., ["/help", "/history"]), and `selected_index = 5`, then `get(5)` returns None.

The index might be out of bounds because navigation doesn't account for filtered results.

**Better Fix**: Validate `selected_index` against filtered list length in navigation:

```rust
// ui_trait.rs
pub fn navigate_up(&mut self) {
    let filtered = self.filtered_commands();  // Get once
    let count = filtered.len();
    if count > 0 {
        self.selected_index = (self.selected_index.saturating_sub(1)) % count;
    }
}

pub fn navigate_down(&mut self) {
    let filtered = self.filtered_commands();
    let count = filtered.len();
    if count > 0 {
        self.selected_index = (self.selected_index + 1) % count;
    }
}
```

### Fix 3: Add Popup Scrolling

**File**: `src/ui/tui/renderer.rs`
**Change**: Calculate scroll offset and only render visible items

```rust
pub fn render_command_popup(f: &mut Frame, app: &AppState) {
    let popup = match &app.command_popup {
        Some(p) => p,
        None => return,
    };

    let filtered = popup.filtered_commands();  // Get once
    if filtered.is_empty() {
        return;
    }

    let area = f.area();
    let visible_height = 8.min(area.height.saturating_sub(6) as usize);
    
    // Calculate scroll offset to keep selected item visible
    let max_index = filtered.len().saturating_sub(1);
    let selected = popup.selected_index.min(max_index);
    
    // Ensure selected is within visible range
    let scroll_offset = if selected >= popup.scroll_offset + visible_height {
        selected - visible_height + 1
    } else if selected < popup.scroll_offset {
        selected
    } else {
        popup.scroll_offset
    };
    
    // Get visible slice
    let visible_commands: Vec<_> = filtered
        .iter()
        .skip(scroll_offset)
        .take(visible_height)
        .enumerate()
        .collect();
    
    // Build list with adjusted indices
    let items: Vec<ListItem> = visible_commands
        .map(|(i, cmd)| {
            let actual_index = scroll_offset + i;
            let style = if actual_index == selected {
                Style::default().fg(Color::White).bg(Color::Blue)
            } else {
                Style::default().fg(Color::White).bg(Color::Black)
            };
            let takes_arg_str = if cmd.takes_args { " <args>" } else { "" };
            let cmd_str = format!("/{}{} - {}", cmd.name, takes_arg_str, cmd.description);
            ListItem::new(cmd_str).style(style)
        })
        .collect();
    
    // ... rest of rendering
}
```

**Also**: Update `CommandPopupState` to track `scroll_offset`:

```rust
// ui_trait.rs
pub struct CommandPopupState {
    pub prefix: String,
    pub selected_index: usize,
    pub scroll_offset: usize,  // NEW
}
```

And add navigation methods to update scroll:

```rust
pub fn ensure_selection_visible(&mut self, visible_height: usize) {
    let count = self.filtered_commands().len();
    if count == 0 { return; }
    
    self.scroll_offset = self.scroll_offset.min(self.selected_index);
    if self.selected_index >= self.scroll_offset + visible_height {
        self.scroll_offset = self.selected_index - visible_height + 1;
    }
}
```

---

## Implementation Order

1. **Fix 1 (Enter)**: Remove Enter from `should_capture()`, add Enter handler to special keys branch - **HIGHEST PRIORITY**
2. **Fix 2 (Tab)**: The index validation in navigation should fix this - **MEDIUM PRIORITY**
3. **Fix 3 (Popup scroll)**: Add scrolling support - **MEDIUM PRIORITY**

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/ui/tui/input_handler.rs` | Remove `KeyCode::Enter` from `should_capture()` |
| `src/ui/tui/tui_impl.rs` | Move Enter handler, re-fetch state in Tab |
| `src/ui/tui/renderer.rs` | Add scroll offset calculation in popup |
| `src/ui/ui_trait.rs` | Add `scroll_offset` to `CommandPopupState`, fix navigation |
| `src/ui/app_state.rs` | N/A - no changes needed |

---

## Testing Checklist

After fixes, verify:
- [ ] Type message → Press Enter → Message appears in chat
- [ ] Type `/` → Popup appears → Press Tab → Command executes
- [ ] Type `/` → Press Down 10 times → Selection scrolls into view
- [ ] Type `/h` → Only "/help" shown → Press Enter → "/help" executes
- [ ] Press Esc → Popup dismisses
- [ ] Small terminal (20 rows) → Popup still renders correctly

---

## Zen Principles Applied

1. **Fewer pieces → fewer things that can go wrong**: Removed duplicate Enter handling code
2. **Parse at boundary, trust inside**: State is fetched once per operation, not carried around
3. **Don't duplicate data**: `filtered_commands()` called once and reused
4. **Simple is better than clever**: Direct Enter handling in special keys branch vs. complex character parsing
