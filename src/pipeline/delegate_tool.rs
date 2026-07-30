//! `DelegateTool` — lets the orchestrator delegate one task to one sub-agent.
//!
//! The tool surface is deliberately minimal: `delegate(role, task) -> string`.
//! Sequential-only is enforced *structurally* — there is no parallel mode to
//! express. The orchestrator sequences by calling `delegate` repeatedly.
//!
//! A delegation runs the role's agent to completion on a **fresh** history
//! (pure agents-as-tools: role preamble + the one task + its own tool loop),
//! then returns the final text. The orchestrator's wire history records only
//! `ToolCall(delegate, {role, task})` + `ToolResult(final_text)` — the
//! sub-agent's internal turns never enter the orchestrator's context (they are
//! TEE'd to the shared transcript tagged `SubAgent { role }`, and filtered out
//! of the orchestrator wire by `get_agent_history`).

use crate::config::{BashConfig, SearXngConfig};
use crate::hooks::SessionHook;
use crate::hooks::events::SourcedEvent;
use crate::pipeline::ActiveSubAgentHook;
use crate::pipeline::handoff;
use crate::pipeline::registry::SubAgentRegistry;
use crate::state::StateManager;
use crate::tools::ShellKind;
use crate::tools::todo::TodoList;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Build context a sub-agent needs, captured where the orchestrator agent is
/// constructed (inside `add_builtin_tools`). A per-delegation fresh agent
/// genuinely needs the same build inputs the orchestrator had — searxng, the
/// bash env, the session `StateManager` (session cwd + bg registry), the
/// detected shell, the vector store, `max_turns` — plus the event sink to TEE
/// its turns and the active-hook cell for stop routing.
#[derive(Clone)]
pub struct SubAgentDeps {
    pub registry: Arc<SubAgentRegistry>,
    pub searxng: Option<SearXngConfig>,
    pub bash_config: BashConfig,
    pub tools_filter: crate::config::ToolsConfig,
    pub state_manager: Arc<StateManager>,
    pub shell_kind: Option<ShellKind>,
    pub vector_store: Option<crate::vector::VectorStore>,
    pub max_turns: usize,
    /// The discovered skills, used to build each role's per-role-filtered
    /// skills section in the sub-agent preamble (derived at delegation time).
    pub skills: crate::skills::SkillRegistry,
    /// The orchestrator's event sink. Sub-agent events are pushed here tagged
    /// `SubAgent { role }`. `None` under Ollama (hookless) — see `build_sub_agent`.
    pub event_sink: Option<mpsc::UnboundedSender<SourcedEvent>>,
    /// The cell holding the currently-running sub-agent hook, for `/stop`.
    pub active_hook: ActiveSubAgentHook,
    /// Retry policy for a delegation's wire calls — the orchestrator's own.
    pub retry: crate::config::RetryConfig,
    /// Wall-clock budgets — the delegation's prompt loop reads `delegate_secs`
    /// from here, so the operator's config drives it rather than a constant.
    pub timeouts: crate::config::TimeoutsConfig,
}

/// Build a sub-agent's preamble: `role_prompt` + the live env block + this
/// role's filtered skills, optionally followed by the repo's `agents.md`
/// (only when the role sets `agents_md: true`). Deliberately lean — no
/// persona, core guidance, or memory. Sections are separated by blank lines;
/// empty pieces (no skills shown, no agents.md) contribute nothing.
fn build_sub_agent_preamble(
    role_prompt: &str,
    shell_kind: Option<&ShellKind>,
    cwd: &std::path::Path,
    skills: &crate::skills::SkillRegistry,
    filter: &crate::config::SkillFilter,
    agents_md: bool,
) -> String {
    let mut preamble = role_prompt.to_string();
    preamble.push_str(&crate::env_block(shell_kind, cwd));
    preamble.push_str(&skills.to_system_prompt_section_filtered(filter));
    if agents_md {
        preamble.push_str(&crate::agents_md_section(cwd));
    }
    preamble
}

