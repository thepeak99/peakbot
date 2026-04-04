# Ratatui Keyboard Input Testing — Agent Prompt

## Context

You are working on a Rust TUI application using **Ratatui**. The project already uses snapshot testing for UI output (e.g. with `insta` and `TestBackend`). The goal is to add keyboard input testing.

## Core Principle

**Do not test raw crossterm terminal I/O.** Instead, keep event handling as pure logic on the app state struct, then test that logic directly. The boundary between I/O and logic is what makes testing possible.

---

## Architecture Rule

The app's event handler must be a **pure method** on the state struct:

```rust
impl App {
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        // no I/O here — pure state transition
    }
}
```

The event loop in `main` reads from the terminal and passes `KeyEvent` values in. `handle_key` only processes them.

---

## Types to Use

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
```

Construct test key events with:

```rust
KeyEvent::new(code, KeyModifiers::NONE)     // regular key
KeyEvent::new(code, KeyModifiers::CONTROL)  // Ctrl+key
KeyEvent::new(code, KeyModifiers::SHIFT)    // Shift+key
KeyEvent::new(code, KeyModifiers::ALT)      // Alt+key
```

---

## Example: App State + Handler

```rust
#[derive(Debug, PartialEq)]
pub enum AppAction {
    Quit,
    Continue,
}

pub struct App {
    pub counter: u32,
    pub input: String,
}

impl App {
    pub fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Char('q') => AppAction::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                AppAction::Quit
            }
            KeyCode::Up => {
                self.counter += 1;
                AppAction::Continue
            }
            KeyCode::Down => {
                self.counter = self.counter.saturating_sub(1);
                AppAction::Continue
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                AppAction::Continue
            }
            KeyCode::Backspace => {
                self.input.pop();
                AppAction::Continue
            }
            _ => AppAction::Continue,
        }
    }
}
```

---

## Example: Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn quit_on_q() {
        let mut app = App { counter: 0, input: String::new() };
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), AppAction::Quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App { counter: 0, input: String::new() };
        assert_eq!(app.handle_key(ctrl(KeyCode::Char('c'))), AppAction::Quit);
    }

    #[test]
    fn up_increments_counter() {
        let mut app = App { counter: 5, input: String::new() };
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.counter, 6);
    }

    #[test]
    fn down_saturates_at_zero() {
        let mut app = App { counter: 0, input: String::new() };
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.counter, 0);
    }

    #[test]
    fn typing_builds_input() {
        let mut app = App { counter: 0, input: String::new() };
        for c in ['h', 'e', 'l', 'l', 'o'] {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut app = App { counter: 0, input: "hello".into() };
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "hell");
    }
}
```

---

## Example: Combining with Snapshot Tests

To assert rendered output *after* a sequence of key presses:

```rust
#[test]
fn snapshot_after_input() {
    let mut app = App { counter: 0, input: String::new() };

    // Simulate a key sequence
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Char('h')));

    // Render and snapshot
    let mut terminal = ratatui::Terminal::new(
        ratatui::backend::TestBackend::new(80, 24)
    ).unwrap();
    terminal.draw(|f| app.render(f, f.area())).unwrap();

    insta::assert_snapshot!(terminal.backend().to_string());
}
```

---

## Rules for the Agent

1. **Never put terminal I/O inside `handle_key`.** It must be a pure state transition.
2. **Always return an action enum** (`AppAction` or equivalent) from `handle_key` so the event loop can decide what to do (quit, redraw, etc.).
3. **Use `saturating_sub`** for any counter that should not underflow below zero.
4. Use **`KeyEvent::new(code, modifiers)`** to construct events in tests — no mocking framework needed.
5. **Helper functions `key()` and `ctrl()`** (and `shift()`, `alt()` as needed) keep tests concise — define them in the test module.
6. When snapshot-testing after input, **mutate state first, then render** — never the other way around.
7. Do not test the crossterm event polling loop itself — that is I/O infrastructure, not app logic.
