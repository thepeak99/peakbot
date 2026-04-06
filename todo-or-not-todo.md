# Todo Panel Design Proposal

**TL;DR** — Add a side panel todo widget to the REPL TUI with Ctrl+T toggle. One new file, minimal state changes, horizontal layout split when visible.

---

## TL;DR

Add a collapsible todo panel to the REPL TUI that displays alongside the chat. Toggle with Ctrl+T. Keep it minimal — one new file, extend existing layout, wire into existing keyboard handling.

---

## What's Working

- **Clean MVC separation**: StateManager (Model) → ReplUi (View) → AgentRunner (Controller)
- **Existing todo state**: `TodoState` and `TodoItem` already exist in `app_state.rs`
- **Ratatui layout**: Already uses `Layout` with `Direction::Vertical`, extensible to horizontal splits
- **Keyboard handling**: Well-organized `handle_keyboard_input()` in `repl_impl.rs`
- **State sync**: `StateManager::update_todo()` already exists

---

## Design

### Layout (Horizontal Split)

```
┌─────────────────────────────────────┬────────────────────┐
│                                     │     TODO PANEL     │
│         CHAT HISTORY                │  ──────────────── │
│                                     │  ○ Task 1          │
│                                     │  ◐ Task 2          │
│                                     │  ● Task 3          │
│─────────────────────────────────────│                    │
│         INPUT AREA                  │  ──────────────── │
│                                     │  Ctrl+T toggle     │
└─────────────────────────────────────┴────────────────────┘
│              STATUS BAR                               │
└────────────────────────────────────────────────────────┘
```

**When todo panel is hidden** (default):
- Current vertical layout: Chat (flex) → Input (min) → Status (1)

**When todo panel is visible**:
- Horizontal split: Chat+Input (flex) | Todo (30% or fixed 40 cols)
- Status bar spans full width

### Key Measurements

| Component | Min Width | Default Width |
|-----------|-----------|---------------|
| Todo Panel | 20 cols | 40 cols (30%) |
| Chat+Input | 40 cols | remaining |

### File Structure

```
src/ui/
├── mod.rs
├── repl/
│   ├── mod.rs
│   ├── repl_impl.rs      # Modified: add Ctrl+T handler, layout change
│   └── todo_panel.rs     # NEW: todo widget rendering
├── app_state.rs         # Existing: TodoState, TodoItem
└── state_manager.rs    # Existing: already has todo state
```

---

## Implementation Details

### 1. New File: `src/ui/repl/todo_panel.rs`

**Responsibilities**:
- Render the todo list as a ratatui widget
- Handle todo-specific rendering (checkboxes, status icons, selection)
- Calculate content height for scrolling

**Public API**:
```rust
pub struct TodoPanel;

impl TodoPanel {
    /// Build the todo list paragraph for rendering
    pub fn build_paragraph<'a>(state: &'a TodoState) -> Paragraph<'a>;
    
    /// Render the todo panel to a frame area
    pub fn render(f: &mut Frame, area: Rect, state: &TodoState);
    
    /// Get the line count for scrollbar state
    pub fn line_count(state: &TodoState, width: u16) -> usize;
}
```

**Styling**:
- Block title: "✓ TODO" (or similar)
- Status icons:
  - Pending: `○` (light gray)
  - In Progress: `◐` (yellow)
  - Completed: `●` (green, strikethrough text)
  - Cancelled: `✗` (red/dark gray)
- Selected item: highlighted background
- Empty state: "No tasks" message

### 2. Modified: `src/ui/repl/repl_impl.rs`

**Changes**:

```rust
// In UiState struct, add:
pub show_todo_panel: bool,
pub todo_scroll_position: u16,
pub todo_selected_index: usize,

// In render(), change layout:
let chunks = if self.ui_state.show_todo_panel {
    // Horizontal: [Chat|Input] | Todo
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(70),
            Constraint::Percentage(30),
        ])
        .split(size)
} else {
    // Vertical only (current layout)
    vec![size]
};

// Then split left pane vertically
let main_area = chunks[0];
let main_chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([...])  // existing constraints
    .split(main_area);

// Render todo panel if visible
if self.ui_state.show_todo_panel {
    TodoPanel::render(f, chunks[1], &state.todo);
}

// In handle_keyboard_input(), add:
KeyCode::Char('t' | 'T') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    self.ui_state.show_todo_panel = !self.ui_state.show_todo_panel;
}
// Also handle ↑/↓ when todo is focused (optional)
// For v1: no focus, just display
```