/// Guard against an empty sub-agent result (#222). A sub-agent that produced
/// only tool calls (or nothing) yields `""`, which becomes an empty
/// `ToolResult` — provider adapters reject empty tool-result content and the
/// orchestrator's loop crashes. Replace whitespace-only output with a sentinel
/// naming the role; non-empty output passes through unchanged.
fn normalize_delegate_output(role: &str, raw: String) -> String {
    if raw.trim().is_empty() {
        format!("[sub-agent '{role}' produced no output — re-run with a more focused task.]")
    } else {
        raw
    }
}

/// Merge a role's `env:` over a base bash env. Role keys win; base-only keys
/// are kept. Returns a fresh `BashConfig` with the merged env — the base is
/// not mutated (delegations must not leak env into the orchestrator's bash).
fn merge_role_env(base: &BashConfig, role_env: Option<&HashMap<String, String>>) -> BashConfig {
    let Some(role_env) = role_env else {
        return base.clone();
    };
    let mut merged = base.env.clone().unwrap_or_default();
    for (k, v) in role_env {
        merged.insert(k.clone(), v.clone());
    }
    BashConfig { env: Some(merged) }
}

/// Fire `request_stop` on the active sub-agent hook, if a delegation is
/// running. A no-op when the cell is empty. Called by the `/stop` dispatcher
/// alongside the orchestrator hook so a stop lands on the innermost running
/// agent; the whole turn then unwinds out (D6).
pub fn fire_stop(active: &ActiveSubAgentHook) {
    if let Some(hook) = active.lock().unwrap().as_ref() {
        hook.request_stop();
    }
}

/// RAII guard: registers the running sub-agent hook in the shared cell on
/// construction and clears it on drop — so the cell is cleared even if the
/// sub-agent run panics or returns early.
struct ActiveHookGuard<'a> {
    cell: &'a ActiveSubAgentHook,
}

impl<'a> ActiveHookGuard<'a> {
    fn set(cell: &'a ActiveSubAgentHook, hook: Arc<SessionHook>) -> Self {
        *cell.lock().unwrap() = Some(hook);
        Self { cell }
    }
}

impl Drop for ActiveHookGuard<'_> {
    fn drop(&mut self) {
        *self.cell.lock().unwrap() = None;
    }
}

/// Tool for delegating a task to a sub-agent.
#[derive(Clone)]
pub struct DelegateTool {
    deps: Arc<SubAgentDeps>,
}

impl DelegateTool {
    pub fn new(deps: Arc<SubAgentDeps>) -> Self {
        Self { deps }
    }

    /// Sorted role names, for the tool description + error messages.
    fn available_roles(&self) -> Vec<String> {
        let mut roles: Vec<String> = self
            .deps
            .registry
            .list_agents()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        roles.sort();
        roles
    }
}

impl Tool for DelegateTool {
    const NAME: &'static str = "delegate";

