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
        match agent
            .prompt_with_history(args.task.as_str(), &mut history)
            .await
        {
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
}
