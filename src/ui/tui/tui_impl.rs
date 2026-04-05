//! TUI Implementation
//!
//! This module provides the main TUI struct that implements the Ui trait.
//! It ties together the renderer, input handler, and state manager.
//!
//! ## MVC View
//!
//! Tui is a View. It:
//! - Subscribes to StateManager for state updates
//! - Sends UiActions over a channel to the Controller
//! - Renders using ratatui based on current state

use anyhow::{Result, anyhow};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::ui::app_state::AppState;
use crate::ui::tui::input_handler::{InputHandler, TerminalSetup};
use crate::ui::tui::renderer::ui as render_ui;
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction};

/// Terminal User Interface implementation for PeakBot
pub struct Tui {
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    input_handler: InputHandler,
    state_manager: Arc<StateManager>,
    action_sender: UnboundedSender<UiAction>,
    input_buffer: String,
    in_command_mode: bool,
    running: bool,
}

impl Tui {
    pub fn new(state_manager: Arc<StateManager>, action_sender: UnboundedSender<UiAction>) -> Self {
        Self {
            terminal: None,
            input_handler: InputHandler::new(),
            state_manager,
            action_sender,
            input_buffer: String::new(),
            in_command_mode: false,
            running: true,
        }
    }

    fn get_state(&self) -> AppState {
        self.state_manager.get_state()
    }

    fn update_input_state(&self) {
        let mut state = self.state_manager.get_state();
        state.input.buffer = self.input_buffer.clone();
        state.input.in_command_mode = self.in_command_mode;
        state.input.cursor_pos = self.input_buffer.len();

        if self.in_command_mode && !state.input.buffer.is_empty() {
            let prefix = state.input.buffer.trim_start_matches('/').to_string();
            state.command_popup =
                Some(crate::ui::ui_trait::CommandPopupState::new(prefix));
        } else if !self.in_command_mode {
            state.command_popup = None;
        }

        self.state_manager.update_state(state);
    }

    fn handle_char_input(&mut self, c: char) {
        if c == '/' && self.input_buffer.is_empty() {
            self.in_command_mode = true;
        }
        self.input_buffer.push(c);
        self.update_input_state();
    }

    fn handle_backspace(&mut self) {
        if self.in_command_mode && self.input_buffer.starts_with('/') {
            if self.input_buffer.len() > 1 {
                self.input_buffer.pop();
                self.update_input_state();
            }
        } else {
            self.input_buffer.pop();
            if self.input_buffer.is_empty() {
                self.in_command_mode = false;
            }
            self.update_input_state();
        }
    }

    fn handle_popup_navigation(&mut self, direction: i32) {
        let mut state = self.state_manager.get_state();
        if let Some(ref mut popup) = state.command_popup {
            if direction < 0 {
                popup.navigate_up();
            } else {
                popup.navigate_down();
            }
        }
        self.state_manager.update_state(state);
    }

    fn handle_popup_select(&mut self) -> Option<String> {
        let state = self.state_manager.get_state();
        if let Some(ref popup) = state.command_popup {
            if let Some(cmd) = popup.selected_command() {
                let command = format!("/{}", cmd.name);
                return Some(command);
            }
        }
        None
    }

    fn cancel_popup(&mut self) {
        let mut state = self.state_manager.get_state();
        state.command_popup = None;
        self.in_command_mode = false;
        self.input_buffer.clear();
        drop(state);
        self.state_manager.set_command_popup(None);
        self.update_input_state();
    }

    fn send_action(&self, action: UiAction) {
        let _ = self.action_sender.send(action);
    }
}

impl Ui for Tui {
    async fn init(&mut self) -> Result<()> {
        TerminalSetup::enable_raw_mode()?;
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    async fn run(&mut self) -> Result<()> {
        if self.terminal.is_none() {
            return Err(anyhow!("TUI not initialized. Call init() first."));
        }

        self.running = true;

        while self.running {
            let app = self.get_state();

            if let Some(ref mut terminal) = self.terminal {
                terminal.draw(|f| render_ui(f, &app))?;
            }

            if let Some(key) = self.input_handler.poll_event()? {
                if self.input_handler.should_capture(&key) {
                    match key.code {
                        crossterm::event::KeyCode::Esc => {
                            if app.command_popup.is_some() {
                                self.cancel_popup();
                            } else {
                                self.running = false;
                            }
                        }
                        crossterm::event::KeyCode::Tab => {
                            if app.command_popup.is_some() {
                                if let Some(command) = self.handle_popup_select() {
                                    self.send_action(UiAction::ExecuteCommand(command));
                                    self.cancel_popup();
                                }
                            }
                        }
                        crossterm::event::KeyCode::Up if app.command_popup.is_some() => {
                            self.handle_popup_navigation(-1);
                        }
                        crossterm::event::KeyCode::Char('k')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                && app.command_popup.is_some() =>
                        {
                            self.handle_popup_navigation(-1);
                        }
                        crossterm::event::KeyCode::Down if app.command_popup.is_some() => {
                            self.handle_popup_navigation(1);
                        }
                        crossterm::event::KeyCode::Char('j')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                                && app.command_popup.is_some() =>
                        {
                            self.handle_popup_navigation(1);
                        }
                        crossterm::event::KeyCode::Char('t')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            // Toggle TODO panel visibility — local UI concern
                            let mut state = self.state_manager.get_state();
                            state.todo.toggle_visibility();
                            self.state_manager.update_todo_state(state.todo.clone());
                        }
                        crossterm::event::KeyCode::Char('q')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            self.running = false;
                        }
                        _ => {}
                    }
                } else if let crossterm::event::KeyCode::Char(c) = key.code {
                    if c == '/' && self.input_buffer.is_empty() {
                        self.in_command_mode = true;
                        self.handle_char_input(c);
                    } else if self.in_command_mode {
                        match c {
                            '\t' => {
                                if let Some(command) = self.handle_popup_select() {
                                    self.send_action(UiAction::ExecuteCommand(command));
                                    self.cancel_popup();
                                }
                            }
                            '\n' | '\r' => {
                                if !self.input_buffer.is_empty() {
                                    if self.in_command_mode {
                                        self.send_action(UiAction::ExecuteCommand(
                                            self.input_buffer.clone(),
                                        ));
                                    } else {
                                        self.send_action(UiAction::SendMessage(
                                            self.input_buffer.clone(),
                                        ));
                                    }
                                    self.input_buffer.clear();
                                    self.in_command_mode = false;
                                    self.update_input_state();
                                }
                            }
                            '\x7f' => {
                                self.handle_backspace();
                            }
                            _ => {
                                self.handle_char_input(c);
                            }
                        }
                    } else {
                        self.handle_char_input(c);
                    }
                } else if key.code == crossterm::event::KeyCode::Backspace {
                    self.handle_backspace();
                } else if key.code == crossterm::event::KeyCode::Enter {
                    if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
                        // Shift+Enter inserts a newline
                        self.input_buffer.push('\n');
                        self.update_input_state();
                    } else if !self.input_buffer.is_empty() {
                        if self.in_command_mode {
                            self.send_action(UiAction::ExecuteCommand(
                                self.input_buffer.clone(),
                            ));
                        } else {
                            self.send_action(UiAction::SendMessage(self.input_buffer.clone()));
                        }
                        self.input_buffer.clear();
                        self.in_command_mode = false;
                        self.update_input_state();
                    }
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        TerminalSetup::disable_raw_mode()?;
        self.terminal = None;
        self.running = false;
        Ok(())
    }
}
