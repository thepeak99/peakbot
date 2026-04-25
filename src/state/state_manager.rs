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
    AppState, ChatMessage, ChatState, ContextState, SessionState, TodoItem, TodoState,
    WelcomeState,
};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
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

    // ── Rendering Coalescence ─────────────────────────────────────────────────
    /// Monotonic counter bumped on every state mutation.
    ///
    /// Views (e.g. `ReplUi`) cache the revision they last rendered and skip
    /// their render pass when nothing has changed, turning an idle REPL from
    /// `20 draws/sec × O(N)` into a no-op. See `slow-messages.md` §4.4.
    revision: AtomicU64,
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
            revision: AtomicU64::new(0),
        }
    }

    // ── Context Compaction ──────────────────────────────────────────────────────

    /// Initialize the context manager for automatic compaction.
    /// Must be called once after construction with the agent, config, and system prompt.
    /// After this, compaction triggers automatically when messages are added.
    pub(crate) fn init_context_manager(&self, cm: ContextManager, system_prompt: String) {
        // Seed AppState.context with the static parts (window size + policy) so
        // the status bar can render a real percentage immediately, before any
        // API call has reported token usage. `current_usage` stays 0 until the
        // first request lands and `sync_stats_to_ui` refreshes it.
        {
            let mut state = self.state.write().unwrap();
            state.context.window_size = cm.context_window() as u64;
            state.context.compaction_enabled = cm.is_enabled();
            state.context.compaction_threshold = cm.threshold_fraction();
        }

        *self.context_manager.lock().unwrap() = Some(cm);
        *self.system_prompt.write().unwrap() = system_prompt;

        let state = self.state.read().unwrap();
        self.notify_update(&state);
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
                    sm.add_system_message(format!(
                        "Context compacted: {} → {} messages, {} compacted",
                        result.original_count, result.compacted_count, result.num_discarded
                    ));
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
        // Snapshot the live context-token count while no locks on `state` are held.
        // `last_input_tokens()` returns the most recent request's input tokens —
        // that IS the current context size (not cumulative).
        let last_input = stats.last_input_tokens().unwrap_or(0);

        let mut state = self.state.write().unwrap();
        state.stats.total_input_tokens = stats.total_input_tokens;
        state.stats.total_output_tokens = stats.total_output_tokens;
        state.stats.total_api_calls = stats.total_api_calls;
        state.stats.total_cost = stats.total_cost;
        state.context.current_usage = last_input;
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

    /// Clear finished todo items (completed and cancelled)
    pub fn clear_completed_todos(&self) -> String {
        let cleared = {
            let mut list = self.todo_list.lock().unwrap();
            let cleared = list.clear_completed();
            self.sync_todo_to_ui(&list);
            cleared
        };
        format!("Cleared {} finished tasks (completed and cancelled)", cleared)
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
        // Don't broadcast — welcome is set once before the View subscribes.
        // But still bump the revision so the first render after startup
        // sees a non-zero value and doesn't skip itself.
        self.revision.fetch_add(1, Ordering::Release);
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

    /// Set whether the agent is currently running (processing a message).
    ///
    /// Stamps `run_started_at = Some(Instant::now())` when starting, and clears
    /// both the start-time and `status_message` when stopping. The `workin-baby`
    /// TUI indicator keys off these fields — do not split the state.
    pub fn set_running(&self, running: bool) {
        let mut state = self.state.write().unwrap();
        state.is_running = running;
        state.run_started_at = if running {
            Some(std::time::Instant::now())
        } else {
            None
        };
        if !running {
            state.status_message = None;
        }
        self.notify_update(&state);
    }

    /// Check if the agent is currently running
    pub fn is_running(&self) -> bool {
        self.state.read().unwrap().is_running
    }

    /// Signal the UI to quit on its next tick (the `/exit` command path).
    ///
    /// Bypasses the Ctrl+C confirmation dialog — `/exit` is an explicit,
    /// unconditional request. Idempotent: calling twice is harmless.
    /// Notifies subscribers so the View's skip-idle-tick guard wakes up
    /// and observes the flag.
    pub fn request_exit(&self) {
        let mut state = self.state.write().unwrap();
        state.exit_requested = true;
        self.notify_update(&state);
    }

    /// Whether an exit has been requested (Views read this each tick).
    pub fn exit_requested(&self) -> bool {
        self.state.read().unwrap().exit_requested
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
        // Bump the revision before cloning/dispatching so any thread that
        // reads `revision()` between a mutation and the broadcast sees the
        // new value. Every mutation path in this file goes through
        // `notify_update`, so this is the single source of truth.
        self.revision.fetch_add(1, Ordering::Release);

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

    /// Current state revision — monotonically increasing counter bumped on
    /// every mutation. Views compare this against their last-rendered value
    /// to decide whether a redraw is necessary.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
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

    /// Add a user message carrying image attachments.
    ///
    /// Same persistence and compaction behaviour as [`add_user_message`], but
    /// the `ChatMessage` carries `attachments` that downstream code converts
    /// to `rig::UserContent::Image` at the wire boundary.
    pub fn add_user_message_with_attachments(
        &self,
        content: String,
        attachments: Vec<crate::vision::ImageAttachment>,
    ) {
        let msg = ChatMessage::user_with_attachments(content, attachments);
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
                        content: user_content_from_chat_message(msg),
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

    /// Build the `rig::Message` representing the current user turn — the one
    /// that gets passed as the `prompt` argument to
    /// `DynAgent::prompt_with_history`, alongside the history returned by
    /// [`get_agent_history`].
    ///
    /// Uses the last non-compacted user message as the source:
    /// - text only → plain `Message::User` with a single text part
    /// - attachments present → multimodal `Message::User` with
    ///   `OneOrMany::many([Image*, Text])`
    ///
    /// Returns `None` only if there is no user message in the chat history —
    /// which shouldn't happen in the production dispatch flow, where
    /// `add_user_message(_with_attachments)` is called before this.
    pub fn build_current_turn_message(&self) -> Option<rig::completion::message::Message> {
        use crate::ui::app_state::MessageRole;
        use rig::completion::message::Message as RigMessage;

        let state = self.state.read().unwrap();
        let last_user = state
            .chat
            .messages
            .iter()
            .rev()
            .find(|m| !m.compacted && m.role == MessageRole::User)?;

        Some(RigMessage::User {
            content: user_content_from_chat_message(last_user),
        })
    }
}

/// Build a `OneOrMany<UserContent>` from a `ChatMessage` (User role).
///
/// Order is `[Image*, Text]` — matches rig's sample message and provider
/// expectations. A text-only message collapses to `OneOrMany::one(Text)`;
/// otherwise attachments come first, then the caption.
fn user_content_from_chat_message(
    msg: &crate::ui::app_state::ChatMessage,
) -> rig::one_or_many::OneOrMany<rig::completion::message::UserContent> {
    use rig::completion::message::{Text, UserContent};
    use rig::one_or_many::OneOrMany;

    if msg.attachments.is_empty() {
        return OneOrMany::one(UserContent::Text(Text {
            text: msg.content.clone(),
        }));
    }

    let mut parts: Vec<UserContent> = msg
        .attachments
        .iter()
        .map(user_content_from_attachment)
        .collect();
    parts.push(UserContent::Text(Text {
        text: msg.content.clone(),
    }));

    // `OneOrMany::many` errors on empty input; we always have ≥2 parts here.
    OneOrMany::many(parts).expect("attachments present → non-empty parts")
}

/// Adapter at the wire boundary — converts a UI-level `ImageAttachment` into
/// a `rig::UserContent::Image`.
///
/// **Detail defaulting (load-bearing):** rig-core's OpenAI provider rejects
/// base64 images with `detail: None` (`"OpenAI image URI must have image
/// detail"`). URL-shaped images get `unwrap_or_default()` → `Auto` for free
/// inside rig, but base64 does not. We default to `Auto` here for both
/// sources so the contract is uniform regardless of provider or source kind.
/// Explicit user-set details (Low/High) are preserved.
fn user_content_from_attachment(
    att: &crate::vision::ImageAttachment,
) -> rig::completion::message::UserContent {
    use crate::vision::ImageSource;
    use rig::completion::message::{DocumentSourceKind, Image, ImageDetail, UserContent};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let detail = Some(att.detail.clone().unwrap_or(ImageDetail::Auto));

    match &att.source {
        ImageSource::Base64 { bytes, media_type } => UserContent::Image(Image {
            // rig stores Base64 as a string; encode at the boundary so we
            // keep our in-memory representation honest (raw bytes).
            data: DocumentSourceKind::Base64(STANDARD.encode(bytes)),
            media_type: Some(media_type.clone()),
            detail: detail.clone(),
            additional_params: None,
        }),
        ImageSource::Url(url) => UserContent::Image(Image {
            data: DocumentSourceKind::Url(url.clone()),
            media_type: None,
            detail,
            additional_params: None,
        }),
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
    fn test_revision_starts_at_zero_and_bumps_on_mutation() {
        let sm = StateManager::new();
        assert_eq!(sm.revision(), 0, "fresh manager should have revision 0");

        sm.add_user_message("hello".to_string());
        let r1 = sm.revision();
        assert!(r1 > 0, "mutation must bump revision, got {r1}");

        sm.add_assistant_message("world".to_string());
        let r2 = sm.revision();
        assert!(r2 > r1, "second mutation must bump again ({r1} -> {r2})");
    }

    #[test]
    fn test_revision_stable_on_pure_reads() {
        let sm = StateManager::new();
        sm.add_user_message("hi".to_string());
        let before = sm.revision();

        // Pure reads must not bump — otherwise idle-tick skipping is broken.
        let _ = sm.get_state();
        let _ = sm.get_state();
        let _ = sm.revision();

        assert_eq!(sm.revision(), before, "reads must not bump revision");
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

    // ─── workin-baby: working-indicator state transitions ────────────────

    #[test]
    fn set_running_true_stamps_run_started_at() {
        let sm = StateManager::new();
        let before = sm.get_state();
        assert!(before.run_started_at.is_none(), "idle state has no start time");
        assert!(!before.is_running);

        sm.set_running(true);

        let after = sm.get_state();
        assert!(after.is_running, "is_running flips to true");
        assert!(after.run_started_at.is_some(), "run_started_at is stamped");
    }

    #[test]
    fn set_running_false_clears_run_started_at_and_status() {
        let sm = StateManager::new();
        sm.set_running(true);
        sm.set_status(Some("bash".to_string()));

        // Mid-run: both fields populated.
        let mid = sm.get_state();
        assert!(mid.is_running);
        assert!(mid.run_started_at.is_some());
        assert_eq!(mid.status_message.as_deref(), Some("bash"));

        sm.set_running(false);

        let after = sm.get_state();
        assert!(!after.is_running, "is_running flips to false");
        assert!(
            after.run_started_at.is_none(),
            "run_started_at must be cleared on stop"
        );
        assert!(
            after.status_message.is_none(),
            "status_message must be cleared on stop (per workin-baby §5.2)"
        );
    }

    #[test]
    fn set_running_toggle_restamps_run_started_at() {
        let sm = StateManager::new();
        sm.set_running(true);
        let first = sm.get_state().run_started_at.expect("stamped on start");

        // Sleep a hair so the second stamp is observably later than the first.
        std::thread::sleep(std::time::Duration::from_millis(5));

        sm.set_running(false);
        sm.set_running(true);
        let second = sm.get_state().run_started_at.expect("re-stamped on restart");

        assert!(
            second > first,
            "second run start must be strictly after the first"
        );
    }

    // ─── /exit command: StateManager-side signal ─────────────────────────
    //
    // `/exit` can't call `ReplUi::running = false` directly — the command
    // dispatcher runs in the agent loop, not the view. Instead it sets
    // `AppState.exit_requested = true`, which the view observes on its
    // next tick and uses to break its run loop. Same pattern as
    // `is_running`: state lives in the Model, Views react.

    #[test]
    fn exit_requested_defaults_to_false() {
        let sm = StateManager::new();
        assert!(!sm.get_state().exit_requested);
        assert!(!sm.exit_requested());
    }

    #[test]
    fn request_exit_sets_the_flag() {
        let sm = StateManager::new();
        sm.request_exit();
        assert!(sm.get_state().exit_requested);
        assert!(sm.exit_requested());
    }

    #[test]
    fn request_exit_notifies_subscribers() {
        // The REPL's skip-idle-tick guard only redraws on revision bumps
        // or local_dirty. If request_exit didn't notify, the view could
        // stay parked in its idle branch and never observe the flag.
        let sm = StateManager::new();
        let before = sm.revision();
        sm.request_exit();
        assert!(
            sm.revision() > before,
            "request_exit must bump revision so the UI wakes up"
        );
    }

    // ── Context State Sync ──────────────────────────────────────────────────
    // The status bar reads AppState.context.usage_percentage(). That number
    // is computed from current_usage / window_size. If we never populate
    // those fields the indicator is stuck at 0% forever — which is exactly
    // what users saw before these tests existed.

    fn sm_with_context_window(window: usize) -> Arc<StateManager> {
        use crate::config::ContextConfig;
        let sm = StateManager::new_arc();
        let cfg = ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            context_window: Some(window),
            compaction_model: None,
        };
        // No compaction model — we're not exercising compact() here.
        let cm = ContextManager::new(cfg, "mock-model", sm.clone(), None);
        sm.init_context_manager(cm, String::new());
        sm
    }

    #[test]
    fn context_window_populated_after_init_context_manager() {
        let sm = sm_with_context_window(128_000);
        let state = sm.get_state();
        assert_eq!(
            state.context.window_size, 128_000,
            "window_size must be seeded at init so usage_percentage() isn't stuck on 0/0"
        );
        assert!(
            state.context.compaction_enabled,
            "compaction_enabled must flow from ContextConfig into AppState"
        );
        assert!(
            (state.context.compaction_threshold - 0.8).abs() < f64::EPSILON,
            "compaction_threshold must flow from ContextConfig into AppState"
        );
    }

    #[test]
    fn context_current_usage_tracks_last_input_tokens() {
        let sm = sm_with_context_window(200_000);

        // Before any request: usage is 0 but window is non-zero.
        let state = sm.get_state();
        assert_eq!(state.context.current_usage, 0);
        assert_eq!(state.context.window_size, 200_000);
        assert_eq!(state.context.usage_percentage(), 0.0);

        // After a request: current_usage reflects the last input-token count.
        sm.add_request(50_000, 1_000, 0.0);
        let state = sm.get_state();
        assert_eq!(
            state.context.current_usage, 50_000,
            "current_usage must be updated when stats are synced — otherwise the status bar shows 0% forever"
        );
        // 50k / 200k = 25%
        assert!(
            (state.context.usage_percentage() - 25.0).abs() < 0.01,
            "expected ~25% usage, got {}",
            state.context.usage_percentage()
        );

        // A later request overwrites (tokens aren't cumulative — see SessionStats docs).
        sm.add_request(100_000, 2_000, 0.0);
        let state = sm.get_state();
        assert_eq!(state.context.current_usage, 100_000);
    }

    // ── Vision / multimodal history ───────────────────────────────────────

    fn sample_attachment(name: &str) -> crate::vision::ImageAttachment {
        use crate::vision::{ImageAttachment, ImageSource};
        use rig::completion::message::ImageMediaType;
        ImageAttachment {
            display_name: name.to_string(),
            source: ImageSource::Base64 {
                bytes: vec![1, 2, 3, 4],
                media_type: ImageMediaType::PNG,
            },
            detail: None,
        }
    }

    #[test]
    fn get_agent_history_emits_text_only_user_when_no_attachments() {
        use rig::completion::message::{Message as RigMessage, UserContent};

        let sm = StateManager::new();
        sm.add_user_message("first".to_string());
        sm.add_assistant_message("reply".to_string());
        sm.add_user_message("second".to_string());

        // Trailing user is excluded; only the first User + Assistant show up.
        let history = sm.get_agent_history();
        assert_eq!(history.len(), 2);
        match &history[0] {
            RigMessage::User { content } => {
                let parts: Vec<_> = content.iter().collect();
                assert_eq!(parts.len(), 1);
                assert!(matches!(parts[0], UserContent::Text(_)));
            }
            _ => panic!("expected User message at index 0"),
        }
    }

    #[test]
    fn get_agent_history_emits_image_then_text_order_when_attachments_present() {
        use rig::completion::message::{Message as RigMessage, UserContent};

        let sm = StateManager::new();
        sm.add_user_message_with_attachments(
            "what's this?".to_string(),
            vec![sample_attachment("a.png"), sample_attachment("b.png")],
        );
        sm.add_assistant_message("a cat".to_string());
        // Add a trailing text user to push the multimodal message *into* history.
        sm.add_user_message("follow-up".to_string());

        let history = sm.get_agent_history();
        assert_eq!(history.len(), 2, "trailing user must be excluded");
        match &history[0] {
            RigMessage::User { content } => {
                let parts: Vec<_> = content.iter().collect();
                assert_eq!(parts.len(), 3, "expected [Image, Image, Text]");
                assert!(matches!(parts[0], UserContent::Image(_)));
                assert!(matches!(parts[1], UserContent::Image(_)));
                assert!(matches!(parts[2], UserContent::Text(_)));
            }
            other => panic!("expected User message, got {other:?}"),
        }
    }

    #[test]
    fn get_agent_history_still_excludes_trailing_user_even_with_attachments() {
        let sm = StateManager::new();
        sm.add_user_message_with_attachments(
            "hi".to_string(),
            vec![sample_attachment("cat.png")],
        );
        // Trailing user with attachments → excluded (matches existing behaviour).
        let history = sm.get_agent_history();
        assert!(history.is_empty());
    }

    #[test]
    fn build_current_turn_message_returns_multimodal_when_attachments_present() {
        use rig::completion::message::{Message as RigMessage, UserContent};

        let sm = StateManager::new();
        sm.add_user_message_with_attachments(
            "explain".to_string(),
            vec![sample_attachment("x.png")],
        );

        let msg = sm.build_current_turn_message().expect("user msg exists");
        match msg {
            RigMessage::User { content } => {
                let parts: Vec<_> = content.iter().collect();
                assert_eq!(parts.len(), 2);
                assert!(matches!(parts[0], UserContent::Image(_)));
                assert!(matches!(parts[1], UserContent::Text(_)));
            }
            _ => panic!("expected User message"),
        }
    }

    /// Regression: rig-core's OpenAI provider rejects base64 images that don't
    /// carry an `ImageDetail` (`"OpenAI image URI must have image detail"`).
    /// PeakBot constructs attachments with `detail: None`, so the wire-boundary
    /// adapter MUST default to `ImageDetail::Auto` to keep OpenAI happy and
    /// match the behaviour rig already gives URL-shaped images.
    #[test]
    fn user_content_from_attachment_defaults_base64_detail_to_auto() {
        use rig::completion::message::{ImageDetail, UserContent};

        let att = sample_attachment("cat.png");
        assert!(att.detail.is_none(), "fixture must start with no detail");

        match user_content_from_attachment(&att) {
            UserContent::Image(img) => {
                assert_eq!(
                    img.detail,
                    Some(ImageDetail::Auto),
                    "base64 attachments must carry a detail at the wire boundary"
                );
            }
            other => panic!("expected Image content, got {other:?}"),
        }
    }

    /// Same defaulting must apply to URL attachments — keeps the contract
    /// uniform across `ImageSource` variants. (rig's OpenAI provider already
    /// `unwrap_or_default()`s URLs, but we shouldn't rely on that asymmetry.)
    #[test]
    fn user_content_from_attachment_defaults_url_detail_to_auto() {
        use crate::vision::{ImageAttachment, ImageSource};
        use rig::completion::message::{ImageDetail, UserContent};

        let att = ImageAttachment {
            display_name: "https://example.com/x.png".to_string(),
            source: ImageSource::Url("https://example.com/x.png".to_string()),
            detail: None,
        };

        match user_content_from_attachment(&att) {
            UserContent::Image(img) => {
                assert_eq!(img.detail, Some(ImageDetail::Auto));
            }
            other => panic!("expected Image content, got {other:?}"),
        }
    }

    /// An explicitly-set detail must NOT be overwritten by the default.
    #[test]
    fn user_content_from_attachment_preserves_explicit_detail() {
        use crate::vision::{ImageAttachment, ImageSource};
        use rig::completion::message::{ImageDetail, ImageMediaType, UserContent};

        let att = ImageAttachment {
            display_name: "x.png".to_string(),
            source: ImageSource::Base64 {
                bytes: vec![1, 2, 3],
                media_type: ImageMediaType::PNG,
            },
            detail: Some(ImageDetail::High),
        };

        match user_content_from_attachment(&att) {
            UserContent::Image(img) => assert_eq!(img.detail, Some(ImageDetail::High)),
            other => panic!("expected Image content, got {other:?}"),
        }
    }

    #[test]
    fn build_current_turn_message_returns_text_only_when_no_attachments() {
        use rig::completion::message::{Message as RigMessage, UserContent};

        let sm = StateManager::new();
        sm.add_user_message("hello".to_string());

        let msg = sm.build_current_turn_message().expect("user msg exists");
        match msg {
            RigMessage::User { content } => {
                let parts: Vec<_> = content.iter().collect();
                assert_eq!(parts.len(), 1);
                assert!(matches!(parts[0], UserContent::Text(_)));
            }
            _ => panic!("expected User message"),
        }
    }

    #[test]
    fn build_current_turn_message_returns_none_when_no_user_messages() {
        let sm = StateManager::new();
        sm.add_system_message("startup banner".to_string());
        sm.add_assistant_message("hi, how can I help?".to_string());

        assert!(sm.build_current_turn_message().is_none());
    }
}
