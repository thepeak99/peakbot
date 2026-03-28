//! State Manager
//!
//! This module provides centralized state management for all UI implementations.
//! It keeps `AppState` in sync with the core PeakBot state (TodoList, SessionStats, etc.)
//! and distributes updates to subscribed UIs.

use crate::hooks::events::AgentEvent;
use crate::hooks::session_hook::SessionStats;
use crate::TodoList;
use crate::ui::app_state::{
    AppState, ChatMessage, ChatState, ContextState, SessionState, TodoItem, TodoState,
};
use crate::ui::ui_trait::UiAction;
use anyhow::Result;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

/// Manages AppState and distributes updates to UI
///
/// The StateManager serves as the single source of truth for all UI-renderable state.
/// It synchronizes with core PeakBot components (TodoList, SessionStats, AgentRunner)
/// and notifies UIs of state changes via channels.
pub struct StateManager {
    state: Arc<RwLock<AppState>>,
    update_sender: Sender<AppState>,
    action_receiver: Arc<Mutex<Receiver<UiAction>>>,
}

impl StateManager {
    /// Create a new StateManager
    pub fn new() -> Self {
        let (update_sender, update_receiver) = mpsc::channel();
        let (_action_sender, action_receiver) = mpsc::channel();

        let state = Arc::new(RwLock::new(AppState::new()));

        // Spawn a thread to process state updates
        // This ensures updates don't block the sender
        let state_clone = state.clone();
        std::thread::spawn(move || {
            let receivers = vec![update_receiver];
            while let Ok(update) = receivers[0].recv() {
                let mut state = state_clone.write().unwrap();
                *state = update;
            }
            tracing::debug!("StateManager update thread exiting");
        });

        Self {
            state,
            update_sender,
            action_receiver: Arc::new(Mutex::new(action_receiver)),
        }
    }

    /// Get current state snapshot
    pub fn get_state(&self) -> AppState {
        self.state.read().unwrap().clone()
    }

    /// Get state update channel for UI to subscribe
    pub fn subscribe(&self) -> Receiver<AppState> {
        // For now, return a receiver that gets notified on state changes
        // In a more sophisticated implementation, we'd use a publish-subscribe pattern
        let state = self.state.clone();
        let (sender, receiver) = mpsc::channel();
        
        // Send current state immediately
        let _ = sender.send(state.read().unwrap().clone());
        
        // TODO: Implement proper subscription with update notifications
        receiver
    }

    /// Send action to be processed
    ///
    /// Returns a sender that can be used to send actions to this StateManager
    pub fn action_sender(&self) -> Sender<UiAction> {
        // We need to store this somewhere - for now, return a dummy
        // The actual implementation would store the sender in StateManager
        unimplemented!("Action sender not yet implemented - use action_receiver directly")
    }

     /// Receive an action from the UI
    pub fn receive_action(&self) -> Result<UiAction> {
        self.action_receiver.lock().unwrap().recv().map_err(|e| e.into())
    }

    /// Update chat messages
    ///
    /// Called by AgentRunner when new messages are added
    pub fn update_chat(&self, message: ChatMessage) {
        let mut state = self.state.write().unwrap();
        state.chat.add_message(message);
        self.notify_update(&state);
    }

    /// Clear all chat messages
    pub fn clear_chat(&self) {
        let mut state = self.state.write().unwrap();
        state.chat.clear();
        self.notify_update(&state);
    }

    /// Set loading state
    pub fn set_loading(&self, loading: bool) {
        let mut state = self.state.write().unwrap();
        state.is_loading = loading;
        self.notify_update(&state);
    }

    /// Update chat state entirely
    pub fn update_chat_state(&self, chat_state: ChatState) {
        let mut state = self.state.write().unwrap();
        state.chat = chat_state;
        self.notify_update(&state);
    }

    /// Update TODO state (syncs with core TodoList)
    pub fn update_todo(&self, todo_list: &TodoList) {
        let items: Vec<TodoItem> = todo_list
            .list()
            .iter()
            .map(|item| TodoItem::from(item))
            .collect();

        let mut state = self.state.write().unwrap();
        state.todo.items = items;
        self.notify_update(&state);
    }

    /// Update TODO state entirely
    pub fn update_todo_state(&self, todo_state: TodoState) {
        let mut state = self.state.write().unwrap();
        state.todo = todo_state;
        self.notify_update(&state);
    }

