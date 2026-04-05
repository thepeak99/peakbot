//! DelegateTool - allows the entrypoint agent to delegate tasks to sub-agents.
//!
//! This tool implements the `Tool` trait and provides a way for the main agent
//! to call specialized sub-agents defined in the pipeline configuration.

use crate::pipeline::registry::SubAgentRegistry;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use std::sync::Mutex;

/// Default timeout for delegation (2 minutes)
const DEFAULT_TIMEOUT_SECONDS: u64 = 120;

/// Default delegation mode
const DEFAULT_MODE: &str = "series";

/// Tool for delegating tasks to sub-agents
#[derive(Clone)]
pub struct DelegateTool {
    /// Registry of available sub-agents
    registry: Arc<SubAgentRegistry>,
    /// Cost tracker for aggregating costs
    #[allow(unused)]
    cost_tracker: Arc<Mutex<crate::hooks::SessionStats>>,
    /// Default timeout for agent execution
    #[allow(unused)]
    default_timeout: Duration,
}

impl DelegateTool {
    /// Create a new delegate tool
    pub fn new(
        registry: Arc<SubAgentRegistry>,
        cost_tracker: Arc<Mutex<crate::hooks::SessionStats>>,
    ) -> Self {
        Self {
            registry,
            cost_tracker,
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
        }
    }

    /// Create a new delegate tool with custom timeout
    pub fn with_timeout(
        registry: Arc<SubAgentRegistry>,
        cost_tracker: Arc<Mutex<crate::hooks::SessionStats>>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            registry,
            cost_tracker,
            default_timeout: Duration::from_secs(timeout_seconds),
        }
    }

    /// Get the list of available agents for tool description
    fn available_agents_description(&self) -> String {
        let agents = self.registry.list_agents();
        if agents.is_empty() {
            "No agents configured".to_string()
        } else {
            agents.join(", ")
        }
    }

    /// Run a single agent to completion
    async fn run_single_agent(
        &self,
        name: &str,
        task: &str,
        timeout_duration: Duration,
    ) -> Result<AgentExecutionResult, DelegateError> {
        let start = Instant::now();

        let (agent, _info) = self
            .registry
            .create_agent(name)
            .map_err(|e| DelegateError::AgentCreation {
                agent: name.to_string(),
                error: e.to_string(),
            })?;

        // Run the agent with timeout
        let result = timeout(timeout_duration, async {
            agent.prompt(task).await
        })
        .await;

        let duration = start.elapsed();

        match result {
            Ok(Ok(response)) => {
                tracing::info!(
                    "Agent '{}' completed successfully in {:?}",
                    name,
                    duration
                );

                Ok(AgentExecutionResult {
                    agent_name: name.to_string(),
                    response,
                    duration_secs: duration.as_secs_f64(),
                    tokens_used: None,
                    cost: None,
                    timed_out: false,
                })
            }
            Ok(Err(e)) => {
                tracing::error!("Agent '{}' failed: {}", name, e);
                Err(DelegateError::AgentRun {
                    agent: name.to_string(),
                    error: e.to_string(),
                })
            }
            Err(_) => {
                tracing::warn!("Agent '{}' timed out after {:?}", name, timeout_duration);
                Err(DelegateError::Timeout {
                    agent: name.to_string(),
                    timeout_secs: timeout_duration.as_secs(),
                })
            }
        }
    }

    /// Execute agents in series (one after another, passing results)
    #[allow(dead_code)]
    async fn execute_series(
        &self,
        agents: &[String],
        initial_task: &str,
        timeout_seconds: u64,
    ) -> Result<String, DelegateError> {
        let timeout_duration = Duration::from_secs(timeout_seconds);
        let mut current_task = initial_task.to_string();
        let mut results: Vec<AgentExecutionResult> = Vec::new();

        for agent_name in agents {
            tracing::info!("Series: Running agent '{}'", agent_name);

            let result = self
                .run_single_agent(agent_name, &current_task, timeout_duration)
                .await?;

            // Pass result to next agent
            current_task = result.response.clone();
            results.push(result);
        }

        Ok(Self::format_series_results_static(&results))
    }

    /// Execute agents in parallel (all at once)
    async fn execute_parallel(
        &self,
        agents: &[String],
        task: &str,
        timeout_seconds: u64,
    ) -> Result<String, DelegateError> {
        let timeout_duration = Duration::from_secs(timeout_seconds);

        tracing::info!(
            "Parallel: Running {} agents simultaneously",
            agents.len()
        );

        // Create a tool clone for each task
        let mut handles = Vec::new();

        for agent_name in agents {
            let agent_name = agent_name.clone();
            let task = task.to_string();
            let tool = self.clone();
            let timeout = timeout_duration;

            let handle = tokio::spawn(async move {
                tool.run_single_agent(&agent_name, &task, timeout).await
            });

            handles.push(handle);
        }

        // Wait for all to complete
        let mut results: Vec<AgentExecutionResult> = Vec::new();

        for (i, handle) in handles.into_iter().enumerate() {
            match handle.await {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(e)) => {
                    tracing::error!("Agent {} failed: {}", i, e);
                    results.push(AgentExecutionResult {
                        agent_name: agents[i].clone(),
                        response: format!("Error: {}", e),
                        duration_secs: 0.0,
                        tokens_used: None,
                        cost: None,
                        timed_out: true,
                    });
                }
                Err(e) => {
                    tracing::error!("Agent {} panicked: {}", i, e);
                    results.push(AgentExecutionResult {
                        agent_name: agents[i].clone(),
                        response: format!("Panic: {}", e),
                        duration_secs: 0.0,
                        tokens_used: None,
                        cost: None,
                        timed_out: true,
                    });
                }
            }
        }

        Ok(Self::format_parallel_results_static(&results))
    }

    /// Format results from series execution
    #[allow(dead_code)]
    fn format_series_results_static(results: &[AgentExecutionResult]) -> String {
        let mut output = String::new();
        output.push_str("# Series Execution Results\n\n");

        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "## Step {}: {} ({}s)\n\n{}\n\n",
                i + 1,
                result.agent_name,
                format_duration(result.duration_secs),
                result.response
            ));
        }

        output
    }

    /// Format results from parallel execution
    fn format_parallel_results_static(results: &[AgentExecutionResult]) -> String {
        let mut output = String::new();
        output.push_str("# Parallel Execution Results\n\n");

        for result in results {
            output.push_str(&format!(
                "## {} ({}s){}\n\n{}\n\n",
                result.agent_name,
                format_duration(result.duration_secs),
                if result.timed_out { " [TIMED OUT]" } else { "" },
                result.response
            ));
        }

        output
    }
}