    type Error = DelegateError;
    type Args = DelegateArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let roles = self.available_roles().join(", ");
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Delegate ONE task to ONE specialised sub-agent, which runs to \
                completion with its own fresh context and returns a single result. \
                You see only the result string — never the sub-agent's internal \
                steps. Delegations are sequential: call `delegate` again to chain \
                work. Each call starts fresh — the sub-agent has NO memory of prior \
                delegations or of this conversation, so put everything it needs in \
                `task`.\n\n\
                Write `task` as a self-contained brief: state the objective, the \
                expected output shape, and the boundaries (what NOT to do). Scale \
                effort to complexity — don't delegate a one-liner you can do yourself.\n\n\
                Available roles: {roles}\n\n\
                Keep a todo list while you orchestrate. Before every `delegate` call, \
                add a todo item describing the work you are handing off, then pass that \
                item's id as `parent_task_id` — the sub-agent's own subtasks are \
                displayed nested under it. Update the item's status when the delegation \
                returns."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "description": format!("Which sub-agent to run. One of: {roles}")
                    },
                    "task": {
                        "type": "string",
                        "description": "Self-contained brief for the sub-agent: objective, \
                                        expected output, and boundaries. Include ALL context — \
                                        the sub-agent cannot see this conversation."
                    },
                    "parent_task_id": {
                        "type": "integer",
                        "description": "The id of YOUR todo item that this delegation fulfils. \
                            Add the item first with the todo tool — its result gives you the id \
                            (\"Added task #3: …\") — or read `todo action=list`. Must be an id \
                            that currently exists in your todo list."
                    }
                },
                "required": ["role", "task", "parent_task_id"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let deps = &self.deps;

        let role = deps
            .registry
            .role(&args.role)
            .ok_or_else(|| DelegateError::UnknownRole {
                role: args.role.clone(),
                available: self.available_roles(),
            })?;

        // Snapshot todo list and validate parent before any awaits (lock discipline).
        let snapshot = deps.state_manager.get_todo_list();
        validate_parent(&snapshot, args.parent_task_id)?;

        let bash_config = merge_role_env(&deps.bash_config, role.env.as_ref());

        // Enrich the role prompt with a lean shared context: the live env
        // block (cwd/time/shell) + this role's filtered skills, plus the repo's
        // agents.md only when the role opts in (`agents_md: true`). No persona,
        // no core tool guidance, no memory — a sub-agent's `prompt` is its whole
        // persona; everything else it needs goes in the task. Derived here (not
        // cached) so cwd/time are always current.
        let preamble = build_sub_agent_preamble(
            &role.prompt,
            deps.shell_kind.as_ref(),
            &deps.state_manager.session_cwd(),
            &deps.skills,
            &role.skills,
            role.agents_md,
        );

        // Size the gate against THIS role's model, not the orchestrator's:
        // roles routinely run on smaller-context models.
        let context_budget = deps
            .state_manager
            .compaction_threshold()
            .map(|fraction| (fraction * role.model.context_size as f64) as usize);

        let (agent, hook) = crate::providers::build_sub_agent(
            &role.model.provider_config,
            &preamble,
            deps.event_sink.clone(),
            &args.role,
            deps.searxng.as_ref(),
            deps.max_turns,
            &bash_config,
            &deps.tools_filter,
            deps.state_manager.clone(),
            deps.shell_kind.as_ref(),
            deps.vector_store.as_ref(),
            context_budget,
            &deps.timeouts,
        )
        .map_err(|e| DelegateError::Build {
            role: args.role.clone(),
            error: e.to_string(),
        })?;

        // Register the hook so `/stop` reaches this innermost agent. The guard
        // clears the cell on any exit path. The failure path still needs the
        // hook to read its history snapshot, hence the clone.
        let _guard = ActiveHookGuard::set(&deps.active_hook, hook.clone());

        // Fresh history — pure agents-as-tools; no memory of prior delegations.
        let mut history = Vec::new();
        let mut attempt = 0;
        // The whole loop sits inside the deadline, so the wire-retry budget is
        // part of it rather than additive to it. On expiry the delegation is
        // cancelled and its transcript is still salvaged through the normal
        // handoff path — the outer `budget_for("delegate")` sits a salvage
        // margin above this bound, leaving room for that summarisation.
        let budget = crate::tools::time_budget::delegate_loop_budget(&deps.timeouts);
        let bounded = tokio::time::timeout(budget, async {
            loop {
                match agent
                    .prompt_with_history(args.task.as_str(), &mut history)
                    .await
                {
                    Ok(text) => break Ok(text),
                    // Transient wire failures (429/5xx/transport) get the same
                    // in-place retry the main loop gives its own turns — only what
                    // outlives the budget is worth handing back. The retry re-runs
                    // the delegation from the task: rig owns the sub-agent's
                    // internal loop state, which isn't resumable from out here.
                    Err(e) => {
                        let Some(delay) =
                            crate::providers::retry::next_retry_delay(&e, attempt, &deps.retry)
                        else {
                            break Err(e);
                        };
                        tracing::warn!(
                            target: "peakbot",
                            role = %args.role,
                            attempt = attempt + 1,
                            max_retries = deps.retry.max_retries,
                            backoff_ms = delay.as_millis(),
                            error = %e,
                            "Sub-agent request failed transiently; backing off before retry"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                }
            }
        })
        .await;

        let Ok(outcome) = bounded else {
            tracing::error!(
                target: "peakbot",
                role = %args.role,
                budget_secs = budget.as_secs(),
                "Delegation exceeded its wall-clock budget; cancelling and salvaging a handoff"
            );
            // Bypass `classify` — there is no `PromptError` here, we cancelled
            // it ourselves — and hand the snapshot straight to the renderer so
            // the work already done still reaches the orchestrator.
            let h = handoff::Handoff::Failed {
                error: format!(
                    "exceeded its {}s wall-clock budget and was cancelled",
                    budget.as_secs()
                ),
                history: hook.history_snapshot(),
            };
            return Ok(handoff::build(&args.role, h, &role.model.provider_config).await);
        };

        match outcome {
            Ok(text) => Ok(normalize_delegate_output(&args.role, text)),
            // A dead sub-agent still knows things. Everything but a user stop
            // comes back as a summarised handoff so the orchestrator can decide
            // what to re-delegate instead of re-running the same wall.
            Err(e) => match handoff::classify(e, hook.history_snapshot()) {
                handoff::Handoff::Abort(err) => Err(DelegateError::Run {
                    role: args.role.clone(),
                    error: err.to_string(),
                }),
                h => Ok(handoff::build(&args.role, h, &role.model.provider_config).await),
            },
        }
    }
}

/// Arguments for the delegate tool: one role, one task, and the parent todo id.
#[derive(Debug, Deserialize)]
pub struct DelegateArgs {
    /// Which sub-agent role to run.
    pub role: String,
    /// The self-contained task brief for the sub-agent.
    pub task: String,
    /// The id of the orchestrator's todo item this delegation fulfils.
    pub parent_task_id: usize,
}

/// Errors from the delegate tool.
#[derive(Debug, thiserror::Error)]
pub enum DelegateError {
    #[error("Unknown role '{role}'. Available: {available:?}")]
    UnknownRole {
        role: String,
        available: Vec<String>,
    },

    #[error("Failed to build sub-agent for role '{role}': {error}")]
    Build { role: String, error: String },

    #[error("Sub-agent '{role}' failed: {error}")]
    Run { role: String, error: String },

    #[error(
        "Unknown parent_task_id {id}: no such item in your todo list. \
         Add a todo item for this delegation first (todo action=add), then pass \
         its id. Your list:\n{list}"
    )]
    UnknownParentTask { id: usize, list: String },
}

