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

use crate::context_manager::{CompactionResult, ContextManager};
use crate::conversation::Conversation;
use crate::hooks::session_hook::SessionStats;
use crate::storage::{ConversationStorage, ConversationSummary};
use crate::tools::todo::{TodoList, TodoStatus};
use crate::ui::app_state::{
    AppState, ChatMessage, ChatState, ContextState, Notification, NotificationKind, SessionState,
    TodoItem, TodoState, WelcomeState,
};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Manages AppState and distributes updates to subscribed Views.
///
/// Owns the ContextManager internally. Compaction is triggered automatically
/// when messages are added — callers never touch ContextManager directly.
pub struct StateManager {
    state: Arc<RwLock<AppState>>,
    /// Todo list (single source of truth for todo state)
    todo_list: Arc<Mutex<TodoList>>,
    /// Persistent subscribers that receive every state update
    subscribers: Arc<RwLock<Vec<mpsc::Sender<AppState>>>>,
    /// Session statistics (tokens, cost, API calls) - single source of truth
    stats: Arc<Mutex<SessionStats>>,

    // ── Context Compaction ───────────────────────────────────────────────────
    /// Context manager for automatic compaction (private, not exposed)
    context_manager: Mutex<Option<ContextManager>>,
    /// System prompt needed for summarization during compaction
    system_prompt: RwLock<String>,
    /// Weak self-reference for spawning async compaction tasks from sync methods
    self_ref: RwLock<Option<Weak<Self>>>,

    // ── Conversation Persistence (Single Source of Truth) ─────────────────────
    /// Storage backend for conversations
    storage: Option<Arc<dyn ConversationStorage>>,
    /// Current conversation being edited
    current_conversation: Arc<Mutex<Option<Conversation>>>,
}

impl StateManager {
    /// Create a new StateManager wrapped in Arc.
    /// The Arc is required so StateManager can spawn async compaction tasks.
    pub fn new_arc() -> Arc<Self> {
        let sm = Arc::new(Self::new_inner(None));
        *sm.self_ref.write().unwrap() = Some(Arc::downgrade(&sm));
        sm
    }

    /// Create a new StateManager with conversation storage, wrapped in Arc.
    pub fn new_arc_with_storage(storage: Arc<dyn ConversationStorage>) -> Arc<Self> {
        let sm = Arc::new(Self::new_inner(Some(storage)));
        *sm.self_ref.write().unwrap() = Some(Arc::downgrade(&sm));
        sm
    }

    /// Create a bare StateManager (no Arc, no auto-compaction).
    /// Use `new_arc()` in production for automatic compaction support.
    pub fn new() -> Self {
        Self::new_inner(None)
    }

