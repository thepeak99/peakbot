# Multi-Agent Pipelines for PeakBot

## TL;DR

Add a `DelegateTool` that lets the main agent (entrypoint) call sub-agents defined in config, with support for series and parallel execution. Each delegation is independent with fresh history. Sub-agents cannot delegate (cycle prevention). Keep it minimal—Rig already has agent loops; we just need config and a tool.

---

## 1. Motivation

The current PeakBot architecture uses a single agent. For complex tasks, users often need different models for different subtasks (e.g., a fast model for planning, a capable model for execution, a cheap model for research). Multi-agent pipelines solve this by letting multiple specialized agents work together.

### Use Cases

- **Research + Write**: One agent researches a topic, another writes the output
- **Code Review Pipeline**: One agent writes code, another reviews it
- **Parallel Exploration**: Multiple agents explore different approaches simultaneously
- **Specialized Models**: Use Gemini Flash for fast tasks, Claude Sonnet for complex reasoning

---

## 2. Design Principles (Zen of Software Engineering)

### 2.1 Simplicity is the Key
- Don't reinvent agent execution. Rig already has agent loops.
- Just need config + DelegateTool + agent registry.

### 2.2 Fewer Pieces → Fewer Things That Can Go Wrong
- Reuse existing: DynAgent, Tool pattern, Config system, CostTracker
- Don't build: agent-to-agent communication, complex orchestration graphs, built-in templates

### 2.3 Make Illegal States Unrepresentable
- Sub-agents must have unique names (enforced in config parsing)
- Sub-agents cannot access DelegateTool (prevents delegation cycles)
- If agent is defined, it must be valid (enforced at startup)

### 2.4 YAGNI — You Aren't Gonna Need It
- NOT building:
  - Multi-level delegation (sub-agents calling sub-agents)
  - Agent state persistence across sessions
  - Complex pipeline orchestration (entrypoint decides)
  - Shared conversation history between agents
  - Built-in pipeline templates

### 2.5 Principle of Least Astonishment
- Each delegation is **independent**—fresh history, no context carryover
- Sub-agents look like tools from the entrypoint's perspective
- The delegate tool is invisible to sub-agents

---

## 3. Architecture

### 3.1 High-Level Flow

```
User Input
    │
    ▼
┌─────────────────────────────────────────┐
│  Entrypoint Agent (main model)          │
│  - Has DelegateTool                     │
│  - Decides when/who to delegate to      │
└──────────────┬──────────────────────────┘
               │
               │ delegate(agent_name, task, mode)
               ▼
┌─────────────────────────────────────────┐
│  DelegateTool                           │
│  - Finds sub-agent in registry          │
│  - Creates fresh agent instance         │
│  - Runs agent to completion             │
│  - Returns result to entrypoint         │
└──────────────┬──────────────────────────┘
               │
     ┌─────────┴─────────┐
     │                   │
     ▼                   ▼
┌─────────┐        ┌─────────┐
│ Series  │        │Parallel │
│ Agent A │        │ ┌─────┐ │
│    │    │        │ │ A   │ │
│    ▼    │        │ ├─────┤ │
│ Agent B │        │ │ B   │ │
│         │        │ ├─────┤ │
└─────────┘        │ │ C   │ │
                   │ └─────┘ │
                   └─────────┘
```

### 3.2 Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Fresh history per delegation | Prevents context bloat, predictable behavior |
| Sub-agents can't delegate | Prevents cycles, keeps architecture flat |
| Shared tools (file, bash) | Sub-agents need the same capabilities |
| Aggregated cost tracking | User sees total cost across all agents |
| Config-driven agent definitions | Declarative, no code changes needed |

---

## 4. Data Structures

### 4.1 Configuration (config.rs additions)

```rust
/// Multi-agent pipeline configuration
#[derive(Debug, Deserialize, Clone)]
pub struct PipelineConfig {
    /// Whether multi-agent pipelines are enabled
    #[serde(default)]
    pub enabled: bool,
    
    /// Sub-agent definitions
    #[serde(default)]
    pub agents: HashMap<String, AgentDefinition>,
}

/// Definition of a sub-agent
#[derive(Debug, Deserialize, Clone)]
pub struct AgentDefinition {
    /// Agent type (must match a provider type)
    #[serde(rename = "type")]
    pub agent_type: ProviderType,
    
    /// Model to use for this agent
    #[serde(default)]
    pub model: Option<String>,
    
    /// System prompt / preamble for this agent
    pub prompt: String,
    
    /// Optional: max tokens override
    #[serde(default)]
    pub max_tokens: Option<u64>,
    
    /// Optional: temperature override
    #[serde(default)]
    pub temperature: Option<f32>,
}
```

