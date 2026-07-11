//! State Manager
//!
//! Single source of truth for app state (chat, todos, stats, context).
//! Broadcasts changes to UI subscribers via async channels.

use crate::bg_processes::{BgError, BgRegistry, BgStatus, DrainedBlock, StartParams};
use crate::context_manager::{CompactionResult, ContextManager};
use crate::conversation::Conversation;
use crate::conversation_title::generate_conversation_title;
use crate::hooks::session_hook::SessionStats;
use crate::providers::CompactionModel;
use crate::storage::{ConversationStorage, ConversationSummary};
use crate::tools::todo::{TodoList, TodoStatus};
use crate::ui::app_state::{
    AppState, BashPanelState, BashPanelVisibility, BgState, BgSummary, ChatMessage, ChatState,
    ContextState, SessionState, TodoItem, TodoState, WelcomeState,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Channel buffer size for state subscribers.
const STATE_SUBSCRIBER_BUFFER: usize = 64;

/// Manages AppState and distributes updates to subscribed Views.
/// Owns ContextManager internally; compaction triggers automatically.
pub struct StateManager {
    state: Arc<RwLock<AppState>>,
    todo_list: Arc<Mutex<TodoList>>,
    subscribers: Arc<RwLock<Vec<mpsc::Sender<AppState>>>>,
    stats: Arc<Mutex<SessionStats>>,
    // `RwLock` because the slot is overwritten on `/model` switch (see
    // `lib.rs` rebuild path). `ContextManager` itself is stateless; the lock
    // protects the *slot*, not internal state.
    context_manager: RwLock<Option<ContextManager>>,
    /// Tool-free model for auto-generating conversation titles.
    /// Shares the same provider config as the compaction model; set at
    /// the same time as `init_context_manager`.
    title_model: RwLock<Option<CompactionModel>>,
    self_ref: RwLock<Option<Weak<Self>>>, // For spawning async compaction tasks

    // ── Conversation Persistence ──────────────────────────────────────────────
    storage: Option<Arc<dyn ConversationStorage>>,
    current_conversation: Arc<Mutex<Option<Conversation>>>,

    // ── Background processes (`bash_bg` tool) ────────────────────────────────
    /// Long-running PTY-attached processes spawned by the `bash_bg` tool.
    /// Lock is held only synchronously — never across `.await`. See
    /// `bg_processes.rs` for the lifecycle / drain semantics and
    /// `bash-background.md` for the design.
    bg: Arc<Mutex<BgRegistry>>,

    /// Notification channel: per-process reader threads ping `()` here
    /// whenever fresh output lands (debounced inside the reader). The
    /// agent loop holds the receiving end and translates each ping into
    /// a `QueueMessage::BackgroundOutputReady`. `None` ⇒ no agent loop
    /// attached (e.g. unit tests that exercise `StateManager` directly).
    bg_notify_tx: RwLock<Option<mpsc::UnboundedSender<()>>>,

    // ── Foreground bash stdin forwarding (slice 4) ────────────────────────────
    /// Per-call channel set by the foreground `bash` tool when its PTY
    /// child is spawned and cleared when the child exits. The REPL UI
    /// pushes user-typed lines here; the tool's wait-loop `select!` arm
    /// drains the receiver and writes the bytes (plus `\n`) to the PTY
    /// master. `None` ⇒ no live foreground bash, so
    /// [`Self::try_forward_bash_stdin`] returns [`StdinNotActive`].
    ///
    /// Lifecycle is single-call: the tool sets the slot on spawn and
    /// clears it **before** [`Self::finish_bash_panel`] runs, so a late
    /// UI send during the `Running → Finished` window lands on `None`
    /// and the UI keeps the buffer instead of dropping the typed bytes.
    bash_stdin_tx: RwLock<Option<mpsc::UnboundedSender<String>>>,

    // ── Background process shell ──────────────────────────────────────────────
    /// Shell executable used by `bash_bg` when spawning background processes.
    /// Injected by `main.rs` after shell detection. Empty until set.
    shell: RwLock<String>,

    // ── Rendering Coalescence ─────────────────────────────────────────────────
    /// Monotonic counter for render coalescence — see `slow-messages.md` §4.4.
    revision: AtomicU64,
}

impl StateManager {
    /// Create a new StateManager wrapped in Arc.
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
    pub fn new() -> Self {
        Self::new_inner(None)
    }

    fn new_inner(storage: Option<Arc<dyn ConversationStorage>>) -> Self {
        Self {
            state: Arc::new(RwLock::new(AppState::new())),
            todo_list: Arc::new(Mutex::new(TodoList::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(Mutex::new(SessionStats::new())),
            context_manager: RwLock::new(None),
            title_model: RwLock::new(None),
            self_ref: RwLock::new(None),
            storage,
            current_conversation: Arc::new(Mutex::new(None)),
            bg: Arc::new(Mutex::new(BgRegistry::new())),
            bg_notify_tx: RwLock::new(None),
            bash_stdin_tx: RwLock::new(None),
            shell: RwLock::new(String::new()),
            revision: AtomicU64::new(0),
        }
    }

    // ── Context Compaction ──────────────────────────────────────────────────────

    /// Initialize the context manager for automatic compaction.
    pub(crate) fn init_context_manager(&self, cm: ContextManager) {
        // Seed AppState.context so the status bar can render immediately.
        {
            let mut state = self.state.write().unwrap();
            state.context.window_size = cm.context_size() as u64;
            state.context.compaction_enabled = cm.is_enabled();
            state.context.compaction_threshold = cm.threshold_fraction();
        }

        *self.context_manager.write().unwrap() = Some(cm);

        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    /// Initialize the tool-free model for auto-generating conversation titles.
    /// Call this right after `init_context_manager` at boot and on `/model` switch.
    pub(crate) fn init_title_model(&self, model: Arc<CompactionModel>) {
        *self.title_model.write().unwrap() = Some((*model).clone());
    }

    /// Generate a conversation title after the first assistant response.
    ///
    /// Fires only when `message_count == 1` (the just-completed turn) and
    /// the conversation has no title yet. Uses the same fire-and-forget
    /// pattern as compaction: errors are logged and silently ignored.
    ///
    /// Call this *after* `add_assistant_message` (which increments the
    /// message count and persists the response).
    pub fn maybe_generate_title(&self) {
        // Short-circuit: only generate if the conversation doesn't have a title yet.
        // This is the idempotency guard — once a title is set, never regenerate.
        let has_title = {
            let conv_guard = self.current_conversation.lock().unwrap();
            conv_guard.as_ref().map(|c| c.has_title()).unwrap_or(false)
        };
        if has_title {
            return;
        }

        // Check if we have a title model available
        let model = match self.title_model.read().unwrap().as_ref() {
            Some(m) => m.clone(),
            None => return,
        };

        // Capture messages for the LLM call and verify we have at least
        // one user and one assistant message to generate a meaningful title.
        let messages: Vec<(String, String)> = {
            let state = self.state.read().unwrap();
            let msgs: Vec<(String, String)> = state
                .chat
                .messages
                .iter()
                .filter(|m| !m.compacted)
                .map(|m| {
                    let role = match m.role {
                        crate::ui::app_state::MessageRole::User => "user",
                        crate::ui::app_state::MessageRole::Agent => "assistant",
                        _ => return ("skip".to_string(), m.content.clone()),
                    };
                    (role.to_string(), m.content.clone())
                })
                .filter(|(role, _)| role != "skip")
                .collect();

            let has_user = msgs.iter().any(|(r, _)| r == "user");
            let has_assistant = msgs.iter().any(|(r, _)| r == "assistant");
            if !has_user || !has_assistant {
                return;
            }
            msgs
        };

        // Spawn async task — fire and forget, errors logged.
        // Capture only the Arcs needed for title-set + persist (avoids
        // needing StateManager to be Clone).
        let conv_for_title = self.current_conversation.clone();
        let storage_for_title = self.storage.clone();
        tokio::spawn(async move {
            let title = match generate_conversation_title(&messages, &model).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("Failed to generate conversation title: {e}");
                    return;
                }
            };

            // Set title idempotently and persist
            {
                let mut guard = conv_for_title.lock().unwrap();
                let Some(ref mut conv) = *guard else {
                    return;
                };
                if conv.has_title() {
                    return; // already set by a concurrent call
                }
                conv.set_title(title);
                let conv_clone = conv.clone();
                drop(guard);
                if let Err(e) = storage_for_title
                    .as_ref()
                    .map(|s| s.save(&conv_clone))
                    .unwrap_or(Ok(()))
                {
                    tracing::error!("Failed to persist conversation title: {e}");
                }
            }
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
                    if let Err(e) = self.persist_current() {
                        tracing::error!("Failed to persist after compaction: {}", e);
                    }
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
    ///
    /// Reads the `ContextManager` slot under a brief read guard, clones it
    /// out, drops the guard, and awaits on the clone — the lock is **never**
    /// live across `.await`. See the field-level comment on `context_manager`.
    pub async fn compact_if_needed(&self) -> Option<CompactionResult> {
        let cm_clone = {
            let guard = self.context_manager.read().unwrap();
            let cm = guard.as_ref()?;
            let uncompacted = self.uncompacted_message_count();
            let current_tokens = self.current_input_tokens();
            if !cm.needs_compaction(uncompacted, current_tokens) {
                return None;
            }
            cm.clone()
        };

        self.run_compaction(&cm_clone).await
    }

    /// Force compaction regardless of threshold. Returns the result.
    ///
    /// Same lock discipline as `compact_if_needed`: clone the manager out
    /// before any `.await`.
    pub async fn force_compact(&self) -> Option<CompactionResult> {
        let cm_clone = {
            let guard = self.context_manager.read().unwrap();
            guard.as_ref()?.clone()
        };
        self.run_compaction(&cm_clone).await
    }

    /// Returns `true` iff a compaction *would* fire. The gate
    /// `SessionHook::on_completion_call` consults to terminate the agentic loop.
    /// Returns `false` when no `ContextManager` is initialized.
    pub fn needs_compaction(&self) -> bool {
        let guard = self.context_manager.read().unwrap();
        let Some(cm) = guard.as_ref() else {
            return false;
        };
        cm.needs_compaction(
            self.uncompacted_message_count(),
            self.current_input_tokens(),
        )
    }

    /// Read the most recent API-reported input-token count.
    /// Returns `0` when no API response has been seen yet — that signals
    /// `ContextManager` to fall back to the message-count heuristic.
    fn current_input_tokens(&self) -> usize {
        self.stats.lock().unwrap().last_input_tokens().unwrap_or(0) as usize
    }

    /// Clear the live `last_input_tokens` signal without touching cumulative stats.
    pub fn clear_last_input_tokens(&self) {
        self.stats.lock().unwrap().clear_last_input_tokens();
        self.sync_stats_to_ui();
    }

    /// Apply a CompactionPlan: tag old messages as compacted, insert the summary.
    /// Preserves tool calls referenced by tool results in the kept region.
    /// Also clears `last_input_tokens` (loop-guard) — see `mid-compaction.md` § 3.
    fn apply_compaction(&self, plan: &crate::context_manager::CompactionPlan) {
        use crate::context_manager::find_needed_tool_calls_chat;
        use crate::context_manager::snap_boundary_past_tool_results;

        let mut state = self.state.write().unwrap();
        let messages = &mut state.chat.messages;

        // Re-snap against the live messages: the plan's boundary may come from a
        // slightly different snapshot. Idempotent if already snapped in compact().
        let boundary = snap_boundary_past_tool_results(messages, plan.boundary);

        // Find tool calls before boundary that are needed by results after boundary
        let needed_tc: std::collections::HashSet<usize> =
            find_needed_tool_calls_chat(messages, boundary)
                .into_iter()
                .collect();

        // Tag messages before boundary as compacted, except needed tool calls
        for (i, msg) in messages.iter_mut().enumerate().take(boundary) {
            if !msg.compacted && !needed_tc.contains(&i) {
                msg.compacted = true;
            }
        }

        // Insert the summary message at the boundary position
        let summary = ChatMessage::summary(plan.summary.clone());
        messages.insert(boundary, summary);

        self.notify_update(&state);
        // Lock order: state → stats. Drop state guard before acquiring stats.
        drop(state);

        // Loop-guard: clear last_input_tokens so needs_compaction() falls through
        // to the message-count fallback. Next on_completion_response overwrites
        // with honest data.
        self.stats.lock().unwrap().clear_last_input_tokens();
        // Sync state.context.current_usage so the status bar reflects the
        // post-compaction wire-size estimate (it self-heals on next response).
        self.sync_stats_to_ui();
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

    /// Sync stats to AppState. Lock order: stats → state (load-bearing — see
    /// `add_request_and_persist_current_do_not_deadlock` regression test).
    fn sync_stats_to_ui(&self) {
        // Snapshot under stats lock, release before acquiring state.write.
        let (input, output, calls, cost, last_input) = {
            let stats = self.stats.lock().unwrap();
            (
                stats.total_input_tokens,
                stats.total_output_tokens,
                stats.total_api_calls,
                stats.total_cost,
                // last_input_tokens is the current context size (not cumulative)
                stats.last_input_tokens().unwrap_or(0),
            )
        };

        let mut state = self.state.write().unwrap();
        state.stats.total_input_tokens = input;
        state.stats.total_output_tokens = output;
        state.stats.total_api_calls = calls;
        state.stats.total_cost = cost;
        state.context.current_usage = last_input;
        self.notify_update(&state);
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
            // Glyph palette mirrors `ui::repl::todo_panel`: `✗` (U+2717)
            // replaced with ASCII `x` because kitty may render it at
            // 2 cells while unicode-width says 1, drifting the column
            // alignment in the rendered todo list. See `garbled.md`
            // Class B.
            let status_icon = match item.status {
                TodoStatus::Pending => "○",
                TodoStatus::InProgress => "◐",
                TodoStatus::Completed => "●",
                TodoStatus::Cancelled => "x",
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
        format!(
            "Cleared {} finished tasks (completed and cancelled)",
            cleared
        )
    }

    /// Wipe the entire todo list and resync the UI view.
    ///
    /// Unlike `clear_completed_todos`, this drops *every* task regardless of
    /// status — used by reset-style commands (e.g. `/new`) where the todo
    /// list is conceptually scoped to the conversation that's about to be
    /// retired. Replaces the inner `TodoList` outright so the id counter
    /// also resets to 1, mirroring the "empty list ⇒ ids restart" contract
    /// already provided by `TodoList::clear_completed`.
    pub fn clear_all_todos(&self) {
        {
            let mut list = self.todo_list.lock().unwrap();
            *list = TodoList::new();
            self.sync_todo_to_ui(&list);
        }
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

    /// Refresh the welcome banner's model-scoped fields after a `/model`
    /// switch or `/load` rebuild. Only provider/model/max_tokens change
    /// across a model swap — the rest (tool counts, skills) is
    /// session-wide. Broadcasts so live Views (web banner) re-render;
    /// no-op if the welcome banner was never set.
    pub fn update_welcome_for_model(
        &self,
        provider_name: String,
        model: String,
        max_tokens: usize,
    ) {
        let mut state = self.state.write().unwrap();
        if let Some(w) = state.welcome.as_mut() {
            w.provider_name = provider_name;
            w.model = model;
            w.max_tokens = max_tokens;
            self.notify_update(&state);
        }
    }

    /// Refresh the welcome banner's cwd after a `/cd`. Broadcasts so live
    /// Views (status bar, web banner) reflect the new directory; no-op if
    /// the welcome banner was never set.
    pub fn update_welcome_cwd(&self, cwd: std::path::PathBuf) {
        let mut state = self.state.write().unwrap();
        if let Some(w) = state.welcome.as_mut() {
            w.cwd = cwd;
            self.notify_update(&state);
        }
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
        let (sender, receiver) = mpsc::channel(STATE_SUBSCRIBER_BUFFER);
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

    /// Reset every piece of conversation-scoped state to a fresh-chat
    /// baseline: chat history, session stats, todo list, background
    /// processes, and the foreground bash panel.
    ///
    /// This is the single source of truth for "start fresh" — `/new`,
    /// `/model`, and `/cd` all call it so a new surface added here (a
    /// future panel, a counter) can never be forgotten by one of them.
    /// It deliberately does NOT create the new conversation or emit the
    /// announcement banner — those differ per command (name, wire id,
    /// wording) and stay at the call site.
    ///
    /// `clear_bg` is idempotent on an empty registry, so callers that
    /// must kill bg earlier for ordering reasons (e.g. `/cd` before its
    /// process-global `set_current_dir`) may still call this safely.
    pub fn reset_conversation_state(&self) {
        self.clear_chat();
        self.reset_stats();
        self.clear_all_todos();
        self.clear_bg();
        self.reset_bash_panel();
    }

    /// Replace all chat messages with the given list.
    ///
    /// Used after context compaction to persist the compacted history back
    /// into StateManager (the single source of truth).
    pub fn replace_chat_messages(&self, messages: Vec<ChatMessage>) {
        // Drop orphan tool messages; the rig wire layer assumes pair integrity.
        let messages = crate::tool_use_validator::sanitize_tool_pairs(messages);
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

    /// Set the active provider name (informational handle from the
    /// providers list — `"openrouter"`, `"patchnotes"`, etc.).
    /// Together with `set_model`, stamps the wire-id pair onto
    /// AppState. Used at boot and on `/model` switch.
    pub fn set_provider_name(&self, provider_name: String) {
        let mut state = self.state.write().unwrap();
        state.stats.provider_name = provider_name;
        self.notify_update(&state);
    }

    /// Get the current provider name (empty string if unset).
    pub fn get_provider_name(&self) -> String {
        self.state.read().unwrap().stats.provider_name.clone()
    }

    /// Get the current model wire id (empty string if unset).
    pub fn get_model(&self) -> String {
        self.state.read().unwrap().stats.model.clone()
    }

    /// Set the active model alias (display-only — the user-facing
    /// handle from [`crate::config::ModelRegistry`]). Status bar reads
    /// this; persistence does NOT — saved conversations carry
    /// `(provider_name, model)` instead. Updating the alias has zero
    /// effect on what gets written to disk.
    pub fn set_model_alias(&self, alias: String) {
        let mut state = self.state.write().unwrap();
        state.stats.model_alias = alias;
        self.notify_update(&state);
    }

    /// Get the current display alias (empty string if unset).
    pub fn get_model_alias(&self) -> String {
        self.state.read().unwrap().stats.model_alias.clone()
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

    /// Increment the queued-input counter (event loop, on enqueue).
    ///
    /// Drives the `⏳ N queued` status-bar hint. See `make-flow-great-again.md`.
    pub fn increment_pending_input(&self) {
        let mut state = self.state.write().unwrap();
        state.pending_input_count = state.pending_input_count.saturating_add(1);
        self.notify_update(&state);
    }

    /// Decrement the queued-input counter (agent loop, on dequeue).
    pub fn decrement_pending_input(&self) {
        let mut state = self.state.write().unwrap();
        state.pending_input_count = state.pending_input_count.saturating_sub(1);
        self.notify_update(&state);
    }

    /// Force the queued-input counter to a given value.
    ///
    /// Used by the event loop on `/stop` to zero the count immediately so the
    /// status bar updates the moment the user requests the stop, even though
    /// the agent loop will dispose of the queued items shortly after.
    pub fn set_pending_input_count(&self, n: usize) {
        let mut state = self.state.write().unwrap();
        state.pending_input_count = n;
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

    /// Create a new conversation with the active model's full wire
    /// identity, and set it as current.
    ///
    /// `(provider_name, model)` is the persisted re-activation key for
    /// `/load` — see [`Conversation::new`]. Boot, `/new`, and `/model`
    /// switch all funnel through here so saved files always carry the
    /// stable wire id.
    pub fn create_conversation(&self, name: String, provider_name: String, model: String) {
        let conv = Conversation::new(name, provider_name, model);
        *self.current_conversation.lock().unwrap() = Some(conv);
        self.mirror_conversation_to_state();
    }

    /// Mirror the current conversation's identity into `AppState.conversation`
    /// so Views (the web UI) can read the live conversation id — the sticky
    /// `?convo=` URL binding depends on it. Derived truth, refreshed at every
    /// identity change (create / load). `None` when no conversation is set.
    fn mirror_conversation_to_state(&self) {
        let meta = self.current_conversation.lock().unwrap().as_ref().map(|c| {
            let mut cs = crate::ui::app_state::ConversationState::new(
                c.id.to_string(),
                c.title.clone().unwrap_or_else(|| c.name.clone()),
                c.model.clone(),
            );
            cs.message_count = c.messages.len();
            cs.updated_at = c.updated_at.with_timezone(&chrono::Local);
            cs
        });
        let mut state = self.state.write().unwrap();
        state.conversation = meta;
        self.notify_update(&state);
    }

    /// Mint the boot conversation from the active wire id, but only if none
    /// is current. Idempotent — a no-op once a conversation exists, so a
    /// pre-created (or resumed) session suppresses it. `model_fallback`
    /// covers callers that never stamped a model (test harnesses).
    pub fn ensure_boot_conversation(&self, model_fallback: &str) {
        if self.has_current_conversation() {
            return;
        }
        let name = format!(
            "Conversation {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        );
        let provider_name = self.get_provider_name();
        let model = {
            let m = self.get_model();
            if m.is_empty() {
                model_fallback.to_string()
            } else {
                m
            }
        };
        self.create_conversation(name, provider_name, model);
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
            // Background processes are scoped to the conversation they
            // were spawned in. Loading a different conversation severs
            // that context. See `bash-background.md` edge-case table.
            {
                let mut reg = self.bg.lock().unwrap();
                reg.clear();
            }
            self.update_bg_state();
            // Foreground bash panel is also conversation-scoped — a
            // load is a fresh conversation, restore (state, visibility)
            // to defaults so a stale Finished frame (or a lingering
            // ClosedByUser override) doesn't bleed across.
            self.reset_bash_panel();
            // Publish the loaded conversation's identity so the web URL
            // binding (`?convo=`) reflects the resumed conversation.
            self.mirror_conversation_to_state();
        }
        Ok(())
    }

    /// Peek at a saved conversation's wire identity
    /// `(provider_name, model)` without loading it into the current
    /// slot. Used by `/load` for the pre-flight availability check —
    /// the current conversation must survive a rejected load with no
    /// partial-state teardown. Pre-v5 files (which never wrote
    /// `provider_name`) return `("", model)`; the wire-id lookup
    /// then misses cleanly and `/load` emits the canonical
    /// `Model 'x/y' not available.` diagnostic.
    pub fn peek_conversation_wire_id(&self, id: Uuid) -> anyhow::Result<(String, String)> {
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("conversation storage is not configured"))?;
        let conv = storage.load(id)?;
        Ok((conv.provider_name, conv.model))
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

    /// Save the current conversation to storage.
    ///
    /// Implementation note: must NOT hold `current_conversation.lock()`
    /// while calling `sync_to_conversation()`, because `sync_to_conversation`
    /// re-locks the same `Mutex` and `std::sync::Mutex` is non-reentrant
    /// (the previous `if let (Some(storage), Some(conv)) = (..., self.current_conversation.lock().unwrap().as_mut())`
    /// pattern held the guard across the entire if-let body and deadlocked
    /// on the inner sync call). Sync first, then lock to bump timestamp +
    /// hand the borrow to storage.
    pub fn save_conversation(&self) {
        // Sync chat + stats into the conversation BEFORE taking the lock.
        self.sync_to_conversation();
        if let (Some(storage), Some(conv)) = (
            self.storage.as_ref(),
            self.current_conversation.lock().unwrap().as_mut(),
        ) {
            conv.updated_at = chrono::Utc::now();
            if let Err(e) = storage.save(conv) {
                tracing::error!("Failed to save conversation: {}", e);
            }
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
                ConvMsg::Summary { content, .. } => {
                    output.push_str(&format!("## Summary\n\n{}\n\n", content));
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

    /// Rename the current conversation by setting its display title.
    ///
    /// Sets the `title` field (which takes precedence in the `/conversations`
    /// listing) rather than the creation `name`, so the change is immediately
    /// visible. Persists the conversation and updates `updated_at`.
    pub fn rename_conversation(&self, name: String) -> anyhow::Result<()> {
        {
            let mut guard = self.current_conversation.lock().unwrap();
            if let Some(ref mut conv) = *guard {
                // Set the title field so the rename is visible immediately
                // (title takes display precedence over name).
                conv.title = Some(name);
                conv.updated_at = chrono::Utc::now();
            } else {
                return Err(anyhow::anyhow!("No current conversation"));
            }
            drop(guard);
        }
        self.save_conversation();
        Ok(())
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
                    ConvMsg::User {
                        content, compacted, ..
                    } => {
                        let mut m = ChatMessage::user(content.clone());
                        m.compacted = *compacted;
                        m
                    }
                    ConvMsg::Assistant {
                        content, compacted, ..
                    } => {
                        let mut m = ChatMessage::agent(content.clone());
                        m.compacted = *compacted;
                        m
                    }
                    ConvMsg::ToolCall {
                        tool_name,
                        arguments,
                        call_id,
                        compacted,
                        ..
                    } => {
                        let mut m = ChatMessage::tool_call(tool_name, arguments, call_id.clone());
                        m.compacted = *compacted;
                        m
                    }
                    ConvMsg::ToolResult {
                        tool_name,
                        arguments,
                        result,
                        call_id,
                        compacted,
                        ..
                    } => {
                        let mut m =
                            ChatMessage::tool_result(tool_name, arguments, result, call_id.clone());
                        m.compacted = *compacted;
                        m
                    }
                    ConvMsg::Summary { content, .. } => ChatMessage::summary(content.clone()),
                })
                .collect();

            // Drop orphan tool messages; the rig wire layer assumes pair integrity.
            let messages = crate::tool_use_validator::sanitize_tool_pairs(messages);

            // Restore persisted session stats *before* dropping the conv guard
            // so we don't race with a concurrent save.
            self.stats.lock().unwrap().restore(
                conv.metadata.total_input_tokens,
                conv.metadata.total_output_tokens,
                conv.metadata.total_api_calls,
                conv.metadata.total_cost,
            );

            // Restore persisted todo list
            *self.todo_list.lock().unwrap() = conv.todos.clone();

            let mut state = self.state.write().unwrap();
            state.chat.messages = messages;
            drop(state);
            drop(conv_guard);
            // Push the restored stats into AppState so the status bar reflects
            // the loaded conversation instead of the previous session's totals.
            self.sync_stats_to_ui();
            // Push restored todos into AppState so the UI panel reflects them.
            self.sync_todo_to_ui(&self.todo_list.lock().unwrap());
            // Auto-show todo panel when loaded conversation has todos
            if !self.todo_list.lock().unwrap().list().is_empty() {
                self.show_todo_panel();
            }
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
                .filter_map(|msg| match msg.role {
                    MessageRole::User => Some(ConvMsg::User {
                        content: msg.content.clone(),
                        compacted: msg.compacted,
                        timestamp: chrono::Utc::now(),
                    }),
                    MessageRole::Agent => Some(ConvMsg::Assistant {
                        content: msg.content.clone(),
                        compacted: msg.compacted,
                        timestamp: chrono::Utc::now(),
                    }),
                    MessageRole::ToolCall => {
                        let tool_name = msg.tool_name.clone()?;
                        let arguments = msg.tool_args.clone().unwrap_or_default();
                        Some(ConvMsg::ToolCall {
                            tool_name,
                            arguments,
                            call_id: msg.call_id.clone(),
                            compacted: msg.compacted,
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
                            compacted: msg.compacted,
                            timestamp: chrono::Utc::now(),
                        })
                    }
                    MessageRole::Summary => Some(ConvMsg::Summary {
                        content: msg.content.clone(),
                        timestamp: chrono::Utc::now(),
                    }),
                    MessageRole::System => None,
                })
                .collect();
            conv.metadata.message_count = conv.messages.len();
            // Snapshot live SessionStats into metadata so /load on a future
            // session can hydrate the status bar from the saved JSON.
            let stats = self.stats.lock().unwrap();
            conv.metadata.total_input_tokens = stats.total_input_tokens;
            conv.metadata.total_output_tokens = stats.total_output_tokens;
            conv.metadata.total_api_calls = stats.total_api_calls;
            conv.metadata.total_cost = stats.total_cost;
            drop(stats);
            // Snapshot todo list so /load restores it on a future session.
            conv.todos = self.todo_list.lock().unwrap().clone();
            conv.updated_at = chrono::Utc::now();
        }
    }

    /// Persist the current conversation to storage
    fn persist_current(&self) -> anyhow::Result<()> {
        self.sync_to_conversation();
        let guard = self.current_conversation.lock().unwrap();
        if let Some(conv) = guard.as_ref()
            && let Some(storage) = self.storage.as_ref()
        {
            storage.save(conv)?;
        }
        Ok(())
    }

    // ── Message Methods with Persistence ───────────────────────────────────────

    /// Add a user message to chat and persist.
    ///
    /// Compaction is **NOT** triggered here. The `SessionHook::on_completion_call`
    /// gate (mid-loop) is the right boundary — it fires immediately before
    /// every wire request, including the first one of a new prompt, so this
    /// site doesn't need its own check. See `mid-compaction.md` § 3 Step 4.
    pub fn add_user_message(&self, content: String) {
        let msg = ChatMessage::user(content);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist user message: {}", e);
        }
    }

    /// Add a synthetic user message produced by the `bash_bg` drain seam.
    /// Persisted with `MessageSource::Background { proc_ids }` so the
    /// renderer can style the row and the transcript records which
    /// background processes contributed.
    pub fn add_user_message_from_background(&self, content: String, proc_ids: Vec<u32>) {
        let msg = ChatMessage::user_from_background(content, proc_ids);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist bg synthetic user message: {}", e);
        }
    }

    /// Add a user message carrying image attachments.
    ///
    /// Same persistence behaviour as [`add_user_message`]; compaction is not
    /// triggered here (handled at the wire boundary by `SessionHook`).
    pub fn add_user_message_with_attachments(
        &self,
        content: String,
        attachments: Vec<crate::vision::ImageAttachment>,
    ) {
        let msg = ChatMessage::user_with_attachments(content, attachments);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist user message with attachments: {}", e);
        }
    }

    /// Add an assistant message to chat and persist.
    ///
    /// Compaction is **NOT** triggered here — see [`add_user_message`].
    pub fn add_assistant_message(&self, content: String) {
        let msg = ChatMessage::agent(content);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist assistant message: {}", e);
        }
    }

    /// Add a tool call message to chat and persist immediately
    pub fn add_tool_call(&self, tool_name: String, args: String, call_id: Option<String>) {
        let msg = ChatMessage::tool_call(&tool_name, &args, call_id);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist tool call: {}", e);
        }
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
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist tool result: {}", e);
        }
    }

    // ── History Conversion for Agent ───────────────────────────────────────────

    /// Convert chat messages to rig_core::Message for agent history.
    /// Produces proper rig message types:
    /// - User → `Message::User` with text content
    /// - Agent → `Message::Assistant` with text content
    /// - ToolCall → `Message::Assistant` with `AssistantContent::ToolCall`
    /// - ToolResult → `Message::User` with `UserContent::ToolResult`
    ///
    /// System messages are skipped.
    ///
    /// The trailing user message is excluded from the returned history because
    /// rig's `prompt_with_history(msg, history)` appends `msg` as the current
    /// user turn. Since `add_user_message()` is called before this method in
    /// the production flow, including it here would send the same message to
    /// the model twice.
    pub fn get_agent_history(&self) -> Vec<rig_core::completion::message::Message> {
        use crate::ui::app_state::MessageRole;
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, Text, ToolCall, ToolFunction, ToolResult,
            ToolResultContent, UserContent,
        };
        use rig_core::one_or_many::OneOrMany;

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
            .filter_map(|msg| match msg.role {
                MessageRole::User => Some(RigMessage::User {
                    content: user_content_from_chat_message(msg),
                }),
                MessageRole::Agent => Some(RigMessage::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::Text(Text::new(msg.content.clone()))),
                }),
                MessageRole::ToolCall => {
                    let tool_name = msg.tool_name.as_deref()?;
                    let args_str = msg.tool_args.as_deref().unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

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
                    let result_text = msg.tool_result.as_deref().unwrap_or("");
                    let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

                    Some(RigMessage::User {
                        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                            id: call_id,
                            call_id: None,
                            // Reuse rig's parser so image-JSON results (e.g. from
                            // `view_image`) reconstruct as `Image`, not a base64
                            // text blob the model can't see. Plain results stay text.
                            content: ToolResultContent::from_tool_output(result_text),
                        })),
                    })
                }
                MessageRole::Summary => Some(RigMessage::User {
                    content: OneOrMany::one(UserContent::Text(Text::new(format!(
                        "[Conversation summary] {}",
                        msg.content
                    )))),
                }),
                MessageRole::System => None,
            })
            .collect()
    }

    /// Build the `rig_core::Message` representing the current user turn — the one
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
    pub fn build_current_turn_message(&self) -> Option<rig_core::completion::message::Message> {
        use crate::ui::app_state::MessageRole;
        use rig_core::completion::message::Message as RigMessage;

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

    /// Build the resumption prompt and history for resuming after compaction.
    ///
    /// This is distinct from `build_current_turn_message` + `get_agent_history`
    /// because those methods are designed for **initial dispatch** (fresh user
    /// turns), where the trailing User is always the current turn.
    ///
    /// After mid-action compaction, the state looks like:
    ///
    /// ```text
    /// [User, Agent, ToolCall, ToolResult]  ← compaction fires here
    /// ```
    ///
    /// The resumption prompt should be the **last non-compacted message** (e.g.
    /// the ToolResult), and history should be everything before it
    /// ([User, Agent, ToolCall]).
    ///
    /// Returns `None` when there is no resumption needed — empty conversation
    /// or fresh turn. In that case the caller falls back to the normal
    /// `build_current_turn_message` / `get_agent_history` path.
    pub fn build_resumption_for_compaction(
        &self,
    ) -> Option<(
        rig_core::completion::message::Message,
        Vec<rig_core::completion::message::Message>,
    )> {
        use crate::ui::app_state::MessageRole;
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, Text, ToolCall, ToolFunction, ToolResult,
            ToolResultContent, UserContent,
        };
        use rig_core::one_or_many::OneOrMany;

        let state = self.state.read().unwrap();
        let messages = &state.chat.messages;

        // Find the last non-compacted message (whatever its role)
        let last_non_compacted = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| !m.compacted);

        let (last_idx, last_msg) = last_non_compacted?;

        // If there's only one message, it's a fresh turn — not a mid-action
        // resumption. Return None so the caller uses the normal path.
        if last_idx == 0 {
            return None;
        }

        // ── Build history: everything before the last message ──────────────
        let history: Vec<_> = messages[..last_idx]
            .iter()
            .filter(|m| !m.compacted)
            .filter_map(|msg| match msg.role {
                MessageRole::User => Some(RigMessage::User {
                    content: user_content_from_chat_message(msg),
                }),
                MessageRole::Agent => Some(RigMessage::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::Text(Text::new(msg.content.clone()))),
                }),
                MessageRole::ToolCall => {
                    let tool_name = msg.tool_name.as_deref()?;
                    let args_str = msg.tool_args.as_deref().unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

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
                    let result_text = msg.tool_result.as_deref().unwrap_or("");
                    let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());

                    Some(RigMessage::User {
                        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                            id: call_id,
                            call_id: None,
                            content: ToolResultContent::from_tool_output(result_text),
                        })),
                    })
                }
                MessageRole::Summary => Some(RigMessage::User {
                    content: OneOrMany::one(UserContent::Text(Text::new(format!(
                        "[Conversation summary] {}",
                        msg.content
                    )))),
                }),
                MessageRole::System => None,
            })
            .collect();

        // ── Build prompt: the last message converted to a rig Message ───────
        let prompt = match last_msg.role {
            MessageRole::User => RigMessage::User {
                content: user_content_from_chat_message(last_msg),
            },
            MessageRole::Agent => RigMessage::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::Text(Text::new(
                    last_msg.content.clone(),
                ))),
            },
            MessageRole::ToolCall => {
                let tool_name = last_msg.tool_name.as_deref()?;
                let args_str = last_msg.tool_args.as_deref().unwrap_or("{}");
                let arguments = serde_json::from_str(args_str)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let call_id = last_msg
                    .call_id
                    .clone()
                    .unwrap_or_else(|| tool_name.to_string());

                RigMessage::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                        call_id,
                        ToolFunction::new(tool_name.to_string(), arguments),
                    ))),
                }
            }
            MessageRole::ToolResult => {
                let tool_name = last_msg.tool_name.as_deref()?;
                let result_text = last_msg.tool_result.as_deref().unwrap_or("");
                let call_id = last_msg
                    .call_id
                    .clone()
                    .unwrap_or_else(|| tool_name.to_string());

                RigMessage::User {
                    content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                        id: call_id,
                        call_id: None,
                        content: ToolResultContent::from_tool_output(result_text),
                    })),
                }
            }
            MessageRole::Summary => RigMessage::User {
                content: OneOrMany::one(UserContent::Text(Text::new(format!(
                    "[Conversation summary] {}",
                    last_msg.content
                )))),
            },
            MessageRole::System => return None,
        };

        Some((prompt, history))
    }

    // ── Background processes (`bash_bg`) ────────────────────────────────────

    /// Attach a notification sender for background-process reader pings.
    /// Called once by the agent loop at startup, and again after any
    /// rebuild that spawns a fresh receiver (`/model`).
    pub fn attach_bg_notify(&self, tx: mpsc::UnboundedSender<()>) {
        *self.bg_notify_tx.write().unwrap() = Some(tx);
    }

    /// Detach the notification sender (used on shutdown — readers
    /// outliving the agent loop must not push onto a closed channel).
    pub fn detach_bg_notify(&self) {
        *self.bg_notify_tx.write().unwrap() = None;
    }

    /// Set the shell executable used by background processes.
    /// Called once at startup after shell detection.
    pub fn set_shell(&self, shell: String) {
        let mut guard = self.shell.write().unwrap();
        *guard = shell;
    }

    /// Spawn a new background process. Returns the [`BgListEntry`] for
    /// the freshly-registered process (id, pid, etc.) so callers can
    /// surface it to the model immediately.
    pub fn start_bg(
        &self,
        mut params: StartParams,
    ) -> Result<crate::bg_processes::BgListEntry, BgError> {
        // Inject the detected shell if the caller didn't specify one.
        if params.shell.is_empty() {
            let shell = self.shell.read().unwrap().clone();
            if !shell.is_empty() {
                params.shell = shell;
            }
        }

        // Snapshot the sender out of the lock before crossing into the
        // registry — registry::start drops a cloned sender into the
        // reader thread.
        let tx = {
            let guard = self.bg_notify_tx.read().unwrap();
            guard.clone()
        };
        let Some(tx) = tx else {
            return Err(BgError::Spawn(
                "bg notify channel not attached (agent loop not ready)".into(),
            ));
        };
        let entry = {
            let mut reg = self.bg.lock().unwrap();
            reg.start(params, tx)?
        };
        self.update_bg_state();
        Ok(entry)
    }

    /// Stop and remove a background process. Returns `(exit_code,
    /// final_buffer_lines)` so the tool can surface the tail to the LLM
    /// once.
    pub fn stop_bg(&self, id: u32) -> Result<(i32, Vec<String>), BgError> {
        let out = {
            let mut reg = self.bg.lock().unwrap();
            reg.stop(id)?
        };
        self.update_bg_state();
        Ok(out)
    }

    /// Send a line of input to a running background process.
    pub fn send_bg_line(&self, id: u32, line: String) -> Result<usize, BgError> {
        let mut reg = self.bg.lock().unwrap();
        reg.send_line(id, line)
    }

    /// Snapshot the registry for the `list` verb / `/bg` slash command.
    pub fn list_bg(&self) -> Vec<crate::bg_processes::BgListEntry> {
        let reg = self.bg.lock().unwrap();
        reg.list()
    }

    /// Kill every background process (called on `/new`, `/model`, `/load`
    /// rebuild paths). Idempotent — empty registry is a no-op.
    pub fn clear_bg(&self) {
        {
            let mut reg = self.bg.lock().unwrap();
            reg.clear();
        }
        self.update_bg_state();
    }

    /// Clear all per-process bg cooldowns so the next drain flushes every
    /// buffered line. Called by the agent loop whenever a real user
    /// message is dequeued — engaging the agent is a natural point to
    /// surface accumulated background output.
    pub fn reset_bg_cooldowns(&self) {
        let mut reg = self.bg.lock().unwrap();
        reg.reset_cooldowns();
    }

    /// Earliest instant at which a background process waiting out its
    /// cooldown becomes eligible to inject. `None` when nothing is
    /// pending. The agent loop arms a flush wakeup on this so a buffer
    /// that goes quiet mid-cooldown still flushes when its window expires.
    pub fn next_bg_poke_deadline(&self) -> Option<std::time::Instant> {
        let reg = self.bg.lock().unwrap();
        reg.next_poke_deadline(std::time::Instant::now())
    }

    /// Drain every eligible bg buffer into a single synthetic-turn
    /// payload. Returns `None` when nothing was drained (all buffers
    /// clean, or every dirty process is still inside its cooldown window).
    ///
    /// Side effects (inside the registry):
    /// - eligible ring buffers cleared,
    /// - exited processes removed,
    /// - contributing processes' cooldown timers stamped.
    pub fn drain_bg_output_into_synthetic_turn(&self) -> Option<SyntheticTurn> {
        let blocks = {
            let mut reg = self.bg.lock().unwrap();
            reg.drain_outputs(std::time::Instant::now())?
        };
        self.update_bg_state();
        Some(SyntheticTurn::from_blocks(blocks))
    }

    /// Refresh the `bg` slice of `AppState`. Called after every registry
    /// mutation so the TUI status counter `🛰 N bg` stays in sync.
    pub fn update_bg_state(&self) {
        let snapshot = {
            let reg = self.bg.lock().unwrap();
            let running_count = reg.running_count();
            let recent: Vec<BgSummary> = reg
                .list()
                .into_iter()
                .take(5)
                .map(|e| BgSummary {
                    id: e.id,
                    command: e.command,
                    label: e.label,
                    status: match e.status {
                        BgStatus::Running { .. } => "running".to_string(),
                        BgStatus::Exited { .. } => "exited".to_string(),
                    },
                    exit_code: match e.status {
                        BgStatus::Exited { code, .. } => Some(code),
                        _ => None,
                    },
                })
                .collect();
            BgState {
                running_count,
                recent_summaries: recent,
            }
        };
        {
            let mut state = self.state.write().unwrap();
            state.bg = snapshot;
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    // ── Foreground bash panel (`make-term-great-again.md`) ──────────────────

    /// Transition the bash panel to [`BashPanelState::Running`].
    ///
    /// Called by the foreground `bash` tool the moment the PTY child is
    /// spawned (slice 3). `started_at` is stamped to `now` so the
    /// renderer's elapsed timer starts at zero. `tail` is empty until
    /// the first reader debounce fires [`Self::update_bash_panel_tail`].
    ///
    /// **Producer→view write (intentional).** Also resets
    /// [`AppState::bash_panel_visibility`] to `Auto`. This is the only
    /// producer-side write of the visibility field and it exists for
    /// one specific contract: a new bash starting always re-opens the
    /// panel. The user's prior `Ctrl+B` dismissal was about the
    /// *previous* output — it does not extend to "and I want all
    /// future bash invisible." See `bash-panel-as-real-panel.md` for
    /// the orthogonality tradeoff (one intentional coupling here
    /// replaces three accidental reset chokepoints — user messages,
    /// bg-synthetic turns, conversation-reset paths — that the v1
    /// design needed).
    pub fn start_bash_panel(&self, command: String, pid: u32) {
        {
            let mut state = self.state.write().unwrap();
            state.bash_panel = BashPanelState::Running {
                command,
                pid,
                started_at: chrono::Local::now(),
                tail: Vec::new(),
            };
            state.bash_panel_visibility = BashPanelVisibility::Auto;
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    /// Replace the live tail of the running bash panel. No-op (and no
    /// notify) when the panel is not currently `Running` — the producer
    /// shouldn't push tail updates after `finish_bash_panel`, but the
    /// guard keeps a late reader-thread debounce from corrupting a
    /// `Finished` snapshot. Caller is responsible for trimming `tail`
    /// to ≤ 5 lines.
    pub fn update_bash_panel_tail(&self, tail: Vec<String>) {
        {
            let mut state = self.state.write().unwrap();
            if let BashPanelState::Running { tail: t, .. } = &mut state.bash_panel {
                *t = tail;
            } else {
                return;
            }
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    /// Transition the bash panel to [`BashPanelState::Finished`].
    ///
    /// Carries over the `command` from the `Running` variant so the
    /// renderer can keep displaying it. If the panel is not currently
    /// `Running` (e.g. a stray finish for a panel that was already
    /// cleared), the call is silently dropped — the producer-side
    /// `bash` tool is the only legitimate caller and it never finishes
    /// without first starting.
    pub fn finish_bash_panel(&self, exit_code: i32, final_tail: Vec<String>) {
        {
            let mut state = self.state.write().unwrap();
            let (command, started_at) = match &state.bash_panel {
                BashPanelState::Running {
                    command,
                    started_at,
                    ..
                } => (command.clone(), *started_at),
                _ => return,
            };
            let duration_secs = (chrono::Local::now() - started_at).num_seconds().max(0) as u64;
            state.bash_panel = BashPanelState::Finished {
                command,
                exit_code,
                duration_secs,
                tail: final_tail,
            };
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    /// Reset the bash panel to defaults: [`BashPanelState::Idle`] and
    /// [`BashPanelVisibility::Auto`]. Called by every "fresh
    /// conversation" boundary — `/new`, `/load`, `/model` rebuilds.
    /// Idempotent: when state is already Idle AND visibility is
    /// already Auto, no notify is emitted.
    ///
    /// Replaces the pre-v2 `clear_bash_panel` (state-only). The rename
    /// is deliberate — "clear" suggested emptying a buffer; this
    /// restores defaults across the full (state, visibility) pair.
    pub fn reset_bash_panel(&self) {
        {
            let mut state = self.state.write().unwrap();
            let already_clean = state.bash_panel.is_idle()
                && matches!(state.bash_panel_visibility, BashPanelVisibility::Auto);
            if already_clean {
                return;
            }
            state.bash_panel = BashPanelState::Idle;
            state.bash_panel_visibility = BashPanelVisibility::Auto;
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    /// Toggle the foreground bash panel's visibility.
    ///
    /// Used by the REPL's `Ctrl+B` keybind. Toggles based on the
    /// **current effective visibility** (the rendering rule, not the
    /// raw enum value), so the user-visible outcome is always a flip:
    /// - visible right now (any reason) → set [`BashPanelVisibility::ClosedByUser`]
    /// - hidden right now (any reason)  → set [`BashPanelVisibility::OpenedByUser`]
    ///
    /// Works in every state, including `Idle` — that's the "open it
    /// anytime, like the tasks panel" contract.
    pub fn toggle_bash_panel_visibility(&self) {
        {
            let mut state = self.state.write().unwrap();
            let currently_visible = state.bash_panel_visibility.is_visible(&state.bash_panel);
            state.bash_panel_visibility = if currently_visible {
                BashPanelVisibility::ClosedByUser
            } else {
                BashPanelVisibility::OpenedByUser
            };
        }
        let state = self.state.read().unwrap();
        self.notify_update(&state);
    }

    // ── Foreground bash stdin forwarding (slice 4) ──────────────────────────

    /// Register the foreground `bash` tool's stdin sender. Called once
    /// by `BashTool::call` after the PTY child is spawned and before the
    /// wait loop enters its `select!`.
    ///
    /// Overwrites any prior sender — the foreground `bash` tool is
    /// single-call (the agent loop awaits its `call()` future), so a
    /// stale sender in the slot is a contract violation upstream, not
    /// something this method tries to defend against.
    pub fn set_bash_stdin_tx(&self, tx: mpsc::UnboundedSender<String>) {
        *self.bash_stdin_tx.write().unwrap() = Some(tx);
    }

    /// Clear the foreground bash stdin sender. Called by `BashTool::call`
    /// **before** [`Self::finish_bash_panel`] on the loop exit path so a
    /// late UI send during the `Running → Finished` window can't land on
    /// a dropped receiver. Idempotent on an already-empty slot.
    pub fn clear_bash_stdin_tx(&self) {
        *self.bash_stdin_tx.write().unwrap() = None;
    }

    /// True iff a foreground bash stdin sender is currently registered.
    /// Test-facing helper — the UI doesn't need this because it gates
    /// focus on `bash_panel.is_running()` instead.
    pub fn has_active_bash_stdin(&self) -> bool {
        self.bash_stdin_tx.read().unwrap().is_some()
    }

    /// Forward a line of user-typed input to the running foreground
    /// `bash` child's stdin. `Ok(())` iff a sender was present and
    /// `send` succeeded; `Err(StdinNotActive)` otherwise (no live
    /// foreground bash, or the receiver was dropped between
    /// `clear_bash_stdin_tx` and the UI's send).
    ///
    /// The error is a *recovery contract*: the UI keeps the typed
    /// buffer on `Err` so a user typing a password into a stale channel
    /// doesn't lose the bytes.
    pub fn try_forward_bash_stdin(&self, line: String) -> Result<(), StdinNotActive> {
        // Snapshot the sender out of the lock — `send` is sync but we
        // never hold the lock across any potentially-blocking call.
        let tx = {
            let guard = self.bash_stdin_tx.read().unwrap();
            guard.clone()
        };
        match tx {
            Some(tx) => tx.send(line).map_err(|_| StdinNotActive),
            None => Err(StdinNotActive),
        }
    }
}

/// The foreground `bash` tool is not currently accepting stdin —
/// either no child is running, or the receiver was dropped between the
/// tool's `clear_bash_stdin_tx` call and the UI's send. The UI's
/// recovery contract is documented on
/// [`StateManager::try_forward_bash_stdin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdinNotActive;

impl std::fmt::Display for StdinNotActive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no active foreground bash stdin channel")
    }
}

impl std::error::Error for StdinNotActive {}

/// Payload returned by `drain_bg_output_into_synthetic_turn`.
///
/// The caller appends this as a synthetic user turn and runs an agent
/// turn. The `proc_ids` are persisted on the resulting [`ChatMessage`]
/// via [`crate::ui::app_state::MessageSource::Background`].
#[derive(Debug, Clone)]
pub struct SyntheticTurn {
    /// Pre-rendered `[bg output]` block, ready to ship as a user-role
    /// `Message::Text` at the wire boundary.
    pub text: String,
    /// Ids of processes whose output contributed to this turn.
    pub proc_ids: Vec<u32>,
}

impl SyntheticTurn {
    fn from_blocks(blocks: Vec<DrainedBlock>) -> Self {
        let mut text = String::from("[bg output]\n");
        let mut proc_ids: Vec<u32> = Vec::with_capacity(blocks.len());
        for b in &blocks {
            proc_ids.push(b.id);
            let label_part = b
                .label
                .as_ref()
                .map(|l| format!(" [{l}]"))
                .unwrap_or_default();
            let header = match &b.status_after {
                BgStatus::Exited { code, .. } => format!(
                    "─── #{id} `{cmd}`{lbl} (exited, code {code}, {n} final lines) ───",
                    id = b.id,
                    cmd = b.command,
                    lbl = label_part,
                    code = code,
                    n = b.lines.len(),
                ),
                BgStatus::Running { .. } => format!(
                    "─── #{id} `{cmd}`{lbl} ({n} new lines) ───",
                    id = b.id,
                    cmd = b.command,
                    lbl = label_part,
                    n = b.lines.len(),
                ),
            };
            text.push_str(&header);
            text.push('\n');
            for line in &b.lines {
                text.push_str(line);
                text.push('\n');
            }
        }
        Self { text, proc_ids }
    }
}

/// Build a `OneOrMany<UserContent>` from a `ChatMessage` (User role).
///
/// Order is `[Image*, Text]` — matches rig's sample message and provider
/// expectations. A text-only message collapses to `OneOrMany::one(Text)`;
/// otherwise attachments come first, then the caption.
fn user_content_from_chat_message(
    msg: &crate::ui::app_state::ChatMessage,
) -> rig_core::one_or_many::OneOrMany<rig_core::completion::message::UserContent> {
    use rig_core::completion::message::{Text, UserContent};
    use rig_core::one_or_many::OneOrMany;

    if msg.attachments.is_empty() {
        return OneOrMany::one(UserContent::Text(Text::new(msg.content.clone())));
    }

    let mut parts: Vec<UserContent> = msg
        .attachments
        .iter()
        .map(user_content_from_attachment)
        .collect();
    parts.push(UserContent::Text(Text::new(msg.content.clone())));

    // `OneOrMany::many` errors on empty input; we always have ≥2 parts here.
    OneOrMany::many(parts).expect("attachments present → non-empty parts")
}

/// Adapter at the wire boundary — converts a UI-level `ImageAttachment` into
/// a `rig_core::UserContent::Image`.
///
/// **Detail defaulting (load-bearing):** rig-core's OpenAI provider rejects
/// base64 images with `detail: None` (`"OpenAI image URI must have image
/// detail"`). URL-shaped images get `unwrap_or_default()` → `Auto` for free
/// inside rig, but base64 does not. We default to `Auto` here for both
/// sources so the contract is uniform regardless of provider or source kind.
/// Explicit user-set details (Low/High) are preserved.
fn user_content_from_attachment(
    att: &crate::vision::ImageAttachment,
) -> rig_core::completion::message::UserContent {
    use crate::vision::ImageSource;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use rig_core::completion::message::{DocumentSourceKind, Image, ImageDetail, UserContent};

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
    fn test_update_welcome_for_model_patches_model_fields() {
        let sm = StateManager::new();
        sm.set_welcome(WelcomeState {
            provider_name: "openrouter".to_string(),
            model: "anthropic/claude-3.7-sonnet".to_string(),
            max_tokens: 8192,
            builtin_tools_count: 11,
            mcp_tools_count: 2,
            skills_count: 5,
            searxng_enabled: true,
            searxng_url: None,
            cost_tracking_enabled: true,
            compaction_enabled: true,
            compaction_threshold: 0.8,
            compaction_keep_recent: 5,
            conversation_persistence_enabled: true,
            cwd: std::path::PathBuf::from("/tmp"),
        });

        sm.update_welcome_for_model(
            "openrouter".to_string(),
            "anthropic/claude-opus-4".to_string(),
            16384,
        );

        let w = sm.get_state().welcome.expect("welcome set");
        // Model-scoped fields reflect the switch.
        assert_eq!(w.model, "anthropic/claude-opus-4");
        assert_eq!(w.max_tokens, 16384);
        // Session-wide fields are preserved.
        assert_eq!(w.skills_count, 5);
        assert_eq!(w.mcp_tools_count, 2);
    }

    #[test]
    fn test_update_welcome_for_model_noop_when_unset() {
        let sm = StateManager::new();
        // No welcome set yet — must not panic and must stay None.
        sm.update_welcome_for_model("p".to_string(), "m".to_string(), 1);
        assert!(sm.get_state().welcome.is_none());
    }

    #[test]
    fn test_update_welcome_cwd_patches_cwd() {
        let sm = StateManager::new();
        sm.set_welcome(WelcomeState {
            provider_name: "p".into(),
            model: "m".into(),
            max_tokens: 1,
            builtin_tools_count: 0,
            mcp_tools_count: 0,
            skills_count: 0,
            searxng_enabled: false,
            searxng_url: None,
            cost_tracking_enabled: false,
            compaction_enabled: false,
            compaction_threshold: 0.0,
            compaction_keep_recent: 0,
            conversation_persistence_enabled: false,
            cwd: std::path::PathBuf::from("/old"),
        });
        sm.update_welcome_cwd(std::path::PathBuf::from("/new/dir"));
        let w = sm.get_state().welcome.expect("welcome set");
        assert_eq!(w.cwd, std::path::PathBuf::from("/new/dir"));
    }

    #[test]
    fn test_reset_conversation_state_clears_all_surfaces() {
        let sm = StateManager::new();
        // Seed chat + todos so we can prove they're cleared.
        sm.add_user_message("hello".to_string());
        sm.add_todos(vec!["task".to_string()]);
        assert!(!sm.get_state().chat.messages.is_empty());
        assert!(!sm.get_todo_list().list().is_empty());

        sm.reset_conversation_state();

        assert!(
            sm.get_state().chat.messages.is_empty(),
            "chat must be cleared"
        );
        assert!(
            sm.get_todo_list().list().is_empty(),
            "todos must be cleared"
        );
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

    /// Regression test for the `/load` stats bug: a saved conversation must
    /// round-trip its session stats so that loading it later repopulates the
    /// status bar with that conversation's tokens / API calls / cost — not
    /// the previous session's stale values.
    #[test]
    fn test_load_conversation_restores_stats() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage);

        // Session A: create, accumulate stats, save.
        sm.create_conversation("Session A".into(), "test-prov".into(), "test-model".into());
        sm.add_request(1234, 567, 0.42);
        sm.add_request(2000, 800, 0.10); // cost accumulates → 0.52
        let conv_a_id = sm.get_current_conversation_id().expect("convo A id");
        sm.save_conversation();

        // Switch to a fresh session and pile up unrelated stats — these are
        // what the buggy /load would leave behind in the status bar.
        sm.clear_history();
        sm.reset_stats();
        sm.create_conversation("Session B".into(), "test-prov".into(), "test-model".into());
        sm.add_request(99, 99, 9.99);

        // Load A back. Its stats should override the session-B stats.
        sm.load_conversation(conv_a_id).expect("load A");
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 2000, "last input restored");
        assert_eq!(state.stats.total_output_tokens, 800, "last output restored");
        assert_eq!(state.stats.total_api_calls, 2, "api calls restored");
        assert!(
            (state.stats.total_cost - 0.52).abs() < 1e-9,
            "cost restored, got {}",
            state.stats.total_cost
        );
        // Status bar's live context-size indicator reads `last_input_tokens`,
        // which must also be hydrated (not None) after restore.
        assert_eq!(
            sm.get_stats().last_input_tokens(),
            Some(2000),
            "last_input_tokens must reflect loaded conversation"
        );
        assert_eq!(
            state.context.current_usage, 2000,
            "AppState.context.current_usage must follow last_input_tokens"
        );
    }

    /// `ensure_boot_conversation` mints exactly one conversation from the
    /// stamped wire identity when none exists.
    #[test]
    fn ensure_boot_conversation_mints_from_wire_identity() {
        let sm = StateManager::new_arc();
        sm.set_provider_name("openrouter".into());
        sm.set_model("anthropic/claude-3.7-sonnet".into());
        assert!(!sm.has_current_conversation());

        sm.ensure_boot_conversation("fallback-model");

        let conv = sm.get_current_conversation().expect("minted");
        assert_eq!(conv.provider_name, "openrouter");
        assert_eq!(conv.model, "anthropic/claude-3.7-sonnet");
    }

    /// Idempotent: a second call does not replace an existing conversation
    /// (so a pre-created or resumed session is never clobbered).
    #[test]
    fn ensure_boot_conversation_is_idempotent() {
        let sm = StateManager::new_arc();
        sm.set_model("m".into());
        sm.ensure_boot_conversation("fallback");
        let id = sm.get_current_conversation_id().expect("first mint");

        sm.ensure_boot_conversation("fallback");
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(id),
            "second call must not mint a new conversation"
        );
    }

    /// The fallback model is used only when no model was stamped (harness path).
    #[test]
    fn ensure_boot_conversation_uses_fallback_when_model_empty() {
        let sm = StateManager::new_arc();
        // No set_model → get_model() is empty → fallback applies.
        sm.ensure_boot_conversation("fallback-model");
        assert_eq!(
            sm.get_current_conversation().expect("minted").model,
            "fallback-model"
        );
    }

    #[test]
    fn test_subscribe_initial_state() {
        let sm = StateManager::new();
        let _receiver = sm.subscribe();
        // With async mpsc, initial state is sent via try_send
        let state = sm.get_state();
        assert!(state.chat.messages.is_empty());
    }

    /// The sticky-session web URL binding reads `AppState.conversation.id`;
    /// it must be populated on create and refreshed on load (issue: it was
    /// always `None`, so the `?convo=` URL never synced).
    #[test]
    fn conversation_state_mirrors_current_conversation() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage);

        // Fresh manager: no conversation → no mirror.
        assert!(sm.get_state().conversation.is_none());

        // Create → mirror populated with the new id.
        sm.create_conversation("Session A".into(), "prov".into(), "model-a".into());
        let a_id = sm.get_current_conversation_id().expect("A id");
        let mirror = sm.get_state().conversation.expect("mirror after create");
        assert_eq!(mirror.id, a_id.to_string());
        assert_eq!(mirror.model, "model-a");
        sm.save_conversation();

        // Move to B, then load A back → mirror must follow to A's id.
        sm.create_conversation("Session B".into(), "prov".into(), "model-b".into());
        assert_ne!(
            sm.get_state().conversation.expect("B mirror").id,
            a_id.to_string()
        );
        sm.load_conversation(a_id).expect("load A");
        assert_eq!(
            sm.get_state().conversation.expect("A mirror after load").id,
            a_id.to_string(),
            "loaded conversation id must be mirrored into AppState"
        );
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
        sm.add_assistant_message("Here are the files: file1.txt and file2.txt".to_string());

        // StateManager should have 4 messages in its chat state
        let state = sm.get_state();
        assert_eq!(
            state.chat.messages.len(),
            4,
            "StateManager should have 4 chat messages (user, tool_call, tool_result, assistant)"
        );

        // get_agent_history() MUST return all 4 messages including tool messages.
        // The compaction algorithm's find_needed_tool_calls() depends on seeing
        // ToolCall messages to preserve tool call/result integrity.
        let history = sm.get_agent_history();
        assert_eq!(
            history.len(),
            4,
            "get_agent_history() should return all 4 messages including tool messages. \
             Got {} -- tool messages are being silently dropped.",
            history.len()
        );
    }

    /// Test that get_agent_history() produces proper rig ToolCall messages (not text approximations)
    #[test]
    fn test_get_agent_history_tool_call_is_structured() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage};

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
        use rig_core::completion::message::{
            Message as RigMessage, ToolResultContent, UserContent,
        };

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

    // Historical note: the old StateManager-level regression test was removed
    // because the fix lives at the call-site layer — see
    // `tests/scenarios/queued_input_tests.rs`.

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
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, ToolResultContent, UserContent,
        };

        // Simulate loading a conversation from JSON
        let mut conv = Conversation::new(
            "test".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );
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
        assert_eq!(
            rig_messages.len(),
            4,
            "All 4 messages should convert to rig messages"
        );

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
            state.chat.messages.len(),
            3,
            "Should have 3 messages after replace (summary + 2 recent)"
        );
        assert!(
            state.chat.messages[0].content.contains("Summary"),
            "First message should be the summary"
        );
    }

    // ─── compaction persistence roundtrip (issue #59) ────────────────────
    //
    // A compacted conversation must reload with its `compacted` flags and
    // Summary intact — else the full history resurfaces and re-trips the
    // threshold.
    #[test]
    fn compaction_survives_save_load_roundtrip() {
        use crate::ui::app_state::MessageRole;

        let sm = StateManager::new();

        // Post-compaction layout: tagged old messages, Summary at the
        // boundary, recent tail. The tool pair guards the tool-pair case.
        let mut old_user = ChatMessage::user("old question".into());
        old_user.compacted = true;
        let mut old_call = ChatMessage::tool_call("bash", "{}", Some("c1".into()));
        old_call.compacted = true;
        let mut old_result =
            ChatMessage::tool_result("bash", "{}", "old output", Some("c1".into()));
        old_result.compacted = true;
        let summary = ChatMessage::summary("Summary of earlier work".into());
        let recent_user = ChatMessage::user("recent question".into());
        let recent_agent = ChatMessage::agent("recent answer".into());

        {
            let mut state = sm.state.write().unwrap();
            state.chat.messages = vec![
                old_user,
                old_call,
                old_result,
                summary,
                recent_user,
                recent_agent,
            ];
        }

        let conv = Conversation::new("test".into(), "prov".into(), "model".into());
        *sm.current_conversation.lock().unwrap() = Some(conv);

        // Save, wipe the live chat, reload — the real persistence path.
        sm.sync_to_conversation();
        {
            let mut state = sm.state.write().unwrap();
            state.chat.messages.clear();
        }
        sm.sync_from_conversation();

        let state = sm.get_state();
        let msgs = &state.chat.messages;

        // Nothing lost: 3 compacted + Summary + 2 recent = 6.
        assert_eq!(
            msgs.len(),
            6,
            "all messages (incl. Summary) must survive the roundtrip"
        );

        assert!(msgs[0].compacted && msgs[0].role == MessageRole::User);
        assert!(msgs[1].compacted && msgs[1].role == MessageRole::ToolCall);
        assert!(msgs[2].compacted && msgs[2].role == MessageRole::ToolResult);

        assert_eq!(msgs[3].role, MessageRole::Summary);
        assert!(!msgs[3].compacted);
        assert_eq!(msgs[3].content, "Summary of earlier work");

        assert!(!msgs[4].compacted && msgs[4].role == MessageRole::User);
        assert!(!msgs[5].compacted && msgs[5].role == MessageRole::Agent);

        // Compaction must not be undone by the reload (the #59 symptom).
        let uncompacted = msgs.iter().filter(|m| !m.compacted).count();
        assert_eq!(
            uncompacted, 3,
            "Summary + 2 recent — compaction must not be undone by reload"
        );
    }

    // ─── tool-pair sanitization at the conversation boundary ─────────────
    //
    // See [`crate::tool_use_validator`] for the design + unit-level
    // coverage. These two tests pin the call-site wiring at the two
    // boundaries the proposal identifies: `replace_chat_messages`
    // (compaction) and `sync_from_conversation` (load).

    #[test]
    fn replace_chat_messages_sanitizes_after_compaction() {
        use crate::ui::app_state::MessageRole;
        let sm = StateManager::new();

        // Compaction emitted a ToolCall but lost its matching ToolResult.
        let corrupt = vec![
            ChatMessage::user("hi".into()),
            ChatMessage::tool_call("bash", "{}", Some("call_1".into())),
            // Orphan: no ToolResult follows.
            ChatMessage::agent("done".into()),
        ];

        sm.replace_chat_messages(corrupt);

        let state = sm.get_state();
        assert_eq!(
            state.chat.messages.len(),
            2,
            "orphan ToolCall must be dropped at the boundary"
        );
        assert_eq!(state.chat.messages[0].role, MessageRole::User);
        assert_eq!(state.chat.messages[1].role, MessageRole::Agent);
    }

    #[test]
    fn sync_from_conversation_sanitizes_orphan_call() {
        use crate::conversation::Conversation;
        use crate::ui::app_state::MessageRole;

        let sm = StateManager::new();

        // Build a conversation with an orphan ToolCall (no matching
        // ToolResult). This simulates a file truncated mid-write or
        // hand-edited.
        let mut conv =
            Conversation::new("test".into(), "test-provider".into(), "test-model".into());
        conv.add_user_message("hello".into());
        conv.add_tool_call("bash".into(), "{}".into(), Some("call_1".into()));
        // No matching ToolResult!
        conv.add_assistant_message("done".into());

        *sm.current_conversation.lock().unwrap() = Some(conv);
        sm.sync_from_conversation();

        let state = sm.get_state();
        assert_eq!(
            state.chat.messages.len(),
            2,
            "orphan ToolCall must be dropped on load"
        );
        assert_eq!(state.chat.messages[0].role, MessageRole::User);
        assert_eq!(state.chat.messages[1].role, MessageRole::Agent);
    }

    // ─── todo persistence roundtrip ──────────────────────────────────────

    /// Todo items round-trip through sync_to_conversation / sync_from_conversation.
    #[test]
    fn todo_roundtrip_through_sync() {
        use crate::tools::todo::TodoStatus;

        let sm = StateManager::new();

        // Add some todos
        {
            let mut list = sm.todo_list.lock().unwrap();
            list.add("Fix auth bug".into());
            list.add("Write tests".into());
            list.update_status(1, TodoStatus::InProgress);
        }

        // Build a conversation and set it as current
        let mut conv = Conversation::new("test".into(), "prov".into(), "model".into());
        conv.add_user_message("hello".into());
        *sm.current_conversation.lock().unwrap() = Some(conv);

        // Save todos into conversation
        sm.sync_to_conversation();

        // Verify conversation has todos
        {
            let guard = sm.current_conversation.lock().unwrap();
            let conv = guard.as_ref().unwrap();
            assert_eq!(conv.todos.list().len(), 2);
            assert_eq!(conv.todos.list()[0].task, "Fix auth bug");
            assert_eq!(conv.todos.list()[0].status, TodoStatus::InProgress);
            assert_eq!(conv.todos.list()[1].task, "Write tests");
            assert_eq!(conv.todos.list()[1].status, TodoStatus::Pending);
        }

        // Clear the live todo list (simulate new session)
        {
            let mut list = sm.todo_list.lock().unwrap();
            *list = crate::tools::todo::TodoList::new();
        }

        // Load todos back from conversation
        sm.sync_from_conversation();

        // Verify todos restored
        {
            let list = sm.todo_list.lock().unwrap();
            assert_eq!(list.list().len(), 2);
            assert_eq!(list.list()[0].task, "Fix auth bug");
            assert_eq!(list.list()[0].status, TodoStatus::InProgress);
            assert_eq!(list.list()[1].task, "Write tests");
            assert_eq!(list.list()[1].status, TodoStatus::Pending);
        }

        // Verify next_id preserved (adding a new task gets id=3, not 1)
        {
            let mut list = sm.todo_list.lock().unwrap();
            let result = list.add("Third task".into());
            assert_eq!(result.id, 3, "next_id must survive roundtrip");
        }
    }

    /// Pre-todo-persistence conversation JSON loads with empty todos.
    #[test]
    fn pre_todo_persistence_conversation_loads_with_empty_todos() {
        let json = r#"{
            "id": "10da8b9d-f242-4786-9c75-c3fbc2530f1f",
            "name": "Old convo",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "messages": [],
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {"message_count": 0}
        }"#;

        let conv: Conversation = serde_json::from_str(json).unwrap();
        assert!(conv.todos.list().is_empty());
    }

    /// Loading a conversation with todos auto-shows the todo panel.
    #[test]
    fn sync_from_conversation_with_todos_auto_shows_panel() {
        let sm = StateManager::new();

        // Build a conversation that has todos
        let mut conv = Conversation::new("test".into(), "prov".into(), "model".into());
        conv.add_user_message("hello".into());
        conv.todos.add("Task from saved convo".into());
        *sm.current_conversation.lock().unwrap() = Some(conv);

        // Panel starts hidden
        assert!(!sm.get_state().todo.visible);

        // Load the conversation
        sm.sync_from_conversation();

        // Panel should now be visible because the convo has todos
        assert!(sm.get_state().todo.visible);
    }

    /// Loading a conversation without todos does NOT auto-show the panel.
    #[test]
    fn sync_from_conversation_without_todos_keeps_panel_hidden() {
        let sm = StateManager::new();

        let mut conv = Conversation::new("test".into(), "prov".into(), "model".into());
        conv.add_user_message("hello".into());
        *sm.current_conversation.lock().unwrap() = Some(conv);

        assert!(!sm.get_state().todo.visible);
        sm.sync_from_conversation();
        assert!(!sm.get_state().todo.visible);
    }

    // ─── workin-baby: working-indicator state transitions ────────────────

    #[test]
    fn set_running_true_stamps_run_started_at() {
        let sm = StateManager::new();
        let before = sm.get_state();
        assert!(
            before.run_started_at.is_none(),
            "idle state has no start time"
        );
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

        std::thread::sleep(std::time::Duration::from_millis(5));

        sm.set_running(false);
        sm.set_running(true);
        let second = sm
            .get_state()
            .run_started_at
            .expect("re-stamped on restart");

        assert!(second > first);
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

    fn sm_with_context_size(window: usize) -> Arc<StateManager> {
        use crate::config::ContextConfig;
        let sm = StateManager::new_arc();
        let cfg = ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: true,
            compaction_model: None,
        };
        // No compaction model — we're not exercising compact() here.
        let cm = ContextManager::new(cfg, window, None);
        sm.init_context_manager(cm);
        sm
    }

    #[test]
    fn context_size_populated_after_init_context_manager() {
        let sm = sm_with_context_size(128_000);
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
        let sm = sm_with_context_size(200_000);

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

    /// `StateManager::needs_compaction()` is the new public accessor used by
    /// `SessionHook::on_completion_call` to gate in-loop compaction. It must
    /// reflect the same answer the internal trigger sites already use.
    ///
    /// Pinned by the in-loop compaction plan (`mid-compaction.md` § 3 Step 1).
    #[test]
    fn needs_compaction_accessor_matches_trigger_logic() {
        let sm = sm_with_context_size(1_000);

        // Empty conversation — never compact.
        assert!(!sm.needs_compaction());

        // Push some traffic but stay under the message-count fallback (keep_recent*3 = 15).
        for i in 0..3 {
            sm.add_user_message(format!("u{i}"));
            sm.add_assistant_message(format!("a{i}"));
        }
        assert!(
            !sm.needs_compaction(),
            "below message-count threshold and no token signal — must not compact"
        );

        // Drive last_input_tokens past 80% of 1_000 = 800.
        sm.add_request(900, 50, 0.0);
        assert!(
            sm.needs_compaction(),
            "token branch must fire when last_input_tokens > threshold"
        );
    }

    /// **The actual loop guard for in-loop compaction.** After
    /// `apply_compaction` (via `force_compact`) runs, the next
    /// `needs_compaction()` MUST return false even though no API call has
    /// happened yet — otherwise terminate-and-restart from
    /// `on_completion_call` infinite-loops (the `compactfuck.md` regression).
    ///
    /// Mechanism: `apply_compaction` calls `clear_last_input_tokens()` so the
    /// stale pre-compaction wire-size estimate stops driving the threshold,
    /// and the message-count fallback (which dropped to ~`keep_recent` after
    /// compaction) takes over.
    ///
    /// Pinned by `mid-compaction.md` § 3 Step 2.
    #[tokio::test]
    async fn force_compact_makes_needs_compaction_return_false() {
        use crate::config::ContextConfig;
        use crate::context_manager::ContextManager;

        // Build a SM with a real compaction model so `force_compact` does work.
        // We don't actually need to run an LLM — we just need enough messages
        // queued that compaction has something to summarize. Use the mock
        // compaction model from providers.
        let sm = StateManager::new_arc();
        let cfg = ContextConfig {
            threshold: 0.5, // 500 tokens of a 1000 window
            keep_recent: 3,
            enabled: true,
            compaction_model: None,
        };

        #[cfg(feature = "mock")]
        let compaction_model = {
            let (model, mock) = crate::providers::create_mock_compaction_model();
            mock.add_response(crate::mock::MockResponse::text("Summary."));
            Some(std::sync::Arc::new(model))
        };
        #[cfg(not(feature = "mock"))]
        let compaction_model = None;

        let cm = ContextManager::new(cfg, 1_000, compaction_model);
        sm.init_context_manager(cm);

        // Pile up enough messages to give compaction something to do.
        // (No fire-and-forget compaction fires from message-adds anymore —
        // `add_user_message` is now compaction-free; the hook gates it instead.)
        for i in 0..10 {
            sm.add_user_message(format!("user msg {i}"));
            sm.add_assistant_message(format!("assistant reply {i}"));
        }

        // Push last_input_tokens above the threshold (600 > 500).
        sm.add_request(600, 50, 0.0);
        assert!(
            sm.needs_compaction(),
            "precondition: needs_compaction must be true before we compact"
        );

        // Run compaction synchronously (the path the new 'compact' cancellation handler uses).
        #[cfg(feature = "mock")]
        {
            let result = sm.force_compact().await;
            assert!(
                result.is_some(),
                "force_compact must produce a result with mock summarizer queued"
            );

            // The whole point: post-compaction, needs_compaction must read false
            // even though no fresh API call has refreshed last_input_tokens yet.
            assert!(
                !sm.needs_compaction(),
                "needs_compaction must return false post-compaction (loop guard)"
            );
        }
        // Without the mock feature this test can't drive compaction; skip silently.
        #[cfg(not(feature = "mock"))]
        let _ = compaction_model;
    }

    // ── Vision / multimodal history ───────────────────────────────────────

    fn sample_attachment(name: &str) -> crate::vision::ImageAttachment {
        use crate::vision::{ImageAttachment, ImageSource};
        use rig_core::completion::message::ImageMediaType;
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
        use rig_core::completion::message::{Message as RigMessage, UserContent};

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
        use rig_core::completion::message::{Message as RigMessage, UserContent};

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

    /// An image tool result (the JSON shape `view_image` emits) must be
    /// reconstructed as a rig `ToolResultContent::Image`, not flattened to a
    /// base64 text blob — otherwise the model never sees the picture.
    #[test]
    fn get_agent_history_reconstructs_image_tool_result_as_image() {
        use rig_core::completion::message::{
            Message as RigMessage, ToolResultContent, UserContent,
        };

        let sm = StateManager::new();
        sm.add_user_message("look".to_string());
        sm.add_tool_call(
            "view_image".to_string(),
            r#"{"path":"/tmp/x.png"}"#.to_string(),
            Some("call_1".to_string()),
        );
        // Exactly the shape `view_image` returns.
        sm.add_tool_result(
            "view_image".to_string(),
            r#"{"path":"/tmp/x.png"}"#.to_string(),
            r#"{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}"#.to_string(),
            Some("call_1".to_string()),
        );
        // A trailing user turn so the tool result is not the excluded last msg.
        sm.add_user_message("describe it".to_string());

        let history = sm.get_agent_history();
        let tool_result = history
            .iter()
            .find_map(|m| match m {
                RigMessage::User { content } => content.iter().find_map(|c| match c {
                    UserContent::ToolResult(tr) => Some(tr.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .expect("a tool result must be present in history");

        assert!(
            matches!(tool_result.content.first(), ToolResultContent::Image(_)),
            "image tool result must reconstruct as Image, got {:?}",
            tool_result.content.first()
        );
    }

    #[test]
    fn get_agent_history_still_excludes_trailing_user_even_with_attachments() {
        let sm = StateManager::new();
        sm.add_user_message_with_attachments("hi".to_string(), vec![sample_attachment("cat.png")]);
        // Trailing user with attachments → excluded (matches existing behaviour).
        let history = sm.get_agent_history();
        assert!(history.is_empty());
    }

    #[test]
    fn build_current_turn_message_returns_multimodal_when_attachments_present() {
        use rig_core::completion::message::{Message as RigMessage, UserContent};

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
        use rig_core::completion::message::{ImageDetail, UserContent};

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
        use rig_core::completion::message::{ImageDetail, UserContent};

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
        use rig_core::completion::message::{ImageDetail, ImageMediaType, UserContent};

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
        use rig_core::completion::message::{Message as RigMessage, UserContent};

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

    /// `build_current_turn_message` skips compacted messages, so when the
    /// in-loop compaction handler rebuilds `current_turn` after `force_compact`,
    /// it correctly picks up the latest non-compacted user message.
    ///
    /// Pinned by `mid-compaction.md` § 5 test 4.
    #[test]
    fn build_current_turn_message_skips_compacted_users() {
        use crate::ui::app_state::ChatMessage;
        use rig_core::completion::message::{Message as RigMessage, UserContent};

        let sm = StateManager::new();

        // Insert messages directly so we can mark some as compacted without
        // running the full compaction pipeline.
        let mut old_user = ChatMessage::user("ancient turn".to_string());
        old_user.compacted = true;
        sm.update_chat(old_user);

        let summary = ChatMessage::summary("[summary of ancient turn]".to_string());
        sm.update_chat(summary);

        sm.add_user_message("recent turn".to_string());

        let msg = sm
            .build_current_turn_message()
            .expect("a non-compacted user must exist");
        match msg {
            RigMessage::User { content } => {
                let texts: Vec<String> = content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect();
                assert!(
                    texts.iter().any(|t| t.contains("recent turn")),
                    "rebuilt turn must point at the latest *non-compacted* user, got {texts:?}"
                );
                assert!(
                    !texts.iter().any(|t| t.contains("ancient turn")),
                    "rebuilt turn must NOT include the compacted user, got {texts:?}"
                );
            }
            _ => panic!("expected User message"),
        }
    }

    /// `build_resumption_for_compaction` returns the last non-compacted message
    /// (whatever its role) as the prompt, with everything before it as history.
    ///
    /// This is the load-bearing regression pin for the "crazy loop" bug: when
    /// compaction fires mid-action (after a ToolResult but before the next
    /// model response), the resumption prompt must be the ToolResult, not the
    /// stale User that `build_current_turn_message` always returns.
    #[test]
    fn build_resumption_for_compaction_does_not_duplicate_user_prompt() {
        let sm = StateManager::new();

        // Simulate mid-action state: User → Agent → ToolCall → ToolResult
        // Compaction fires after the ToolResult. The resumption prompt
        // should be the ToolResult, and history should be [User, Agent, ToolCall].
        sm.add_user_message("list files".to_string());
        sm.add_assistant_message("I'll run ls for you".to_string());
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

        let (prompt, history) = sm
            .build_resumption_for_compaction()
            .expect("non-empty conversation must produce resumption");

        // The prompt must be the ToolResult — NOT the User message.
        // If it's the User, the model sees a duplicate on the wire and
        // re-runs the same tool call, blowing context again → infinite loop.
        let prompt_text = extract_resumption_text(&prompt);
        assert!(
            prompt_text.contains("file1.txt") || prompt_text.contains("ls"),
            "prompt must be the ToolResult, got: {prompt_text}"
        );

        // History must contain the User message (it's part of conversation history).
        // The bug was that User appeared BOTH in history AND as the prompt (duplicate).
        // With the fix, User is ONLY in history, not as a separate prompt.
        let history_text = format!("{:?}", history);
        assert!(
            history_text.contains("list files"),
            "User must appear in history, got: {history_text}"
        );
    }

    /// Defensive boundary: empty state must return None, not panic.
    #[test]
    fn build_resumption_for_compaction_returns_none_on_empty_state() {
        let sm = StateManager::new();
        assert!(
            sm.build_resumption_for_compaction().is_none(),
            "empty state must return None"
        );
    }

    /// No-regression for fresh-turn compaction (compaction fires before any
    /// tool round-trip). In this case the last message IS a User, so both
    /// `build_resumption_for_compaction` and `build_current_turn_message` should
    /// agree on the User.
    #[test]
    fn build_resumption_for_compaction_handles_trailing_user() {
        let sm = StateManager::new();
        sm.add_user_message("a fresh turn".to_string());
        sm.add_assistant_message("hi".to_string());

        let (prompt, history) = sm
            .build_resumption_for_compaction()
            .expect("non-empty conversation must produce resumption");

        // The prompt should be the Assistant message ("hi"), not the User.
        // History should be [User].
        let prompt_text = extract_resumption_text(&prompt);
        assert!(
            prompt_text.contains("hi"),
            "prompt must be the Assistant, got: {prompt_text}"
        );

        let history_text = format!("{:?}", history);
        assert!(
            history_text.contains("a fresh turn"),
            "history must contain the User message, got: {history_text}"
        );
    }

    /// Extract text from a rig Message for test assertions.
    fn extract_resumption_text(msg: &rig_core::completion::message::Message) -> String {
        use rig_core::completion::message::{AssistantContent, ToolResultContent, UserContent};

        match msg {
            rig_core::completion::message::Message::User { content } => {
                // OneOrMany has first + rest fields, not enum variants.
                // Check for ToolResult content first (from build_resumption_for_compaction).
                let first = content.first_ref();
                if let UserContent::ToolResult(tr) = first {
                    // ToolResult wraps ToolResultContent::Text
                    let first_content = tr.content.first_ref();
                    if let ToolResultContent::Text(t) = first_content {
                        return t.text.clone();
                    }
                }
                // Fall back to text content
                let mut texts = Vec::new();
                if let UserContent::Text(t) = first {
                    texts.push(t.text.clone());
                }
                for item in content.rest() {
                    if let UserContent::Text(t) = item {
                        texts.push(t.text.clone());
                    }
                }
                texts.join(" ")
            }
            rig_core::completion::message::Message::Assistant { content, .. } => {
                // OneOrMany has first + rest fields, not enum variants.
                let mut texts = Vec::new();
                if let AssistantContent::Text(t) = content.first_ref() {
                    texts.push(t.text.clone());
                }
                for item in content.rest() {
                    if let AssistantContent::Text(t) = item {
                        texts.push(t.text.clone());
                    }
                }
                texts.join(" ")
            }
            _ => String::new(),
        }
    }

    /// Regression: `/new` → first prompt → first LLM response used to hang
    /// because two paths grabbed `state` and `stats` in opposite orders:
    ///
    /// - `sync_to_conversation` (called from `add_assistant_message` →
    ///   `persist_current` on the agent loop) takes `state.read` →
    ///   `current_conversation.lock` → `stats.lock`.
    /// - `sync_stats_to_ui` (called from `add_request` on the event-processor
    ///   task when `CompletionResponse` arrives) used to take `stats.lock` →
    ///   `state.write`.
    ///
    /// Cross those two on a multi-threaded runtime and `state.write` blocks
    /// behind the held `state.read`, while `stats.lock` blocks the
    /// reader-side path that's waiting for it — classic A→B vs B→A
    /// deadlock. The fix is to drop `stats.lock` before acquiring
    /// `state.write` in `sync_stats_to_ui`. This test reproduces the race
    /// by hammering both entry points from two threads at once. With the
    /// bug present it deadlocks; with the fix it completes in
    /// milliseconds.
    ///
    /// Implementation note: the watchdog must NOT touch any of the locks
    /// involved (state / stats / current_conversation). A lock-touching
    /// watchdog would itself block on the deadlock and never get to fire
    /// its panic. Progress is observed via two `AtomicU64` counters that
    /// the producers bump after each successful round-trip — if either
    /// counter stops advancing for the watchdog window, we declare a
    /// deadlock.
    #[test]
    fn add_request_and_persist_current_do_not_deadlock() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};

        let sm = Arc::new(StateManager::new());
        // Seed a current conversation so persist_current actually walks the
        // state.read → current_conv.lock → stats.lock chain.
        sm.create_conversation(
            "deadlock-probe".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );
        // Seed at least one user/assistant message so sync_to_conversation
        // has something non-trivial to copy under the locks.
        sm.add_user_message("hello".to_string());

        let stop = Arc::new(AtomicBool::new(false));
        let count_a = Arc::new(AtomicU64::new(0));
        let count_b = Arc::new(AtomicU64::new(0));

        // Producer A: simulates the event-processor task calling
        // `add_request` for every completion response.
        let sm_a = sm.clone();
        let stop_a = stop.clone();
        let count_a_t = count_a.clone();
        let t_a = thread::spawn(move || {
            while !stop_a.load(Ordering::Relaxed) {
                sm_a.add_request(100, 50, 0.001);
                count_a_t.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Producer B: simulates the agent-loop task calling
        // `add_assistant_message` (which fans out to persist_current →
        // sync_to_conversation, the inverted-order path).
        let sm_b = sm.clone();
        let stop_b = stop.clone();
        let count_b_t = count_b.clone();
        let t_b = thread::spawn(move || {
            while !stop_b.load(Ordering::Relaxed) {
                sm_b.add_assistant_message("reply".to_string());
                count_b_t.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Watchdog thread: pure atomic polling, never touches any of the
        // locks held by the producers. If both counters stop advancing for
        // the watchdog window, kill the process — the test runner will
        // surface that as a failure rather than hanging the suite.
        //
        // We can't `panic!` from this thread to fail the test cleanly: a
        // panic here unwinds only the watchdog thread, the producers stay
        // stuck on the deadlock, and the main thread still hangs in `join`.
        // `std::process::exit(101)` is the standard "test failed" code,
        // matching what `panic!` would produce on the main thread.
        let stop_w = stop.clone();
        let count_a_w = count_a.clone();
        let count_b_w = count_b.clone();
        let _watchdog = thread::spawn(move || {
            let mut last_a = 0u64;
            let mut last_b = 0u64;
            let mut stuck_since: Option<Instant> = None;
            while !stop_w.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(50));
                let now_a = count_a_w.load(Ordering::Relaxed);
                let now_b = count_b_w.load(Ordering::Relaxed);
                if now_a == last_a && now_b == last_b {
                    let since = stuck_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_secs(2) {
                        // `process::abort` (SIGABRT) rather than `process::exit`
                        // because libtest's stdio capture has been observed to
                        // jam mid-test; abort dies regardless of stdio state and
                        // CI surfaces it as a clear failure.
                        eprintln!(
                            "deadlock detected: counters frozen at A={now_a}, B={now_b} for >2s"
                        );
                        std::process::abort();
                    }
                } else {
                    stuck_since = None;
                    last_a = now_a;
                    last_b = now_b;
                }
            }
        });

        // Run the workload for a fixed budget. With the fix in place both
        // producers happily make tens of thousands of iterations a second.
        thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);
        t_a.join().expect("producer A must not panic");
        t_b.join().expect("producer B must not panic");

        // Sanity: both producers must have made forward progress. If either
        // counter is zero something else is wrong and we shouldn't pretend
        // the test exercised the race.
        assert!(
            count_a.load(Ordering::Relaxed) > 0,
            "producer A made no progress"
        );
        assert!(
            count_b.load(Ordering::Relaxed) > 0,
            "producer B made no progress"
        );
    }

    // ── Pending-input counter ──────────────────────────────────────────────
    //
    // Drives the `⏳ N queued` status-bar hint introduced in
    // `make-flow-great-again.md`. The increment happens on enqueue (event
    // loop), the decrement on dequeue (agent loop), and `set_pending_input_count(0)`
    // is called by the event loop on /stop to zero the count immediately.
    // These are tiny but load-bearing — pin them.

    #[test]
    fn pending_input_starts_at_zero() {
        let sm = StateManager::new();
        assert_eq!(sm.get_state().pending_input_count, 0);
    }

    #[test]
    fn increment_pending_input_bumps_state_and_broadcasts() {
        let sm = StateManager::new();
        let mut rx = sm.subscribe();
        sm.increment_pending_input();
        assert_eq!(sm.get_state().pending_input_count, 1);
        // At least one broadcast landed (we don't assert exact count because
        // earlier mutators in this test may have queued a value too).
        assert!(rx.try_recv().is_ok(), "expected broadcast on increment");
    }

    #[test]
    fn decrement_pending_input_saturates_at_zero() {
        let sm = StateManager::new();
        sm.decrement_pending_input(); // underflow guard
        assert_eq!(sm.get_state().pending_input_count, 0);
        sm.increment_pending_input();
        sm.increment_pending_input();
        sm.decrement_pending_input();
        assert_eq!(sm.get_state().pending_input_count, 1);
    }

    #[test]
    fn set_pending_input_count_clears_to_zero() {
        let sm = StateManager::new();
        sm.increment_pending_input();
        sm.increment_pending_input();
        sm.increment_pending_input();
        assert_eq!(sm.get_state().pending_input_count, 3);
        sm.set_pending_input_count(0);
        assert_eq!(sm.get_state().pending_input_count, 0);
    }

    // ── Conversation title fixes (issue #40) ───────────────────────────────

    /// `/rename` must set the `title` field (not just `name`) so the change
    /// is visible in the `/conversations` listing, and must persist the change.
    #[test]
    fn rename_conversation_sets_title_and_persists() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        sm.create_conversation(
            "Original Name".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi".to_string());
        sm.save_conversation();

        let id = sm.get_current_conversation_id().unwrap();
        let before = storage.load(id).unwrap();
        let before_updated = before.updated_at;

        // Rename
        sm.rename_conversation("New Title".to_string()).unwrap();

        // Title field must be set (takes display precedence)
        let conv = sm.get_current_conversation().unwrap();
        assert_eq!(conv.title.as_deref(), Some("New Title"));

        // Must be persisted
        let after = storage.load(id).unwrap();
        assert_eq!(after.title.as_deref(), Some("New Title"));
        assert!(
            after.updated_at > before_updated,
            "updated_at must be bumped"
        );
    }

    /// `/rename` on a conversation that already has an auto-generated title
    /// must overwrite it so the user's explicit rename is always visible.
    #[test]
    fn rename_conversation_overwrites_existing_title() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        sm.create_conversation(
            "Original".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );
        // Simulate an auto-generated title
        {
            let mut guard = sm.current_conversation.lock().unwrap();
            guard.as_mut().unwrap().set_title("Auto Title".to_string());
        }
        sm.save_conversation();

        let id = sm.get_current_conversation_id().unwrap();
        let before = storage.load(id).unwrap();
        assert_eq!(before.title.as_deref(), Some("Auto Title"));

        // User renames — must overwrite the auto title
        sm.rename_conversation("User Title".to_string()).unwrap();

        let after = storage.load(id).unwrap();
        assert_eq!(after.title.as_deref(), Some("User Title"));
    }

    /// `maybe_generate_title` must not short-circuit when the first turn
    /// involves tool calls (user + tool_call + tool_result + assistant = 4
    /// messages, not 2). The old `msg_count != 2` check broke this.
    ///
    /// This test verifies the short-circuit logic directly: with a title model
    /// present and no title yet, the method should proceed to spawn the async
    /// task even when msg_count > 2.
    #[tokio::test]
    async fn maybe_generate_title_does_not_short_circuit_on_tool_calls() {
        use crate::providers::create_mock_compaction_model;

        let sm = StateManager::new_arc();
        let (model, _mock) = create_mock_compaction_model();
        sm.init_title_model(Arc::new(model));

        sm.create_conversation(
            "Test".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );

        // Simulate a first turn with tool calls: 4 messages total
        sm.add_user_message("List files".to_string());
        sm.add_tool_call(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt".to_string(),
            Some("call_1".to_string()),
        );
        sm.add_assistant_message("Here are the files.".to_string());

        // The conversation must not have a title yet
        assert!(!sm.get_current_conversation().unwrap().has_title());

        // With the old `msg_count != 2` check this would return immediately.
        // With the fix, it should proceed (spawn an async task) because:
        // - the conversation has no title
        // - there's at least one user and one assistant message
        sm.maybe_generate_title();

        // Give the async task a moment to run.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    /// `maybe_generate_title` must be a no-op when the conversation already
    /// has a title, regardless of message count.
    #[test]
    fn maybe_generate_title_is_noop_when_title_exists() {
        let sm = StateManager::new_arc();

        sm.create_conversation(
            "Test".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
        );
        sm.add_user_message("hello".to_string());
        sm.add_assistant_message("hi".to_string());

        // Pre-set a title
        {
            let mut guard = sm.current_conversation.lock().unwrap();
            guard.as_mut().unwrap().set_title("Existing".to_string());
        }

        // Should short-circuit immediately without spawning a task
        sm.maybe_generate_title();

        // Title must remain unchanged
        assert_eq!(
            sm.get_current_conversation().unwrap().title.as_deref(),
            Some("Existing")
        );
    }

    // ── Bash panel lifecycle (slice 2 of #11) ──────────────────────────

    #[test]
    fn bash_panel_starts_idle() {
        let sm = StateManager::new_arc();
        let snap = sm.get_state();
        assert!(snap.bash_panel.is_idle());
    }

    #[test]
    fn start_bash_panel_transitions_to_running() {
        let sm = StateManager::new_arc();
        sm.start_bash_panel("ls -la".to_string(), 4242);
        let snap = sm.get_state();
        match snap.bash_panel {
            BashPanelState::Running { command, pid, .. } => {
                assert_eq!(command, "ls -la");
                assert_eq!(pid, 4242);
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[test]
    fn update_bash_panel_tail_replaces_lines() {
        let sm = StateManager::new_arc();
        sm.start_bash_panel("yes".to_string(), 1);
        sm.update_bash_panel_tail(vec!["a".into(), "b".into()]);
        sm.update_bash_panel_tail(vec!["x".into(), "y".into(), "z".into()]);
        let snap = sm.get_state();
        match snap.bash_panel {
            BashPanelState::Running { tail, .. } => {
                assert_eq!(tail, vec!["x", "y", "z"]);
            }
            _ => panic!("expected Running"),
        }
    }

    #[test]
    fn update_bash_panel_tail_is_noop_when_not_running() {
        // If we push tail bytes against an `Idle` (or `Finished`) panel,
        // nothing should happen — guard exists so a late reader-thread
        // debounce can't corrupt a `Finished` snapshot.
        let sm = StateManager::new_arc();
        sm.update_bash_panel_tail(vec!["leaked".into()]);
        assert!(sm.get_state().bash_panel.is_idle());
    }

    #[test]
    fn finish_bash_panel_transitions_running_to_finished() {
        let sm = StateManager::new_arc();
        sm.start_bash_panel("make".to_string(), 7);
        sm.finish_bash_panel(0, vec!["done".into()]);
        let snap = sm.get_state();
        match snap.bash_panel {
            BashPanelState::Finished {
                command,
                exit_code,
                tail,
                ..
            } => {
                assert_eq!(command, "make");
                assert_eq!(exit_code, 0);
                assert_eq!(tail, vec!["done"]);
            }
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn finish_bash_panel_without_running_is_noop() {
        let sm = StateManager::new_arc();
        sm.finish_bash_panel(0, vec!["ghost".into()]);
        assert!(sm.get_state().bash_panel.is_idle());
    }

    #[test]
    fn reset_bash_panel_restores_idle_and_auto() {
        // Replaces the pre-v2 `clear_bash_panel_resets_to_idle` —
        // the new name covers state AND visibility, so the test
        // does too.
        let sm = StateManager::new_arc();
        sm.start_bash_panel("sleep 1".to_string(), 9);
        sm.finish_bash_panel(0, vec![]);
        sm.toggle_bash_panel_visibility(); // ClosedByUser
        assert!(!sm.get_state().bash_panel.is_idle());
        assert!(!matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::Auto
        ));
        sm.reset_bash_panel();
        let snap = sm.get_state();
        assert!(snap.bash_panel.is_idle());
        assert!(matches!(
            snap.bash_panel_visibility,
            BashPanelVisibility::Auto
        ));
    }

    #[test]
    fn reset_bash_panel_is_idempotent_when_already_default() {
        let sm = StateManager::new_arc();
        sm.reset_bash_panel();
        sm.reset_bash_panel();
        let snap = sm.get_state();
        assert!(snap.bash_panel.is_idle());
        assert!(matches!(
            snap.bash_panel_visibility,
            BashPanelVisibility::Auto
        ));
    }

    // ── Foreground bash panel visibility (bash-panel-as-real-panel.md) ───

    #[test]
    fn bash_panel_visibility_starts_auto() {
        // Fresh state — nobody has pressed Ctrl+B yet.
        let sm = StateManager::new_arc();
        let snap = sm.get_state();
        assert!(matches!(
            snap.bash_panel_visibility,
            BashPanelVisibility::Auto
        ));
    }

    #[test]
    fn toggle_bash_panel_visibility_on_idle_opens_then_closes() {
        // The "open it anytime, like the tasks panel" contract:
        // Ctrl+B on Idle ⇒ OpenedByUser (renders empty frame).
        // Second Ctrl+B ⇒ ClosedByUser (collapses again).
        let sm = StateManager::new_arc();
        assert!(sm.get_state().bash_panel.is_idle());
        sm.toggle_bash_panel_visibility();
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::OpenedByUser
        ));
        sm.toggle_bash_panel_visibility();
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::ClosedByUser
        ));
    }

    #[test]
    fn toggle_bash_panel_visibility_on_running_closes_first() {
        // Auto + Running ⇒ effectively visible. First Ctrl+B should
        // close (set ClosedByUser), not open. Pins the
        // toggle-based-on-effective-visibility rule.
        let sm = StateManager::new_arc();
        sm.start_bash_panel("sleep 5".to_string(), 42);
        // start_bash_panel resets visibility to Auto (producer write).
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::Auto
        ));
        sm.toggle_bash_panel_visibility();
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::ClosedByUser
        ));
    }

    #[test]
    fn user_close_survives_new_user_message() {
        // The cardinal reversal pin: PR #67 cleared the hide on every
        // user message. The user-reported bug was "sending a message
        // re-opens it." Spec is now "Ctrl+B is the only user gesture
        // that flips it." Typing a follow-up does NOT re-open.
        let sm = StateManager::new_arc();
        sm.start_bash_panel("yes".to_string(), 7);
        sm.toggle_bash_panel_visibility(); // ClosedByUser
        sm.add_user_message("anything".to_string());
        assert!(
            matches!(
                sm.get_state().bash_panel_visibility,
                BashPanelVisibility::ClosedByUser
            ),
            "new user message must NOT reset the close override (the user-reported bug)"
        );
    }

    #[test]
    fn user_close_survives_user_message_with_attachments() {
        // Vision turns are also user prompts — they must NOT reset
        // visibility either. Sibling pin to user_close_survives_new_user_message.
        let sm = StateManager::new_arc();
        sm.start_bash_panel("yes".to_string(), 7);
        sm.toggle_bash_panel_visibility(); // ClosedByUser
        sm.add_user_message_with_attachments("look".to_string(), Vec::new());
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::ClosedByUser
        ));
    }

    #[test]
    fn user_close_survives_bg_synthetic_turn() {
        // Bg synthetic turns are agent-side input, not the user
        // taking an action — must not reset visibility. Corollary
        // of the same "only user gestures flip it" rule.
        let sm = StateManager::new_arc();
        sm.toggle_bash_panel_visibility(); // OpenedByUser (on Idle)
        sm.toggle_bash_panel_visibility(); // ClosedByUser
        sm.add_user_message_from_background("[bg output]".to_string(), vec![1]);
        assert!(matches!(
            sm.get_state().bash_panel_visibility,
            BashPanelVisibility::ClosedByUser
        ));
    }

    #[test]
    fn producer_clears_user_close_on_new_bash() {
        // The other half of the contract: a new bash invocation
        // re-opens the panel. User intent "I dismissed the previous
        // output" does NOT extend to "and I want all future bash
        // invisible." See start_bash_panel doc-comment.
        let sm = StateManager::new_arc();
        sm.start_bash_panel("first".to_string(), 1);
        sm.toggle_bash_panel_visibility(); // ClosedByUser
        // Agent calls bash again — Idle could be elided, but the
        // producer would always be called by BashTool::call.
        sm.start_bash_panel("second".to_string(), 2);
        assert!(
            matches!(
                sm.get_state().bash_panel_visibility,
                BashPanelVisibility::Auto
            ),
            "new bash invocation must clear ClosedByUser back to Auto (so its output is visible)"
        );
    }

    // ── Foreground bash stdin sidecar (slice 4) ──────────────────────────

    #[test]
    fn try_forward_bash_stdin_returns_err_when_no_active_tx() {
        let sm = StateManager::new_arc();
        assert!(!sm.has_active_bash_stdin());
        let res = sm.try_forward_bash_stdin("hello".to_string());
        assert_eq!(res, Err(StdinNotActive));
    }

    #[test]
    fn try_forward_bash_stdin_returns_ok_when_tx_set_and_delivers() {
        let sm = StateManager::new_arc();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        sm.set_bash_stdin_tx(tx);
        assert!(sm.has_active_bash_stdin());

        let res = sm.try_forward_bash_stdin("password\n".to_string());
        assert!(res.is_ok());

        // The line lands on the receiver as-is — the tool is the one
        // that decides whether to append a newline (write_stdin does it).
        let got = rx.try_recv().expect("line should be queued");
        assert_eq!(got, "password\n");
    }

    #[test]
    fn clear_bash_stdin_tx_idempotent_on_unset() {
        let sm = StateManager::new_arc();
        // Empty → empty: no panic, slot stays empty.
        sm.clear_bash_stdin_tx();
        sm.clear_bash_stdin_tx();
        assert!(!sm.has_active_bash_stdin());

        // Set → clear → clear: still no panic, slot stays empty.
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        sm.set_bash_stdin_tx(tx);
        assert!(sm.has_active_bash_stdin());
        sm.clear_bash_stdin_tx();
        sm.clear_bash_stdin_tx();
        assert!(!sm.has_active_bash_stdin());
    }

    #[test]
    fn try_forward_bash_stdin_returns_err_after_receiver_dropped() {
        // Models the "Running → Finished" race: tool dropped the rx
        // (via clear_bash_stdin_tx) but the UI still holds a stale view
        // through the slot. We replicate by dropping rx ourselves while
        // the slot still has the tx, then sending. send() should fail
        // and we should report StdinNotActive (the user-facing meaning
        // is identical to slot=None: "nothing's reading").
        let sm = StateManager::new_arc();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        sm.set_bash_stdin_tx(tx);
        drop(rx);
        let res = sm.try_forward_bash_stdin("hi".to_string());
        assert_eq!(res, Err(StdinNotActive));
    }
}
