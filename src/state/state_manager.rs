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
    ContextState, MessageSource, SessionState, TodoItem, TodoState, WelcomeState,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Channel buffer size for state subscribers.
const STATE_SUBSCRIBER_BUFFER: usize = 64;

/// What a Stop actually killed. Snapshotted at the kill site so the message
/// (rendered in `lib.rs::stop_message`) and the kill are in lock-step — the
/// tally is captured **before** the kill, not derived afterwards, so a stop
/// that races with natural exits still reports the truth.
///
/// #183: `shell: bool` (not a count) because the foreground shell tool is
/// single-call by construction ([`StateManager::set_bash_stdin_tx`] /
/// [`StateManager::clear_bash_stdin_tx`], see `state_manager.rs:2315-2320`);
/// "two concurrent foreground shells" is unrepresentable. Background
/// (`bash_bg`) processes are deliberately spared — they survive Stop and
/// are only killed by the rebuild paths (`/new`, `/model`, `/load`, `/cd`,
/// shutdown).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StopTally {
    /// A foreground shell (`bash` / `powershell`) was mid-call when the stop
    /// landed. The tool is single-call by construction — hence `bool`, not a
    /// count.
    pub shell: bool,
}

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

    // ── Per-session working directory ─────────────────────────────────────────
    /// The directory every path-aware tool resolves against and every shell
    /// spawns in. Single source of truth for the session's cwd — never mutated
    /// process-globally (no `set_current_dir`). Changed only at session
    /// boundaries (construction, `/cd`, `/load`), after which the agent is
    /// rebuilt so its tools re-snapshot this value.
    session_cwd: RwLock<std::path::PathBuf>,

    // ── Turn cancellation (#183) ─────────────────────────────────────────────
    /// Cancels the in-flight turn by dropping its future — which kills the
    /// foreground PTY child through `PtyHandle::drop` and unwinds any running
    /// sub-agent with it. Replaced with a fresh token by `set_running(true)`,
    /// so a Stop pressed while idle can never poison the next turn.
    /// Lock discipline: taken only synchronously, never across `.await`.
    turn_cancel: RwLock<CancellationToken>,

    // ── Rendering Coalescence ─────────────────────────────────────────────────
    /// Monotonic counter for render coalescence — see `slow-messages.md` §4.4.
    revision: AtomicU64,

    // ── Wire reasoning gate (design §3.4) ─────────────────────────────────
    /// True → `get_agent_history` may emit `Reasoning` content for
    /// Anthropic-captured thinking blocks; false → drops every block,
    /// even on a freshly loaded Anthropic transcript. Set from the
    /// active `ProviderInfo` (`provider == "anthropic" && info.preserve_reasoning`).
    wire_reasoning: RwLock<bool>,

    // ── Display reasoning gate (design §5) ─────────────────────────────────
    /// True → the web snapshot carries thinking text on `ChatMessage` rows;
    /// false (default) → thinking is stripped server-side before any
    /// broadcast to browsers. Signatures are never transmitted regardless.
    display_reasoning: RwLock<bool>,

    // ── Pending thinking coalescence (design §4 / CONCERN 6) ───────────────
    /// Captured Anthropic thinking blocks for the orchestrator's current
    /// assistant turn. The `CompletionResponse` event arrives before the
    /// prose is appended via `add_assistant_message`, so we stage the blocks
    /// here and attach them to the first orchestrator message added for the
    /// turn (prose or tool call). This avoids the phantom empty assistant
    /// row that the previous implementation produced. The LLM wire is
    /// unaffected because `get_agent_history` gathers thinking across the
    /// run regardless of which `ChatMessage` row carries the blocks.
    pending_thinking: RwLock<Vec<crate::reasoning::ThinkingBlock>>,
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
            session_cwd: RwLock::new(std::env::current_dir().unwrap_or_default()),
            // Initial token is fresh (never cancelled). `set_running(true)`
            // re-mints on every turn start (D-D / invariant I1), so the
            // construction-time value is only read until the first turn.
            turn_cancel: RwLock::new(CancellationToken::new()),
            revision: AtomicU64::new(0),
            // Default `true` — matches `resolve_preserve_reasoning`'s default
            // (design §2.1: "Unset anywhere → `true`: on Anthropic, replaying
            // thinking is what the API wants"). The agent-build paths
            // explicitly set this from `ProviderInfo` after `create_provider`
            // returns; a non-Anthropic provider flips the gate off so a
            // loaded Claude transcript cannot leak signatures onto a
            // foreign wire.
            wire_reasoning: RwLock::new(true),
            // Default `false` — matches `resolve_display_reasoning`'s default
            // (design §5: thinking is invisible by default). Set from
            // `ProviderInfo` at the same sites as `wire_reasoning` so the
            // web snapshot is gated server-side.
            display_reasoning: RwLock::new(false),
            pending_thinking: RwLock::new(Vec::new()),
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

    /// Compaction threshold as a fraction (e.g. `0.8`), or `None` when
    /// compaction is disabled or no `ContextManager` is initialized. The
    /// delegate tool sizes each sub-agent's budget against *its own* model's
    /// context size, so it needs the fraction rather than the token count.
    pub fn compaction_threshold(&self) -> Option<f64> {
        let guard = self.context_manager.read().unwrap();
        let cm = guard.as_ref()?;
        cm.is_enabled().then(|| cm.threshold_fraction())
    }

    /// Read the most recent API-reported input-token count.
    /// Returns `0` when no API response has been seen yet — that signals
    /// `ContextManager` to fall back to the message-count heuristic.
    fn current_input_tokens(&self) -> usize {
        self.stats
            .lock()
            .unwrap()
            .last_orchestrator_input_tokens()
            .unwrap_or(0) as usize
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

        let mut state = self.state.write().unwrap();
        let messages = &mut state.chat.messages;

        // The plan was computed from a snapshot taken before the summarizer
        // await; the live vector may have shrunk since. Clamp once, here.
        let boundary = plan.boundary.min(messages.len());

        // Find tool calls before boundary that are needed by results after boundary
        let needed_tc: std::collections::HashSet<usize> =
            find_needed_tool_calls_chat(messages, boundary)
                .into_iter()
                .collect();

        // Tag messages before boundary as compacted, except needed tool calls.
        // Lane-blind and positional: `compacted` means "behind the boundary",
        // not "belongs to a lane".
        for (i, msg) in messages.iter_mut().enumerate().take(boundary) {
            if !needed_tc.contains(&i) {
                msg.compacted = true;
            }
        }

        // INVARIANT: the summary is *prepended* to the surviving wire sequence,
        // never spliced into it — otherwise it lands between a rescued ToolCall
        // and its ToolResult and Anthropic rejects the turn. Derived from the
        // tags written just above, so it cannot drift from them, and it MUST run
        // after them: both index the pre-insert vector, and `insert` shifts
        // every index past it.
        let insert_at = messages[..boundary]
            .iter()
            .position(ChatMessage::is_orchestrator_context)
            .unwrap_or(boundary);
        messages.insert(insert_at, ChatMessage::summary(plan.summary.clone()));
        // `plan.boundary` is meaningless from here on — do not use it.

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

    /// Count of messages in the orchestrator's live context (what the LLM
    /// would see) — see `ChatMessage::is_orchestrator_context`.
    fn uncompacted_message_count(&self) -> usize {
        let state = self.state.read().unwrap();
        state
            .chat
            .messages
            .iter()
            .filter(|m| m.is_orchestrator_context())
            .count()
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

    /// Add a request's stats to the session, attributed to the producing lane.
    ///
    /// `source` keys the per-lane breakdown: orchestrator turns bucket under
    /// `"orchestrator"`, a sub-agent's under its role. The flat grand totals
    /// accumulate regardless of lane.
    pub fn add_request(&self, source: &MessageSource, input: u64, output: u64, cost: f64) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.add_request(source.lane_label(), input, output, cost);
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
        let (input, output, calls, cost, last_input, lanes) = {
            let stats = self.stats.lock().unwrap();
            (
                stats.total_input_tokens,
                stats.total_output_tokens,
                stats.total_api_calls,
                stats.total_cost,
                // Read the orchestrator's last input tokens — the same signal the
                // compaction gate uses. Sub-agent turns must not move this meter.
                stats.last_orchestrator_input_tokens().unwrap_or(0),
                stats.lanes_sorted(),
            )
        };

        let mut state = self.state.write().unwrap();
        state.stats.total_input_tokens = input;
        state.stats.total_output_tokens = output;
        state.stats.total_api_calls = calls;
        state.stats.total_cost = cost;
        // Scoped so the borrows end before `state.stats.lanes` is assigned.
        // Both models are DERIVED: the orchestrator's from `model_alias` (kept
        // current by every rebuild), a role's from the selected pipeline's
        // member list. Nothing is stored twice.
        let rows: Vec<crate::ui::app_state::LaneStat> = {
            let active_alias = &state.stats.model_alias;
            let members = state
                .selected_pipeline
                .as_deref()
                .and_then(|name| state.pipelines.iter().find(|p| p.name == name))
                .map(|p| p.members.as_slice())
                .unwrap_or(&[]);
            lanes
                .into_iter()
                .map(|(lane, s)| crate::ui::app_state::LaneStat {
                    model: if lane == crate::ui::app_state::ORCHESTRATOR_LANE {
                        active_alias.clone()
                    } else {
                        members
                            .iter()
                            .find(|(role, _)| role == &lane)
                            .map(|(_, alias)| alias.clone())
                            .unwrap_or_default()
                    },
                    lane,
                    input_tokens: s.input_tokens,
                    output_tokens: s.output_tokens,
                    api_calls: s.api_calls,
                    cost: s.cost,
                })
                .collect()
        };
        state.stats.lanes = rows;
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

        let (msg, is_new) = {
            let mut list = self.todo_list.lock().unwrap();
            let before = list.list().len();
            let msg = list.add_one(task);
            (msg, list.list().len() > before)
        };

        self.sync_todo_to_ui();

        // Auto-show todo panel when first task is added
        if was_empty && is_new {
            self.show_todo_panel();
        }

        msg
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

        let (msg, added_any) = {
            let mut list = self.todo_list.lock().unwrap();
            let before = list.list().len();
            let msg = list.add_batch(tasks);
            (msg, list.list().len() > before)
        };
        self.sync_todo_to_ui();

        // Auto-show todo panel when first tasks are added to empty list
        if was_empty && added_any {
            self.show_todo_panel();
        }

        msg
    }

    /// Update todo item status
    pub fn update_todo_status(&self, id: usize, status: TodoStatus) -> String {
        let (msg, existed) = {
            let mut list = self.todo_list.lock().unwrap();
            let existed = list.get(id).is_some();
            (list.update_one(id, status), existed)
        };
        if existed {
            self.sync_todo_to_ui();
        }
        msg
    }

    /// Remove a todo item
    pub fn remove_todo(&self, id: usize) -> String {
        let (msg, existed) = {
            let mut list = self.todo_list.lock().unwrap();
            let existed = list.get(id).is_some();
            (list.remove_one(id), existed)
        };
        if existed {
            self.sync_todo_to_ui();
        }
        msg
    }

    /// List all todo items
    pub fn list_todos(&self) -> String {
        self.todo_list.lock().unwrap().render()
    }

    /// Clear finished todo items (completed and cancelled)
    pub fn clear_completed_todos(&self) -> String {
        let msg = { self.todo_list.lock().unwrap().clear_finished() };
        self.sync_todo_to_ui();
        msg
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
        }
        self.sync_todo_to_ui();
    }

    /// Sync todo list to UI state. Lock order: `todo_list` → `state`
    /// (load-bearing — `todo_list` is a leaf; see
    /// `update_todo_and_persist_current_do_not_deadlock`). Snapshots under
    /// the `todo_list` lock and releases it before acquiring `state.write`,
    /// mirroring `sync_stats_to_ui`.
    fn sync_todo_to_ui(&self) {
        let items: Vec<TodoItem> = {
            let list = self.todo_list.lock().unwrap();
            list.list().iter().map(TodoItem::from).collect()
        };

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
        // Send current state immediately so subscriber is up-to-date.
        // Apply the same display-reasoning gate as every broadcast.
        let mut current = self.state.read().unwrap().clone();
        self.strip_thinking_from_app_state(&mut current);
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
    /// session-cwd flip on `state_manager.session_cwd`) may still call
    /// this safely.
    pub fn reset_conversation_state(&self) {
        self.clear_chat();
        self.reset_stats();
        self.clear_all_todos();
        self.clear_bg();
        self.reset_bash_panel();
    }

    /// Set whether the agent is currently running (processing a message).
    ///
    /// Stamps `run_started_at = Some(Instant::now())` when starting, and clears
    /// both the start-time and `status_message` when stopping. The `workin-baby`
    /// TUI indicator keys off these fields — do not split the state.
    ///
    /// **#183**: also re-mints the per-turn cancellation token on every
    /// `true` write, so a Stop that fires while idle cannot poison the next
    /// turn (design D-D, invariant I1). Lock discipline: the `turn_cancel`
    /// write is taken after the `state` guard is released — never nested.
    pub fn set_running(&self, running: bool) {
        {
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
        if running {
            // Freshness (D-D): every start gets a never-cancelled token so a
            // stale flag from a prior turn can't carry into this one.
            *self.turn_cancel.write().unwrap() = CancellationToken::new();
        }
    }

    /// Check if the agent is currently running
    pub fn is_running(&self) -> bool {
        self.state.read().unwrap().is_running
    }

    /// The current turn's cancellation token. Cancelling it ⇒ the turn's
    /// future is dropped at the next poll, which unwinds the in-flight
    /// provider HTTP request, the in-flight tool call, the in-flight
    /// sub-agent, and — via `PtyHandle::drop` — the foreground PTY child.
    /// Cloning is cheap (Arc-backed). Replaced with a fresh token by
    /// [`Self::set_running`] on every `true` write (design D-D), so a Stop
    /// pressed while idle cannot poison the next turn.
    pub fn turn_cancel_token(&self) -> CancellationToken {
        self.turn_cancel.read().unwrap().clone()
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

        let mut state = state.clone();
        self.strip_thinking_from_app_state(&mut state);
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

    /// Server-side gate for `display_reasoning` (design §5). When the gate
    /// is closed, clears every `ChatMessage.thinking` vector on the
    /// provided `AppState` clone so the broadcast snapshot contains no
    /// thinking field (the custom serializer skips empty vecs). The
    /// persisted state and the LLM wire are untouched.
    fn strip_thinking_from_app_state(&self, state: &mut AppState) {
        let display_reasoning = *self.display_reasoning.read().unwrap();
        if !display_reasoning {
            for msg in &mut state.chat.messages {
                msg.thinking.clear();
            }
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
    /// identity and the directory it is rooted in, and set it as current.
    ///
    /// `(provider_name, model)` is the persisted re-activation key for
    /// `/load` — see [`Conversation::new`]. Boot, `/new`, and `/model`
    /// switch all funnel through here so saved files always carry the
    /// stable wire id.
    ///
    /// `cwd` is the directory tree this conversation is bound to —
    /// the per-session `session_cwd` for normal mints, or the new
    /// target for `/cd`. The conversation is persisted 1:1 with this
    /// path; `/load` reapplies it.
    pub fn create_conversation(
        &self,
        name: String,
        provider_name: String,
        model: String,
        cwd: String,
    ) {
        // Snapshot the selection under the state lock and release it *before*
        // locking the conversation — never hold both guards together (plan §4).
        // `/new` mints on the current pipeline (D2: the selection survives).
        let selected = self.state.read().unwrap().selected_pipeline.clone();
        let mut conv = Conversation::new(name, provider_name, model, cwd);
        conv.pipeline = selected;
        *self.current_conversation.lock().unwrap() = Some(conv);
        self.mirror_conversation_to_state();
    }

    /// Publish the pipeline catalogue (the boot-built `PipelineSet` projected
    /// into wire shape). Stamped once at session build; "no pipelines
    /// configured" is an empty vec, never a separate flag.
    pub fn set_pipelines(&self, pipelines: Vec<crate::pipeline::PipelineInfo>) {
        let mut state = self.state.write().unwrap();
        state.pipelines = pipelines;
        self.notify_update(&state);
    }

    /// The pipeline this conversation is bound to, or `None` for single-agent
    /// mode. Reads the live `AppState` mirror — the same value
    /// [`Self::set_selected_pipeline`] writes.
    pub fn selected_pipeline(&self) -> Option<String> {
        self.state.read().unwrap().selected_pipeline.clone()
    }

    /// Whether the current conversation has had a real turn (any user or agent
    /// message). System banners don't count — a fresh session showing only a
    /// welcome/warning is still "not started". This is the lock signal for the
    /// pipeline selection: mutable before the first turn, frozen after.
    pub fn conversation_has_turns(&self) -> bool {
        use crate::ui::app_state::MessageRole;
        self.state
            .read()
            .unwrap()
            .chat
            .messages
            .iter()
            .any(|m| matches!(m.role, MessageRole::User | MessageRole::Agent))
    }

    /// Record the pipeline selection: onto the current conversation (persisted
    /// on the next save) and into the live `AppState`. The caller owns the
    /// lock-after-first-turn rule and the agent rebuild — this only records
    /// the fact.
    ///
    /// **Lock rule:** the conversation guard is taken and released before the
    /// state guard. Holding both would invert the `state → current_conversation`
    /// order the persist path uses and deadlock (see
    /// `select_pipeline_and_persist_current_do_not_deadlock`).
    pub fn set_selected_pipeline(&self, name: Option<String>) {
        if let Some(conv) = self.current_conversation.lock().unwrap().as_mut() {
            conv.pipeline = name.clone();
        }
        let mut state = self.state.write().unwrap();
        state.selected_pipeline = name;
        self.notify_update(&state);
    }

    /// Re-stamp the current conversation's wire identity `(provider_name,
    /// model)`. Used when a pipeline selection swaps the orchestrator's model
    /// in place: the conversation keeps its id (sticky `?convo=`) but its
    /// persisted re-activation key must follow the model it actually runs on.
    pub fn set_conversation_wire_id(&self, provider_name: String, model: String) {
        if let Some(conv) = self.current_conversation.lock().unwrap().as_mut() {
            conv.provider_name = provider_name;
            conv.model = model;
        }
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
    ///
    /// `cwd` is the per-session `session_cwd` the caller has already
    /// resolved (resume → saved cwd, or boot cwd). It is persisted into
    /// the conversation 1:1 — `/load` re-applies it on the next session.
    /// Mandatory `&Path` (no `Option`): the caller's job to know the
    /// session's cwd by the time this fires.
    pub fn ensure_boot_conversation(&self, cwd: &std::path::Path, model_fallback: &str) {
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
        self.create_conversation(
            name,
            provider_name,
            model,
            cwd.to_string_lossy().into_owned(),
        );
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
            // Restore the saved pipeline selection so a resumed conversation
            // rebuilds on its team's orchestrator and the Agents panel shows
            // the right row. A name that is no longer configured is dropped by
            // the caller (`PipelineSet::resolve_saved`), not here.
            let restored = self
                .current_conversation
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|c| c.pipeline.clone());
            {
                let mut state = self.state.write().unwrap();
                state.selected_pipeline = restored;
            }
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

    /// Peek at a saved conversation's persisted pipeline selection without
    /// loading it. Used by `create_session` on the resume path to build the
    /// boot agent on the right orchestrator *before* the conversation is
    /// loaded. Pre-field files return `None`.
    pub fn peek_conversation_pipeline(&self, id: Uuid) -> anyhow::Result<Option<String>> {
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("conversation storage is not configured"))?;
        Ok(storage.load(id)?.pipeline)
    }

    /// Peek at a saved conversation's persisted `cwd` without loading
    /// it. Used by `create_session` to resolve the per-session
    /// `session_cwd` on the resume path — the conversation is loaded
    /// only *after* the agent is built, so tools snapshot the right
    /// value. Pre-cwd files return `""`; the caller treats that as
    /// "no saved cwd" and falls back to the boot cwd.
    pub fn peek_conversation_cwd(&self, id: Uuid) -> anyhow::Result<String> {
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("conversation storage is not configured"))?;
        Ok(storage.load(id)?.cwd)
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
                        content,
                        compacted,
                        source,
                        timestamp,
                    } => {
                        let mut m = ChatMessage::user(content.clone());
                        m.compacted = *compacted;
                        m.source = source.clone();
                        m.timestamp = timestamp.with_timezone(&chrono::Local);
                        m
                    }
                    ConvMsg::Assistant {
                        content,
                        compacted,
                        source,
                        thinking,
                        timestamp,
                    } => {
                        let mut m = ChatMessage::agent(content.clone());
                        m.compacted = *compacted;
                        m.source = source.clone();
                        m.thinking = thinking.clone();
                        m.timestamp = timestamp.with_timezone(&chrono::Local);
                        m
                    }
                    ConvMsg::ToolCall {
                        tool_name,
                        arguments,
                        call_id,
                        compacted,
                        source,
                        timestamp,
                    } => {
                        let mut m = ChatMessage::tool_call(tool_name, arguments, call_id.clone());
                        m.compacted = *compacted;
                        m.source = source.clone();
                        m.timestamp = timestamp.with_timezone(&chrono::Local);
                        m
                    }
                    ConvMsg::ToolResult {
                        tool_name,
                        arguments,
                        result,
                        call_id,
                        compacted,
                        source,
                        timestamp,
                    } => {
                        let mut m =
                            ChatMessage::tool_result(tool_name, arguments, result, call_id.clone());
                        m.compacted = *compacted;
                        m.source = source.clone();
                        m.timestamp = timestamp.with_timezone(&chrono::Local);
                        m
                    }
                    ConvMsg::Summary { content, timestamp } => {
                        let mut m = ChatMessage::summary(content.clone());
                        m.timestamp = timestamp.with_timezone(&chrono::Local);
                        m
                    }
                })
                .collect();

            // Restore persisted session stats *before* dropping the conv guard
            // so we don't race with a concurrent save.
            self.stats.lock().unwrap().restore(
                conv.metadata.total_input_tokens,
                conv.metadata.total_output_tokens,
                conv.metadata.total_api_calls,
                conv.metadata.total_cost,
            );
            // total_input_tokens is lane-blind — when a sub-agent turn
            // persisted mid-delegation, restore() seeded the orchestrator
            // gate from it. Override with the lane-scoped field if present;
            // legacy files (None) keep the total_input_tokens fallback for free.
            if let Some(v) = conv.metadata.last_orchestrator_input_tokens {
                self.stats
                    .lock()
                    .unwrap()
                    .restore_orchestrator_input_tokens(v);
            }
            // Rehydrate the per-lane breakdown so the Session panel can scope to
            // a sub-agent on resume instead of reading zeros.
            self.stats
                .lock()
                .unwrap()
                .restore_lanes(conv.metadata.lanes.iter().map(|l| {
                    (
                        l.lane.clone(),
                        crate::hooks::LaneStats {
                            input_tokens: l.input_tokens,
                            output_tokens: l.output_tokens,
                            api_calls: l.api_calls,
                            cost: l.cost,
                        },
                    )
                }));

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
            self.sync_todo_to_ui();
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
                        source: msg.source.clone(),
                        timestamp: msg.timestamp.with_timezone(&chrono::Utc),
                    }),
                    MessageRole::Agent => Some(ConvMsg::Assistant {
                        content: msg.content.clone(),
                        compacted: msg.compacted,
                        source: msg.source.clone(),
                        thinking: msg.thinking.clone(),
                        timestamp: msg.timestamp.with_timezone(&chrono::Utc),
                    }),
                    MessageRole::ToolCall => {
                        let tool_name = msg.tool_name.clone()?;
                        let arguments = msg.tool_args.clone().unwrap_or_default();
                        Some(ConvMsg::ToolCall {
                            tool_name,
                            arguments,
                            call_id: msg.call_id.clone(),
                            compacted: msg.compacted,
                            source: msg.source.clone(),
                            timestamp: msg.timestamp.with_timezone(&chrono::Utc),
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
                            source: msg.source.clone(),
                            timestamp: msg.timestamp.with_timezone(&chrono::Utc),
                        })
                    }
                    MessageRole::Summary => Some(ConvMsg::Summary {
                        content: msg.content.clone(),
                        timestamp: msg.timestamp.with_timezone(&chrono::Utc),
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
            // Lane-scoped orchestrator signal must round-trip independently
            // from total_input_tokens — that field is lane-blind and gets
            // overwritten by a sub-agent's last request mid-delegation.
            conv.metadata.last_orchestrator_input_tokens = stats.last_orchestrator_input_tokens();
            conv.metadata.lanes = stats
                .lanes_sorted()
                .into_iter()
                .map(|(lane, s)| crate::conversation::LaneMetadata {
                    lane,
                    input_tokens: s.input_tokens,
                    output_tokens: s.output_tokens,
                    api_calls: s.api_calls,
                    cost: s.cost,
                })
                .collect();
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
        self.clear_pending_thinking();
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
        self.clear_pending_thinking();
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
        self.clear_pending_thinking();
        let msg = ChatMessage::user_with_attachments(content, attachments);
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist user message with attachments: {}", e);
        }
    }

    /// One place builds the assistant ChatMessage, so a new field can never be
    /// forgotten by half the callers. For the orchestrator lane, also
    /// consumes any `pending_thinking` staged by this turn's
    /// `SessionHook::on_completion_response` and attaches it to this message,
    /// avoiding a separate empty-content row (CONCERN 6).
    fn push_assistant(
        &self,
        source: MessageSource,
        content: String,
        thinking: Vec<crate::reasoning::ThinkingBlock>,
    ) {
        let is_orchestrator = source.is_orchestrator_lane();
        let mut msg = ChatMessage::agent(content).with_source(source);
        msg.thinking = if is_orchestrator {
            // Adopt whatever the turn's `CompletionResponse` staged, unless the
            // caller supplied blocks itself — an explicit argument always wins,
            // and `take_` clears the staging slot either way so the blocks can
            // never leak onto a later turn.
            let staged = self.take_pending_thinking();
            if thinking.is_empty() {
                staged
            } else {
                thinking
            }
        } else {
            // Sub-agent lane: its blocks ride on its own row and it must not
            // consume the orchestrator's staged set.
            thinking
        };
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist assistant message: {}", e);
        }
    }

    /// Add an assistant message to chat and persist.
    ///
    /// Compaction is **NOT** triggered here — see [`add_user_message`].
    pub fn add_assistant_message(&self, content: String) {
        self.push_assistant(MessageSource::Human, content, Vec::new());
    }

    /// Add an assistant message tagged with the producing lane, and persist.
    ///
    /// The orchestrator's own prose flows through [`Self::add_assistant_message`]
    /// (the `prompt_with_history` return value). A **sub-agent**'s prose has no
    /// such return path — its final text becomes the `delegate` ToolResult, and
    /// its intermediate prose would otherwise vanish. So `process_event_for_ui`
    /// calls this with the `SubAgent { role }` lane for sub-agent
    /// `CompletionResponse`s, surfacing that prose on its own `🧩 role` lane.
    /// Persistence keeps the lane on the serialized message.
    pub fn add_assistant_message_sourced(&self, source: MessageSource, content: String) {
        self.push_assistant(source, content, Vec::new());
    }

    /// Add an assistant message that carries captured Anthropic thinking
    /// blocks alongside its prose. Blocks are stored losslessly on the
    /// `ChatMessage` so `get_agent_history` can replay them into the same
    /// rig `Message::Assistant` as a `ToolCall`, per Anthropic's tool-loop
    /// contract.
    pub fn add_assistant_message_with_thinking(
        &self,
        source: MessageSource,
        content: String,
        thinking: Vec<crate::reasoning::ThinkingBlock>,
    ) {
        self.push_assistant(source, content, thinking);
    }

    /// Provider-gate bool set from `ProviderInfo.preserve_reasoning && provider == "anthropic"`.
    ///
    /// `get_agent_history` consults this when assembling the rig wire —
    /// false drops `Reasoning` content from any rebuild, true lets the
    /// captured blocks through. Lives on StateManager because the
    /// rebuild helper is the only seam that needs to know, and because
    /// `/model` rebuilds need a stable place to thread it (every test
    /// path exercises this directly).
    pub fn set_wire_reasoning(&self, on: bool) {
        *self.wire_reasoning.write().unwrap() = on;
    }

    /// Display-gate bool set from `ProviderInfo.display_reasoning`.
    ///
    /// When false (the default), every web snapshot broadcast strips
    /// `ChatMessage.thinking` from the cloned `AppState` before it
    /// reaches subscribers. Signatures are never transmitted either way.
    pub fn set_display_reasoning(&self, on: bool) {
        *self.display_reasoning.write().unwrap() = on;
    }

    /// Stage captured thinking blocks for the orchestrator's next assistant
    /// message. Called synchronously from `SessionHook::on_completion_response`:
    /// rig awaits that hook inline and `prompt_with_history` returns only
    /// afterwards, so the next `push_assistant`/`add_tool_call` is guaranteed
    /// to see what this staged.
    ///
    /// Replaces the slot rather than appending — a non-empty slot here always
    /// means a consumer was skipped, and accumulating would splice one
    /// response's reasoning onto another's row.
    pub fn stage_thinking_for_next_assistant(
        &self,
        thinking: Vec<crate::reasoning::ThinkingBlock>,
    ) {
        let mut pending = self.pending_thinking.write().unwrap();
        if !pending.is_empty() {
            tracing::warn!(
                dropped = pending.len(),
                "overwriting staged thinking blocks — a consumer row was skipped"
            );
        }
        *pending = thinking;
    }

    fn clear_pending_thinking(&self) {
        self.pending_thinking.write().unwrap().clear();
    }

    fn take_pending_thinking(&self) -> Vec<crate::reasoning::ThinkingBlock> {
        std::mem::take(&mut *self.pending_thinking.write().unwrap())
    }

    /// Add a tool call message to chat and persist immediately.
    ///
    /// `source` tags the producing lane: [`MessageSource::Human`] for the
    /// orchestrator, [`MessageSource::SubAgent`] for a sub-agent's turn (so
    /// the renderer labels it and `get_agent_history` filters it out of the
    /// orchestrator wire context).
    pub fn add_tool_call(
        &self,
        source: MessageSource,
        tool_name: String,
        args: String,
        call_id: Option<String>,
    ) {
        let is_orchestrator = source.is_orchestrator_lane();
        let mut msg = ChatMessage::tool_call(&tool_name, &args, call_id).with_source(source);
        if is_orchestrator {
            msg.thinking = self.take_pending_thinking();
        }
        self.update_chat(msg);
        if let Err(e) = self.persist_current() {
            tracing::error!("Failed to persist tool call: {}", e);
        }
    }

    /// Add a tool result message to chat and persist immediately.
    ///
    /// `source` tags the producing lane (see [`Self::add_tool_call`]).
    pub fn add_tool_result(
        &self,
        source: MessageSource,
        tool_name: String,
        args: String,
        result: String,
        call_id: Option<String>,
    ) {
        let msg = ChatMessage::tool_result(&tool_name, &args, &result, call_id).with_source(source);
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
    ///
    /// Messages are run through [`sanitize_tool_pairs`] first: this is the wire
    /// boundary, and an unpaired `ToolCall` here is a hard provider error, not
    /// a display glitch.
    ///
    /// [`sanitize_tool_pairs`]: crate::tool_use_validator::sanitize_tool_pairs
    pub fn get_agent_history(&self) -> Vec<rig_core::completion::message::Message> {
        use crate::ui::app_state::MessageRole;

        let state = self.state.read().unwrap();

        // If the very last message of the orchestrator's live context is a User
        // message, exclude it. It will be supplied separately as the prompt
        // argument to prompt_with_history() (or by the post-compaction
        // resumption path), so including it here would duplicate the current
        // turn. Only exclude it when it's truly trailing — if there are
        // assistant/tool messages after it, it's part of the conversation
        // history and must be kept. This applies even after compaction:
        // `build_current_turn_message` already returns the latest non-compacted
        // user message as the prompt, and `build_resumption_for_compaction`
        // supplies the last live message itself.
        let last_live = state
            .chat
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, msg)| msg.is_orchestrator_context());
        let skip_last_idx = last_live
            .filter(|(_, msg)| msg.role == MessageRole::User)
            .map(|(i, _)| i);

        let live: Vec<&crate::ui::app_state::ChatMessage> = state
            .chat
            .messages
            .iter()
            .enumerate()
            // Isolation boundary: a sub-agent's internal turns live in the
            // transcript (for display + persistence) but must NEVER enter the
            // orchestrator model's context. `is_orchestrator_context()` is the
            // single definition of this set — see also `uncompacted_message_count`,
            // `ContextManager::compact`, `build_resumption_for_compaction`.
            .filter(|(_, msg)| msg.is_orchestrator_context())
            .filter(|(i, _)| Some(*i) != skip_last_idx)
            .map(|(_, msg)| msg)
            .collect();

        // Last stop before the wire: concurrent appends (the bg drain seam vs.
        // the event-processor task) can split a ToolCall/ToolResult pair, and
        // every provider 400s on that — permanently, for the rest of the
        // conversation. Dropping the broken pair here self-heals instead.
        let sanitized: Vec<crate::ui::app_state::ChatMessage> =
            crate::tool_use_validator::sanitize_tool_pairs(live)
                .into_iter()
                .cloned()
                .collect();

        // Read the cross-provider wire gate (design §3.4). When off, no
        // ThinkingBlock survives the rebuild — even a Claude-transcript
        // loaded under a non-Anthropic provider. The capture seam already
        // prevents fresh captures outside Anthropic, so this guards
        // `/load` on a foreign provider.
        let wire_reasoning = *self.wire_reasoning.read().unwrap();

        Self::convert_history_to_rig(&sanitized, wire_reasoning)
    }

    fn last_msg_to_rig(
        msg: &crate::ui::app_state::ChatMessage,
    ) -> rig_core::completion::message::Message {
        use crate::ui::app_state::MessageRole;
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, Text, ToolCall, ToolFunction, ToolResult,
            ToolResultContent, UserContent,
        };
        use rig_core::one_or_many::OneOrMany;
        match msg.role {
            MessageRole::User => RigMessage::User {
                content: user_content_from_chat_message(msg),
            },
            MessageRole::Agent => RigMessage::Assistant {
                id: None,
                content: OneOrMany::one(AssistantContent::Text(Text::new(msg.content.clone()))),
            },
            MessageRole::ToolCall => {
                let tool_name = match msg.tool_name.as_deref() {
                    Some(n) => n,
                    None => {
                        return RigMessage::Assistant {
                            id: None,
                            content: OneOrMany::one(AssistantContent::Text(Text::new(
                                msg.content.clone(),
                            ))),
                        };
                    }
                };
                let args_str = msg.tool_args.as_deref().unwrap_or("{}");
                let arguments = serde_json::from_str(args_str)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                RigMessage::Assistant {
                    id: None,
                    content: OneOrMany::one(AssistantContent::ToolCall(ToolCall::new(
                        call_id,
                        ToolFunction::new(tool_name.to_string(), arguments),
                    ))),
                }
            }
            MessageRole::ToolResult => {
                let tool_name = match msg.tool_name.as_deref() {
                    Some(n) => n,
                    None => {
                        return RigMessage::User {
                            content: OneOrMany::one(UserContent::Text(Text::new(
                                msg.content.clone(),
                            ))),
                        };
                    }
                };
                let result_text = msg.tool_result.as_deref().unwrap_or("");
                let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());
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
                    msg.content
                )))),
            },
            MessageRole::System => RigMessage::User {
                content: OneOrMany::one(UserContent::Text(Text::new(msg.content.clone()))),
            },
        }
    }

    /// Convert a sanitised chat slice into the rig wire array.
    ///
    /// The conversion has two shapes:
    ///   - **Thinking-bearing assistant run** (Agent + 1+ ToolCall(s)
    ///     carrying at least one `ThinkingBlock`): coalesced into ONE
    ///     `RigMessage::Assistant` whose content order is
    ///     `[Reasoning…, Text?, ToolCall…]`. Anthropic's wire contract.
    ///   - **No thinking**: one `RigMessage::Assistant` per Agent row and
    ///     one per ToolCall row — byte-identical to the pre-change
    ///     output for every non-Anthropic provider and every knob-off
    ///     run.
    ///
    /// `wire_reasoning=false` collapses the first case into the second
    /// (all blocks filtered) so a foreign provider running a Claude
    /// transcript cannot 400.
    fn convert_history_to_rig(
        sanitized: &[crate::ui::app_state::ChatMessage],
        wire_reasoning: bool,
    ) -> Vec<rig_core::completion::message::Message> {
        use crate::ui::app_state::{ChatMessage, MessageRole};
        use rig_core::completion::message::{
            AssistantContent, Message as RigMessage, Reasoning, Text, ToolCall, ToolFunction,
            ToolResult, ToolResultContent, UserContent,
        };
        use rig_core::one_or_many::OneOrMany;

        let mut out: Vec<RigMessage> = Vec::new();
        let mut i = 0;
        while i < sanitized.len() {
            let msg = &sanitized[i];
            match msg.role {
                MessageRole::User => {
                    out.push(RigMessage::User {
                        content: user_content_from_chat_message(msg),
                    });
                    i += 1;
                }
                MessageRole::Summary => {
                    out.push(RigMessage::User {
                        content: OneOrMany::one(UserContent::Text(Text::new(format!(
                            "[Conversation summary] {}",
                            msg.content
                        )))),
                    });
                    i += 1;
                }
                MessageRole::System => {
                    i += 1;
                }
                MessageRole::Agent | MessageRole::ToolCall => {
                    let run_start = i;
                    let mut j = i;
                    while j < sanitized.len() {
                        match sanitized[j].role {
                            MessageRole::Agent
                            | MessageRole::ToolCall
                            | MessageRole::ToolResult => j += 1,
                            _ => break,
                        }
                    }
                    let run = &sanitized[run_start..j];

                    let agent_row = run.iter().find(|m| m.role == MessageRole::Agent);
                    let tool_call_rows: Vec<&ChatMessage> = run
                        .iter()
                        .filter(|m| m.role == MessageRole::ToolCall)
                        .collect();
                    let tool_result_rows: Vec<&ChatMessage> = run
                        .iter()
                        .filter(|m| m.role == MessageRole::ToolResult)
                        .collect();

                    let blocks: Vec<crate::reasoning::ThinkingBlock> = run
                        .iter()
                        .flat_map(|m| m.thinking.iter().cloned())
                        .collect();
                    let blocks_kept: Vec<crate::reasoning::ThinkingBlock> = if wire_reasoning {
                        blocks
                            .into_iter()
                            .filter(|b| match b {
                                crate::reasoning::ThinkingBlock::Thinking { signature, .. } => {
                                    !signature.is_empty()
                                }
                                crate::reasoning::ThinkingBlock::Redacted { .. } => true,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };

                    if blocks_kept.is_empty() {
                        // No-thinking path — must be BYTE-IDENTICAL to the pre-change
                        // per-message rebuild (design §3.2: "byte-identical"). Iterate
                        // the run in transcript order and emit each row on its own,
                        // matching the legacy single-message-per-ChatMessage shape.
                        // The reasoning-bearing branch below is the only one that
                        // coalesces. ToolResults are emitted inline here so the
                        // transcript order matches the legacy per-row output; the
                        // after-block re-emission below is therefore skipped (the
                        // thinking-bearing branch is the only caller that needs it).
                        for m in run {
                            match m.role {
                                MessageRole::Agent => {
                                    out.push(RigMessage::Assistant {
                                        id: None,
                                        content: OneOrMany::one(AssistantContent::Text(Text::new(
                                            m.content.clone(),
                                        ))),
                                    });
                                }
                                MessageRole::ToolCall => {
                                    let Some(tool_name) = m.tool_name.as_deref() else {
                                        continue;
                                    };
                                    let args_str = m.tool_args.as_deref().unwrap_or("{}");
                                    let arguments = serde_json::from_str(args_str).unwrap_or(
                                        serde_json::Value::Object(serde_json::Map::new()),
                                    );
                                    let call_id =
                                        m.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                                    out.push(RigMessage::Assistant {
                                        id: None,
                                        content: OneOrMany::one(AssistantContent::ToolCall(
                                            ToolCall::new(
                                                call_id,
                                                ToolFunction::new(tool_name.to_string(), arguments),
                                            ),
                                        )),
                                    });
                                }
                                MessageRole::ToolResult => {
                                    let Some(tool_name) = m.tool_name.as_deref() else {
                                        continue;
                                    };
                                    let result_text = m.tool_result.as_deref().unwrap_or("");
                                    let call_id =
                                        m.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                                    out.push(RigMessage::User {
                                        content: OneOrMany::one(UserContent::ToolResult(
                                            ToolResult {
                                                id: call_id,
                                                call_id: None,
                                                content: ToolResultContent::from_tool_output(
                                                    result_text,
                                                ),
                                            },
                                        )),
                                    });
                                }
                                _ => {}
                            }
                        }
                    } else {
                        let mut content: Vec<AssistantContent> = Vec::new();
                        for b in &blocks_kept {
                            match b {
                                crate::reasoning::ThinkingBlock::Thinking { text, signature } => {
                                    content.push(AssistantContent::Reasoning(
                                        Reasoning::new_with_signature(
                                            text,
                                            Some(signature.clone()),
                                        ),
                                    ));
                                }
                                crate::reasoning::ThinkingBlock::Redacted { data } => {
                                    content.push(AssistantContent::Reasoning(Reasoning::redacted(
                                        data.clone(),
                                    )));
                                }
                            }
                        }
                        if let Some(agent) = agent_row
                            && !agent.content.is_empty()
                        {
                            content.push(AssistantContent::Text(Text::new(agent.content.clone())));
                        }
                        for tc in &tool_call_rows {
                            let Some(tool_name) = tc.tool_name.as_deref() else {
                                continue;
                            };
                            let args_str = tc.tool_args.as_deref().unwrap_or("{}");
                            let arguments = serde_json::from_str(args_str)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            let call_id =
                                tc.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                            content.push(AssistantContent::ToolCall(ToolCall::new(
                                call_id,
                                ToolFunction::new(tool_name.to_string(), arguments),
                            )));
                        }
                        out.push(RigMessage::Assistant {
                            id: None,
                            content: OneOrMany::many(content)
                                .expect("non-empty: at least one thinking block"),
                        });

                        // Thinking-bearing path: ToolResults stay as separate
                        // Message::User entries (not coalesced into the
                        // Assistant message — only Thinking+Text+ToolCall go
                        // together per the Anthropic wire contract).
                        for tr in &tool_result_rows {
                            let Some(tool_name) = tr.tool_name.as_deref() else {
                                continue;
                            };
                            let result_text = tr.tool_result.as_deref().unwrap_or("");
                            let call_id =
                                tr.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                            out.push(RigMessage::User {
                                content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                                    id: call_id,
                                    call_id: None,
                                    content: ToolResultContent::from_tool_output(result_text),
                                })),
                            });
                        }
                    }

                    i = j;
                }
                MessageRole::ToolResult => {
                    let tool_name = match msg.tool_name.as_deref() {
                        Some(n) => n,
                        None => {
                            i += 1;
                            continue;
                        }
                    };
                    let result_text = msg.tool_result.as_deref().unwrap_or("");
                    let call_id = msg.call_id.clone().unwrap_or_else(|| tool_name.to_string());
                    out.push(RigMessage::User {
                        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                            id: call_id,
                            call_id: None,
                            content: ToolResultContent::from_tool_output(result_text),
                        })),
                    });
                    i += 1;
                }
            }
        }
        out
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
            .find(|m| m.is_orchestrator_context() && m.role == MessageRole::User)?;

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
        let state = self.state.read().unwrap();
        let messages = &state.chat.messages;

        // Find the last message of the orchestrator's live context (whatever its role)
        let last_live = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.is_orchestrator_context());

        let (last_idx, last_msg) = last_live?;

        // If there's only one message, it's a fresh turn — not a mid-action
        // resumption. Return None so the caller uses the normal path.
        if last_idx == 0 {
            return None;
        }

        // ── Build history (design §6.1 mirror): the same helper as
        // `get_agent_history` so a post-compaction resume replays survivors'
        // thinking blocks in the same thinking-first wire order — the
        // one place where forgetting the change produces a live 400.
        let history_msgs: Vec<crate::ui::app_state::ChatMessage> = messages[..last_idx]
            .iter()
            .filter(|m| m.is_orchestrator_context())
            .cloned()
            .collect();
        let wire_reasoning = *self.wire_reasoning.read().unwrap();
        let history = Self::convert_history_to_rig(&history_msgs, wire_reasoning);

        // ── Build prompt: the last message converted to a rig Message ───────
        // Mirror the pre-change shape — for a single message at the tail of
        // the transcript, the resumption helper keeps it as a one-element
        // rig message regardless of whether it carries thinking.
        let prompt = Self::last_msg_to_rig(last_msg);

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

    /// The session's working directory — the single source of truth that
    /// path-aware tools resolve against and shells spawn in. Cloned per read
    /// (tools snapshot it at agent-build time; reads are rare).
    pub fn session_cwd(&self) -> std::path::PathBuf {
        self.session_cwd.read().unwrap().clone()
    }

    /// Replace the session's working directory. Called only at session
    /// boundaries (construction, `/cd`, `/load`); the caller rebuilds the
    /// agent afterwards so its tools re-snapshot the new value. Never mutates
    /// the process-global cwd.
    pub fn set_session_cwd(&self, cwd: std::path::PathBuf) {
        *self.session_cwd.write().unwrap() = cwd;
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

    /// Number of bg PTY children still running under this session. Feeds the
    /// web reaper's quiescence check (#158): a session with a live bg child
    /// is "working" and must not be reaped even with no sockets attached.
    pub fn bg_running_count(&self) -> usize {
        self.bg.lock().unwrap().running_count()
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

    /// Kill what the current turn owns and report what died.
    ///
    /// Steps (in order — order matters because the cancel must precede the
    /// state-reset so a tool mid-await observes the cancelled token first):
    /// 1. snapshot `shell = state.bash_panel.is_running()` — captured
    ///    **before** the kill so the rendered message and the kill are in
    ///    lock-step;
    /// 2. `self.turn_cancel.read().unwrap().cancel()` — drops the turn's
    ///    future at its next poll, unwinding the wire request, the tool call,
    ///    the sub-agent, and — via `PtyHandle::drop` — the foreground PTY
    ///    child (design §0.1 / T7 keystone);
    /// 3. `self.clear_bash_stdin_tx()` then `self.reset_bash_panel()` —
    ///    both idempotent.
    ///
    /// Background (`bash_bg`) processes are deliberately spared: Stop does
    /// not touch them. The rebuild paths (`/new`, `/model`, `/load`, `/cd`,
    /// shutdown) call [`StateManager::clear_bg`] directly.
    ///
    /// Idempotent: a second call kills nothing and returns
    /// `StopTally::default()` (the cancel of an already-cancelled token is a
    /// no-op, the cleared-stdin and reset-panel are no-ops on their
    /// already-cleaned-up states).
    pub fn stop_turn_processes(&self) -> StopTally {
        let shell = self.state.read().unwrap().bash_panel.is_running();
        self.turn_cancel.read().unwrap().cancel();
        self.clear_bash_stdin_tx();
        self.reset_bash_panel();
        StopTally { shell }
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
    use std::path::PathBuf;
    // Only the deadlock probes need this — the production state moved off
    // atomics onto `AppState`.
    use std::sync::atomic::AtomicBool;

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
            peakbot_version: "test".to_string(),
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
            peakbot_version: String::new(),
        });
        sm.update_welcome_cwd(std::path::PathBuf::from("/new/dir"));
        let w = sm.get_state().welcome.expect("welcome set");
        assert_eq!(w.cwd, std::path::PathBuf::from("/new/dir"));
    }

    #[test]
    fn test_session_cwd_defaults_to_process_cwd() {
        let sm = StateManager::new();
        assert_eq!(sm.session_cwd(), std::env::current_dir().unwrap());
    }

    #[test]
    fn test_set_session_cwd_replaces_value() {
        let sm = StateManager::new();
        let target = std::path::PathBuf::from("/some/session/dir");
        sm.set_session_cwd(target.clone());
        assert_eq!(sm.session_cwd(), target);
    }

    /// Contract for `/cd` and `/load` at the SM level. The handlers mutate
    /// `sm.session_cwd` and persist a new `cwd` on the freshly-minted
    /// conversation — *without* touching the process-global cwd. This test
    /// exercises the same SM-level operations the handlers do and asserts
    /// (a) the SM's `session_cwd` reflects the new value, (b) the new
    /// conversation's persisted `cwd` reflects it too, and (c) the process
    /// cwd is byte-for-byte unchanged across the whole sequence. The grep
    /// test `no_set_current_dir_in_src` (in `tests/scenarios`) is the
    /// source-level lock for the same invariant.
    #[test]
    fn test_cd_handlers_never_mutate_process_cwd() {
        use crate::storage::InMemoryStorage;

        let storage: Arc<dyn crate::storage::ConversationStorage> =
            Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());
        // Seed at the process cwd so the seed value is whatever the test
        // process happens to be in — the point is the *delta*.
        sm.set_session_cwd(std::env::current_dir().unwrap());

        let process_cwd_before = std::env::current_dir().unwrap();
        let target = std::path::PathBuf::from("/new/session/tree/for/cd");

        // Same operations `/cd` performs: flip the SM's session_cwd, mint
        // a new conversation persisting the new cwd, stamp wire identity.
        sm.set_session_cwd(target.clone());
        sm.create_conversation(
            "after /cd".into(),
            "test-prov".into(),
            "test-model".into(),
            target.to_string_lossy().into_owned(),
        );

        // (a) SM reflects the new session cwd.
        assert_eq!(
            sm.session_cwd(),
            target,
            "SM.session_cwd must reflect the post-/cd value"
        );
        // (b) The new conversation persists the new cwd.
        let conv = sm.get_current_conversation().expect("current conversation");
        assert_eq!(
            PathBuf::from(&conv.cwd),
            target,
            "freshly-minted conversation's cwd must equal the new session cwd"
        );
        // (c) The process-global cwd is byte-for-byte unchanged.
        assert_eq!(
            std::env::current_dir().unwrap(),
            process_cwd_before,
            "session-cwd flip must not mutate the process-global cwd"
        );
    }

    /// `peek_conversation_cwd` returns the saved cwd without loading the
    /// conversation. The current slot stays untouched — important because
    /// `create_session` calls this on the resume path *before* building the
    /// agent, and a partial-state teardown there would be a real desync.
    #[test]
    fn test_peek_conversation_cwd_returns_saved_cwd() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let saved = Conversation::new(
            "peeking".into(),
            "test-prov".into(),
            "test-model".into(),
            "/saved/cwd/from/conversation".into(),
        );
        let id = saved.id;
        storage.save(&saved).unwrap();

        // No current conversation yet — peek must still succeed.
        assert!(!sm.has_current_conversation());
        assert_eq!(
            sm.peek_conversation_cwd(id).unwrap(),
            "/saved/cwd/from/conversation"
        );
        assert!(
            !sm.has_current_conversation(),
            "peek must not load the conversation"
        );
    }

    /// `peek_conversation_cwd` is an Err (not a panic) when storage is
    /// disabled — the caller falls back to the boot cwd.
    #[test]
    fn test_peek_conversation_cwd_no_storage_errors() {
        let sm = StateManager::new();
        let bogus = Uuid::new_v4();
        assert!(sm.peek_conversation_cwd(bogus).is_err());
    }

    /// Pre-cwd files persist `cwd = ""`. `peek_conversation_cwd` returns
    /// that empty string verbatim — the caller (`create_session`) treats
    /// it as "no saved cwd" and falls back to the boot cwd.
    #[test]
    fn test_peek_conversation_cwd_returns_empty_for_pre_cwd_files() {
        use crate::storage::InMemoryStorage;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let pre_cwd = Conversation::new(
            "legacy".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(), // pre-cwd file
        );
        let id = pre_cwd.id;
        storage.save(&pre_cwd).unwrap();

        assert_eq!(sm.peek_conversation_cwd(id).unwrap(), "");
    }

    /// `create_conversation` stamps the session's pipeline selection onto every
    /// conversation it mints, so the choice survives persistence/reload — and
    /// `/new` keeps the current team (D2).
    #[test]
    fn create_conversation_stamps_pipeline_selection() {
        let sm = StateManager::new();

        // Default session: no pipeline.
        sm.create_conversation("a".into(), "prov".into(), "model".into(), String::new());
        assert!(sm.get_current_conversation().unwrap().pipeline.is_none());

        // A session with a selection → the minted conversation carries it.
        sm.set_selected_pipeline(Some("web-team".into()));
        sm.create_conversation("b".into(), "prov".into(), "model".into(), String::new());
        assert_eq!(
            sm.get_current_conversation().unwrap().pipeline.as_deref(),
            Some("web-team")
        );
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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.123);
        let state = sm.get_state();
        assert_eq!(state.stats.total_input_tokens, 100);
        assert_eq!(state.stats.total_output_tokens, 50);
        assert_eq!(state.stats.total_api_calls, 1);
        assert!((state.stats.total_cost - 0.123).abs() < f64::EPSILON);
    }

    /// Per-lane stats reach the live `AppState` so the web Agents panel can
    /// scope `/stats` to a role. Orchestrator (Human) and a sub-agent role
    /// bucket separately; the orchestrator lane sorts first.
    #[test]
    fn add_request_exposes_lane_breakdown_on_state() {
        use crate::ui::app_state::MessageSource;
        let sm = StateManager::new();
        sm.add_request(&MessageSource::Human, 100, 40, 0.10);
        sm.add_request(
            &MessageSource::SubAgent {
                role: "researcher".into(),
            },
            200,
            80,
            0.20,
        );

        let lanes = sm.get_state().stats.lanes;
        // Orchestrator first (Human buckets under "orchestrator"), then the role.
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].lane, "orchestrator");
        assert_eq!(lanes[0].input_tokens, 100);
        assert!((lanes[0].cost - 0.10).abs() < f64::EPSILON);

        assert_eq!(lanes[1].lane, "researcher");
        assert_eq!(lanes[1].input_tokens, 200);
        assert_eq!(lanes[1].api_calls, 1);
        assert!((lanes[1].cost - 0.20).abs() < f64::EPSILON);
    }

    /// A fresh session's lane rows must be true cumulative sums by the time
    /// they reach the wire, and the model must be named per role. This is the
    /// whole Session-panel contract: every row's `in ÷ calls` stays a plausible
    /// per-request size, and no lane's tokens are a single request's count.
    #[test]
    fn fresh_session_lane_rows_are_cumulative_and_carry_the_model() {
        use crate::pipeline::PipelineInfo;
        use crate::ui::app_state::MessageSource;
        let sm = StateManager::new();
        sm.set_model_alias("opus-5".to_string());
        // Role models are derived from the selected pipeline's catalogue entry.
        sm.set_pipelines(vec![PipelineInfo {
            name: "web-team".to_string(),
            orchestrator_model: "opus-5".to_string(),
            members: vec![("junior".to_string(), "qwen3.6-27B".to_string())],
        }]);
        sm.set_selected_pipeline(Some("web-team".to_string()));

        let junior = MessageSource::SubAgent {
            role: "junior".into(),
        };
        // Three orchestrator requests and two junior ones, all realistic sizes.
        sm.add_request(&MessageSource::Human, 4_000, 200, 0.01);
        sm.add_request(&junior, 5_000, 300, 0.02);
        sm.add_request(&MessageSource::Human, 4_500, 250, 0.01);
        sm.add_request(&junior, 6_000, 400, 0.02);
        sm.add_request(&MessageSource::Human, 5_000, 100, 0.01);

        let lanes = sm.get_state().stats.lanes;
        let orch = &lanes[0];
        assert_eq!(orch.lane, "orchestrator");
        assert_eq!(orch.input_tokens, 13_500, "4000 + 4500 + 5000");
        assert_eq!(orch.output_tokens, 550, "200 + 250 + 100");
        assert_eq!(orch.api_calls, 3);
        assert_eq!(
            orch.model, "opus-5",
            "the orchestrator row derives its model from the active alias"
        );

        let jr = &lanes[1];
        assert_eq!(jr.lane, "junior");
        assert_eq!(jr.input_tokens, 11_000, "5000 + 6000");
        assert_eq!(jr.output_tokens, 700, "300 + 400");
        assert_eq!(jr.api_calls, 2);
        assert_eq!(
            jr.model, "qwen3.6-27B",
            "roles carry their configured model"
        );

        // The regression guard: last-request semantics would put in/call far
        // below one request's size (the 36-tokens-per-call symptom).
        for l in &lanes {
            let per_call = l.input_tokens / l.api_calls;
            assert!(
                per_call >= 4_000,
                "lane {} shows {per_call} input tokens per call — tokens are not accumulating",
                l.lane
            );
        }
    }

    /// The opt-in lock signal ignores system banners: a fresh conversation
    /// showing only a welcome/warning is still "not started".
    #[test]
    fn conversation_has_turns_ignores_system_messages() {
        let sm = StateManager::new();
        assert!(!sm.conversation_has_turns());

        sm.add_system_message("welcome banner".to_string());
        assert!(
            !sm.conversation_has_turns(),
            "system banners must not count as a turn"
        );

        sm.add_user_message("hello".to_string());
        assert!(sm.conversation_has_turns());
    }

    #[test]
    fn test_stats_accumulation() {
        let sm = StateManager::new();
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.10);
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 200, 100, 0.20);
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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.10);
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
        sm.create_conversation(
            "Session A".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 1234, 567, 0.42);
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 2000, 800, 0.10); // cost accumulates → 0.52
        let conv_a_id = sm.get_current_conversation_id().expect("convo A id");
        sm.save_conversation();

        // Switch to a fresh session and pile up unrelated stats — these are
        // what the buggy /load would leave behind in the status bar.
        sm.clear_history();
        sm.reset_stats();
        sm.create_conversation(
            "Session B".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 99, 99, 9.99);

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

    /// Regression: the orchestrator-scoped context meter must survive a
    /// save/load round-trip even when the *last* persisted request was a
    /// sub-agent's.
    ///
    /// Today `SessionStats::total_input_tokens` is lane-blind — it stores
    /// the input tokens of the last request on *any* lane — and
    /// `StateManager::sync_to_conversation` snapshots that value into
    /// `conv.metadata.total_input_tokens` on every persist. When the
    /// orchestrator's last API response was small (4_000) and a subsequent
    /// sub-agent's last API response was large (60_000), the metadata
    /// records 60_000. On `/load`, `SessionStats::restore` seeds
    /// `last_orchestrator_input_tokens = Some(total_input_tokens)` from
    /// that lane-blind value, and `sync_stats_to_ui` copies it into
    /// `AppState.context.current_usage`. Result: the status bar shows the
    /// sub-agent's context size (60_000), not the orchestrator's (4_000),
    /// until the orchestrator's next response arrives.
    ///
    /// Scenario: orchestrator turn → sub-agent turn → save → fresh
    /// `StateManager` loads the same conversation → assert the meter
    /// shows the orchestrator's last input, not the sub-agent's.
    #[test]
    fn orchestrator_context_meter_does_not_show_subagent_after_save_load_reload() {
        use crate::storage::InMemoryStorage;
        use crate::ui::app_state::MessageSource;

        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        // 1. Mint a fresh conversation so we have something to persist.
        sm.create_conversation(
            "orch-vs-sub".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );

        // 2. Orchestrator turn — this is what the meter must keep showing.
        sm.add_request(&MessageSource::Human, 4_000, 200, 0.01);
        // 3. Sub-agent turn afterwards — must not move the orchestrator signal.
        sm.add_request(
            &MessageSource::SubAgent {
                role: "researcher".into(),
            },
            60_000,
            400,
            0.02,
        );

        // Pre-persist sanity: the orchestrator-scoped signal must be the
        // orchestrator's last input (4_000), not the sub-agent's (60_000).
        // This part already works today; it's the load-side bug under test.
        assert_eq!(
            sm.stats_arc()
                .lock()
                .unwrap()
                .last_orchestrator_input_tokens(),
            Some(4_000),
            "sub-agent request must not move the orchestrator-scoped signal"
        );

        // 4. Persist the conversation (this snapshots stats into metadata).
        sm.save_conversation();
        let conv_id = sm.get_current_conversation_id().expect("convo id");

        // 5. Fresh session loading the same conversation from storage.
        let sm2 = StateManager::new_arc_with_storage(storage.clone());
        sm2.load_conversation(conv_id).expect("load conversation");

        // 6a. The context meter (status bar / web Session panel) must show
        // the orchestrator's last input (4_000), not the sub-agent's 60_000.
        // Today `sync_stats_to_ui` copies the lane-blind `last_orchestrator_input_tokens`
        // (poisoned by `restore` reading `total_input_tokens`) into
        // `context.current_usage` — this assertion is what fails RED.
        let state = sm2.get_state();
        assert_eq!(
            state.context.current_usage, 4_000,
            "context meter must show the orchestrator's last input (4_000) \
             after save/load, not the sub-agent's 60_000 (got {})",
            state.context.current_usage
        );

        // 6b. The SessionStats-level signal must also be the orchestrator's
        // value, not poisoned by the lane-blind `total_input_tokens` snapshot.
        let stats_arc = sm2.stats_arc();
        let loaded = stats_arc.lock().unwrap();
        assert_eq!(
            loaded.last_orchestrator_input_tokens(),
            Some(4_000),
            "loaded SessionStats must seed last_orchestrator_input_tokens from \
             the orchestrator lane, not from the lane-blind total_input_tokens \
             (got {:?})",
            loaded.last_orchestrator_input_tokens()
        );
    }

    /// Back-compat pin for the orchestrator-scope fix: a
    /// `ConversationMetadata` JSON written before the fix (no
    /// `last_orchestrator_input_tokens` field) must (a) still deserialize,
    /// and (b) the `restore` path must still seed the orchestrator signal
    /// from `total_input_tokens` rather than losing it entirely. This test
    /// passes both before and after the fix — its purpose is to keep the
    /// fix implementer from over-correcting and leaving legacy files with
    /// `last_orchestrator_input_tokens() == None` after load.
    #[test]
    fn conversation_metadata_without_orchestrator_field_falls_back_to_total_input_tokens() {
        use crate::conversation::{Conversation, ConversationMetadata};
        use crate::storage::InMemoryStorage;

        let legacy = r#"{
            "message_count": 1,
            "total_input_tokens": 999,
            "total_output_tokens": 50,
            "total_api_calls": 1,
            "total_cost": 0.01
        }"#;

        // (a) Back-compat deserialize: a JSON without the new field must
        // parse cleanly (the fix must add `#[serde(default)]`).
        let meta: ConversationMetadata = serde_json::from_str(legacy)
            .expect("legacy metadata without orchestrator field must still deserialize");

        // (b) Load-path back-compat: with the new field absent, restore
        // must seed `last_orchestrator_input_tokens = Some(total_input_tokens)`
        // (the old behaviour) so a legacy file's orchestrator signal isn't
        // silently dropped to None.
        let storage = Arc::new(InMemoryStorage::default());
        let mut conv = Conversation::new(
            "legacy".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        conv.metadata = meta;
        let id = conv.id;
        storage.save(&conv).expect("save legacy");

        let sm2 = StateManager::new_arc_with_storage(storage.clone());
        sm2.load_conversation(id).expect("load legacy");

        let stats = sm2.stats_arc();
        let stats = stats.lock().unwrap();
        assert_eq!(
            stats.last_orchestrator_input_tokens(),
            Some(999),
            "back-compat: when last_orchestrator_input_tokens is absent, restore \
             must fall back to Some(total_input_tokens) so the orchestrator \
             signal isn't lost (got {:?})",
            stats.last_orchestrator_input_tokens()
        );
    }

    /// `ensure_boot_conversation` mints exactly one conversation from the
    /// stamped wire identity when none exists. The caller-supplied cwd is
    /// persisted verbatim — no internal `current_dir()` read.
    #[test]
    fn ensure_boot_conversation_mints_from_wire_identity() {
        let sm = StateManager::new_arc();
        sm.set_provider_name("openrouter".into());
        sm.set_model("anthropic/claude-3.7-sonnet".into());
        assert!(!sm.has_current_conversation());

        sm.ensure_boot_conversation(std::path::Path::new("/caller/passed/cwd"), "fallback-model");

        let conv = sm.get_current_conversation().expect("minted");
        assert_eq!(conv.provider_name, "openrouter");
        assert_eq!(conv.model, "anthropic/claude-3.7-sonnet");
        assert_eq!(conv.cwd, "/caller/passed/cwd");
    }

    /// Idempotent: a second call does not replace an existing conversation
    /// (so a pre-created or resumed session is never clobbered).
    #[test]
    fn ensure_boot_conversation_is_idempotent() {
        let sm = StateManager::new_arc();
        sm.set_model("m".into());
        sm.ensure_boot_conversation(std::path::Path::new("/first/cwd"), "fallback");
        let id = sm.get_current_conversation_id().expect("first mint");

        sm.ensure_boot_conversation(std::path::Path::new("/second/cwd"), "fallback");
        assert_eq!(
            sm.get_current_conversation_id(),
            Some(id),
            "second call must not mint a new conversation"
        );
        // The second call's cwd must not have replaced the first.
        assert_eq!(sm.get_current_conversation().unwrap().cwd, "/first/cwd");
    }

    /// The fallback model is used only when no model was stamped (harness path).
    #[test]
    fn ensure_boot_conversation_uses_fallback_when_model_empty() {
        let sm = StateManager::new_arc();
        // No set_model → get_model() is empty → fallback applies.
        sm.ensure_boot_conversation(std::path::Path::new("/cwd"), "fallback-model");
        assert_eq!(
            sm.get_current_conversation().expect("minted").model,
            "fallback-model"
        );
    }

    /// `create_conversation` persists the caller-supplied cwd argument
    /// (`cwd` is an explicit constructor parameter, not an implicit
    /// `std::env::current_dir()` read). The persisted value is the exact
    /// string passed in, not whatever the process cwd is.
    #[test]
    fn create_conversation_persists_explicit_cwd() {
        let sm = StateManager::new_arc();
        sm.set_provider_name("openrouter".into());
        sm.set_model("anthropic/claude-3.7-sonnet".into());

        let explicit = "/explicit/cwd/from/the/caller";
        sm.create_conversation(
            "Test".into(),
            "openrouter".into(),
            "anthropic/claude-3.7-sonnet".into(),
            explicit.into(),
        );

        let conv = sm.get_current_conversation().expect("minted");
        assert_eq!(conv.cwd, explicit);
    }

    /// `create_conversation` with the empty string persists empty (the
    /// same default `#[serde(default)]` provides for pre-cwd files).
    #[test]
    fn create_conversation_persists_empty_cwd() {
        let sm = StateManager::new_arc();
        sm.set_provider_name("openrouter".into());
        sm.set_model("m".into());

        sm.create_conversation(
            "Test".into(),
            "openrouter".into(),
            "m".into(),
            String::new(),
        );

        let conv = sm.get_current_conversation().expect("minted");
        assert_eq!(conv.cwd, "");
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
        sm.create_conversation(
            "Session A".into(),
            "prov".into(),
            "model-a".into(),
            String::new(),
        );
        let a_id = sm.get_current_conversation_id().expect("A id");
        let mirror = sm.get_state().conversation.expect("mirror after create");
        assert_eq!(mirror.id, a_id.to_string());
        assert_eq!(mirror.model, "model-a");
        sm.save_conversation();

        // Move to B, then load A back → mirror must follow to A's id.
        sm.create_conversation(
            "Session B".into(),
            "prov".into(),
            "model-b".into(),
            String::new(),
        );
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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.01);

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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.01);

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
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
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

    /// Test that get_agent_history() produces proper rig ToolCall messages (not text approximations).
    /// The result is added too because an orphan call is dropped at the wire boundary.
    #[test]
    fn test_get_agent_history_tool_call_is_structured() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage};

        let sm = StateManager::new();
        sm.add_tool_call(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls -la"}"#.to_string(),
            Some("call_42".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls -la"}"#.to_string(),
            "ok".to_string(),
            Some("call_42".to_string()),
        );

        let history = sm.get_agent_history();
        assert_eq!(history.len(), 2);

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

    /// Test that get_agent_history() produces proper rig ToolResult messages.
    /// The call is added too because an orphan result is dropped at the wire boundary.
    #[test]
    fn test_get_agent_history_tool_result_is_structured() {
        use rig_core::completion::message::{
            Message as RigMessage, ToolResultContent, UserContent,
        };

        let sm = StateManager::new();
        sm.add_tool_call(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_42".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "file1.txt\nfile2.txt".to_string(),
            Some("call_42".to_string()),
        );

        let history = sm.get_agent_history();
        assert_eq!(history.len(), 2);

        match &history[1] {
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
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
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
            String::new(),
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

        let conv = Conversation::new("test".into(), "prov".into(), "model".into(), String::new());
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

    /// Pin /load survival of an orchestrator `ToolCall`/`ToolResult` pair whose
    /// adjacency is broken by a sub-agent's intervening turns. The load-time
    /// sanitizer is lane-blind and matches only on adjacent `ToolCall` →
    /// `ToolResult`; a `delegate` call followed by sub-agent rows followed by
    /// the delegate result currently loses both orchestrator rows (#271).
    #[test]
    fn load_conversation_preserves_delegate_pair_split_by_sub_agent_turns() {
        use crate::conversation::{Conversation, Message};
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::{MessageRole, MessageSource};
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let mut conv = Conversation::new(
            "delegation".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        // Push via struct literals so sub-agent rows can carry
        // `MessageSource::SubAgent { role }` — the `add_*` helpers hard-code Human.
        conv.messages.push(Message::User {
            content: "research this".into(),
            compacted: false,
            source: MessageSource::Human,
            timestamp: chrono::Utc::now(),
        });
        conv.messages.push(Message::ToolCall {
            tool_name: "delegate".into(),
            arguments: "{}".into(),
            call_id: Some("call-1".into()),
            compacted: false,
            source: MessageSource::Human,
            timestamp: chrono::Utc::now(),
        });
        conv.messages.push(Message::ToolCall {
            tool_name: "bash".into(),
            arguments: "{}".into(),
            call_id: Some("sub-1".into()),
            compacted: false,
            source: MessageSource::SubAgent {
                role: "researcher".into(),
            },
            timestamp: chrono::Utc::now(),
        });
        conv.messages.push(Message::ToolResult {
            tool_name: "bash".into(),
            arguments: "{}".into(),
            result: "sub output".into(),
            call_id: Some("sub-1".into()),
            compacted: false,
            source: MessageSource::SubAgent {
                role: "researcher".into(),
            },
            timestamp: chrono::Utc::now(),
        });
        conv.messages.push(Message::ToolResult {
            tool_name: "delegate".into(),
            arguments: "{}".into(),
            result: "findings".into(),
            call_id: Some("call-1".into()),
            compacted: false,
            source: MessageSource::Human,
            timestamp: chrono::Utc::now(),
        });
        conv.messages.push(Message::Assistant {
            content: "here is the answer".into(),
            compacted: false,
            source: MessageSource::Human,
            thinking: Vec::new(),
            timestamp: chrono::Utc::now(),
        });

        // Go through the real /load path so the MessageSource serde round-trip
        // is exercised alongside `sync_from_conversation`.
        let id = conv.id;
        storage.save(&conv).unwrap();
        sm.load_conversation(id).unwrap();

        // (i) Transcript — the six rows must come back unchanged.
        let msgs = &sm.get_state().chat.messages;
        assert_eq!(
            msgs.len(),
            6,
            "sub-agent turns must not cause /load to drop orchestrator tool rows"
        );
        assert_eq!(
            msgs.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![
                MessageRole::User,
                MessageRole::ToolCall,
                MessageRole::ToolCall,
                MessageRole::ToolResult,
                MessageRole::ToolResult,
                MessageRole::Agent,
            ]
        );
        assert_eq!(msgs[1].role, MessageRole::ToolCall);
        assert_eq!(msgs[1].tool_name.as_deref(), Some("delegate"));
        assert_eq!(msgs[1].call_id.as_deref(), Some("call-1"));
        assert_eq!(msgs[1].source, MessageSource::Human);
        assert_eq!(msgs[4].role, MessageRole::ToolResult);
        assert_eq!(msgs[4].tool_name.as_deref(), Some("delegate"));
        assert_eq!(msgs[4].call_id.as_deref(), Some("call-1"));
        assert_eq!(msgs[4].source, MessageSource::Human);
        assert_eq!(msgs[2].role, MessageRole::ToolCall);
        assert_eq!(
            msgs[2].source,
            MessageSource::SubAgent {
                role: "researcher".into()
            }
        );
        assert_eq!(msgs[3].role, MessageRole::ToolResult);
        assert_eq!(
            msgs[3].source,
            MessageSource::SubAgent {
                role: "researcher".into()
            }
        );

        // (ii) Wire — sub-agent rows are filtered out by `is_orchestrator_context`,
        // and the surviving orchestrator rows form a `ToolCall` → `ToolResult`
        // pair that the wire layer must emit intact.
        let history = sm.get_agent_history();
        assert_eq!(
            history.len(),
            4,
            "wire history must carry the orchestrator tool pair alongside User+Agent"
        );
        let tc_id = match &history[1] {
            RigMessage::Assistant { content, .. } => content
                .iter()
                .find_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
                    _ => None,
                })
                .expect("delegate ToolCall at index 1"),
            other => panic!("expected Assistant ToolCall at index 1, got {other:?}"),
        };
        let tr_id = match &history[2] {
            RigMessage::User { content } => content
                .iter()
                .find_map(|c| match c {
                    UserContent::ToolResult(tr) => Some(tr.id.clone()),
                    _ => None,
                })
                .expect("delegate ToolResult at index 2"),
            other => panic!("expected User ToolResult at index 2, got {other:?}"),
        };
        assert_eq!(tc_id, "call-1");
        assert_eq!(tr_id, "call-1");
    }

    /// An orphan ToolCall persisted by a crash mid-tool must survive the load
    /// into the display transcript (load no longer mutates) but never reach the
    /// provider — repair happens at the wire boundary in `get_agent_history`,
    /// not on the saved rows. Closes the gap left when
    /// `sync_from_conversation_sanitizes_orphan_call` was deleted in #271.
    #[test]
    fn load_conversation_with_orphan_tool_call_yields_valid_wire_on_next_prompt() {
        use crate::conversation::{Conversation, Message};
        use crate::storage::{ConversationStorage, InMemoryStorage};
        use crate::ui::app_state::{MessageRole, MessageSource};
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};
        use std::sync::Arc;

        let storage: Arc<dyn ConversationStorage> = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let mut conv = Conversation::new(
            "orphan-recovery".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        conv.messages.push(Message::User {
            content: "run the thing".into(),
            compacted: false,
            source: MessageSource::Human,
            timestamp: chrono::Utc::now(),
        });
        // Orphan: ToolCall with no following ToolResult, the shape left by a
        // crash that landed the model message but never reached the result row.
        conv.messages.push(Message::ToolCall {
            tool_name: "bash".into(),
            arguments: "{}".into(),
            call_id: Some("call-9".into()),
            compacted: false,
            source: MessageSource::Human,
            timestamp: chrono::Utc::now(),
        });

        let id = conv.id;
        storage.save(&conv).unwrap();
        sm.load_conversation(id).unwrap();

        // (i) Load is honest — the orphan survives into the display transcript.
        let msgs = &sm.get_state().chat.messages;
        assert_eq!(
            msgs.len(),
            2,
            "orphan must survive /load (load no longer sanitises the transcript)"
        );
        assert_eq!(msgs[1].role, MessageRole::ToolCall);
        assert_eq!(msgs[1].call_id.as_deref(), Some("call-9"));

        // (ii) Simulate the next user prompt; it is re-supplied to the
        // provider separately and stripped from the wire history.
        sm.add_user_message("what happened?".to_string());

        let history = sm.get_agent_history();

        // Wire invariant walk (forward): every ToolCall is immediately
        // followed by a User carrying a matching ToolResult. Mirrors the
        // idiom in `get_agent_history_repairs_wedged_bg_user_between_tool_call_and_result`.
        for (idx, msg) in history.iter().enumerate() {
            let RigMessage::Assistant { content, .. } = msg else {
                continue;
            };
            let tool_call_ids: Vec<&str> = content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.id.as_str()),
                    _ => None,
                })
                .collect();
            if tool_call_ids.is_empty() {
                continue;
            }
            let next = history.get(idx + 1).unwrap_or_else(|| {
                panic!("orphan ToolCall leaked to wire at idx {idx}: {history:?}")
            });
            let RigMessage::User {
                content: next_content,
            } = next
            else {
                panic!(
                    "ToolCall at idx {idx} must be followed by a User message, got {next:?} (history: {history:?})"
                );
            };
            let result_ids: Vec<&str> = next_content
                .iter()
                .filter_map(|c| match c {
                    UserContent::ToolResult(tr) => Some(tr.id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                result_ids, tool_call_ids,
                "ToolCall(s) at idx {idx} ({tool_call_ids:?}) must be immediately followed by ToolResult(s) with matching ids, got {result_ids:?} (history: {history:?})"
            );
        }

        // Wire invariant walk (inverse): no ToolResult without its preceding
        // matching ToolCall. Catches a ToolCall/Result reversal as well as a
        // stray Result that sanitize would have stripped.
        for (idx, msg) in history.iter().enumerate() {
            let RigMessage::User { content } = msg else {
                continue;
            };
            for c in content.iter() {
                let UserContent::ToolResult(tr) = c else {
                    continue;
                };
                let prev = if idx == 0 {
                    panic!(
                        "ToolResult {} at idx 0 has no preceding message: {history:?}",
                        tr.id
                    )
                } else {
                    &history[idx - 1]
                };
                let RigMessage::Assistant {
                    content: prev_content,
                    ..
                } = prev
                else {
                    panic!(
                        "ToolResult {} at idx {idx} must be preceded by an Assistant message, got {prev:?} (history: {history:?})",
                        tr.id
                    );
                };
                let prev_tc_ids: Vec<&str> = prev_content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::ToolCall(tc) => Some(tc.id.as_str()),
                        _ => None,
                    })
                    .collect();
                assert!(
                    prev_tc_ids.iter().any(|id| *id == tr.id),
                    "ToolResult {} at idx {idx} has no preceding matching ToolCall (prev tool calls: {prev_tc_ids:?}, history: {history:?})",
                    tr.id
                );
            }
        }

        // The orphan id must not appear in wire history at all.
        for msg in &history {
            let RigMessage::Assistant { content, .. } = msg else {
                continue;
            };
            for c in content.iter() {
                if let AssistantContent::ToolCall(tc) = c {
                    assert_ne!(
                        tc.id, "call-9",
                        "orphan ToolCall leaked to the wire (history: {history:?})"
                    );
                }
            }
        }
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
        let mut conv =
            Conversation::new("test".into(), "prov".into(), "model".into(), String::new());
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

    /// Loading a conversation preserves each message's original timestamp
    /// (converted UTC→Local for display) instead of stamping the load time.
    /// Also covers the save half: sync_to_conversation must carry the live
    /// timestamp through rather than clobbering it with Utc::now().
    #[test]
    fn sync_preserves_original_message_timestamps() {
        use crate::conversation::Message as ConvMsg;
        use chrono::{DateTime, Local, TimeZone, Utc};

        // Two known, distinct times well in the past so they can never be
        // confused with Local::now() / Utc::now().
        let t_user: DateTime<Utc> = Utc.with_ymd_and_hms(2021, 3, 4, 8, 15, 0).unwrap();
        let t_agent: DateTime<Utc> = Utc.with_ymd_and_hms(2021, 3, 4, 8, 16, 30).unwrap();

        // ── Load half ──────────────────────────────────────────────────
        // Build a conversation whose messages carry the known timestamps.
        let mut conv =
            Conversation::new("test".into(), "prov".into(), "model".into(), String::new());
        conv.messages.push(ConvMsg::User {
            content: "hello".into(),
            compacted: false,
            source: crate::ui::app_state::MessageSource::Human,
            timestamp: t_user,
        });
        conv.messages.push(ConvMsg::Assistant {
            content: "hi there".into(),
            compacted: false,
            source: crate::ui::app_state::MessageSource::Human,
            thinking: Vec::new(),
            timestamp: t_agent,
        });

        let sm = StateManager::new();
        *sm.current_conversation.lock().unwrap() = Some(conv);
        sm.sync_from_conversation();

        let state = sm.get_state();
        assert_eq!(state.chat.messages.len(), 2);
        assert_eq!(
            state.chat.messages[0].timestamp,
            t_user.with_timezone(&Local),
            "user message must keep its original timestamp, not the load time"
        );
        assert_eq!(
            state.chat.messages[1].timestamp,
            t_agent.with_timezone(&Local),
            "agent message must keep its original timestamp, not the load time"
        );

        // ── Save half ──────────────────────────────────────────────────
        // Saving must carry the live ChatMessage timestamp back into the
        // persisted Message, not overwrite it with the save time.
        sm.sync_to_conversation();
        let guard = sm.current_conversation.lock().unwrap();
        let saved = guard.as_ref().unwrap();
        match &saved.messages[0] {
            ConvMsg::User { timestamp, .. } => assert_eq!(
                *timestamp, t_user,
                "save must preserve the original timestamp, not stamp Utc::now()"
            ),
            other => panic!("expected User, got {other:?}"),
        }
        match &saved.messages[1] {
            ConvMsg::Assistant { timestamp, .. } => assert_eq!(
                *timestamp, t_agent,
                "save must preserve the original timestamp, not stamp Utc::now()"
            ),
            other => panic!("expected Assistant, got {other:?}"),
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
        let mut conv =
            Conversation::new("test".into(), "prov".into(), "model".into(), String::new());
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

        let mut conv =
            Conversation::new("test".into(), "prov".into(), "model".into(), String::new());
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

    /// The delegate tool sizes each sub-agent's budget from this fraction
    /// against the *role's* context size, so it must reflect config exactly.
    #[test]
    fn compaction_threshold_reports_fraction_when_enabled() {
        let sm = sm_with_context_size(128_000);
        assert_eq!(sm.compaction_threshold(), Some(0.8));
    }

    /// No `ContextManager` (or compaction disabled) means no budget — the
    /// sub-agent gate then stays off rather than guessing a threshold.
    #[test]
    fn compaction_threshold_none_without_manager_or_when_disabled() {
        use crate::config::ContextConfig;

        let bare = StateManager::new_arc();
        assert_eq!(bare.compaction_threshold(), None);

        let sm = StateManager::new_arc();
        let cfg = ContextConfig {
            threshold: 0.8,
            keep_recent: 5,
            enabled: false,
            compaction_model: None,
        };
        sm.init_context_manager(ContextManager::new(cfg, 128_000, None));
        assert_eq!(sm.compaction_threshold(), None);
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

    /// The context meter measures the ORCHESTRATOR's context, not "whatever
    /// lane spoke last". A sub-agent's request runs on its own model with its
    /// own window and must never move the orchestrator's meter — otherwise a
    /// delegate call visibly collapses the user's context gauge to the
    /// sub-agent's tiny wire size.
    ///
    /// Pinned by the multi-pipeline plan § 6 (bonus): `sync_stats_to_ui` reads
    /// `last_orchestrator_input_tokens()`, the same signal the compaction gate
    /// already uses — not `last_input_tokens()` (last request on ANY lane).
    #[test]
    fn context_meter_tracks_orchestrator_lane_only() {
        use crate::ui::app_state::MessageSource;
        let sm = StateManager::new();

        // The orchestrator turn sets the meter.
        sm.add_request(&MessageSource::Human, 1_000, 100, 0.0);
        assert_eq!(
            sm.get_state().context.current_usage,
            1_000,
            "orchestrator request must set the meter"
        );

        // A sub-agent turn must NOT clobber it, however small.
        sm.add_request(
            &MessageSource::SubAgent {
                role: "junior".into(),
            },
            50,
            10,
            0.0,
        );
        assert_eq!(
            sm.get_state().context.current_usage,
            1_000,
            "a sub-agent's request must not move the orchestrator's context meter"
        );
    }

    /// The meter and the compaction gate must read the SAME number. Today they
    /// can disagree — the meter shows a sub-agent's last wire size while the
    /// gate reads the orchestrator's — which is how `/context` ends up
    /// contradicting its own threshold warning.
    #[test]
    fn context_meter_agrees_with_compaction_gate() {
        use crate::ui::app_state::MessageSource;
        // 1_000-token window, 0.8 threshold → the gate fires above 800.
        let sm = sm_with_context_size(1_000);

        // Enough traffic to clear the `keep_recent` floor (5) so the gate can
        // reach its token branch at all.
        for i in 0..5 {
            sm.add_user_message(format!("u{i}"));
            sm.add_assistant_message(format!("a{i}"));
        }

        sm.add_request(&MessageSource::Human, 1_000, 100, 0.0);
        sm.add_request(
            &MessageSource::SubAgent {
                role: "junior".into(),
            },
            50,
            10,
            0.0,
        );

        // The one invariant: what the user sees IS what the gate decides on.
        assert_eq!(
            sm.get_state().context.current_usage as usize,
            sm.current_input_tokens(),
            "the context meter must display the same token count the compaction gate reads"
        );

        // And that shared reading is over threshold, so the meter cannot show a
        // comfortable number while compaction is firing behind it.
        assert!(
            sm.needs_compaction(),
            "precondition: 1000 > 800 threshold on the orchestrator lane"
        );
        assert!(
            sm.get_state().context.current_usage > 800,
            "meter must show the over-threshold value that triggered compaction"
        );
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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 900, 50, 0.0);
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
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 600, 50, 0.0);
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
            MessageSource::Human,
            "view_image".to_string(),
            r#"{"path":"/tmp/x.png"}"#.to_string(),
            Some("call_1".to_string()),
        );
        // Exactly the shape `view_image` returns.
        sm.add_tool_result(
            MessageSource::Human,
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
    fn get_agent_history_excludes_sub_agent_lane_keeps_background() {
        use crate::ui::app_state::{ChatMessage, MessageSource};
        use rig_core::completion::message::{AssistantContent, Message as RigMessage};

        let sm = StateManager::new();

        // Orchestrator turn (Human lane) + a delegate ToolCall/ToolResult (Human lane).
        sm.add_user_message("orchestrate this".to_string());
        sm.add_assistant_message("delegating".to_string());

        // A sub-agent's internal turns land in the transcript tagged SubAgent.
        // They must NEVER reach the orchestrator wire history.
        sm.update_chat(
            ChatMessage::agent("sub-agent thinking".to_string()).with_source(
                MessageSource::SubAgent {
                    role: "researcher".to_string(),
                },
            ),
        );
        sm.update_chat(ChatMessage::tool_call("bash", "{}", None).with_source(
            MessageSource::SubAgent {
                role: "researcher".to_string(),
            },
        ));

        // A background synthetic turn IS orchestrator-lane input and must survive.
        sm.update_chat(ChatMessage::user_from_background(
            "bg output".to_string(),
            vec![1],
        ));

        // Trailing assistant so the background user turn is not stripped as "trailing user".
        sm.add_assistant_message("done".to_string());

        let history = sm.get_agent_history();

        // No sub-agent content leaked into the orchestrator wire.
        let leaked = history.iter().any(|m| match m {
            RigMessage::Assistant { content, .. } => content.iter().any(|c| match c {
                AssistantContent::Text(t) => t.text.contains("sub-agent thinking"),
                _ => false,
            }),
            _ => false,
        });
        assert!(
            !leaked,
            "sub-agent turn leaked into orchestrator wire history"
        );

        // The background turn survived the lane filter.
        let has_bg = history.iter().any(|m| match m {
            RigMessage::User { content } => content.iter().any(|c| {
                matches!(c, rig_core::completion::message::UserContent::Text(t) if t.text.contains("bg output"))
            }),
            _ => false,
        });
        assert!(
            has_bg,
            "background turn was wrongly filtered from orchestrator wire history"
        );
    }

    /// Regression: a `bash_bg` synthetic user message can land between the
    /// `ToolCall` and its matching `ToolResult` when the event-processor task
    /// and the bg-drain seam race. The wire history must never emit a
    /// `ToolCall` that is not immediately followed by its matching
    /// `ToolResult`, or every provider rejects the next request with 400 and
    /// the conversation wedges permanently.
    #[test]
    fn get_agent_history_repairs_wedged_bg_user_between_tool_call_and_result() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};

        let sm = StateManager::new();

        sm.add_user_message("do thing".to_string());
        sm.add_assistant_message("thinking".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            "bash".to_string(),
            "{}".to_string(),
            Some("call_x".to_string()),
        );
        // The wedge: the bg-drain seam lands its user message between the
        // tool call and its matching tool result.
        sm.add_user_message_from_background("bg noise".to_string(), vec![1]);
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            "{}".to_string(),
            "ok".to_string(),
            Some("call_x".to_string()),
        );
        // Trailing user so the wedge is not the trailing-stripped slot.
        sm.add_user_message("next thing".to_string());

        let history = sm.get_agent_history();

        // Wire invariant: every ToolCall in the history is immediately
        // followed by a User message carrying a ToolResult with the same id.
        for (idx, msg) in history.iter().enumerate() {
            let RigMessage::Assistant { content, .. } = msg else {
                continue;
            };
            let tool_call_ids: Vec<&str> = content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.id.as_str()),
                    _ => None,
                })
                .collect();
            if tool_call_ids.is_empty() {
                continue;
            }
            let next = history.get(idx + 1).unwrap_or_else(|| {
                panic!("ToolCall at idx {idx} has no following message: {history:?}")
            });
            let RigMessage::User {
                content: next_content,
            } = next
            else {
                panic!(
                    "ToolCall at idx {idx} must be followed by a User message, got {next:?} (history: {history:?})"
                );
            };
            let result_ids: Vec<&str> = next_content
                .iter()
                .filter_map(|c| match c {
                    UserContent::ToolResult(tr) => Some(tr.id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                result_ids, tool_call_ids,
                "ToolCall(s) at idx {idx} ({tool_call_ids:?}) must be immediately followed by ToolResult(s) with matching ids, got {result_ids:?} (history: {history:?})"
            );
        }
    }

    /// Inverse regression: a canonical transcript (ToolCall immediately
    /// followed by its ToolResult, then a user turn) must pass through
    /// `get_agent_history` untouched. Guards against an overzealous fix that
    /// mangles valid history.
    #[test]
    fn get_agent_history_preserves_canonical_tool_call_then_tool_result_pair() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};

        let sm = StateManager::new();

        sm.add_user_message("do thing".to_string());
        sm.add_assistant_message("thinking".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            "bash".to_string(),
            "{}".to_string(),
            Some("call_x".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
            "bash".to_string(),
            "{}".to_string(),
            "ok".to_string(),
            Some("call_x".to_string()),
        );
        // Trailing user so the tool result is not the trailing-stripped slot.
        sm.add_user_message("trailing".to_string());

        let history = sm.get_agent_history();

        // Trailing user is excluded; the surviving 4 messages pass through.
        assert_eq!(history.len(), 4, "history: {history:?}");

        // The ToolCall at index 2 is immediately followed by the ToolResult
        // at index 3 — no orphan, no mangling.
        let tc_id = match &history[2] {
            RigMessage::Assistant { content, .. } => content
                .iter()
                .find_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.id.clone()),
                    _ => None,
                })
                .expect("ToolCall at index 2"),
            other => panic!("expected Assistant ToolCall at index 2, got {other:?}"),
        };
        let tr_id = match &history[3] {
            RigMessage::User { content } => content
                .iter()
                .find_map(|c| match c {
                    UserContent::ToolResult(tr) => Some(tr.id.clone()),
                    _ => None,
                })
                .expect("ToolResult at index 3"),
            other => panic!("expected User ToolResult at index 3, got {other:?}"),
        };
        assert_eq!(tc_id, "call_x");
        assert_eq!(tr_id, "call_x");
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
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
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
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};

        let sm = Arc::new(StateManager::new());
        // Seed a current conversation so persist_current actually walks the
        // state.read → current_conv.lock → stats.lock chain.
        sm.create_conversation(
            "deadlock-probe".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            String::new(),
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
                sm_a.add_request(&crate::ui::app_state::MessageSource::Human, 100, 50, 0.001);
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

    /// Regression for the `todo_list ↔ state` lock-order inversion (the
    /// second occurrence of the class the test above guards for `stats`).
    ///
    /// - `sync_to_conversation` (agent loop, via `persist_current`) takes
    ///   `state.read` and, still holding it, reaches for `todo_list`.
    /// - `update_todo_status` (todo tool) used to take `todo_list` and, still
    ///   holding it, call `sync_todo_to_ui` → `state.write`.
    ///
    /// Crossed on a multi-threaded runtime, `state.write` blocks behind the
    /// held `state.read` while `todo_list` blocks the reader path waiting for
    /// it — the same A→B vs B→A deadlock. The fix makes `todo_list` a leaf:
    /// `sync_todo_to_ui` snapshots under `todo_list` and drops it before
    /// `state.write` (mirroring `sync_stats_to_ui`). With the bug present this
    /// deadlocks; with the fix it completes in milliseconds.
    ///
    /// Watchdog discipline matches the sibling test: pure `AtomicU64`
    /// polling, touching none of the involved locks, aborting on a frozen
    /// counter.
    #[test]
    fn update_todo_and_persist_current_do_not_deadlock() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};

        let sm = Arc::new(StateManager::new());
        sm.create_conversation(
            "todo-deadlock-probe".to_string(),
            "test-prov".to_string(),
            "test-model".to_string(),
            String::new(),
        );
        sm.add_user_message("hello".to_string());
        // Seed a todo so update_todo_status actually mutates and syncs,
        // walking its full todo_list → state.write chain.
        sm.add_todos(vec!["task one".to_string()]);

        let stop = Arc::new(AtomicBool::new(false));
        let count_a = Arc::new(AtomicU64::new(0));
        let count_b = Arc::new(AtomicU64::new(0));

        // Producer A: agent loop persisting (state.read → … → todo_list).
        let sm_a = sm.clone();
        let stop_a = stop.clone();
        let count_a_t = count_a.clone();
        let t_a = thread::spawn(move || {
            while !stop_a.load(Ordering::Relaxed) {
                sm_a.add_assistant_message("reply".to_string());
                count_a_t.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Producer B: todo tool mutating (todo_list → state.write via
        // sync_todo_to_ui) — the inverted-order path.
        let sm_b = sm.clone();
        let stop_b = stop.clone();
        let count_b_t = count_b.clone();
        let t_b = thread::spawn(move || {
            while !stop_b.load(Ordering::Relaxed) {
                sm_b.update_todo_status(1, TodoStatus::InProgress);
                sm_b.update_todo_status(1, TodoStatus::Pending);
                count_b_t.fetch_add(1, Ordering::Relaxed);
            }
        });

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

        thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);
        t_a.join().expect("producer A must not panic");
        t_b.join().expect("producer B must not panic");

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
            String::new(),
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
            String::new(),
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
            String::new(),
        );

        // Simulate a first turn with tool calls: 4 messages total
        sm.add_user_message("List files".to_string());
        sm.add_tool_call(
            MessageSource::Human,
            "bash".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            Some("call_1".to_string()),
        );
        sm.add_tool_result(
            MessageSource::Human,
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
            String::new(),
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

    // ── Compaction × sub-agent lane (RED for tickets/compaction-subagent-lane) ─
    //
    // These tests pin the bug described in `tickets/compaction-subagent-lane.md`:
    // a sub-agent's internal turns (sourced `MessageSource::SubAgent { role }`)
    // must never count toward or feed into the orchestrator's compaction, and
    // the compaction summary must never splice into a rescued tool-call pair.

    /// Spec § 8.1 — `uncompacted_count_excludes_sub_agent_lane`.
    ///
    /// `uncompacted_message_count` is the count consumed by
    /// `ContextManager::needs_compaction` (the gate) and the
    /// `live.len() <= keep_recent` early-out in `compact()`. It must
    /// reflect only the orchestrator's live context — sub-agent rows
    /// live in the transcript (display + persistence) but never the
    /// orchestrator's wire context.
    #[test]
    fn uncompacted_count_excludes_sub_agent_lane() {
        let sm = StateManager::new();

        // 3 orchestrator-lane rows.
        sm.add_user_message("orch u".to_string());
        sm.add_assistant_message("orch a".to_string());
        sm.add_tool_call(
            crate::ui::app_state::MessageSource::Human,
            "bash".to_string(),
            "{}".to_string(),
            Some("orch-tc-1".to_string()),
        );

        // 20 sub-agent-lane rows: mix of tool_call/tool_result/agent rows.
        let sub = crate::ui::app_state::MessageSource::SubAgent {
            role: "junior".to_string(),
        };
        for i in 0..10 {
            sm.add_tool_call(
                sub.clone(),
                "bash".to_string(),
                format!("{{\"i\":{i}}}"),
                Some(format!("sub-tc-{i}")),
            );
            sm.add_tool_result(
                sub.clone(),
                "bash".to_string(),
                format!("{{\"i\":{i}}}"),
                format!("out-{i}"),
                Some(format!("sub-tc-{i}")),
            );
        }

        // Sanity: 23 rows total — confirms the lane-blind filter sees them all.
        let total = sm.get_state().chat.messages.len();
        assert_eq!(
            total, 23,
            "sanity: should have 3 orchestrator + 20 sub-agent rows"
        );

        // The pin: only the 3 orchestrator-lane rows count.
        assert_eq!(
            sm.uncompacted_message_count(),
            3,
            "uncompacted_message_count must exclude the 20 sub-agent rows; got {} (bug: counts all lanes)",
            sm.uncompacted_message_count(),
        );
    }

    /// Spec § 8.3 — `apply_compaction_never_wedges_summary_between_a_rescued_pair`.
    ///
    /// Models the bf7d62d3 repro conversation: an orchestrator `delegate`
    /// ToolCall followed (non-adjacently) by many sub-agent rows and then
    /// the orchestrator `delegate` ToolResult. After compaction, the
    /// rescued ToolCall and its ToolResult must remain adjacent on the
    /// wire — never separated by the inserted summary, or Anthropic
    /// rejects the turn ("tool_use without tool_result immediately
    /// after").
    ///
    /// Deviation from the spec (§8.3 says `keep_recent: 2`): we use
    /// `keep_recent: 3`. With `keep_recent: 2` and the existing
    /// `snap_boundary_past_tool_results` helper (still present today —
    /// §2.3 of the design deletes it in T3), the boundary snaps past the
    /// ToolResult and *the entire pair is dropped*, which masks the
    /// wedged-summary failure mode. With `keep_recent: 3`, the boundary
    /// lands on a sub-agent row (not a ToolResult), the snap does not
    /// apply, the ToolCall is rescued, and today's bug manifests as
    /// "Summary lands between the rescued ToolCall and its ToolResult" —
    /// the literal RED the spec describes.
    #[tokio::test]
    async fn apply_compaction_never_wedges_summary_between_a_rescued_pair() {
        use rig_core::completion::message::{AssistantContent, Message as RigMessage, UserContent};

        // Build a SM with a real compaction model so `force_compact` does work.
        let sm = StateManager::new_arc();
        let cfg = crate::config::ContextConfig {
            threshold: 0.5,
            keep_recent: 3, // see deviation note in the doc-comment above
            enabled: true,
            compaction_model: None,
        };
        let (model, mock) = crate::providers::create_mock_compaction_model();
        mock.add_response(crate::mock::MockResponse::text("SUMMARY"));
        let cm = crate::context_manager::ContextManager::new(
            cfg,
            1_000,
            Some(std::sync::Arc::new(model)),
        );
        sm.init_context_manager(cm);

        // Arrange the bf7d62d3-shaped transcript:
        //   User m1, Agent, User m2, Agent,
        //   ToolCall(delegate, c1) [orchestrator],
        //   12 SubAgent{junior} rows,
        //   ToolResult(delegate, c1) [orchestrator],
        //   Agent("done")
        sm.add_user_message("m1".to_string());
        sm.add_assistant_message("a1".to_string());
        sm.add_user_message("m2".to_string());
        sm.add_assistant_message("a2".to_string());
        sm.add_tool_call(
            crate::ui::app_state::MessageSource::Human,
            "delegate".to_string(),
            r#"{"role":"researcher","task":"survey"}"#.to_string(),
            Some("c1".to_string()),
        );
        let sub = crate::ui::app_state::MessageSource::SubAgent {
            role: "junior".to_string(),
        };
        // 11 sub-agent rows ending with an Agent message so the compaction
        // boundary lands on the Agent (not a ToolResult) — without that,
        // `snap_boundary_past_tool_results` (still present today, deleted
        // by §2.3 of the design) silently drops the rescued pair and
        // masks the wedged-summary failure mode we want to pin.
        for i in 0..5 {
            sm.add_tool_call(
                sub.clone(),
                "bash".to_string(),
                format!("{{\"i\":{i}}}"),
                Some(format!("sub-c-{i}")),
            );
            sm.add_tool_result(
                sub.clone(),
                "bash".to_string(),
                format!("{{\"i\":{i}}}"),
                format!("sub-out-{i}"),
                Some(format!("sub-c-{i}")),
            );
        }
        sm.add_assistant_message_sourced(sub.clone(), "sub-agent done thinking".to_string());
        sm.add_tool_result(
            crate::ui::app_state::MessageSource::Human,
            "delegate".to_string(),
            r#"{"role":"researcher","task":"survey"}"#.to_string(),
            "DELEGATE_RESULT".to_string(),
            Some("c1".to_string()),
        );
        sm.add_assistant_message("done".to_string());

        // Act
        let result = sm.force_compact().await;
        assert!(
            result.is_some(),
            "force_compact must produce a result with mock summarizer queued"
        );

        // Assert on the orchestrator wire.
        let wire = sm.get_agent_history();

        // 1. The Summary User-text appears BEFORE the delegate ToolCall.
        let delegate_tc_pos = wire
            .iter()
            .position(|m| {
                matches!(m, RigMessage::Assistant { content, .. }
                if content.iter().any(|c| matches!(c, AssistantContent::ToolCall(tc)
                    if tc.id == "c1")))
            })
            .expect("delegate ToolCall(c1) must be on the wire");
        let summary_pos = wire
            .iter()
            .position(|m| {
                matches!(m, RigMessage::User { content }
                if content.iter().any(|c| matches!(c, UserContent::Text(t)
                    if t.text.contains("[Conversation summary]"))))
            })
            .expect("summary User-text must be on the wire (compaction happened)");
        assert!(
            summary_pos < delegate_tc_pos,
            "Summary (pos={summary_pos}) must precede the rescued delegate ToolCall (pos={delegate_tc_pos}); \
             current wire order wedges the pair — Anthropic would reject"
        );

        // 2. Pair adjacency: every Assistant(ToolCall X) is immediately
        //    followed by a User(ToolResult X) with matching call_id.
        for (i, m) in wire.iter().enumerate() {
            if let RigMessage::Assistant { content, .. } = m
                && let Some(AssistantContent::ToolCall(tc)) = content.iter().next()
            {
                let call_id = tc.id.clone();
                let next = wire
                    .get(i + 1)
                    .unwrap_or_else(|| panic!("ToolCall {call_id:?} is the last wire element — no matching ToolResult follows"));
                match next {
                    RigMessage::User { content } => {
                        let found = content.iter().any(|c| match c {
                            UserContent::ToolResult(tr) => tr.id == call_id,
                            _ => false,
                        });
                        assert!(
                            found,
                            "ToolCall {call_id:?} must be immediately followed by ToolResult {call_id:?}; got {next:?}"
                        );
                    }
                    other => panic!(
                        "ToolCall {call_id:?} must be immediately followed by a User ToolResult; got {other:?}"
                    ),
                }
            }
        }

        // 3. No wire element contains any sub-agent marker.
        for (i, m) in wire.iter().enumerate() {
            let leaked = match m {
                RigMessage::Assistant { content, .. } => content.iter().any(|c| match c {
                    AssistantContent::Text(t) => {
                        t.text.contains("sub-out-")
                            || t.text.contains("sub-c-")
                            || t.text.contains("sub-agent done")
                    }
                    AssistantContent::ToolCall(tc) => tc.id.starts_with("sub-c-"),
                    _ => false,
                }),
                RigMessage::User { content } => content.iter().any(|c| match c {
                    UserContent::Text(t) => {
                        t.text.contains("sub-out-")
                            || t.text.contains("sub-c-")
                            || t.text.contains("sub-agent done")
                    }
                    UserContent::ToolResult(tr) => tr.id.starts_with("sub-c-"),
                    _ => false,
                }),
                RigMessage::System { .. } => false,
            };
            assert!(
                !leaked,
                "sub-agent content leaked into the orchestrator wire at index {i}: {m:?}"
            );
        }
    }

    /// Spec § 8.4 — `sub_agent_request_does_not_move_the_compaction_gate`.
    ///
    /// The compaction gate reads the orchestrator-lane's last request, not
    /// the session-wide last request. A sub-agent's 900-token request must
    /// NOT trigger the orchestrator's 500-token gate. Companion assertion:
    /// the status-bar reading (session-wide last) still shows the sub-agent
    /// request — the fix must not hide it.
    ///
    /// Note: this test does not exercise `last_orchestrator_input_tokens()`
    /// directly (it lands with task T4). It exercises the public
    /// `StateManager::needs_compaction()` gate plus the public
    /// `SessionStats::last_input_tokens()` display value — both exist today.
    #[test]
    fn sub_agent_request_does_not_move_the_compaction_gate() {
        use crate::config::ContextConfig;
        use crate::context_manager::ContextManager;

        let sm = StateManager::new_arc();
        // 500-token threshold; no compaction model needed for this assertion.
        let cfg = ContextConfig {
            threshold: 0.5,
            keep_recent: 5, // default
            enabled: true,
            compaction_model: None,
        };
        sm.init_context_manager(ContextManager::new(cfg, 1_000, None));

        // 6 orchestrator-lane rows so uncompacted_count (6) > keep_recent (5)
        // — only then does `needs_compaction` reach the token branch.
        for i in 0..3 {
            sm.add_user_message(format!("u{i}"));
            sm.add_assistant_message(format!("a{i}"));
        }

        // Orchestrator's last request: 100 tokens — well below the 500-token gate.
        sm.add_request(&crate::ui::app_state::MessageSource::Human, 100, 10, 0.0);

        // A sub-agent then makes a 900-token request. Today this overwrites
        // `last_input_tokens` → 900 → `needs_compaction()` reads true.
        sm.add_request(
            &crate::ui::app_state::MessageSource::SubAgent {
                role: "junior".to_string(),
            },
            900,
            10,
            0.0,
        );

        // The pin: a sub-agent's 900-token request must NOT trip the
        // orchestrator's 500-token gate. The orchestrator's last request was
        // only 100 tokens.
        assert!(
            !sm.needs_compaction(),
            "sub-agent's 900-token request must not trip the orchestrator's 500-token gate \
             (RED: today the gate is session-wide and reads true)"
        );

        // Companion: the status-bar reading is the session-wide last value
        // and must be UNCHANGED by the fix (still Some(900)).
        assert_eq!(
            sm.get_stats().last_input_tokens(),
            Some(900),
            "the session-wide last-input reading (status bar) must still show 900 — \
             the fix must not hide the sub-agent's request from the UI"
        );
    }
    // ── Stage 1.2: multi-pipeline selection state ─────────────────────────────
    //
    // These pin §7 / §4 of the multi-pipeline plan: the per-conversation
    // selection state lives on `Conversation.pipeline` (persisted) and is
    // mirrored into `AppState.selected_pipeline` for the live UI. The
    // single boolean pair (`pipeline_available` + `subagents_enabled`)
    // is replaced by one nullable string — the implementer may keep the
    // booleans around for back-compat reads but they MUST NOT be the
    // authority.

    /// `set_selected_pipeline(Some(name))` mirrors the choice into the
    /// current conversation's `pipeline` field AND into
    /// `AppState.selected_pipeline`. The single string is the source of
    /// truth for both surfaces — plan §3 "One nullable fact".
    #[test]
    fn set_selected_pipeline_writes_conversation_and_app_state() {
        let sm = StateManager::new();
        // Seed a current conversation — the persisted mirror requires it.
        sm.create_conversation(
            "pipe-select".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );

        // Default = no pipeline.
        assert_eq!(sm.get_state().selected_pipeline, None);

        // Set some — both surfaces must reflect.
        sm.set_selected_pipeline(Some("web-team".into()));
        assert_eq!(sm.get_state().selected_pipeline, Some("web-team".into()));
        assert_eq!(
            sm.get_current_conversation()
                .expect("current conversation")
                .pipeline
                .as_deref(),
            Some("web-team"),
            "the conversation's persisted `pipeline` field must mirror the choice"
        );

        // Set None — both surfaces cleared.
        sm.set_selected_pipeline(None);
        assert_eq!(sm.get_state().selected_pipeline, None);
        assert_eq!(
            sm.get_current_conversation()
                .expect("current conversation")
                .pipeline,
            None,
            "the conversation's persisted `pipeline` field must be cleared too"
        );
    }

    /// `selected_pipeline()` reads through `AppState.selected_pipeline` —
    /// it's the getter half of the seam `set_selected_pipeline` writes.
    /// Pinning it as a separate test catches an implementer who wires the
    /// setter to one source and the getter to another.
    #[test]
    fn selected_pipeline_getter_reads_app_state() {
        let sm = StateManager::new();
        assert_eq!(sm.selected_pipeline().as_deref(), None);
        sm.set_selected_pipeline(Some("research-crew".into()));
        assert_eq!(sm.selected_pipeline().as_deref(), Some("research-crew"));
        sm.set_selected_pipeline(None);
        assert_eq!(sm.selected_pipeline().as_deref(), None);
    }

    /// Resuming a conversation with `pipeline: Some(name)` stamps the
    /// choice into `AppState.selected_pipeline` so the rebuilt agent
    /// boots on the pipeline's orchestrator. Mirrors the
    /// `load_conversation_restores_subagents_opt_in` test (the existing
    /// subagents toggle) but for the new field.
    #[test]
    fn load_conversation_restores_selected_pipeline() {
        use crate::conversation::Conversation;
        use crate::storage::InMemoryStorage;
        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let mut conv = Conversation::new(
            "opted-in".into(),
            "prov".into(),
            "model".into(),
            String::new(),
        );
        conv.pipeline = Some("web-team".into());
        let id = conv.id;
        storage.save(&conv).unwrap();

        // Session default is no pipeline; loading the opted-in
        // conversation flips it on.
        assert_eq!(sm.selected_pipeline().as_deref(), None);
        sm.load_conversation(id).unwrap();
        assert_eq!(sm.selected_pipeline().as_deref(), Some("web-team"));
        assert_eq!(sm.get_state().selected_pipeline, Some("web-team".into()));
    }

    /// `peek_conversation_pipeline(id)` returns the saved pipeline name
    /// WITHOUT loading the conversation into the current slot — the same
    /// pre-flight pattern as `peek_conversation_wire_id` /
    /// `peek_conversation_subagents_enabled`. Used by `create_session` on
    /// the resume path so the agent can be built on the right orchestrator
    /// before the conversation is loaded.
    #[test]
    fn peek_conversation_pipeline_returns_saved_name() {
        use crate::conversation::Conversation;
        use crate::storage::InMemoryStorage;
        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        let mut conv = Conversation::new(
            "peek-target".into(),
            "prov".into(),
            "model".into(),
            String::new(),
        );
        conv.pipeline = Some("research-crew".into());
        let id = conv.id;
        storage.save(&conv).unwrap();

        let peeked = sm
            .peek_conversation_pipeline(id)
            .expect("peek must succeed when storage is configured");
        assert_eq!(peeked.as_deref(), Some("research-crew"));

        // Peek must NOT mutate current_conversation — the session can
        // still load a *different* conversation after peeking. Re-load
        // and assert the slot survives unchanged.
        assert!(
            !sm.has_current_conversation(),
            "peek must not touch the slot"
        );
    }

    /// Amendment 5: a legacy conversation (one written before the
    /// pipeline field existed) carries `subagents_enabled: true` but no
    /// `pipeline` key. The selection resumes as `None` — there is NO
    /// legacy-mapping code path. Pin: a loaded legacy conversation has
    /// `selected_pipeline() == None`, and `peek_conversation_pipeline`
    /// also returns `None`.
    #[test]
    fn legacy_subagents_enabled_does_not_select_a_pipeline() {
        use crate::conversation::Conversation;
        use crate::storage::InMemoryStorage;
        let storage = Arc::new(InMemoryStorage::default());
        let sm = StateManager::new_arc_with_storage(storage.clone());

        // JSON-shape legacy: `subagents_enabled: true`, no `pipeline` key.
        // `Conversation` has no `deny_unknown_fields`, so the unknown
        // `subagents_enabled` key is silently dropped on load if the
        // field is removed in Stage 1.2 (amendment 5). Either way, the
        // selection must be None.
        let legacy_json = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "old",
            "created_at": "2020-01-01T00:00:00Z",
            "updated_at": "2020-01-01T00:00:00Z",
            "messages": [],
            "provider_name": "openrouter",
            "model": "anthropic/claude-3.7-sonnet",
            "metadata": {},
            "subagents_enabled": true
        }"#;
        let parsed: Conversation = serde_json::from_str(legacy_json)
            .expect("legacy JSON must parse (unknown subagents_enabled key is dropped)");
        let id = parsed.id;
        storage.save(&parsed).unwrap();

        sm.load_conversation(id).unwrap();

        assert_eq!(
            sm.selected_pipeline().as_deref(),
            None,
            "amendment 5: a legacy conversation resumes with selected_pipeline = None; \
             the old subagents_enabled key does NOT auto-map to a 'default' pipeline"
        );
        assert_eq!(
            sm.peek_conversation_pipeline(id).unwrap(),
            None,
            "peek must report None for a legacy conversation"
        );
    }

    /// Lock-order sibling of
    /// `add_request_and_persist_current_do_not_deadlock`. The new
    /// pipeline API touches `state` and `current_conversation` from
    /// `set_selected_pipeline` (writes to both) — if any future caller
    /// holds both guards simultaneously the same A→B vs B→A deadlock
    /// class appears. This test hammers create + select + persist from
    /// two threads and demands forward progress. With the lock rule
    /// "never hold state + current_conversation together" (plan §3) the
    /// producers run tens of thousands of iterations; without it they
    /// wedge.
    #[test]
    fn select_pipeline_and_persist_current_do_not_deadlock() {
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::thread;
        use std::time::{Duration, Instant};

        let sm = StdArc::new(StateManager::new());
        // Seed a conversation so persist_current has a current slot to
        // walk. Mirrors the deadlock-probe fixture.
        sm.create_conversation(
            "select-pipeline-deadlock-probe".into(),
            "test-prov".into(),
            "test-model".into(),
            String::new(),
        );
        // Seed a real turn so sync_to_conversation has work to copy under
        // the locks (mirrors the other deadlock pins).
        sm.add_user_message("hello".into());

        let stop = StdArc::new(AtomicBool::new(false));
        let count_a = StdArc::new(AtomicU64::new(0));
        let count_b = StdArc::new(AtomicU64::new(0));

        // Producer A: hammers `set_selected_pipeline`, alternating Some
        // and None — the new API that touches both `state` and
        // `current_conversation`.
        let sm_a = sm.clone();
        let stop_a = stop.clone();
        let count_a_t = count_a.clone();
        let t_a = thread::spawn(move || {
            let mut toggle = false;
            while !stop_a.load(Ordering::Relaxed) {
                if toggle {
                    sm_a.set_selected_pipeline(Some("web-team".into()));
                } else {
                    sm_a.set_selected_pipeline(None);
                }
                toggle = !toggle;
                count_a_t.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Producer B: hammers the agent-loop path (add_assistant_message
        // → persist_current → sync_to_conversation, which takes
        // state.read → current_conversation.lock). If the new
        // `set_selected_pipeline` holds both guards together, this is
        // the inverse-order path that wedges.
        let sm_b = sm.clone();
        let stop_b = stop.clone();
        let count_b_t = count_b.clone();
        let t_b = thread::spawn(move || {
            while !stop_b.load(Ordering::Relaxed) {
                sm_b.add_assistant_message("reply".into());
                count_b_t.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Watchdog — mirror the existing deadlock pin: pure atomic
        // polling, never touches any involved lock.
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
                        eprintln!(
                            "deadlock detected: select-pipeline counters frozen at A={now_a}, B={now_b} for >2s"
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

        thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);
        t_a.join().expect("producer A must not panic");
        t_b.join().expect("producer B must not panic");

        assert!(
            count_a.load(Ordering::Relaxed) > 0,
            "producer A (set_selected_pipeline) made no progress"
        );
        assert!(
            count_b.load(Ordering::Relaxed) > 0,
            "producer B (add_assistant_message → persist_current) made no progress"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // #183 — "Stop button stops *everything*"
    //
    // The contracts below are the test plan from `issue-183-design.md` §5
    // (T1, T2, T5). They are written against the *new* API the design names
    // (`StateManager::turn_cancel_token`, `StateManager::stop_turn_processes`,
    // and the [`StopTally`] snapshot). Against the current code those
    // methods are **signature-only stubs** (marked `// #183: stub` inline)
    // that compile but do nothing — so these tests go RED on behaviour, not
    // on a missing API.
    // ─────────────────────────────────────────────────────────────────────

    use std::time::Duration;

    /// Build the canonical T1 fixture: a `StateManager` with two live bg
    /// processes (`sleep 30` — they take 30 s to exit naturally, so they're
    /// still running when the test body finishes), a `Running` bash panel,
    /// and a registered bash-stdin sender. Mirrors the design §8 T1 recipe.
    fn build_t1_fixture() -> Arc<StateManager> {
        let sm = StateManager::new_arc();
        // The bg registry refuses starts until a notify channel is attached
        // (see start_bg at `state_manager.rs:2043-2060`); do that first.
        let (bg_tx, _bg_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        sm.attach_bg_notify(bg_tx);

        // Mint a fresh turn token (in production: this is what the
        // implementation does inside `set_running(true)`).
        sm.set_running(true);

        // Two bg PTY-attached processes, each running `sleep 30`.
        for _ in 0..2 {
            sm.start_bg(StartParams {
                command: "sleep 30".into(),
                capture_cap: 0,
                cwd: None,
                label: None,
                cooldown: Duration::ZERO,
                env: None,
                shell: "sh".into(),
            })
            .expect("start_bg must succeed once a notify channel is attached");
        }

        // Foreground shell — drive the panel to Running directly. The pid
        // value is a placeholder; the bash panel's pid field doesn't have
        // to be a real process for the stop_turn_processes path.
        sm.start_bash_panel("sleep 30".into(), 4242);

        // Register a foreground bash stdin sender so the
        // `!has_active_bash_stdin()` post-stop assertion has something to
        // observe going away.
        let (stdin_tx, _stdin_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        sm.set_bash_stdin_tx(stdin_tx);

        sm
    }

    /// T1 — `stop_turn_processes_kills_and_reports` (design §8, T1).
    ///
    /// Asserts that calling `sm.stop_turn_processes()` on a fixture with two
    /// bg processes and a live foreground shell:
    ///   1. returns the snapshotted tally `{ shell: true }`,
    ///   2. leaves `bg_running_count() == 2` — Stop deliberately spares bg
    ///      (`clear_bg` lives on the rebuild paths, not Stop),
    ///   3. leaves `state.bash_panel.is_idle()` (panel was reset to Idle),
    ///   4. leaves `!has_active_bash_stdin()` (the foreground shell's stdin
    ///      slot was cleared),
    ///   5. leaves `turn_cancel_token().is_cancelled() == true` (the token
    ///      that `process_message_internal` raced against was cancelled,
    ///      which is what drops the turn's future at the next poll).
    #[test]
    fn stop_turn_processes_kills_and_reports() {
        let sm = build_t1_fixture();

        // Sanity: the fixture is actually in the state we expect.
        assert_eq!(
            sm.bg_running_count(),
            2,
            "fixture: both bg processes must be running before stop_turn_processes"
        );
        assert!(
            !sm.get_state().bash_panel.is_idle(),
            "fixture: bash panel must be Running before stop_turn_processes"
        );
        assert!(
            sm.has_active_bash_stdin(),
            "fixture: bash stdin sender must be registered before stop_turn_processes"
        );

        // The act — production body (design §4 step 5) snapshots the tally,
        // cancels the token, clears stdin, resets the panel. Background
        // processes are deliberately spared.
        let tally = sm.stop_turn_processes();

        // (1) The tally reports the foreground-shell kill; bg is not
        // reported because Stop does not kill bg.
        assert_eq!(
            tally,
            StopTally { shell: true },
            "stop_turn_processes must return {{ shell: true }} on the T1 fixture"
        );

        // (2) The bg registry is untouched — the two `sleep 30` children
        // survive Stop. (`BgRegistry::drop` on the test teardown will
        // SIGHUP them; that cleanup is per-StateManager, so it does not
        // leak into other tests.)
        assert_eq!(
            sm.bg_running_count(),
            2,
            "stop_turn_processes must NOT kill bg — Stop spares them"
        );

        // (3) The bash panel is back to Idle.
        assert!(
            sm.get_state().bash_panel.is_idle(),
            "stop_turn_processes must reset the bash panel to Idle"
        );

        // (4) The bash stdin sender is gone.
        assert!(
            !sm.has_active_bash_stdin(),
            "stop_turn_processes must clear the bash-stdin sender"
        );

        // (5) The turn-cancel token is cancelled. Production: this is what
        // `process_message_internal`'s `select!` observed and what made
        // the turn's future be dropped.
        assert!(
            sm.turn_cancel_token().is_cancelled(),
            "stop_turn_processes must cancel the turn's cancellation token"
        );
    }

    /// T2 — `stop_turn_processes_is_idempotent` (design §8, T2).
    ///
    /// Pins the idempotence half: a second call kills nothing and returns
    /// `StopTally::default()`. Background (`bash_bg`) processes are
    /// deliberately spared by Stop in both calls, so they survive unchanged
    /// — pinning that here too.
    ///
    /// Note: the second-call tally assertion *currently* passes on the stub
    /// (which always returns `default()`), so the test's RED signal is the
    /// "leaves the panel idle and the stdin slot cleared after the first
    /// call" assertion — which fails on the stub because the first call
    /// does not touch the panel or stdin.
    #[test]
    fn stop_turn_processes_is_idempotent() {
        let sm = build_t1_fixture();

        // First call. Production: returns { shell: true } and unwinds.
        let first = sm.stop_turn_processes();
        assert_eq!(
            first,
            StopTally { shell: true },
            "first stop_turn_processes on the T1 fixture must report the tally"
        );
        // The structural "exactly once" property: after the first call,
        // every state transition is already done — panel Idle, stdin slot
        // cleared. Production satisfies this; the stub does not.
        assert!(
            sm.get_state().bash_panel.is_idle(),
            "after the first stop_turn_processes, the bash panel must be Idle"
        );
        assert!(
            !sm.has_active_bash_stdin(),
            "after the first stop_turn_processes, the bash stdin sender must be cleared"
        );
        // Background processes are untouched (Stop spares them) — both
        // calls leave the same `bg_running_count()`.
        assert_eq!(
            sm.bg_running_count(),
            2,
            "stop must not touch bg — both bg children survive"
        );

        // Second call. Production: a no-op that returns default() because
        // (a) cancel of an already-cancelled token is a no-op, (b)
        // clearing an already-cleared stdin slot is a no-op, (c)
        // resetting an already-Idle panel is a no-op.
        let second = sm.stop_turn_processes();
        assert_eq!(
            second,
            StopTally::default(),
            "a second stop_turn_processes on a clean fixture must return StopTally::default()"
        );
        // The state survives the second call.
        assert!(sm.get_state().bash_panel.is_idle(), "panel must stay Idle");
        assert!(!sm.has_active_bash_stdin(), "stdin slot must stay cleared");
        assert_eq!(
            sm.bg_running_count(),
            2,
            "second stop must still leave bg untouched"
        );
    }

    /// T5 — `set_running_true_mints_a_fresh_token` (design §8, T5, D-D).
    ///
    /// Pins the freshness invariant: a `set_running(true)` write that
    /// transitions the SM from idle to running must replace the turn-cancel
    /// token with a fresh one. Concretely: after `stop_turn_processes()`
    /// (which cancels the *current* turn's token), the token must read
    /// cancelled; after `set_running(true)`, the next read must return a
    /// fresh, uncancelled token (so the next turn starts on a clean slate).
    #[test]
    fn set_running_true_mints_a_fresh_token() {
        let sm = StateManager::new_arc();
        // No set_running() yet ⇒ no current turn. We mint the first turn
        // explicitly so we have a known token to cancel.
        sm.set_running(true);
        let a = sm.turn_cancel_token();
        assert!(
            !a.is_cancelled(),
            "freshly-minted token must not be cancelled"
        );

        // Stop the turn. Production: cancels `a`; with the stub, no-op.
        sm.stop_turn_processes();
        assert!(
            a.is_cancelled(),
            "stop_turn_processes must cancel the current turn's token"
        );

        // Start the next turn. Production: mints a fresh token `b` that is
        // independent of `a`. The stub does not re-mint, so `b` equals
        // `a` and inherits its cancelled state.
        sm.set_running(true);
        let b = sm.turn_cancel_token();
        assert!(
            !b.is_cancelled(),
            "the next turn's token must be fresh (not cancelled from a prior turn)"
        );
        assert!(
            a.is_cancelled(),
            "the previous turn's token must remain cancelled \
             (so a stale token from a prior turn cannot leak into the next turn)"
        );
    }
}