### 4.2 Config File Format (YAML)

```yaml
# config.yaml

# Default provider (used by entrypoint)
provider:
  type: openrouter
  config:
    api_key: sk-or-v1-xxx
    model: anthropic/claude-3.7-sonnet
    max_tokens: 4096

# Multi-agent pipeline configuration
pipeline:
  enabled: true
  agents:
    researcher:
      type: openrouter
      model: google/gemini-2.0-flash-001
      prompt: |
        You are a research agent. Your job is to thoroughly research 
        the given topic and provide detailed findings.
        
        Tools available: file_read, bash, web_search, fetch_url
        
        Be comprehensive. Return your findings in a structured format.
        
    coder:
      type: openrouter
      model: anthropic/claude-3.7-sonnet
      prompt: |
        You are a coding agent. Based on the research provided, 
        write clean, working code.
        
        Tools available: file_edit, file_read, bash
        
        Always verify your code compiles/works before finishing.
        
    reviewer:
      type: openrouter
      model: anthropic/claude-3.5-sonnet
      prompt: |
        You are a code reviewer. Review the provided code and 
        suggest improvements.
        
        Tools available: file_read, bash
        
        Be constructive and specific in your feedback.
```

### 4.3 DelegateTool Interface

```rust
/// Arguments for the delegate tool
#[derive(Deserialize)]
pub struct DelegateToolArgs {
    /// Name of the sub-agent to delegate to
    pub agent: String,
    
    /// Task description for the sub-agent
    pub task: String,
    
    /// Execution mode: "series" or "parallel"
    #[serde(default = "default_mode")]
    pub mode: String,
    
    /// Optional: timeout in seconds (default: 120)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

/// Result from delegation
#[derive(Serialize)]
pub struct DelegateToolOutput {
    /// The agent that executed the task
    pub agent: String,
    
    /// The result from the sub-agent
    pub result: String,
    
    /// Execution time in seconds
    pub duration_seconds: f64,
    
    /// Token usage (if available)
    pub tokens_used: Option<u64>,
    
    /// Cost incurred (if available)
    pub cost: Option<f64>,
}
```

---

## 5. Module Structure

### 5.1 New Files

```
src/
├── pipeline/                    # NEW: Multi-agent pipeline module
│   ├── mod.rs                   # Module exports
│   ├── config.rs               # PipelineConfig, AgentDefinition
│   ├── registry.rs             # SubAgentRegistry
│   ├── delegate_tool.rs         # DelegateTool implementation
│   └── execution.rs            # Series/parallel execution logic
```

### 5.2 Modified Files

| File | Changes |
|------|---------|
| `src/config.rs` | Add PipelineConfig, AgentDefinition, update Config |
| `src/lib.rs` | Add pipeline module, integrate DelegateTool into entrypoint |
| `src/providers/mod.rs` | Add `create_agent_from_definition()` helper |

---

## 6. Component Details

### 6.1 SubAgentRegistry

```rust
/// Registry of available sub-agents
pub struct SubAgentRegistry {
    agents: HashMap<String, AgentDefinition>,
    api_key: String,  // Shared API key from entrypoint config
}

impl SubAgentRegistry {
    /// Create a new agent instance from a definition
    pub fn create_agent(&self, name: &str) -> Result<DynAgent> {
        let def = self.agents.get(name)
            .ok_or_else(|| anyhow!("Unknown agent: {}", name))?;
        
        // Create agent using existing provider abstraction
        create_agent_from_definition(def, &self.api_key)
    }
    
    /// Check if agent exists
    pub fn has_agent(&self, name: &str) -> bool {
        self.agents.contains_key(name)
    }
    
    /// List all available agents
    pub fn list_agents(&self) -> Vec<&str> {
        self.agents.keys().map(|s| s.as_str()).collect()
    }
}
```

### 6.2 DelegateTool