    /// Update session stats (syncs with CostTracker/SessionStats)
    pub fn update_stats(&self, stats: &Arc<std::sync::Mutex<SessionStats>>) {
        let stats_lock = stats.lock().unwrap();
        
        let mut state = self.state.write().unwrap();
        state.stats.total_input_tokens = stats_lock.total_input_tokens;
        state.stats.total_output_tokens = stats_lock.total_output_tokens;
        state.stats.total_api_calls = stats_lock.total_api_calls;
        state.stats.total_cost = stats_lock.total_cost;
        
        self.notify_update(&state);
    }

    /// Update session state entirely
    pub fn update_session_state(&self, session_state: SessionState) {
        let mut state = self.state.write().unwrap();
        state.stats = session_state;
        self.notify_update(&state);
    }

    /// Update context state
    pub fn update_context(&self, context_state: ContextState) {
        let mut state = self.state.write().unwrap();
        state.context = context_state;
        self.notify_update(&state);
    }

    /// Update UI preferences
    pub fn update_preferences(&self, preferences: crate::ui::UiPreferences) {
        let mut state = self.state.write().unwrap();
        state.preferences = preferences;
        self.notify_update(&state);
    }

    /// Set command popup state
    pub fn set_command_popup(&self, popup: Option<crate::ui::ui_trait::CommandPopupState>) {
        let mut state = self.state.write().unwrap();
        state.command_popup = popup;
        self.notify_update(&state);
    }

    /// Process an AgentEvent and update state accordingly
    pub fn process_agent_event(&self, event: AgentEvent) {
        match event {
            AgentEvent::CompletionResponse { content, usage, .. } => {
                // Update stats from token usage
                let mut state = self.state.write().unwrap();
                state.stats.total_input_tokens += usage.input_tokens;
                state.stats.total_output_tokens += usage.output_tokens;
                state.stats.total_api_calls += 1;
                
                // Add agent message
                state.chat.add_message(ChatMessage::agent(content));
                
                self.notify_update(&state);
            }
            AgentEvent::ToolCall { tool_name, arguments, .. } => {
                // Add tool call message
                let mut state = self.state.write().unwrap();
                state.chat.add_message(ChatMessage::tool_call(
                    &tool_name,
                    &arguments,
                    "",
                ));
                self.notify_update(&state);
            }
            AgentEvent::ToolResult { tool_name, result, .. } => {
                // Add tool result message
                let mut state = self.state.write().unwrap();
                state.chat.add_message(ChatMessage::tool_result(
                    &tool_name,
                    &result,
                ));
                self.notify_update(&state);
            }
            // Ignore other events
            AgentEvent::CompletionRequest { .. } |
            AgentEvent::SessionStart { .. } |
            AgentEvent::SessionEnd { .. } => {}
        }
    }

    /// Notify subscribers of state update
    fn notify_update(&self, state: &AppState) {
        // Send update to all subscribers
        let _ = self.update_sender.send(state.clone());
    }

    /// Get a clone of the internal state Arc
    ///
    /// This allows UIs to read state without going through the StateManager
    pub fn state_arc(&self) -> Arc<RwLock<AppState>> {
        self.state.clone()
    }

    /// Update the entire state
    ///
    /// This is used when multiple state fields have been modified
    pub fn update_state(&self, new_state: AppState) {
        let mut state = self.state.write().unwrap();
        *state = new_state;
        self.notify_update(&state);
    }
}

impl Default for StateManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_manager() {
        let sm = StateManager::new();
        let state = sm.get_state();
        assert!(state.chat.messages.is_empty());
        assert!(state.todo.items.is_empty());
    }

    #[test]
    fn test_update_chat() {
        let sm = StateManager::new();
        let message = ChatMessage::user("Hello".to_string());
        sm.update_chat(message);
        
        let state = sm.get_state();
        assert_eq!(state.chat.messages.len(), 1);
        assert_eq!(state.chat.messages[0].content, "Hello");
    }

    #[test]
    fn test_update_stats() {
        let sm = StateManager::new();
        let stats = Arc::new(Mutex::new(SessionStats::new()));
        let mut stats_lock = stats.lock().unwrap();
        stats_lock.total_input_tokens = 100;
        stats_lock.total_output_tokens = 50;
        stats_lock.total_api_calls = 5;
        stats_lock.total_cost = 0.123;
        
        sm.update_stats(&stats);
        
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 100);
        assert_eq!(state.stats.total_output_tokens, 50);
        assert_eq!(state.stats.total_api_calls, 5);
        assert!((state.stats.total_cost - 0.123).abs() < f64::EPSILON);
    }
}
