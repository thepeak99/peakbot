//! TUI Input Handler
//!
//! This module handles keyboard input for the TUI implementation.
//! It translates raw key events into UiActions that can be processed by the UI.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crate::ui::ui_trait::TuiAction;

/// Input handler for the TUI
///
/// This struct manages input state and translates key events into actions.
pub struct InputHandler {
    /// Whether the input handler is enabled
    enabled: bool,
}

impl InputHandler {
    /// Create a new input handler
    pub fn new() -> Self {
        Self { enabled: true }
    }

    /// Poll for the next key event (blocking)
    pub fn poll_event(&self) -> Result<Option<KeyEvent>, std::io::Error> {
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                Ok(Some(key))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Translate a key event into a TuiAction
    ///
    /// Returns None if the key should be ignored
    pub fn translate_key(&self, key: KeyEvent, _in_command_mode: bool, _todo_visible: bool) -> Option<TuiAction> {
        match key.code {
            KeyCode::Enter => {
                Some(TuiAction::SelectPopupItem)
            }
            KeyCode::Esc => {
                Some(TuiAction::CancelPopup)
            }
            KeyCode::Tab => {
                Some(TuiAction::SelectPopupItem)
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(TuiAction::NavigatePopup(-1))
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(TuiAction::NavigatePopup(1))
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(TuiAction::ToggleTodoPanel)
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Exit is handled by UiAction, not here
                return None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Interrupt/Cancel
                return None; // Let caller handle Ctrl+C
            }
            KeyCode::Char(_c) => {
                // Regular character input
                // This should be handled by the caller to update input buffer
                return None;
            }
            KeyCode::Backspace => {
                // Backspace - handled by caller for input buffer
                return None;
            }
            KeyCode::Left | KeyCode::Right => {
                // Cursor movement - handled by caller for input buffer
                return None;
            }
            _ => None,
        }
    }

    /// Check if a key event should be captured (not passed through)
    pub fn should_capture(&self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Up | KeyCode::Down => true,
            KeyCode::Char(c) if c == 't' && key.modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char(c) if c == 'q' && key.modifiers.contains(KeyModifiers::CONTROL) => true,
            _ => false,
        }
    }

    /// Set whether the input handler is enabled
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if the input handler is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Terminal setup for raw mode
pub struct TerminalSetup;

impl TerminalSetup {
    /// Enable raw mode and hide cursor
    pub fn enable_raw_mode() -> Result<(), std::io::Error> {
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide
        )?;
        crossterm::terminal::enable_raw_mode()?;
        Ok(())
    }

    /// Disable raw mode and show cursor
    pub fn disable_raw_mode() -> Result<(), std::io::Error> {
        crossterm::terminal::disable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_key_enter() {
        let handler = InputHandler::new();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let action = handler.translate_key(key, false, false);
        assert!(action.is_some());
    }

    #[test]
    fn test_translate_key_esc() {
        let handler = InputHandler::new();
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        let action = handler.translate_key(key, false, false);
        assert_eq!(action, Some(TuiAction::CancelPopup));
    }

    #[test]
    fn test_translate_key_ctrl_t() {
        let handler = InputHandler::new();
        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        let action = handler.translate_key(key, false, false);
        assert_eq!(action, Some(TuiAction::ToggleTodoPanel));
    }

    #[test]
    fn test_translate_key_ctrl_q() {
        let handler = InputHandler::new();
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let action = handler.translate_key(key, false, false);
        // Ctrl+Q returns None — exit is handled by the main TUI loop via key.code check
        assert!(action.is_none());
    }

    #[test]
    fn test_translate_key_character() {
        let handler = InputHandler::new();
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::empty());
        let action = handler.translate_key(key, false, false);
        assert!(action.is_none()); // Characters are handled by caller
    }
}