```rust
pub struct DelegateTool {
    registry: Arc<SubAgentRegistry>,
    cost_tracker: Arc<CostTracker>,
    timeout: Duration,
}

impl Tool for DelegateTool {
    const NAME: &'static str = "delegate";
    
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            description: "Delegate a task to a specialized sub-agent. \
                         Use this when a task requires a different model or expertise. \
                         
                         Modes:
                         - 'series': Task is sent to one agent after another (for dependent steps)
                         - 'parallel': Same task is sent to multiple agents simultaneously (for exploration)
                         
                         Available agents: {list_of_agents}",
            parameters: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Name of the sub-agent to use"
                    },
                    "task": {
                        "type": "string", 
                        "description": "Task description for the agent"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["series", "parallel"],
                        "default": "series",
                        "description": "Execution mode"
                    }
                },
                "required": ["agent", "task"]
            })
        }
    }
    
    async fn call(&self, args: DelegateToolArgs) -> Result<String, DelegateToolError> {
        // Validate agent exists
        if !self.registry.has_agent(&args.agent) {
            return Err(DelegateToolError::UnknownAgent(args.agent));
        }
        
        // Execute based on mode
        match args.mode.as_str() {
            "series" => self.execute_series(&args.agent, &args.task, args.timeout_seconds).await,
            "parallel" => self.execute_parallel(&args.agent, &args.task, args.timeout_seconds).await,
            _ => Err(DelegateToolError::InvalidMode(args.mode)),
        }
    }
}
```

### 6.3 Execution Modes

```rust
impl DelegateTool {
    /// Series execution: run agents one after another
    /// Each agent receives the result of the previous
    async fn execute_series(&self, agents: &[String], initial_task: &str, timeout: u64) 
        -> Result<String, DelegateToolError> 
    {
        let mut current_task = initial_task.to_string();
        let mut results = Vec::new();
        
        for agent_name in agents {
            let result = self.run_single_agent(agent_name, &current_task, timeout).await?;
            results.push((agent_name.clone(), result.clone()));
            current_task = result;  // Pass result to next agent
        }
        
        Ok(self.format_series_results(results))
    }
    
    /// Parallel execution: run all agents simultaneously
    async fn execute_parallel(&self, agents: &[String], task: &str, timeout: u64) 
        -> Result<String, DelegateToolError> 
    {
        let handles: Vec<_> = agents.iter()
            .map(|name| {
                let task = task.to_string();
                let name = name.clone();
                let tool = self.clone();
                let timeout = timeout;
                tokio::spawn(async move {
                    tool.run_single_agent(&name, &task, timeout).await
                })
            })
            .collect();
        
        let mut results = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(e)) => results.push(format!("Error: {}", e)),
                Err(e) => results.push(format!("Task panicked: {}", e)),
            }
        }
        
        Ok(self.format_parallel_results(agents.iter().zip(results)))
    }
    
    /// Run a single agent to completion
    async fn run_single_agent(&self, name: &str, task: &str, timeout: u64) 
        -> Result<String, DelegateToolError> 
    {
        let agent = self.registry.create_agent(name)
            .map_err(|e| DelegateToolError::AgentCreation(e.to_string()))?;
        
        let start = std::time::Instant::now();
        
        let result = tokio::time::timeout(
            Duration::from_secs(timeout),
            agent.prompt(task)
        ).await
        .map_err(|_| DelegateToolError::Timeout(name.to_string()))?;
        
        let duration = start.elapsed();
        
        // Track cost
        if let Some(stats) = self.cost_tracker.get_last_request_stats() {
            tracing::info!("Agent '{}' completed in {:?}", name, duration);
        }
        
        result.map_err(|e| DelegateToolError::AgentRun(e.to_string()))
    }
}
```

---

## 7. Integration Points

### 7.1 Config Loading (src/config.rs)

```rust
impl Config {
    /// Get pipeline configuration
    pub fn pipeline(&self) -> Option<&PipelineConfig> {
        self.pipeline.as_ref()
    }
    
    /// Check if multi-agent pipelines are enabled
    pub fn pipeline_enabled(&self) -> bool {
        self.pipeline.as_ref()
            .map(|p| p.enabled)
            .unwrap_or(false)
    }
}
```

### 7.2 AgentRunner Integration (src/lib.rs)