/// Result from a single agent execution
#[derive(Debug, Clone)]
struct AgentExecutionResult {
    agent_name: String,
    response: String,
    duration_secs: f64,
    #[allow(dead_code)]
    tokens_used: Option<u64>,
    #[allow(dead_code)]
    cost: Option<f64>,
    timed_out: bool,
}

impl Tool for DelegateTool {
    const NAME: &'static str = "delegate";

    type Error = DelegateError;
    type Args = DelegateArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let agents_list = self.available_agents_description();

        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Delegate a task to a specialized sub-agent. Use this when a task requires \
                a different model or expertise that the current agent doesn't have. \
                \n\n\
                Available agents: {}\n\n\
                Modes:\n\
                - 'series': Task is sent to one agent after another. Each agent receives \
                the result of the previous agent (for dependent steps like research → write → review)\n\
                - 'parallel': Same task is sent to multiple agents simultaneously for exploration \
                (for comparing different approaches or perspectives)\n\n\
                Note: Each delegation starts with fresh context - the sub-agent does not have \
                access to the conversation history.",
                agents_list
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Name of the sub-agent to use. Available: ".to_string() + &agents_list
                    },
                    "task": {
                        "type": "string",
                        "description": "Detailed description of the task for the sub-agent. \
                                      Include all context needed to complete the task."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["series", "parallel"],
                        "default": "series",
                        "description": "Execution mode: 'series' for sequential execution with result passing, \
                                      'parallel' for simultaneous execution"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "default": 120,
                        "description": "Maximum time to wait for the sub-agent (default: 120 seconds)"
                    }
                },
                "required": ["agent", "task"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Get the agent names list before async block
        let agents_list = self.registry.list_agents();
        let agents_vec: Vec<String> = agents_list.into_iter().map(|s| s.to_string()).collect();
        
        // Validate agent exists
        if !self.registry.has_agent(&args.agent) {
            return Err(DelegateError::UnknownAgent {
                agent: args.agent,
                available: agents_vec,
            });
        }

        let timeout_seconds = args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        let mode = args.mode.as_deref().unwrap_or(DEFAULT_MODE);

        match mode {
            "series" => {
                // Series mode: just one agent
                let result = self
                    .run_single_agent(
                        &args.agent,
                        &args.task,
                        Duration::from_secs(timeout_seconds),
                    )
                    .await?;

                Ok(format!(
                    "# Delegation Result\n\n\
                     Agent: {}\n\
                     Duration: {}s\n\n\
                     ## Response:\n\n{}",
                    result.agent_name,
                    format_duration(result.duration_secs),
                    result.response
                ))
            }
            "parallel" => {
                // Parallel mode: same task to multiple agents
                // Parse comma-separated agent list
                let agents: Vec<String> = args
                    .agent
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();

                if agents.len() < 2 {
                    return Err(DelegateError::InvalidMode {
                        mode: mode.to_string(),
                        reason: "parallel mode requires at least 2 agents (comma-separated)".to_string(),
                    });
                }

                self.execute_parallel(&agents, &args.task, timeout_seconds)
                    .await
            }
            _ => Err(DelegateError::InvalidMode {
                mode: mode.to_string(),
                reason: "use 'series' or 'parallel'".to_string(),
            }),
        }
    }
}