/// Validate that `parent_task_id` exists in the orchestrator's todo list.
/// Accepts any status (including completed) — no status policing (design §3.1).
fn validate_parent(list: &TodoList, parent_task_id: usize) -> Result<(), DelegateError> {
    if list.get(parent_task_id).is_some() {
        Ok(())
    } else {
        Err(DelegateError::UnknownParentTask {
            id: parent_task_id,
            list: list.render(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::MessageSource;

    fn bash_with(env: &[(&str, &str)]) -> BashConfig {
        BashConfig {
            env: if env.is_empty() {
                None
            } else {
                Some(
                    env.iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                )
            },
        }
    }

    /// Build a minimal `DelegateTool` for unit tests that need its schema
    /// (`definition()`) but do not actually run a delegation. An empty
    /// `SubAgentRegistry` is fine because `definition()` only reads role
    /// names — never `call()`s them — and `Self::available_roles()` returns
    /// an empty vec when no roles are configured. The wiring is the same
    /// shape `providers::add_builtin_tools` builds in production, but every
    /// optional / empty slot is stubbed out.
    async fn minimal_delegate_tool() -> DelegateTool {
        use crate::config::{
            ModelEntry, ModelRegistry, PipelineConfig, ProviderEntry, ProviderType,
        };

        let provider = ProviderEntry {
            name: "openai".into(),
            kind: ProviderType::OpenAI,
            api_key: Some("sk-test".into()),
            base_url: None,
            models: vec![ModelEntry {
                name: "gpt-4o".into(),
                alias: Some("gpt4".into()),
                max_tokens: None,
                temperature: None,
                extra_params: None,
                prompt_caching: None,
                vision: None,
                context_size: None,
            }],
        };
        let model_registry =
            ModelRegistry::build(&[provider], Some("gpt4")).expect("test model registry builds");
        let pipeline_config = PipelineConfig {
            enabled: false,
            orchestrator_prompt: None,
            agents: std::collections::HashMap::new(),
        };
        let registry = SubAgentRegistry::new(&pipeline_config, &model_registry, &[])
            .expect("empty role registry builds");

        let deps = SubAgentDeps {
            registry: Arc::new(registry),
            searxng: None,
            bash_config: BashConfig::default(),
            tools_filter: crate::config::ToolsConfig::default(),
            state_manager: StateManager::new_arc(),
            shell_kind: None,
            vector_store: None,
            max_turns: 0,
            skills: crate::skills::SkillRegistry::default(),
            event_sink: None,
            active_hook: Arc::new(std::sync::Mutex::new(None)),
            retry: crate::config::RetryConfig::default(),
            timeouts: crate::config::TimeoutsConfig::default(),
        };
        DelegateTool::new(Arc::new(deps))
    }

    /// The delegate surface is exactly `{role, task, parent_task_id}` — the
    /// parent link is required, not optional (sub-agent todo nesting, design
    /// §3.1). An orphan delegation is unrepresentable at the boundary.
    #[test]
    fn delegate_args_parse_role_and_task() {
        let args: DelegateArgs = serde_json::from_value(serde_json::json!({
            "role": "reviewer",
            "task": "review the diff",
            "parent_task_id": 3,
        }))
        .expect("role+task+parent_task_id parse");
        assert_eq!(args.role, "reviewer");
        assert_eq!(args.task, "review the diff");
        assert_eq!(args.parent_task_id, 3);
    }

    /// A delegate call without `parent_task_id` must FAIL to deserialise.
    /// This pins the "unrepresentable" invariant: a delegation cannot be born
    /// without naming its parent todo item (design §3.1). Any transcript
    /// argument that drops the field is rejected at the Rust boundary.
    #[test]
    fn delegate_args_missing_parent_task_id_fails_to_deserialize() {
        let result: Result<DelegateArgs, _> = serde_json::from_value(serde_json::json!({
            "role": "reviewer",
            "task": "review the diff",
        }));
        assert!(
            result.is_err(),
            "DelegateArgs without parent_task_id must be Err — an orphan delegation is unrepresentable; got: {:?}",
            result.map(|a| (a.role, a.task))
        );
    }

    /// The model-facing schema advertises `parent_task_id` as a required
    /// integer property of the `delegate` tool. The description points the
    /// model at the todo tool (which echoes the id) so it can fetch the
    /// numeric id it needs to send (design §5.1).
    #[tokio::test]
    async fn delegate_definition_requires_parent_task_id_in_schema() {
        let tool = minimal_delegate_tool().await;
        let def = <DelegateTool as Tool>::definition(&tool, String::new()).await;

        // `required` must contain all three names (order-independent).
        let required = def
            .parameters
            .get("required")
            .and_then(|v| v.as_array())
            .expect("schema must have a `required` array");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        for name in ["role", "task", "parent_task_id"] {
            assert!(
                required_names.contains(&name),
                "delegate schema `required` must contain {name:?}; got {required_names:?}"
            );
        }

        // `properties.parent_task_id` is an integer and its description
        // mentions the todo tool so the model knows where to fetch the id.
        let parent_prop = def
            .parameters
            .get("properties")
            .and_then(|p| p.get("parent_task_id"))
            .expect("schema must have a `parent_task_id` property");
        assert_eq!(
            parent_prop.get("type").and_then(|v| v.as_str()),
            Some("integer"),
            "parent_task_id must be typed `integer`"
        );
        let desc = parent_prop
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            desc.to_lowercase().contains("todo"),
            "parent_task_id description must reference the todo tool so the model knows where to fetch the id; got: {desc:?}"
        );
    }

    /// `validate_parent` accepts an existing id, including one whose status is
    /// completed (design §3.1 — we don't police parent status; least
    /// astonishing for the orchestrator).
    #[test]
    fn validate_parent_accepts_existing_id_including_completed() {
        use crate::tools::todo::{TodoList, TodoStatus};

        let mut list = TodoList::new();
        list.add("live task".to_string());
        list.add("done task".to_string());
        list.update_status(2, TodoStatus::Completed)
            .expect("completed update");

        // Pending item → accepted.
        assert!(
            validate_parent(&list, 1).is_ok(),
            "validate_parent must accept a pending parent; list: {}",
            list.render()
        );
        // Completed item → still accepted (no status policing).
        assert!(
            validate_parent(&list, 2).is_ok(),
            "validate_parent must accept a completed parent (design §3.1 — no status policing)"
        );
    }

    /// `validate_parent` rejects a missing id; the error must surface the id,
    /// the literal token `parent_task_id`, and the same rendered list the
    /// `todo` tool returns (design §3.2). The model self-corrects from this.
    #[test]
    fn validate_parent_rejects_missing_id_with_actionable_error() {
        use crate::tools::todo::TodoList;

        let mut list = TodoList::new();
        list.add("alpha".to_string());
        list.add("beta".to_string());
        let rendered = list.render();

        let err = validate_parent(&list, 99).expect_err("id 99 is not in the list");
        let msg = err.to_string();

        assert!(
            msg.contains("99"),
            "error must surface the bad id (99) so the model can see which id it sent; got: {msg}"
        );
        assert!(
            msg.contains("parent_task_id"),
            "error must name the parameter so the model knows which arg to fix; got: {msg}"
        );
        assert!(
            msg.contains(&rendered) || msg.contains("beta") || msg.contains("alpha"),
            "error must include the rendered todo list (so the model can pick a real id); got: {msg}"
        );
    }

    /// `validate_parent` against an empty TodoList must produce an error whose
    /// rendered list block reads "No tasks in the todo list." — the same
    /// sentinel `TodoList::render()` uses (todo.rs:L250-254). The orchestrator
    /// sees an empty list and knows to add an item first.
    #[test]
    fn validate_parent_against_empty_list_returns_no_tasks_message() {
        use crate::tools::todo::TodoList;

        let list = TodoList::new();
        let err = validate_parent(&list, 1).expect_err("empty list rejects any id");
        let msg = err.to_string();

        assert!(
            msg.contains("No tasks in the todo list."),
            "error against an empty list must include the canonical empty-list sentinel; got: {msg}"
        );
    }

    /// A role's `env:` overrides base keys and unions in role-only keys; base
    /// keys the role doesn't touch survive.
    #[test]
    fn merge_role_env_overrides_and_unions() {
        let base = bash_with(&[("SHARED", "base"), ("BASE_ONLY", "keep")]);
        let role = HashMap::from([
            ("SHARED".to_string(), "role".to_string()),
            ("ROLE_ONLY".to_string(), "added".to_string()),
        ]);
        let merged = merge_role_env(&base, Some(&role)).env.expect("some env");
        assert_eq!(merged.get("SHARED").map(String::as_str), Some("role"));
        assert_eq!(merged.get("BASE_ONLY").map(String::as_str), Some("keep"));
        assert_eq!(merged.get("ROLE_ONLY").map(String::as_str), Some("added"));
    }

    /// No role env → the base bash config is returned unchanged.
    #[test]
    fn merge_role_env_none_keeps_base() {
        let base = bash_with(&[("BASE_ONLY", "keep")]);
        let merged = merge_role_env(&base, None).env.expect("some env");
        assert_eq!(merged.get("BASE_ONLY").map(String::as_str), Some("keep"));
        assert_eq!(merged.len(), 1);
    }

    /// `fire_stop` requests stop on the active sub-agent hook — the mechanism
    /// that lets `/stop` reach the innermost running agent (D6).
    #[test]
    fn fire_stop_requests_stop_on_active_hook() {
        let hook = Arc::new(SessionHook::new(None));
        let cell: ActiveSubAgentHook = Arc::new(std::sync::Mutex::new(Some(hook.clone())));
        assert!(!hook.is_stop_requested());
        fire_stop(&cell);
        assert!(hook.is_stop_requested(), "stop must reach the active hook");
    }

    /// `fire_stop` is a no-op when no delegation is running (empty cell).
    #[test]
    fn fire_stop_none_is_noop() {
        let cell: ActiveSubAgentHook = Arc::new(std::sync::Mutex::new(None));
        fire_stop(&cell); // must not panic
        assert!(cell.lock().unwrap().is_none());
    }

    /// The `ActiveHookGuard` clears the cell on drop, even on an early return
    /// path — so a stop after a delegation ends can't hit a stale hook.
    #[test]
    fn active_hook_guard_clears_cell_on_drop() {
        let cell: ActiveSubAgentHook = Arc::new(std::sync::Mutex::new(None));
        let hook = Arc::new(SessionHook::new(None));
        {
            let _guard = ActiveHookGuard::set(&cell, hook);
            assert!(cell.lock().unwrap().is_some(), "set registers the hook");
        }
        assert!(cell.lock().unwrap().is_none(), "drop clears the cell");
    }

    /// The events-only sub-agent hook stamps `SubAgent { role }` on its lane —
    /// so its turns TEE to the transcript tagged with the role and its cost
    /// rolls up. (Asserts the flavor `build_sub_agent` gives the hook.)
    #[test]
    fn sub_agent_hook_stamps_subagent_source() {
        let hook = SessionHook::new(None).with_source(MessageSource::SubAgent {
            role: "reviewer".to_string(),
        });
        assert_eq!(
            hook.source(),
            &MessageSource::SubAgent {
                role: "reviewer".to_string()
            }
        );
    }

    /// A sub-agent that returns empty or whitespace-only text must never reach
    /// the orchestrator as an empty `ToolResult` (#222 — empty tool-result
    /// content crashes provider adapters). The normalizer replaces it with a
    /// human-readable sentinel naming the role; non-empty output passes through
    /// byte-identical.
    #[test]
    fn delegate_output_never_empty() {
        for empty in ["", "   ", "\n\t  \n"] {
            let out = normalize_delegate_output("reviewer", empty.to_string());
            assert!(
                !out.trim().is_empty(),
                "empty sub-agent output {empty:?} must become a non-empty sentinel"
            );
            assert!(
                out.contains("reviewer"),
                "sentinel must name the role for a useful orchestrator signal"
            );
        }

        let real = "Found 3 issues in the diff.".to_string();
        assert_eq!(
            normalize_delegate_output("reviewer", real.clone()),
            real,
            "non-empty output must pass through unchanged"
        );
    }

    /// A role that opts in (`agents_md: true`) gets the repo's agents.md
    /// injected into its preamble; a role that doesn't opt in never sees it.
    #[test]
    fn preamble_includes_agents_md_only_when_role_opts_in() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agents.md"), "SENTINEL-SUBAGENT-CONTEXT").unwrap();
        let skills = crate::skills::SkillRegistry::default();
        let filter = crate::config::SkillFilter::default();

        let opted_in =
            build_sub_agent_preamble("role prompt", None, dir.path(), &skills, &filter, true);
        assert!(
            opted_in.contains("SENTINEL-SUBAGENT-CONTEXT"),
            "agents_md: true must inject the repo's agents.md"
        );

        let opted_out =
            build_sub_agent_preamble("role prompt", None, dir.path(), &skills, &filter, false);
        assert!(
            !opted_out.contains("SENTINEL-SUBAGENT-CONTEXT"),
            "default (agents_md: false) must keep the preamble lean"
        );
    }

    // The §3.4 ordering invariant for delegate — "the inner loop budget
    // strictly sits below the outer decorator budget, with the slack being
    // for handoff::build's summarisation LLM call" — used to be pinned here
    // against `DELEGATE_BUDGET` and `budget_for("delegate")` directly. After
    // the postmortem fix both APIs become configurable (`SubAgentDeps`
    // owns a `TimeoutsConfig`, `budget_for` takes one). The new home for
    // this invariant is `delegate_registration_strictly_exceeds_the_delegate_loop`
    // in `time_budget.rs`, which is the right module — it's an invariant
    // about the *budget table*, not about this tool.
}
