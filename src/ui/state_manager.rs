//! State Manager
//!
//! This module provides centralized state management for all UI implementations.
//! It serves as the single source of truth for UI-renderable state.
//!
//! ## MVC Model
//!
//! StateManager is the Model. It holds `AppState` and broadcasts changes to
//! all subscribed Views. Controller writes to it, Views read from it.

use crate::hooks::events::AgentEvent;
use crate::hooks::session_hook::SessionStats;
use crate::TodoList;
use crate::ui::app_state::{
    AppState, ChatMessage, ChatState, ContextState, SessionState, TodoItem, TodoState,
    WelcomeState,
};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

/// Manages AppState and distributes updates to subscribed Views
pub struct StateManager {
    state: Arc<RwLock<AppState>>,
    /// Persistent subscribers that receive every state update
    subscribers: Arc<RwLock<Vec<Sender<AppState>>>>,
}

impl StateManager {
    /// Create a new StateManager
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(AppState::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get current state snapshot
    pub fn get_state(&self) -> AppState {
        self.state.read().unwrap().clone()
    }

    /// Set the welcome banner state (called once at startup by main.rs)
    pub fn set_welcome(&self, welcome: WelcomeState) {
        let mut state = self.state.write().unwrap();
        state.welcome = Some(welcome);
        // Don't broadcast — welcome is set once before the View subscribes
    }

    /// Mark the next broadcast as final (called before final agent response)
    pub fn set_final_broadcast(&self, final_flag: bool) {
        let mut state = self.state.write().unwrap();
        state.is_final = final_flag;
    }

    /// Subscribe to state updates. Returns a channel that receives AppState on every change.
    /// The sender is automatically removed when the receiver is dropped.
    pub fn subscribe(&self) -> Receiver<AppState> {
        let (sender, receiver) = mpsc::channel();
        // Send current state immediately so subscriber is up-to-date
        let current = self.state.read().unwrap().clone();
        let _ = sender.send(current);
        self.subscribers.write().unwrap().push(sender);
        receiver
    }

    /// Update chat messages — called by Controller after agent response
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

    /// Update TODO state — syncs with core TodoList
    pub fn update_todo(&self, todo_list: &TodoList) {
        let items: Vec<TodoItem> = todo_list
            .list()
            .iter()
            .map(TodoItem::from)
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

    /// Update session stats — syncs with CostTracker/SessionStats
    pub fn update_stats(&self, stats: &Arc<Mutex<SessionStats>>) {
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
                let mut state = self.state.write().unwrap();
                state.stats.total_input_tokens += usage.input_tokens;
                state.stats.total_output_tokens += usage.output_tokens;
                state.stats.total_api_calls += 1;
                state.chat.add_message(ChatMessage::agent(content));
                self.notify_update(&state);
            }
            AgentEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                let mut state = self.state.write().unwrap();
                state.chat.add_message(ChatMessage::tool_call(&tool_name, &arguments, ""));
                self.notify_update(&state);
            }
            AgentEvent::ToolResult {
                tool_name, result, ..
            } => {
                let mut state = self.state.write().unwrap();
                state.chat.add_message(ChatMessage::tool_result(&tool_name, &result));
                self.notify_update(&state);
            }
            AgentEvent::CompletionRequest { .. }
            | AgentEvent::SessionStart { .. }
            | AgentEvent::SessionEnd { .. } => {}
        }
    }

    /// Broadcast current state to all subscribers.
    /// Removes dead subscribers (receivers that have been dropped).
    fn notify_update(&self, state: &AppState) {
        let state = state.clone();
        let mut dead = Vec::new();
        let mut subs = self.subscribers.write().unwrap();
        for (i, sender) in subs.iter().enumerate() {
            if sender.send(state.clone()).is_err() {
                dead.push(i);
            }
        }
        // Remove dead subscribers in reverse order
        for i in dead.into_iter().rev() {
            subs.remove(i);
        }
    }

    /// Get a clone of the internal state Arc
    pub fn state_arc(&self) -> Arc<RwLock<AppState>> {
        self.state.clone()
    }

    /// Update the entire state — used when multiple fields have been modified
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
        drop(stats_lock);
        sm.update_stats(&stats);
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 100);
        assert_eq!(state.stats.total_output_tokens, 50);
        assert_eq!(state.stats.total_api_calls, 5);
        assert!((state.stats.total_cost - 0.123).abs() < f64::EPSILON);
    }

    #[test]
    fn test_subscribe() {
        let sm = StateManager::new();
        let receiver = sm.subscribe();
        // Should receive initial state
        assert!(receiver.recv().is_ok());
    }
}
