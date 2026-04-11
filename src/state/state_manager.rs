//! State Manager
//!
//! This module provides centralized state management for the application.
//! It serves as the single source of truth for all state (stats, todos, chat, etc.).
//!
//! ## MVC Model
//!
//! StateManager is the Model. It holds `AppState` and broadcasts changes to
//! all subscribed Views. Controller writes to it, Views read from it.
//!
//! ## Async Stream Subscription
//!
//! The `subscribe()` method returns an async stream (`mpsc::Receiver<AppState>`)
//! that can be used directly with `tokio::select!` or iterated with `while let`.

use crate::hooks::session_hook::SessionStats;
use crate::tools::todo::{TodoList, TodoStatus};
use crate::ui::app_state::{
    AppState, ChatMessage, ChatState, ContextState, Notification, NotificationKind, SessionState,
    TodoItem, TodoState, WelcomeState,
};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

/// Manages AppState and distributes updates to subscribed Views
pub struct StateManager {
    state: Arc<RwLock<AppState>>,
    /// Todo list (single source of truth for todo state)
    todo_list: Arc<Mutex<TodoList>>,
    /// Persistent subscribers that receive every state update
    subscribers: Arc<RwLock<Vec<mpsc::Sender<AppState>>>>,
    /// Session statistics (tokens, cost, API calls) - single source of truth
    stats: Arc<Mutex<SessionStats>>,
}

