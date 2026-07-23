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
use crate::pipeline::registry::SubAgentRegistry;
use crate::state::StateManager;
use crate::tools::ShellKind;
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
    /// The orchestrator's event sink. Sub-agent events are pushed here tagged
    /// `SubAgent { role }`. `None` under Ollama (hookless) — see `build_sub_agent`.
    pub event_sink: Option<mpsc::UnboundedSender<SourcedEvent>>,
    /// The cell holding the currently-running sub-agent hook, for `/stop`.
    pub active_hook: ActiveSubAgentHook,
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
                Available roles: {roles}"
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
                    }
                },
                "required": ["role", "task"]
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

        let bash_config = merge_role_env(&deps.bash_config, role.env.as_ref());

        let (agent, hook) = crate::providers::build_sub_agent(
            &role.model.provider_config,
            &role.prompt,
            deps.event_sink.clone(),
            &args.role,
            deps.searxng.as_ref(),
            deps.max_turns,
            &bash_config,
            &deps.tools_filter,
            deps.state_manager.clone(),
            deps.shell_kind.as_ref(),
            deps.vector_store.as_ref(),
        )
        .map_err(|e| DelegateError::Build {
            role: args.role.clone(),
            error: e.to_string(),
        })?;

        // Register the hook so `/stop` reaches this innermost agent. The guard
        // clears the cell on any exit path.
        let _guard = ActiveHookGuard::set(&deps.active_hook, hook);

        // Fresh history — pure agents-as-tools; no memory of prior delegations.
        let mut history = Vec::new();
        agent
            .prompt_with_history(args.task.as_str(), &mut history)
            .await
            .map_err(|e| DelegateError::Run {
                role: args.role.clone(),
                error: e.to_string(),
            })
    }
}

/// Arguments for the delegate tool: one role, one task.
#[derive(Debug, Deserialize)]
pub struct DelegateArgs {
    /// Which sub-agent role to run.
    pub role: String,
    /// The self-contained task brief for the sub-agent.
    pub task: String,
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

    /// The delegate surface is exactly `{role, task}` — no `mode`, no
    /// `timeout`, no comma-split agent list. Sequential-only is unrepresentable.
    #[test]
    fn delegate_args_parse_role_and_task() {
        let args: DelegateArgs = serde_json::from_value(
            serde_json::json!({"role": "reviewer", "task": "review the diff"}),
        )
        .expect("role+task parse");
        assert_eq!(args.role, "reviewer");
        assert_eq!(args.task, "review the diff");
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
}
