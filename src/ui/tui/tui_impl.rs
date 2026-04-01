//! TUI Implementation
//!
//! This module provides the main TUI struct that implements the Ui trait.
//! It ties together the renderer, input handler, and state manager.

use anyhow::{Result, anyhow};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use crate::ui::app_state::{AppState, ChatMessage};
use crate::ui::tui::input_handler::{InputHandler, TerminalSetup};
use crate::ui::tui::renderer::ui as render_ui;
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::{Ui, UiAction, TodoItemAction};

/// Terminal User Interface implementation for PeakBot
///
/// This struct implements the `Ui` trait and provides a terminal-based
/// interface using ratatui and crossterm.
pub struct Tui {
    /// The terminal backend
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
    
    /// Input handler for processing keyboard events
    input_handler: InputHandler,
    
    /// Reference to the state manager
    state_manager: Arc<StateManager>,
    
    /// Channel to send actions to the agent
    action_sender: Option<UnboundedSender<UiAction>>,
    
    /// Current input buffer
    input_buffer: String,
    
    /// Whether we're in command mode (after typing /)
    in_command_mode: bool,
    
    /// Whether the TUI should continue running
    running: bool,
}

impl Tui {
    /// Create a new TUI instance
    pub fn new(state_manager: Arc<StateManager>, action_sender: Option<UnboundedSender<UiAction>>) -> Self {
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

    /// Get current state
    fn get_state(&self) -> AppState {
        self.state_manager.get_state()
    }

    /// Update input state in the state manager
    fn update_input_state(&self) {
        let mut state = self.state_manager.get_state();
        state.input.buffer = self.input_buffer.clone();
        state.input.in_command_mode = self.in_command_mode;
        state.input.cursor_pos = self.input_buffer.len();
        
        // Update command popup if in command mode
        if self.in_command_mode && !state.input.buffer.is_empty() {
            let prefix = state.input.buffer.trim_start_matches('/').to_string();
            state.command_popup = Some(
                crate::ui::ui_trait::CommandPopupState::new(prefix)
            );
        } else if !self.in_command_mode {
            state.command_popup = None;
        }
        
        // Notify update
        self.state_manager.update_state(state);
    }

    /// Process character input
    fn handle_char_input(&mut self, c: char) {
        self.input_buffer.push(c);
        self.update_input_state();
    }

    /// Handle backspace
    fn handle_backspace(&mut self) {
        if self.in_command_mode && self.input_buffer.starts_with('/') {
            // In command mode, don't delete the leading /
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

    /// Handle command popup navigation
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

    /// Handle popup selection
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

    /// Cancel popup
    fn cancel_popup(&mut self) {
        let mut state = self.state_manager.get_state();
        state.command_popup = None;
        self.in_command_mode = false;
        self.input_buffer.clear();
        drop(state);
        self.state_manager.set_command_popup(None);
        self.update_input_state();
    }
}

impl Ui for Tui {
    /// Initialize the TUI (setup terminal, raw mode, etc.)
    fn init(&mut self) -> Result<()> {
        // Enable raw mode
        TerminalSetup::enable_raw_mode()?;
        
        // Create terminal
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        self.terminal = Some(terminal);
        
        Ok(())
    }

    /// Run the TUI event loop (blocking)
    fn run(&mut self) -> Result<()> {
        if self.terminal.is_none() {
            return Err(anyhow!("TUI not initialized. Call init() first."));
        }

        self.running = true;
        
        while self.running {
            // Get current state before borrowing terminal
            let app = self.get_state();
            
            // Render the UI
            if let Some(ref mut terminal) = self.terminal {
                terminal.draw(|f| render_ui(f, &app))?;
            }
            
            // Handle input events
            if let Some(key) = self.input_handler.poll_event()? {
                // Check for special keys first
                if self.input_handler.should_capture(&key) {
                    match key.code {
                        crossterm::event::KeyCode::Esc => {
                            if app.command_popup.is_some() {
                                self.cancel_popup();
                            } else {
                                // Check for exit confirmation or just exit
                                self.running = false;
                            }
                        }
                        crossterm::event::KeyCode::Tab => {
                            if app.command_popup.is_some() {
                                if let Some(command) = self.handle_popup_select() {
                                    // Execute the command
                                    let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(command)));
                                    self.cancel_popup();
                                }
                            }
                        }
                        crossterm::event::KeyCode::Up
                            if app.command_popup.is_some() => {
                                self.handle_popup_navigation(-1);
                            }
                        crossterm::event::KeyCode::Char('k')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && app.command_popup.is_some() => {
                                self.handle_popup_navigation(-1);
                            }
                        crossterm::event::KeyCode::Down
                            if app.command_popup.is_some() => {
                                self.handle_popup_navigation(1);
                            }
                        crossterm::event::KeyCode::Char('j')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && app.command_popup.is_some() => {
                                self.handle_popup_navigation(1);
                            }
                        crossterm::event::KeyCode::Char('t') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                            // Toggle TODO panel
                            let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ToggleTodoPanel));
                        }
                        crossterm::event::KeyCode::Char('q') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                            self.running = false;
                        }
                        _ => {}
                    }
                } else if let crossterm::event::KeyCode::Char(c) = key.code {
                    // Handle character input
                    if c == '/' {
                        self.in_command_mode = true;
                        self.handle_char_input(c);
                    } else if self.in_command_mode {
                        // In command mode, handle special keys
                        match c {
                            '\t' => {
                                // Tab - select popup
                                if let Some(command) = self.handle_popup_select() {
                                    let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(command)));
                                    self.cancel_popup();
                                }
                            }
                            '\n' | '\r' => {
                                // Enter - execute command or send message
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
                            '\x7f' => {
                                // Delete/Backspace
                                self.handle_backspace();
                            }
                            _ => {
                                self.handle_char_input(c);
                            }
                        }
                    } else {
                        // Normal character input
                        self.handle_char_input(c);
                    }
                } else if key.code == crossterm::event::KeyCode::Backspace {
                    self.handle_backspace();
                } else if key.code == crossterm::event::KeyCode::Enter {
                    // Send message or execute command
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
            }
        }
        
        Ok(())
    }

    /// Shutdown the TUI gracefully
    fn shutdown(&mut self) -> Result<()> {
        // Disable raw mode
        TerminalSetup::disable_raw_mode()?;
        
        self.terminal = None;
        self.running = false;
        
        Ok(())
    }

    /// Handle a user action
    fn on_action(&mut self, action: UiAction) -> Result<()> {
        match action {
            UiAction::SendMessage(msg) => {
                // Add user message to chat
                let message = ChatMessage::user(msg);
                self.state_manager.update_chat(message);
            }
            UiAction::ExecuteCommand(cmd) => {
                // Execute command - this would be handled by the agent
                let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(cmd)));
            }
            UiAction::ToggleTodoPanel => {
                let mut state = self.state_manager.get_state();
                state.todo.toggle_visibility();
                self.state_manager.update_todo_state(state.todo.clone());
            }
            UiAction::UpdateTodoItem(index, item_action) => {
                let mut state = self.state_manager.get_state();
                match item_action {
                    TodoItemAction::ToggleComplete => {
                        state.todo.update_item(index, crate::ui::ui_trait::TodoItemAction::ToggleComplete);
                    }
                    TodoItemAction::UpdateStatus(status) => {
                        state.todo.update_item(index, crate::ui::ui_trait::TodoItemAction::UpdateStatus(status));
                    }
                    TodoItemAction::Delete => {
                        state.todo.update_item(index, crate::ui::ui_trait::TodoItemAction::Delete);
                    }
                }
                self.state_manager.update_todo_state(state.todo.clone());
            }
            UiAction::AddTodoItem(text) => {
                let mut state = self.state_manager.get_state();
                state.todo.add_item(text);
                self.state_manager.update_todo_state(state.todo.clone());
            }
            UiAction::ClearCompletedTodos => {
                let mut state = self.state_manager.get_state();
                state.todo.clear_completed();
                self.state_manager.update_todo_state(state.todo.clone());
            }
            UiAction::Exit => {
                self.running = false;
            }
            UiAction::NavigatePopup(direction) => {
                self.handle_popup_navigation(direction);
            }
            UiAction::SelectPopupItem => {
                if let Some(command) = self.handle_popup_select() {
                    let _ = self.action_sender.as_ref().map(|s| s.send(UiAction::ExecuteCommand(command)));
                    self.cancel_popup();
                }
            }
            UiAction::CancelPopup => {
                self.cancel_popup();
            }
        }
        
        Ok(())
    }
}