**State sync** (already handled):
- `StateManager::update_todo()` is called by AgentRunner when todo changes
- UI reads `state.todo` on each render tick

### 3. No changes needed to:

- `app_state.rs` — TodoState already exists
- `state_manager.rs` — already has `update_todo()`
- `lib.rs` / `main.rs` — state flows through existing channels

---

## Keyboard Interactions

| Key | Action |
|-----|--------|
| `Ctrl+T` | Toggle todo panel visibility |
| `↑` / `↓` | Navigate todo items (optional v1 enhancement) |
| `Space` | Toggle selected item status (optional v1 enhancement) |
| `Enter` | Send message (input focused) |

**Design decision**: For v1, todo panel is **display-only**. Navigation/editing happens via the `todo` tool in chat. This keeps v1 minimal.

---

## Edge Cases

| Case | Behavior |
|------|----------|
| Terminal too narrow (<60 cols) | Hide todo panel automatically, show notification |
| Empty todo list | Show "No tasks" placeholder |
| Many items (>20) | Scrollbar appears, mouse wheel scrolls |
| Terminal resize | Re-layout on next tick |

---

## Zen of Engineering Review

### What can go wrong?
- **Resize during render**: Use `size` from each frame draw, no cached dimensions
- **State race**: StateManager already handles this with RwLock
- **Panel takes too much space**: Fixed percentage (30%) prevents takeover

### What can be removed?
- **Focus state for todo**: Not needed for v1 (display-only)
- **Custom todo action handlers**: Reuse existing keyboard handler
- **Separate todo component state**: Use existing TodoState

### What will confuse people?
- **Ctrl+T is vim-style** (toggle list): Expected behavior for terminal users
- **Panel appears on right, not left**: Intuitive — chat is primary

### What is superfluous?
- **Todo-specific state in UiState**: Reuse `TodoState` from app_state.rs
- **Custom UiAction for todo**: Existing Ctrl+T handler is sufficient
- **Separate TodoItem type**: Already exists in app_state.rs

### Are the boundaries clean?
- **View-only**: TodoPanel reads from TodoState, never writes
- **State lives in StateManager**: Single source of truth
- **No new abstractions**: Just a rendering function

---

## Files to Modify

| File | Change | Lines |
|------|--------|-------|
| `src/ui/repl/todo_panel.rs` | **NEW** — todo widget | ~150 |
| `src/ui/repl/repl_impl.rs` | Layout + Ctrl+T | ~50 |
| `src/ui/repl/mod.rs` | Export new module | ~3 |

---

## Alternatives Considered

### A. Floating popup instead of side panel
- **Rejected**: Obstructs chat, feels modal
- Side panel is non-intrusive, always visible when toggled

### B. Bottom panel instead of side
- **Rejected**: Competes with input area
- Side panel leaves input free, better use of horizontal space

### C. Full MVC for todo panel
- **Rejected**: Over-engineering — display-only v1
- No need for TodoPanelController, TodoPanelState

### D. Separate UiAction enum variant
- **Rejected**: Ctrl+T doesn't need controller action
- Direct state toggle in view is sufficient

---

## Implementation Order

1. Create `src/ui/repl/todo_panel.rs` with basic rendering
2. Add `show_todo_panel` to UiState
3. Modify layout in `render()` for horizontal split
4. Add Ctrl+T handler in `handle_keyboard_input()`
5. Test with various terminal sizes
6. Add scrollbar if items exceed panel height

---

## Guiding Heuristic

**Display-only first, interactions later.** The todo panel v1 is a live view into state that the agent already manages. Don't add complexity (focus, selection, editing) until the basic display works. The agent's `todo` tool already provides full CRUD operations — the panel just makes the state visible.