```rust
impl AgentRunner {
    // Add to new() method:
    
    // If pipeline is enabled, create registry and add delegate tool
    if config.pipeline_enabled() {
        let registry = SubAgentRegistry::new(
            config.pipeline().unwrap(),
            config.openrouter_api_key().unwrap_or("")
        );
        
        let delegate_tool = DelegateTool::new(
            Arc::new(registry),
            cost_tracker.clone(),
        );
        
        // Add delegate tool to agent (needs modification to provider creation)
        // This requires passing additional tools to create_provider()
    }
}
```

### 7.3 Modified create_provider (src/providers/mod.rs)

```rust
/// Create an agent from a definition
pub fn create_agent_from_definition(
    def: &AgentDefinition,
    api_key: &str,
    system_prompt: &str,
    mcp_tools: Option<Vec<Box<dyn ToolDyn>>>,
) -> Result<DynAgent> {
    match def.agent_type {
        ProviderType::OpenRouter => {
            // Create OpenRouter agent with definition's settings
            // Uses provided api_key and model
        }
        // ... other providers
    }
}
```

---

## 8. Error Handling

### 8.1 Error Types

```rust
#[derive(thiserror::Error, Debug)]
pub enum DelegateToolError {
    #[error("Unknown agent: {0}")]
    UnknownAgent(String),
    
    #[error("Invalid mode: {0}. Use 'series' or 'parallel'.")]
    InvalidMode(String),
    
    #[error("Agent '{0}' timed out after {1}s")]
    Timeout(String, u64),
    
    #[error("Failed to create agent: {0}")]
    AgentCreation(String),
    
    #[error("Agent execution failed: {0}")]
    AgentRun(String),
    
    #[error("Invalid agent configuration: {0}")]
    ConfigError(String),
}
```

### 8.2 Error Recovery Strategy

| Error Type | Recovery |
|------------|----------|
| UnknownAgent | Return error to entrypoint, let it decide |
| Timeout | Return partial results if any, mark as timed out |
| AgentCreation | Return error, don't retry |
| AgentRun | Retry once, then return error |
| ConfigError | Fail at startup (validation) |

---

## 9. Cost Tracking

### 9.1 Aggregated Costs

- Each agent's cost is tracked individually via CostTracker
- DelegateTool aggregates costs from all sub-agents
- Final cost shown to user includes all agent costs

### 9.2 Implementation

```rust
// In DelegateTool::run_single_agent():
if let Some(cost) = self.cost_tracker.get_last_request_cost() {
    total_cost.fetch_add(cost, Ordering::Relaxed);
}
```

---

## 10. Security Considerations

### 10.1 Cycle Prevention

Sub-agents are created WITHOUT the DelegateTool:
- Sub-agents only get the built-in tools (file_edit, bash, etc.)
- They cannot delegate further
- This is enforced in the agent creation code

### 10.2 Resource Limits

- Timeout per delegation (configurable, default 120s)
- Max tokens per sub-agent (configurable per agent definition)
- No arbitrary code execution in sub-agents beyond what's available to main agent

---

## 11. Testing Strategy

### 11.1 Unit Tests

- `SubAgentRegistry`: test agent creation, listing, validation
- `DelegateTool`: test series/parallel execution, error cases
- `config.rs`: test YAML parsing

### 11.2 Integration Tests

- End-to-end delegation flow
- Cost tracking across multiple agents
- Timeout handling
- Error propagation

### 11.3 Example Test

```rust
#[tokio::test]
async fn test_delegate_series() {
    let registry = SubAgentRegistry::new(...);
    let tool = DelegateTool::new(registry, cost_tracker);
    
    let result = tool.call(DelegateToolArgs {
        agent: "researcher".to_string(),
        task: "Research Rust async".to_string(),
        mode: "series".to_string(),
        timeout_seconds: 30,
    }).await;
    
    assert!(result.is_ok());
}
```

---

## 12. Migration Path

### Phase 1: Core Pipeline (This Implementation)
- Config structure for agents
- SubAgentRegistry
- DelegateTool with series/parallel modes
- Integration into AgentRunner

### Phase 2: Enhanced Features (Future)
- Shared context between agents (optional)
- Pipeline templates
- Built-in pipeline presets

### Phase 3: Advanced (If Needed)
- Stateful pipelines
- Agent-to-agent communication channels
- Dynamic agent spawning

---