    fn new_inner(storage: Option<Arc<dyn ConversationStorage>>) -> Self {
        Self {
            state: Arc::new(RwLock::new(AppState::new())),
            todo_list: Arc::new(Mutex::new(TodoList::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(Mutex::new(SessionStats::new())),
            context_manager: Mutex::new(None),
            system_prompt: RwLock::new(String::new()),
            self_ref: RwLock::new(None),
            storage,
            current_conversation: Arc::new(Mutex::new(None)),
        }
    }

    // ── Context Compaction ──────────────────────────────────────────────────────

    /// Initialize the context manager for automatic compaction.
    /// Must be called once after construction with the agent, config, and system prompt.
    /// After this, compaction triggers automatically when messages are added.
    pub(crate) fn init_context_manager(&self, cm: ContextManager, system_prompt: String) {
        *self.context_manager.lock().unwrap() = Some(cm);
        *self.system_prompt.write().unwrap() = system_prompt;
    }

    /// Check if compaction is needed and trigger it in the background.
    /// Called internally after each message is added. Synchronous check,
    /// spawns an async task if compaction is needed.
    fn maybe_compact(&self) {
        let cm_clone = {
            let guard = self.context_manager.lock().unwrap();
            let cm = match guard.as_ref() {
                Some(cm) => cm,
                None => return,
            };
            let uncompacted = self.uncompacted_message_count();
            if !cm.needs_compaction(uncompacted) {
                return;
            }
            cm.clone()
        };

        let sm = match self.self_ref.read().unwrap().as_ref().and_then(Weak::upgrade) {
            Some(arc) => arc,
            None => return,
        };

        tokio::spawn(async move {
            sm.set_status(Some("Compacting context...".to_string()));
            match sm.run_compaction(&cm_clone).await {
                Some(result) => {
                    sm.push_notification(
                        format!(
                            "Context compacted: {} → {} messages, {} compacted",
                            result.original_count, result.compacted_count, result.num_discarded
                        ),
                        NotificationKind::Info,
                    );
                }
                None => {}
            }
            sm.set_status(None);
        });
    }

    /// Run compaction: produce a plan, apply it, return the result.
    async fn run_compaction(&self, cm: &ContextManager) -> Option<CompactionResult> {
        let messages = self.get_chat_messages();
        match cm.compact(&messages).await {
            Ok(plan) => {
                let result = cm.estimate_compaction(&messages, plan.boundary);
                if result.num_discarded > 0 {
                    self.apply_compaction(&plan);
                    self.persist_current().ok();
                    Some(result)
                } else {
                    None
                }
            }
            Err(e) => {
                tracing::warn!("Context compaction failed: {}", e);
                None
            }
        }
    }

    /// Check if compaction is needed and run it (async).
    /// Used by TestRunner and force_compact. Returns the result if compaction ran.
    pub async fn compact_if_needed(&self) -> Option<CompactionResult> {
        let cm_clone = {
            let guard = self.context_manager.lock().unwrap();
            let cm = guard.as_ref()?;
            let uncompacted = self.uncompacted_message_count();
            if !cm.needs_compaction(uncompacted) {
                return None;
            }
            cm.clone()
        };

        self.run_compaction(&cm_clone).await
    }

    /// Force compaction regardless of threshold. Returns the result.
    pub async fn force_compact(&self) -> Option<CompactionResult> {
        let cm_clone = {
            let guard = self.context_manager.lock().unwrap();
            guard.as_ref()?.clone()
        };

        self.run_compaction(&cm_clone).await
    }

    /// Apply a CompactionPlan: tag old messages as compacted, insert the summary.
    /// Preserves tool calls that are referenced by tool results in the kept region.
    fn apply_compaction(&self, plan: &crate::context_manager::CompactionPlan) {
        use crate::context_manager::find_needed_tool_calls_chat;

        let mut state = self.state.write().unwrap();
        let messages = &mut state.chat.messages;

        // Find tool calls before boundary that are needed by results after boundary
        let needed_tc: std::collections::HashSet<usize> =
            find_needed_tool_calls_chat(messages, plan.boundary)
                .into_iter()
                .collect();

        // Tag messages before boundary as compacted, except needed tool calls
        for i in 0..plan.boundary {
            if !messages[i].compacted && !needed_tc.contains(&i) {
                messages[i].compacted = true;
            }
        }

        // Insert the summary message at the boundary position
        let summary = ChatMessage::summary(plan.summary.clone());
        messages.insert(plan.boundary, summary);

        self.notify_update(&state);
    }

    /// Count of messages that are not compacted (what the LLM would see).
    fn uncompacted_message_count(&self) -> usize {
        let state = self.state.read().unwrap();
        state.chat.messages.iter().filter(|m| !m.compacted).count()
    }

    /// Get a snapshot of the current chat messages.
    fn get_chat_messages(&self) -> Vec<ChatMessage> {
        let state = self.state.read().unwrap();
        state.chat.messages.clone()
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

    /// Add a new todo item (returns result indicating if new or already existed)
    pub fn add_todo(&self, task: String) -> String {
        let was_empty = {
            let list = self.todo_list.lock().unwrap();
            list.list().is_empty()
        };

        let result = {
            let mut list = self.todo_list.lock().unwrap();
            list.add(task)
        };

        // Sync to UI
        self.sync_todo_to_ui(&self.todo_list.lock().unwrap());

        // Auto-show todo panel when first task is added
        if was_empty && result.is_new {
            self.show_todo_panel();
        }

        if result.is_new {
            format!("Added task #{}: {}", result.id, result.task)
        } else {
            format!("Task already exists as #{}: {}", result.id, result.task)
        }
    }

    /// Add multiple todo items
    /// Returns results for all tasks (both new and existing)
    pub fn add_todos(&self, tasks: Vec<String>) -> String {
        if tasks.is_empty() {
            return "No tasks provided.".to_string();
        }

        let was_empty = {
            let list = self.todo_list.lock().unwrap();
            list.list().is_empty()
        };

        let results = {
            let mut list = self.todo_list.lock().unwrap();
            let results = list.add_many(tasks);
            self.sync_todo_to_ui(&list);
            results
        };

        // Separate new and existing tasks
        let new_tasks: Vec<_> = results.iter().filter(|r| r.is_new).collect();
        let existing_tasks: Vec<_> = results.iter().filter(|r| !r.is_new).collect();

        // Auto-show todo panel when first tasks are added to empty list
        if was_empty && !new_tasks.is_empty() {
            self.show_todo_panel();
        }

        // Build response message
        let mut output = String::new();

        if !new_tasks.is_empty() {
            let new_list: Vec<String> = new_tasks
                .iter()
                .map(|r| format!("#{}: {}", r.id, r.task))
                .collect();
            output.push_str(&format!(
                "Added {} task(s): {}\n",
                new_tasks.len(),
                new_list.join(", ")
            ));
        }

        if !existing_tasks.is_empty() {
            let existing_list: Vec<String> = existing_tasks
                .iter()
                .map(|r| format!("#{}: {}", r.id, r.task))
                .collect();
            output.push_str(&format!("Already existed: {}", existing_list.join(", ")));
        }

        output.trim().to_string()
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
    fn update_chat(&self, message: ChatMessage) {
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

    /// Replace all chat messages with the given list.
    ///
    /// Used after context compaction to persist the compacted history back
    /// into StateManager (the single source of truth).
    pub fn replace_chat_messages(&self, messages: Vec<ChatMessage>) {
        let mut state = self.state.write().unwrap();
        state.chat.messages = messages;
        state.chat.auto_scroll = true;
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

    // ── Conversation Management ────────────────────────────────────────────────

    /// Create a new conversation and set it as current
    pub fn create_conversation(&self, name: String, model: String) {
        let conv = Conversation::new(name, model);
        *self.current_conversation.lock().unwrap() = Some(conv);
    }

    /// List all saved conversations
    pub fn list_conversations(&self) -> Option<Vec<ConversationSummary>> {
        self.storage.as_ref().and_then(|s| s.list().ok())
    }

    /// Load a conversation by ID into current
    pub fn load_conversation(&self, id: Uuid) -> anyhow::Result<()> {
        if let Some(ref storage) = self.storage {
            let conv = storage.load(id)?;
            *self.current_conversation.lock().unwrap() = Some(conv);
            self.sync_from_conversation();
        }
        Ok(())
    }

    /// Get the current conversation ID
    pub fn get_current_conversation_id(&self) -> Option<Uuid> {
        self.current_conversation
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.id)
    }

    /// Get the current conversation
    pub fn get_current_conversation(&self) -> Option<Conversation> {
        self.current_conversation.lock().unwrap().clone()
    }

    /// Check if there is a current conversation
    pub fn has_current_conversation(&self) -> bool {
        self.current_conversation.lock().unwrap().is_some()
    }

    /// Clear the current conversation
    pub fn clear_current_conversation(&self) {
        *self.current_conversation.lock().unwrap() = None;
    }

    /// Save the current conversation to storage
    pub fn save_conversation(&self) {
        if let (Some(ref storage), Some(ref mut conv)) = (
            self.storage.as_ref(),
            self.current_conversation.lock().unwrap().as_mut(),
        ) {
            // Sync chat state to conversation before saving
            self.sync_to_conversation();
            // Update timestamp
            conv.updated_at = chrono::Utc::now();
            // Save to storage
            let _ = storage.save(conv);
        }
    }

    /// Delete a conversation by ID
    pub fn delete_conversation(&self, id: Uuid) -> anyhow::Result<()> {
        if let Some(ref storage) = self.storage {
            // If deleting current conversation, clear it
            if self.get_current_conversation_id() == Some(id) {
                self.clear_current_conversation();
                self.clear_chat();
            }
            storage.delete(id)
        } else {
            Err(anyhow::anyhow!("Storage not configured"))
        }
    }

    /// Export a conversation by ID in the specified format (json or markdown)
    pub fn export_conversation(&self, id: Uuid, format: &str) -> anyhow::Result<String> {
        if let Some(ref storage) = self.storage {
            let conv = storage.load(id)?;
            match format {
                "markdown" | "md" => self.export_markdown(&conv),
                "json" => self.export_json(&conv),
                _ => Err(anyhow::anyhow!(
                    "Unknown format '{}'. Use 'json' or 'markdown'.",
                    format
                )),
            }
        } else {
            Err(anyhow::anyhow!("Storage not configured"))
        }
    }

    /// Export conversation as markdown
    fn export_markdown(&self, conv: &Conversation) -> anyhow::Result<String> {
        use crate::conversation::Message as ConvMsg;

        let mut output = format!("# {}\n\n", conv.name);
        output.push_str(&format!("**Model:** {}\n", conv.model));
        output.push_str(&format!(
            "**Created:** {}\n\n",
            conv.created_at.format("%Y-%m-%d %H:%M:%S")
        ));

        for msg in &conv.messages {
            match msg {
                ConvMsg::User { content, .. } => {
                    output.push_str(&format!("## User\n\n{}\n\n", content));
                }
                ConvMsg::Assistant { content, .. } => {
                    output.push_str(&format!("## Assistant\n\n{}\n\n", content));
                }
                ConvMsg::ToolCall {
                    tool_name,
                    arguments,
                    ..
                } => {
                    output.push_str(&format!(
                        "### Tool Call: {}\n\n```json\n{}\n```\n\n",
                        tool_name, arguments
                    ));
                }
                ConvMsg::ToolResult {
                    tool_name, result, ..
                } => {
                    output.push_str(&format!("### Tool Result: {}\n\n{}\n\n", tool_name, result));
                }
            }
        }

        Ok(output)
    }

    /// Export conversation as JSON
    fn export_json(&self, conv: &Conversation) -> anyhow::Result<String> {
        serde_json::to_string_pretty(conv)
            .map_err(|e| anyhow::anyhow!("JSON serialization failed: {}", e))
    }

    /// Rename the current conversation
    pub fn rename_conversation(&self, name: String) -> anyhow::Result<()> {
        if let Some(ref mut conv) = *self.current_conversation.lock().unwrap() {
            conv.name = name;
            Ok(())
        } else {
            Err(anyhow::anyhow!("No current conversation"))
        }
    }

    /// Clear chat history and current conversation
    pub fn clear_history(&self) {
        self.clear_current_conversation();
        self.clear_chat();
    }

    /// Sync chat state from current conversation (on load)
    fn sync_from_conversation(&self) {
        let conv_guard = self.current_conversation.lock().unwrap();
        if let Some(ref conv) = *conv_guard {
            use crate::conversation::Message as ConvMsg;

            let messages: Vec<ChatMessage> = conv
                .messages
                .iter()
                .map(|msg| match msg {
                    ConvMsg::User { content, .. } => ChatMessage::user(content.clone()),
                    ConvMsg::Assistant { content, .. } => ChatMessage::agent(content.clone()),
                    ConvMsg::ToolCall {
                        tool_name,
                        arguments,
                        call_id,
                        ..
                    } => ChatMessage::tool_call(tool_name, arguments, call_id.clone()),
                    ConvMsg::ToolResult {
                        tool_name,
                        arguments,
                        result,
                        call_id,
                        ..
                    } => ChatMessage::tool_result(tool_name, arguments, result, call_id.clone()),
                })
                .collect();

            let mut state = self.state.write().unwrap();
            state.chat.messages = messages;
            drop(state);
            self.notify_update(&self.state.read().unwrap());
        }
    }

    /// Sync chat state to current conversation (before save)
    fn sync_to_conversation(&self) {
        let state = self.state.read().unwrap();
        if let Some(ref mut conv) = *self.current_conversation.lock().unwrap() {
            use crate::conversation::Message as ConvMsg;
            use crate::ui::app_state::MessageRole;

            conv.messages = state
                .chat
                .messages
                .iter()
                .filter_map(|msg| {
                    match msg.role {
                        MessageRole::User => Some(ConvMsg::User {
                            content: msg.content.clone(),
                            timestamp: chrono::Utc::now(),
                        }),
                        MessageRole::Agent => Some(ConvMsg::Assistant {
                            content: msg.content.clone(),
                            timestamp: chrono::Utc::now(),
                        }),
                        MessageRole::ToolCall => {
                            let tool_name = msg.tool_name.clone()?;
                            let arguments = msg.tool_args.clone().unwrap_or_default();
                            Some(ConvMsg::ToolCall {
                                tool_name,
                                arguments,
                                call_id: msg.call_id.clone(),
                                timestamp: chrono::Utc::now(),
                            })
                        }
                        MessageRole::ToolResult => {
                            let tool_name = msg.tool_name.clone()?;
                            let arguments = msg.tool_args.clone().unwrap_or_default();
                            let result = msg.tool_result.clone().unwrap_or_default();
                            Some(ConvMsg::ToolResult {
                                tool_name,
                                arguments,
                                result,
                                call_id: msg.call_id.clone(),
                                timestamp: chrono::Utc::now(),
                            })
                        }
                        MessageRole::Summary | MessageRole::System => None,
                    }
                })
                .collect();
            conv.metadata.message_count = conv.messages.len();
            conv.updated_at = chrono::Utc::now();
        }
    }

    /// Persist the current conversation to storage
    fn persist_current(&self) -> anyhow::Result<()> {
        self.sync_to_conversation();
        let guard = self.current_conversation.lock().unwrap();
        if let Some(ref conv) = *guard {
            if let Some(ref storage) = self.storage {
                storage.save(conv)?;
            }
        }
        Ok(())
    }

    // ── Message Methods with Persistence ───────────────────────────────────────

    /// Add a user message to chat, persist, and trigger compaction if needed.
    pub fn add_user_message(&self, content: String) {
        let msg = ChatMessage::user(content);
        self.update_chat(msg);
        self.persist_current().ok();
        self.maybe_compact();
    }

    /// Add an assistant message to chat, persist, and trigger compaction if needed.
    pub fn add_assistant_message(&self, content: String) {
        let msg = ChatMessage::agent(content);
        self.update_chat(msg);
        self.persist_current().ok();
        self.maybe_compact();
    }

    /// Add a tool call message to chat and persist immediately
    pub fn add_tool_call(&self, tool_name: String, args: String, call_id: Option<String>) {
        let msg = ChatMessage::tool_call(&tool_name, &args, call_id);
        self.update_chat(msg);
        self.persist_current().ok();
    }

    /// Add a tool result message to chat and persist immediately
    pub fn add_tool_result(
        &self,
        tool_name: String,
        args: String,
        result: String,
        call_id: Option<String>,
    ) {
        let msg = ChatMessage::tool_result(&tool_name, &args, &result, call_id);
        self.update_chat(msg);
        self.persist_current().ok();
    }

    // ── History Conversion for Agent ───────────────────────────────────────────

    /// Convert chat messages to rig::Message for agent history.
    /// Produces proper rig message types:
    /// - User → `Message::User` with text content
    /// - Agent → `Message::Assistant` with text content
    /// - ToolCall → `Message::Assistant` with `AssistantContent::ToolCall`
    /// - ToolResult → `Message::User` with `UserContent::ToolResult`
    /// System messages are skipped.
    ///
    /// The trailing user message is excluded from the returned history because
    /// rig's `prompt_with_history(msg, history)` appends `msg` as the current
    /// user turn. Since `add_user_message()` is called before this method in
    /// the production flow, including it here would send the same message to
    /// the model twice.
    pub fn get_agent_history(&self) -> Vec<rig::completion::message::Message> {
        use crate::ui::app_state::MessageRole;
        use rig::completion::message::{
            AssistantContent, Message as RigMessage, Text, ToolCall, ToolFunction, ToolResult,
            ToolResultContent, UserContent,
        };
        use rig::one_or_many::OneOrMany;

        let state = self.state.read().unwrap();

        // If the very last uncompacted message is a User message, exclude it.
        // It will be supplied separately as the prompt argument to prompt_with_history().
        // Only exclude it when it's truly trailing — if there are assistant/tool messages
        // after it, it's part of the conversation history and must be kept.
        let last_uncompacted = state
            .chat
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, msg)| !msg.compacted);
        let skip_last_idx = last_uncompacted
            .filter(|(_, msg)| msg.role == MessageRole::User)
            .map(|(i, _)| i);

        state
            .chat
            .messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| !msg.compacted)
            .filter(|(i, _)| Some(*i) != skip_last_idx)
            .map(|(_, msg)| msg)
            .filter_map(|msg| {
                match msg.role {
                    MessageRole::User => Some(RigMessage::User {
                        content: OneOrMany::one(UserContent::Text(Text {
                            text: msg.content.clone(),
                        })),
                    }),
                    MessageRole::Agent => Some(RigMessage::Assistant {
                        id: None,
                        content: OneOrMany::one(AssistantContent::Text(Text {
                            text: msg.content.clone(),
                        })),
                    }),
                    MessageRole::ToolCall => {
                        let tool_name = msg.tool_name.as_deref()?;
                        let args_str = msg.tool_args.as_deref().unwrap_or("{}");
                        let arguments = serde_json::from_str(args_str)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        let call_id =
                            msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

                        Some(RigMessage::Assistant {
                            id: None,
                            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                                call_id,
                                ToolFunction::new(tool_name.to_string(), arguments),
                            ))),
                        })
                    }
                    MessageRole::ToolResult => {
                        let tool_name = msg.tool_name.as_deref()?;
                        let result_text =
                            msg.tool_result.as_deref().unwrap_or("");
                        let call_id =
                            msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

                        Some(RigMessage::User {
                            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                                id: call_id,
                                call_id: None,
                                content: OneOrMany::one(ToolResultContent::Text(Text {
                                    text: result_text.to_string(),
                                })),
                            })),
                        })
                    }
                    MessageRole::Summary => Some(RigMessage::User {
                        content: OneOrMany::one(UserContent::Text(Text {
                            text: format!("[Conversation summary] {}", msg.content),
                        })),
                    }),
                    MessageRole::System => None,
                }
            })
            .collect()
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

        // Verify the Arc contains the updated data (testing Arc sharing)
        let stats = stats_arc.lock().unwrap();
        assert_eq!(
            stats.total_api_calls, 1,
            "Arc reference should see the shared data"
        );
    }

    /// Test that get_agent_history() includes ToolCall and ToolResult messages
    /// as proper rig message types (AssistantContent::ToolCall and UserContent::ToolResult).
    #[test]
    fn test_get_agent_history_includes_tool_messages() {
        let sm = StateManager::new();

        // Add a realistic conversation with tool calls
        sm.add_user_message("List the files".to_string());
        sm.add_tool_call("bash".to_string(), r#"{"command":"ls"}"#.to_string(), Some("call_1".to_string()));
        sm.add_tool_result("bash".to_string(), r#"{"command":"ls"}"#.to_string(), "file1.txt\nfile2.txt".to_string(), Some("call_1".to_string()));
        sm.add_assistant_message("Here are the files: file1.txt and file2.txt".to_string());

        // StateManager should have 4 messages in its chat state
        let state = sm.get_state();
        assert_eq!(
            state.chat.messages.len(), 4,
            "StateManager should have 4 chat messages (user, tool_call, tool_result, assistant)"
        );

        // get_agent_history() MUST return all 4 messages including tool messages.
        // The compaction algorithm's find_needed_tool_calls() depends on seeing
        // ToolCall messages to preserve tool call/result integrity.
        let history = sm.get_agent_history();
        assert_eq!(
            history.len(), 4,
            "get_agent_history() should return all 4 messages including tool messages. \
             Got {} -- tool messages are being silently dropped.",
            history.len()
        );
    }

    /// Test that get_agent_history() produces proper rig ToolCall messages (not text approximations)
    #[test]
    fn test_get_agent_history_tool_call_is_structured() {
        use rig::completion::message::{AssistantContent, Message as RigMessage};

        let sm = StateManager::new();
        sm.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls -la"}"#.to_string(),
            Some("call_42".to_string()),
        );

        let history = sm.get_agent_history();
        assert_eq!(history.len(), 1);

        match &history[0] {
            RigMessage::Assistant { content, .. } => {
                let first = content.first();
                match first {
                    AssistantContent::ToolCall(tc) => {
                        assert_eq!(tc.function.name, "bash");
                        assert_eq!(tc.id, "call_42");
                        assert_eq!(
                            tc.function.arguments,
                            serde_json::json!({"command": "ls -la"})
                        );
                    }
                    other => panic!("Expected ToolCall, got {:?}", other),
                }
            }
            other => panic!("Expected Assistant message, got {:?}", other),
        }
    }

    /// Test that get_agent_history() produces proper rig ToolResult messages
    #[test]
    fn test_get_agent_history_tool_result_is_structured() {
        use rig::completion::message::{Message as RigMessage, ToolResultContent, UserContent};

        let sm = StateManager::new();
        sm.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_42".to_string()),
        );

        let history = sm.get_agent_history();
        assert_eq!(history.len(), 1);

        match &history[0] {
            RigMessage::User { content } => {
                let first = content.first();
                match first {
                    UserContent::ToolResult(tr) => {
                        assert_eq!(tr.id, "call_42");
                        match tr.content.first() {
                            ToolResultContent::Text(t) => {
                                assert_eq!(t.text, "file1.txt\nfile2.txt");
                            }
                            other => panic!("Expected Text, got {:?}", other),
                        }
                    }
                    other => panic!("Expected ToolResult, got {:?}", other),
                }
            }
            other => panic!("Expected User message, got {:?}", other),
        }
    }

    /// ChatMessage structured fields are correctly populated for tool messages
    #[test]
    fn test_chat_message_structured_fields() {
        let sm = StateManager::new();

        sm.add_user_message("List files".to_string());
        sm.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_1".to_string()),
        );
        sm.add_assistant_message("Here are the files.".to_string());

        let state = sm.get_state();
        assert_eq!(state.chat.messages.len(), 4);

        // User message has no tool fields
        let user_msg = &state.chat.messages[0];
        assert!(user_msg.tool_name.is_none());

        // Tool call preserves structured data
        let tc_msg = &state.chat.messages[1];
        assert_eq!(tc_msg.tool_name.as_deref(), Some("bash"));
        assert_eq!(tc_msg.tool_args.as_deref(), Some(r#"{"command":"ls"}"#));
        assert_eq!(tc_msg.call_id.as_deref(), Some("call_1"));

        // Tool result preserves structured data
        let tr_msg = &state.chat.messages[2];
        assert_eq!(tr_msg.tool_name.as_deref(), Some("bash"));
        assert_eq!(tr_msg.tool_args.as_deref(), Some(r#"{"command":"ls"}"#));
        assert_eq!(tr_msg.tool_result.as_deref(), Some("file1.txt\nfile2.txt"));
        assert_eq!(tr_msg.call_id.as_deref(), Some("call_1"));

        // Assistant message has no tool fields
        let asst_msg = &state.chat.messages[3];
        assert!(asst_msg.tool_name.is_none());
    }

    /// Roundtrip: Conversation JSON -> ChatMessage -> rig Message preserves tool data
    #[test]
    fn test_roundtrip_conversation_json_to_rig_messages() {
        use crate::conversation::{Conversation, Message as ConvMsg};
        use rig::completion::message::{
            AssistantContent, Message as RigMessage, ToolResultContent, UserContent,
        };

        // Simulate loading a conversation from JSON
        let mut conv = Conversation::new("test".to_string(), "test-model".to_string());
        conv.add_user_message("List files".to_string());
        conv.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        conv.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_1".to_string()),
        );
        conv.add_assistant_message("Here are the files.".to_string());

        // Serialize to JSON and back (simulating file persistence)
        let json = serde_json::to_string_pretty(&conv).unwrap();
        let loaded: Conversation = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.messages.len(), 4);

        // Verify tool call survived JSON roundtrip
        match &loaded.messages[1] {
            ConvMsg::ToolCall {
                tool_name,
                arguments,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }

        // Verify tool result survived JSON roundtrip
        match &loaded.messages[2] {
            ConvMsg::ToolResult {
                tool_name,
                arguments,
                result,
                call_id,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
                assert_eq!(result, "file1.txt\nfile2.txt");
                assert_eq!(call_id.as_deref(), Some("call_1"));
            }
            other => panic!("Expected ToolResult, got {:?}", other),
        }

        // Convert to rig messages (the path used by convert_conversation_to_rig_messages)
        let rig_messages = crate::convert_conversation_to_rig_messages(&loaded);
        assert_eq!(rig_messages.len(), 4, "All 4 messages should convert to rig messages");

        // Verify rig tool call is structured
        match &rig_messages[1] {
            RigMessage::Assistant { content, .. } => match content.first() {
                AssistantContent::ToolCall(tc) => {
                    assert_eq!(tc.function.name, "bash");
                    assert_eq!(tc.id, "call_1");
                }
                other => panic!("Expected ToolCall content, got {:?}", other),
            },
            other => panic!("Expected Assistant, got {:?}", other),
        }

        // Verify rig tool result is structured
        match &rig_messages[2] {
            RigMessage::User { content } => match content.first() {
                UserContent::ToolResult(tr) => {
                    assert_eq!(tr.id, "call_1");
                    match tr.content.first() {
                        ToolResultContent::Text(t) => {
                            assert_eq!(t.text, "file1.txt\nfile2.txt");
                        }
                        other => panic!("Expected Text result, got {:?}", other),
                    }
                }
                other => panic!("Expected ToolResult content, got {:?}", other),
            },
            other => panic!("Expected User, got {:?}", other),
        }
    }

    /// Test that replace_chat_messages works correctly for compaction persistence.
    #[test]
    fn test_replace_chat_messages() {
        let sm = StateManager::new();

        // Add some messages
        sm.add_user_message("Hello".to_string());
        sm.add_assistant_message("Hi there".to_string());
        sm.add_user_message("How are you?".to_string());
        sm.add_assistant_message("I'm fine".to_string());
        assert_eq!(sm.get_state().chat.messages.len(), 4);

        // Replace with compacted messages (simulating compaction)
        let compacted = vec![
            ChatMessage::user("[Summary of previous conversation]".to_string()),
            ChatMessage::user("How are you?".to_string()),
            ChatMessage::agent("I'm fine".to_string()),
        ];
        sm.replace_chat_messages(compacted);

        let state = sm.get_state();
        assert_eq!(
            state.chat.messages.len(), 3,
            "Should have 3 messages after replace (summary + 2 recent)"
        );
        assert!(
            state.chat.messages[0].content.contains("Summary"),
            "First message should be the summary"
        );
    }
}