impl StateManager {
    /// Create a new StateManager
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(AppState::new())),
            todo_list: Arc::new(Mutex::new(TodoList::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(Mutex::new(SessionStats::new())),
        }
    }

    /// Get a clone of the todo list (read-only snapshot).
    /// All mutations must go through StateManager methods.
    pub fn get_todo_list(&self) -> TodoList {
        self.todo_list.lock().unwrap().clone()
    }

    // ── Stats Operations ────────────────────────────────────────────────────────

    /// Add a request's stats to the session
    pub fn add_request(&self, input: u64, output: u64, cost: f64) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.add_request(input, output, cost);
        }
        // Sync to UI
        self.sync_stats_to_ui();
    }

    /// Get a clone of the current stats
    pub fn get_stats(&self) -> SessionStats {
        self.stats.lock().unwrap().clone()
    }

    /// Get the raw stats Arc for consumers that need direct access
    pub fn stats_arc(&self) -> Arc<Mutex<SessionStats>> {
        self.stats.clone()
    }

    /// Reset all statistics
    pub fn reset_stats(&self) {
        self.stats.lock().unwrap().reset();
        self.sync_stats_to_ui();
    }

    /// Sync stats to AppState (internal method)
    fn sync_stats_to_ui(&self) {
        let stats = self.stats.lock().unwrap();
        let mut state = self.state.write().unwrap();
        state.stats.total_input_tokens = stats.total_input_tokens;
        state.stats.total_output_tokens = stats.total_output_tokens;
        state.stats.total_api_calls = stats.total_api_calls;
        state.stats.total_cost = stats.total_cost;
        drop(state);
        self.notify_update(&self.state.read().unwrap());
    }

    // ── Todo Operations ────────────────────────────────────────────────────────

    /// Add a new todo item (fails if duplicate exists)
    /// Returns Ok(String) on success, Err(String) if duplicate
    pub fn add_todo(&self, task: String) -> Result<String, String> {
        let was_empty = {
            let list = self.todo_list.lock().unwrap();
            list.list().is_empty()
        };
        
        let add_result = {
            let mut list = self.todo_list.lock().unwrap();
            list.add(task)
        };
        
        match add_result {
            Ok(item) => {
                self.sync_todo_to_ui(&self.todo_list.lock().unwrap());
                // Auto-show todo panel when first task is added
                if was_empty {
                    self.show_todo_panel();
                }
                Ok(format!("Added task #{}: {}", item.id, item.task))
            }
            Err(msg) => Err(msg),
        }
    }

    /// Add multiple todo items (skips duplicates silently)
    pub fn add_todos(&self, tasks: Vec<String>) -> String {
        if tasks.is_empty() {
            return "No tasks provided.".to_string();
        }
        let was_empty = {
            let list = self.todo_list.lock().unwrap();
            list.list().is_empty()
        };
        let items = {
            let mut list = self.todo_list.lock().unwrap();
            let items = list.add_many(tasks);
            self.sync_todo_to_ui(&list);
            items
        };
        // Auto-show todo panel when first tasks are added to empty list
        if was_empty && !items.is_empty() {
            self.show_todo_panel();
        }
        if items.is_empty() {
            "No new tasks added (all were duplicates or empty).".to_string()
        } else if items.len() == 1 {
            format!("Added task #{}: {}", items[0].id, items[0].task)
        } else {
            let task_list: Vec<String> = items
                .iter()
                .map(|i| format!("#{}: {}", i.id, i.task))
                .collect();
            format!("Added {} tasks: {}", items.len(), task_list.join(", "))
        }
    }

    /// Update todo item status
    pub fn update_todo_status(&self, id: usize, status: TodoStatus) -> String {
        let result = {
            let mut list = self.todo_list.lock().unwrap();
            let result = list.update_status(id, status.clone());
            if result.is_some() {
                self.sync_todo_to_ui(&list);
            }
            result
        };
        match result {
            Some(item) => format!("Updated task #{} to {}", item.id, item.status),
            None => format!("Task #{} not found", id),
        }
    }

    /// Remove a todo item
    pub fn remove_todo(&self, id: usize) -> String {
        let result = {
            let mut list = self.todo_list.lock().unwrap();
            let result = list.remove(id);
            if result.is_some() {
                self.sync_todo_to_ui(&list);
            }
            result
        };
        match result {
            Some(item) => format!("Removed task #{}: {}", item.id, item.task),
            None => format!("Task #{} not found", id),
        }
    }

    /// List all todo items
    pub fn list_todos(&self) -> String {
        let list = self.todo_list.lock().unwrap();
        let tasks = list.list();

        if tasks.is_empty() {
            return "No tasks in the todo list.".to_string();
        }

        let (pending, in_progress, completed, cancelled) = list.count_by_status();

        let mut output = String::new();
        output.push_str("## Todo List\n\n");

        for item in tasks {
            let status_icon = match item.status {
                TodoStatus::Pending => "○",
                TodoStatus::InProgress => "◐",
                TodoStatus::Completed => "●",
                TodoStatus::Cancelled => "✗",
            };
            output.push_str(&format!(
                "{} #{} [{}] {}\n",
                status_icon, item.id, item.status, item.task
            ));
        }

        output.push_str(&format!(
            "\n**Summary:** {} pending, {} in progress, {} completed, {} cancelled",
            pending, in_progress, completed, cancelled
        ));

        output
    }

    /// Clear completed todo items
    pub fn clear_completed_todos(&self) -> String {
        let cleared = {
            let mut list = self.todo_list.lock().unwrap();
            let cleared = list.clear_completed();
            self.sync_todo_to_ui(&list);
            cleared
        };
        format!("Cleared {} completed tasks", cleared)
    }

    /// Sync todo list to UI state
    fn sync_todo_to_ui(&self, list: &TodoList) {
        let items: Vec<TodoItem> = list.list().iter().map(TodoItem::from).collect();

        let mut state = self.state.write().unwrap();
        state.todo.items = items;
        self.notify_update(&state);
    }

    // ── Todo Visibility ──────────────────────────────────────────────────────

    /// Get current todo visibility state
    pub fn is_todo_panel_visible(&self) -> bool {
        self.state.read().unwrap().todo.visible
    }

    /// Show the todo panel
    pub fn show_todo_panel(&self) {
        let mut state = self.state.write().unwrap();
        state.todo.visible = true;
        self.notify_update(&state);
    }

    /// Hide the todo panel
    pub fn hide_todo_panel(&self) {
        let mut state = self.state.write().unwrap();
        state.todo.visible = false;
        self.notify_update(&state);
    }

    /// Toggle todo panel visibility
    pub fn toggle_todo_panel(&self) {
        let mut state = self.state.write().unwrap();
        state.todo.visible = !state.todo.visible;
        self.notify_update(&state);
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

    /// Subscribe to state updates. Returns an async stream (mpsc::Receiver) that
    /// yields AppState on every change. The sender is automatically removed when
    /// the receiver is dropped.
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut state_rx = state_manager.subscribe();
    /// while let Some(state) = state_rx.recv().await {
    ///     // render state
    /// }
    /// ```
    pub fn subscribe(&self) -> mpsc::Receiver<AppState> {
        let (sender, receiver) = mpsc::channel(64);
        // Send current state immediately so subscriber is up-to-date
        let current = self.state.read().unwrap().clone();
        let _ = sender.try_send(current);
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

    /// Set whether the agent is currently running (processing a message)
    pub fn set_running(&self, running: bool) {
        let mut state = self.state.write().unwrap();
        state.is_running = running;
        self.notify_update(&state);
    }

    /// Check if the agent is currently running
    pub fn is_running(&self) -> bool {
        self.state.read().unwrap().is_running
    }

    /// Update chat state entirely
    pub fn update_chat_state(&self, chat_state: ChatState) {
        let mut state = self.state.write().unwrap();
        state.chat = chat_state;
        self.notify_update(&state);
    }

    /// Update TODO state — syncs with core TodoList
    pub fn update_todo(&self, todo_list: &TodoList) {
        let items: Vec<TodoItem> = todo_list.list().iter().map(TodoItem::from).collect();

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

    /// Update session state entirely
    pub fn update_session_state(&self, session_state: SessionState) {
        let mut state = self.state.write().unwrap();
        state.stats = session_state;
        self.notify_update(&state);
    }

    /// Set the model name in stats
    pub fn set_model(&self, model: String) {
        let mut state = self.state.write().unwrap();
        state.stats.model = model;
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

    /// Push a notification to be displayed by UI
    pub fn push_notification(&self, message: String, kind: NotificationKind) {
        let notification = Notification::new(message, kind);
        let mut state = self.state.write().unwrap();
        state.notifications.push(notification);
        self.notify_update(&state);
    }

    /// Clear all notifications
    pub fn clear_notifications(&self) {
        let mut state = self.state.write().unwrap();
        state.notifications.clear();
        self.notify_update(&state);
    }

    /// Set agent status message (e.g., "Compacting...", "Stopped")
    pub fn set_status(&self, message: Option<String>) {
        let mut state = self.state.write().unwrap();
        state.status_message = message;
        self.notify_update(&state);
    }

    /// Add a system message to the chat
    pub fn add_system_message(&self, content: String) {
        let msg = ChatMessage::system(content);
        self.update_chat(msg);
    }

    /// Broadcast current state to all subscribers.
    /// Removes dead subscribers (receivers that have been dropped).
    fn notify_update(&self, state: &AppState) {
        let state = state.clone();
        let mut dead = Vec::new();
        let mut subs = self.subscribers.write().unwrap();
        for (i, sender) in subs.iter().enumerate() {
            // Use try_send to avoid blocking — if the buffer is full, skip this subscriber
            if sender.try_send(state.clone()).is_err() {
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
    fn test_add_request() {
        let sm = StateManager::new();
        sm.add_request(100, 50, 0.123);
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 100);
        assert_eq!(state.stats.total_output_tokens, 50);
        assert_eq!(state.stats.total_api_calls, 1);
        assert!((state.stats.total_cost - 0.123).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stats_accumulation() {
        let sm = StateManager::new();
        sm.add_request(100, 50, 0.10);
        sm.add_request(200, 100, 0.20);
        let state = sm.get_state();
        // Tokens are overwritten per request (not accumulated)
        assert_eq!(state.stats.total_input_tokens, 200); // last value
        assert_eq!(state.stats.total_output_tokens, 100); // last value
        assert_eq!(state.stats.total_api_calls, 2);
        // Cost accumulates across requests
        assert!((state.stats.total_cost - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reset_stats() {
        let sm = StateManager::new();
        sm.add_request(100, 50, 0.10);
        sm.reset_stats();
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 0);
        assert_eq!(state.stats.total_output_tokens, 0);
        assert_eq!(state.stats.total_api_calls, 0);
        assert!((state.stats.total_cost - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_subscribe_initial_state() {
        let sm = StateManager::new();
        let _receiver = sm.subscribe();
        // With async mpsc, initial state is sent via try_send
        let state = sm.get_state();
        assert!(state.chat.messages.is_empty());
    }

    #[test]
    fn test_app_state_sync() {
        let sm = StateManager::new();

        // Add stats via StateManager
        sm.add_request(100, 50, 0.01);

        // Get AppState and verify stats are synced
        let app_state = sm.get_state();

        assert_eq!(app_state.stats.total_input_tokens, 100);
        assert_eq!(app_state.stats.total_output_tokens, 50);
        assert_eq!(app_state.stats.total_api_calls, 1);
    }

    #[test]
    fn test_stats_arc_sharing() {
        let sm = StateManager::new();
        let stats_arc = sm.stats_arc();

        // Add request first
        sm.add_request(100, 50, 0.01);

        // Both references should see the same data via stats arc
        let stats = sm.get_stats();
        assert_eq!(stats.total_api_calls, 1);
    }
}