## 13. What Was Considered and Rejected

### 13.1 Rejected: Shared Conversation History

**Rejected because**: Would require significant complexity to handle token limits, context windows, and potential conflicts. Each delegation is independent is simpler and more predictable.

### 13.2 Rejected: Multi-Level Delegation

**Rejected because**: Adds complexity for minimal benefit. Cycles are a concern. One-level delegation is sufficient for all realistic use cases.

### 13.3 Rejected: Built-in Pipeline Templates

**Rejected because**: YAGNI. The entrypoint prompt can define any pipeline logic. No need to hardcode patterns.

### 13.4 Rejected: Agent State Persistence

**Rejected because**: Adds complexity, storage concerns, and potential stale state issues. Fresh sessions are simpler and often better behavior.

---

## 14. Example Usage

### 14.1 Simple Series Pipeline

```
User: Write a Rust web server and review it

Entrypoint (Claude 3.7 Sonnet):
  → delegate(agent="coder", task="Write a Rust web server", mode="series")
    → Coder Agent (Claude 3.7 Sonnet): Writes the web server code
  → delegate(agent="reviewer", task="Review this code: [code]", mode="series")  
    → Reviewer Agent (Claude 3.5 Sonnet): Reviews the code
  
Entrypoint: Here's the code and review...
```

### 14.2 Parallel Exploration

```
User: Explore three different approaches to caching

Entrypoint (Claude 3.7 Sonnet):
  → delegate(agent="researcher", task="Approach 1: Redis caching", mode="parallel")
  → delegate(agent="researcher", task="Approach 2: In-memory LRU cache", mode="parallel")
  → delegate(agent="researcher", task="Approach 3: File-based caching", mode="parallel")
  
  (All run simultaneously, results collected)
  
Entrypoint: Based on research:
  - Redis: Best for distributed systems
  - LRU: Best for single-server, high performance
  - File-based: Simplest, good for persistence
```

### 14.3 Complex Research Pipeline

```
User: Research and implement a feature

Entrypoint (Claude 3.7 Sonnet):
  → delegate(agent="researcher", 
            task="Research best practices for X", 
            mode="series")
    → Researcher (Gemini Flash): Rapid research
  → delegate(agent="coder", 
            task="Implement based on this research: [findings]", 
            mode="series")
    → Coder (Claude 3.7 Sonnet): Implementation
  → delegate(agent="reviewer", 
            task="Review and test this implementation: [code]", 
            mode="series")
    → Reviewer (Claude 3.5 Sonnet): Review

Entrypoint: Here's the implementation with review feedback incorporated...
```

---

## 15. Summary

### What's Added

| Component | Purpose |
|-----------|---------|
| `PipelineConfig` | Config for defining sub-agents |
| `AgentDefinition` | Individual agent definition |
| `SubAgentRegistry` | Registry and factory for sub-agents |
| `DelegateTool` | Tool for entrypoint to call sub-agents |
| Series execution | Sequential agent calls with result passing |
| Parallel execution | Simultaneous agent calls |

### What's NOT Added (and why)

| Feature | Reason |
|---------|--------|
| Multi-level delegation | Complexity, cycle risk, YAGNI |
| Shared history | Simplicity, predictability |
| Built-in templates | YAGNI, flexibility |
| State persistence | Complexity, stale state risk |

### Complexity Budget

- **New modules**: 1 (`pipeline/`)
- **New files**: 4 (`mod.rs`, `config.rs`, `registry.rs`, `delegate_tool.rs`, `execution.rs`)
- **Modified files**: 3 (`config.rs`, `lib.rs`, `providers/mod.rs`)
- **New types**: ~10
- **New dependencies**: 0

---

## 16. Implementation Order

1. **Pipeline config structs** (`pipeline/config.rs`)
2. **SubAgentRegistry** (`pipeline/registry.rs`)
3. **DelegateTool** (`pipeline/delegate_tool.rs`)
4. **Series/Parallel execution** (`pipeline/execution.rs`)
5. **Config integration** (`config.rs` updates)
6. **AgentRunner integration** (`lib.rs` updates)
7. **Provider helper** (`providers/mod.rs` additions)
8. **Tests**

---

*This design follows the Zen of Software Engineering: simplicity first, reuse existing patterns, make illegal states unrepresentable, and don't build what you don't need.*