/// Arguments for the delegate tool
#[derive(Debug, Deserialize)]
pub struct DelegateArgs {
    /// Name of the sub-agent(s) to delegate to
    /// For series: single agent name
    /// For parallel: comma-separated list of agent names
    pub agent: String,

    /// Task description for the sub-agent(s)
    pub task: String,

    /// Execution mode: "series" or "parallel"
    #[serde(default)]
    pub mode: Option<String>,

    /// Optional timeout in seconds
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Errors from the delegate tool
#[derive(Debug, thiserror::Error)]
pub enum DelegateError {
    #[error("Unknown agent: '{agent}'. Available: {available:?}")]
    UnknownAgent {
        agent: String,
        available: Vec<String>,
    },

    #[error("Invalid mode: '{mode}'. {reason}")]
    InvalidMode { mode: String, reason: String },

    #[error("Agent '{agent}' timed out after {timeout_secs}s")]
    Timeout { agent: String, timeout_secs: u64 },

    #[error("Failed to create agent '{agent}': {error}")]
    AgentCreation { agent: String, error: String },

    #[error("Agent '{agent}' failed: {error}")]
    AgentRun { agent: String, error: String },

    #[error("Registry error: {0}")]
    Registry(String),
}

/// Format duration in a human-readable way
fn format_duration(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("{:.1}", seconds * 1000.0) + "ms"
    } else if seconds < 60.0 {
        format!("{:.1}s", seconds)
    } else {
        let mins = (seconds / 60.0) as u32;
        let secs = seconds % 60.0;
        format!("{}m {:.1}s", mins, secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_ms() {
        assert_eq!(format_duration(0.5), "500.0ms");
        assert_eq!(format_duration(0.025), "25.0ms");
    }

    #[test]
    fn test_format_duration_s() {
        assert_eq!(format_duration(5.5), "5.5s");
        assert_eq!(format_duration(59.9), "59.9s");
    }

    #[test]
    fn test_format_duration_m() {
        assert_eq!(format_duration(65.0), "1m 5.0s");
        assert_eq!(format_duration(125.5), "2m 5.5s");
    }
}

