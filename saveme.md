# PeakBot Compilation Fixes Required

**Generated:** 2026-04-05 10:33:30

## Summary

The project fails to compile due to **6 errors** (2 unique issues causing 6 error instances) and has **2 warnings**. Here's a detailed breakdown:

---

## Error #1: `&mut Message` doesn't implement `Into<Message>` (6 instances)

### Problem
The `prompt_with_history` function in `DynAgent` passes `&mut Vec<Message>` to `with_history()`. Rig's `with_history` expects `T: Into<Message>`, but `&mut Message` doesn't implement this trait (only `&Message` does).

### Root Cause
- **File:** `src/providers/mod.rs` (lines 188-191)
- **File:** `src/lib.rs` (line 562)
- **Issue:** The function signature takes `&mut Vec<Message>` and passes it directly to `with_history()`

### Solution (Fix 1)
Change the function signature to take `&[Message]` instead of `&mut Vec<Message>`:

**File:** `src/providers/mod.rs`
```rust
// BEFORE (lines 182-193):
pub async fn prompt_with_history(
    &self,
    prompt: &str,
    history: &mut Vec<Message>,  // <-- Problem
) -> Result<String, PromptError> {
    match self {
        DynAgent::OpenRouter(agent) => agent.prompt(prompt).with_history(history).await,
        // ...
    }
}

// AFTER:
pub async fn prompt_with_history(
    &self,
    prompt: &str,
    history: &[Message],
) -> Result<String, PromptError> {
    match self {
        DynAgent::OpenRouter(agent) => agent.prompt(prompt).with_history(history.iter().cloned()).await,
        // ... same pattern for other variants
    }
}
```

**File:** `src/lib.rs` (line 559-562)
```rust
// BEFORE:
let mut history = chat_history.lock().await;
let result = agent
    .as_ref()
    .prompt_with_history(&current_msg, &mut history)
    .await;

// AFTER:
let history = chat_history.lock().await;
let result = agent
    .as_ref()
    .prompt_with_history(&current_msg, &history)
    .await;
```

---

## Error #2: Missing methods `wrap_text` and `calculate_input_height` in `ReplUi`

### Problem
The `ReplUi` struct calls two methods that don't exist:
1. `wrap_text` at line 118
2. `calculate_input_height` at line 223

### Root Cause
These methods were likely removed or renamed but the calls weren't updated.

### Solution (Fix 2a) - Remove `wrap_text` call
**File:** `src/ui/repl/repl_impl.rs` (lines 116-119)

Replace manual word wrapping with ratatui's native Paragraph wrapping:

```rust
// BEFORE (lines 116-119):
// Use Paragraph's native wrapping instead of manual word wrapping
let wrap_width = (chat_area.width.saturating_sub(4)) as usize;
let wrapped_lines = Self::wrap_text(&msg.content, wrap_width);
message_lines.extend(wrapped_lines);

// AFTER:
// Use Paragraph's built-in wrapping instead
// The wrapping is already handled by the Paragraph widget at line 141-143
// Just push the content directly
message_lines.push(Line::from(Span::raw(&msg.content)));
```

### Solution (Fix 2b) - Remove `calculate_input_height` call
**File:** `src/ui/repl/repl_impl.rs` (lines 222-223)

Replace with a simple fixed height:

```rust
// BEFORE (lines 222-223):
let input_height =
    Self::calculate_input_height(&self.input_buffer, size.width) as u16;

// AFTER:
let input_height: u16 = 5; // Fixed height for input area
```

---

## Fixes List (One by One)

### Fix 1: Update `prompt_with_history` in `src/providers/mod.rs`
- [ ] Change parameter from `&mut Vec<Message>` to `&[Message]`
- [ ] Update all match arms to use `.iter().cloned()` or `history.iter().map(|m| m.clone())`

### Fix 2: Update call site in `src/lib.rs` line 562
- [ ] Change `&mut history` to `&history`
- [ ] Remove unnecessary `mut` from `history` binding

### Fix 3: Fix `wrap_text` call in `src/ui/repl/repl_impl.rs` line 118
- [ ] Remove `wrap_text` call
- [ ] Simplify to just push content directly (Paragraph handles wrapping)

### Fix 4: Fix `calculate_input_height` in `src/ui/repl/repl_impl.rs` line 223
- [ ] Replace with fixed height value like `5` or a simple calculation

---

## Warnings to Address

### Warning 1: Unused variable `chars_remaining`
**File:** `src/ui/tui/renderer.rs` (line 259)
```rust
let mut chars_remaining = word_len;  // Assigned but never used
```
**Fix:** Rename to `_chars_remaining` or remove if not needed.

### Warning 2: Unused assignment to `chars_remaining`
**File:** `src/ui/tui/renderer.rs` (line 269)
```rust
chars_remaining -= 1;
```
**Fix:** Same as above - the variable seems unnecessary.

---

## Quick Fix Commands

To apply all fixes:

```bash
# Fix 1 & 2: Update prompt_with_history signature and usage
sed -i 's/history: \&mut Vec<Message>/history: \&[Message]/g' src/providers/mod.rs
sed -i 's/with_history(history)/with_history(history.iter().cloned())/g' src/providers/mod.rs
sed -i 's/\&mut history/\&history/g' src/lib.rs
sed -i 's/let mut history = chat_history/let history = chat_history/g' src/lib.rs

# Fix 3 & 4: Simplify ReplUi rendering
# These require manual edits - see above
```

---

## Notes

- The rig-core 0.34.0 `with_history` accepts `IntoIterator<Item = T>` where `T: Into<Message>`
- `&Message` implements `Into<Message>` via reference coercion
- `&mut Message` does NOT implement `Into<Message>` - this is the core API mismatch
- The simplest fix is to pass a slice reference and clone the items in the iterator
