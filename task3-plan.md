# Task 3: Token Counting and Cost Tracking via PromptHook - Implementation Plan

## Overview

Implement token usage and cost tracking using rig-core's `PromptHook` trait. Track input/output tokens for each request and accumulate costs based on model pricing.

---

## What We Learned from the Docs

### 1. PromptHook Trait (`rig::agent::trait.PromptHook`)

Key methods we need:
- `on_completion_call(&self, prompt, history)` → Called **before** prompt is sent to the model
- `on_completion_response(&self, prompt, response)` → Called **after** response is received ⚡ **This is where we extract token usage!**

The trait requires: `Clone + WasmCompatSend + WasmCompatSync`

### 2. CompletionResponse Struct (`rig::completion::request::struct.CompletionResponse`)

Contains:
```rust
pub struct CompletionResponse<T> {
    pub choice: OneOrMany<AssistantContent>,
    pub usage: Usage,           // ← Token usage data!
    pub raw_response: T,
    pub message_id: Option<String>,
}
```

### 3. Usage Struct (`rig::completion::request::struct.Usage`)

Contains exactly what we need:
```rust
pub struct Usage {
    pub input_tokens: u64,       // Prompt tokens
    pub output_tokens: u64,      // Completion tokens  
    pub total_tokens: u64,       // Total tokens
    pub cached_input_tokens: u64, // Cached prompt tokens
}
```

---

## Implementation Phases

### Phase 1: Create the Hooks Module (`src/hooks/`)

**File: `src/hooks/mod.rs`**
```rust
pub mod token_cost;
pub use token_cost::{TokenCostHook, SessionStats, ModelPricing};
```

### Phase 2: Create Token Cost Hook (`src/hooks/token_cost.rs`)

#### Step 2.1 - Define ModelPricing

```rust
#[derive(Clone, Debug)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

pub fn get_pricing(model: &str) -> ModelPricing {
    match model {
        "anthropic/claude-3.7-sonnet" => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        "anthropic/claude-3.5-sonnet" => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
        "google/gemini-2.0-flash-001" => ModelPricing {
            input_per_million: 0.075,
            output_per_million: 0.30,
        },
        // Add more models as needed
        _ => ModelPricing {
            input_per_million: 3.0,
            output_per_million: 15.0,
        },
    }
}
```

#### Step 2.2 - Define SessionStats

```rust
#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_api_calls: u64,
    pub total_cost: f64,
    // Per-request history for debugging
    requests: Vec<RequestStats>,
}

#[derive(Clone, Debug)]
pub struct RequestStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: f64,
}

impl SessionStats {
    pub fn new() -> Self { ... }
    
    pub fn add_request(&mut self, input: u64, output: u64, cost: f64) {
        self.total_input_tokens += input;
        self.total_output_tokens += output;
        self.total_api_calls += 1;
        self.total_cost += cost;
        self.requests.push(RequestStats { input_tokens: input, output_tokens: output, cost });
    }
    
    pub fn summary(&self) -> String {
        format!(
            "Total API Calls: {}\nTotal Input Tokens: {}\nTotal Output Tokens: {}\nTotal Tokens: {}\nTotal Cost: ${:.4}",
            self.total_api_calls,
            self.total_input_tokens,
            self.total_output_tokens,
            self.total_input_tokens + self.total_output_tokens,
            self.total_cost
        )
    }
    
    pub fn format_per_request(&self, input: u64, output: u64, cost: f64) -> String {
        format!(
            "[Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}]",
            input, output, cost, self.total_cost
        )
    }
    
    pub fn reset(&mut self) { ... }
    
    pub fn last_request(&self) -> Option<RequestStats> {
        self.requests.last().cloned()
    }
}
```

#### Step 2.3 - Implement TokenCostHook

```rust
use rig::agent::{PromptHook, HookAction};
use rig::completion::{CompletionModel, CompletionResponse, message::Message};
use rig::wasm_compat::{WasmCompatSend, WasmCompatSync};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct TokenCostHook {
    model_name: String,
    pricing: ModelPricing,
    stats: Arc<Mutex<SessionStats>>,
}

impl<M: CompletionModel> PromptHook<M> for TokenCostHook {
    async fn on_completion_call(
        &self,
        _prompt: &Message,
        _history: &[Message],
    ) -> HookAction {
        tracing::debug!("TokenCostHook: Starting completion call");
        HookAction::Continue
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let usage = &response.usage;
        let input = usage.input_tokens;
        let output = usage.output_tokens;
        
        // Calculate cost
        let cost = (input as f64 * self.pricing.input_per_million / 1_000_000.0)
            + (output as f64 * self.pricing.output_per_million / 1_000_000.0);
        
        // Update stats
        let mut stats = self.stats.lock().unwrap();
        stats.add_request(input, output, cost);
        
        // Log the stats
        tracing::info!(
            "Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}",
            input,
            output,
            cost,
            stats.total_cost
        );
        
        HookAction::Continue
    }
}

impl TokenCostHook {
    pub fn new(model_name: String, pricing: ModelPricing) -> Self {
        Self {
            model_name,
            pricing,
            stats: Arc::new(Mutex::new(SessionStats::new())),
        }
    }
    
    pub fn get_stats(&self) -> Arc<Mutex<SessionStats>> {
        self.stats.clone()
    }
}

// Required trait bounds for PromptHook
unsafe impl Send for TokenCostHook {}
unsafe impl Sync for TokenCostHook {}
```

### Phase 3: Update Config (`src/config.rs`)

Add cost tracking options:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    // ... existing fields ...
    
    /// Enable cost tracking (default: true)
    #[serde(default = "default_cost_tracking")]
    pub cost_tracking: bool,
    
    /// Custom pricing (optional)
    #[serde(default)]
    pub custom_pricing: Option<HashMap<String, ModelPricingConfig>>,
}

#[derive(Debug, Deserialize)]
pub struct ModelPricingConfig {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

fn default_cost_tracking() -> bool {
    true
}
```

### Phase 4: Integrate Hook into Agent (`src/lib.rs`)

#### Step 4.1 - Update the imports

```rust
mod hooks;
pub use hooks::{TokenCostHook, SessionStats, ModelPricing, get_pricing};
```

#### Step 4.2 - Update build_agent

```rust
pub async fn build_agent<M, Ext>(
    client: &Client<Ext>,
    config: &Config,
    mcp_server_handles: &[McpServerHandle],
    skills: &SkillRegistry,
) -> Agent<M, TokenCostHook>
where
    M: CompletionModel<Client = Client<Ext>>,
    Ext: Capabilities<Completion = Capable<M>>,
{
    // Create completion model with configured model name
    let model_name = config.openrouter_model.clone();

    // Build the system prompt dynamically with environment info and skills
    let system_prompt = build_system_prompt(skills);

    let mcp_tools = mcp_server_handles
        .iter()
        .flat_map(|handle| handle.dyn_tools())
        .collect();

    // Create the token cost hook if enabled
    let hook = if config.cost_tracking {
        TokenCostHook::new(
            model_name.clone(),
            get_pricing(&model_name),
        )
    } else {
        // Create a no-op hook (or we could use () as default)
        TokenCostHook::new(model_name.clone(), get_pricing("_default"))
    };

    // Build the agent with all tools and the hook
    client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens)
        .default_max_turns(config.agent_max_turns)
        .hook(hook)  // <-- Add the hook here
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(ThinkTool)
        .tools(mcp_tools)
        .build()
}
```

#### Step 4.3 - Update AgentRunner to access hook stats

The AgentRunner already has the generic parameter `P: PromptHook<M>`, so we need to work with that:

```rust
pub struct AgentRunner<M: CompletionModel, P: PromptHook<M>> {
    agent: Agent<M, P>,
    // ...
}
```

### Phase 5: Display Stats in REPL (`src/lib.rs`)

Update AgentRunner::run() to show stats after each response and add /stats command:

```rust
impl<M: CompletionModel, P: PromptHook<M> + 'static> AgentRunner<M, P> {
    pub async fn run(&mut self) -> Result<()> {
        // ... existing setup code ...

        loop {
            print!("> ");
            stdout.flush()?;

            let mut input = String::new();
            stdin.lock().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                println!("Goodbye!");
                break;
            }

            // Handle /stats command
            if input.eq_ignore_ascii_case("/stats") {
                // Need to get stats from the hook - this depends on how we access it
                println!("\n=== Session Statistics ===\n");
                // This will need implementation based on how we access hook state
                continue;
            }

            // Clone history since chat() takes ownership
            match self
                .agent
                .prompt(input)
                .with_history(&mut chat_history)
                .await
            {
                Ok(response) => {
                    println!("\n{}", response);
                    // TODO: Display token stats - depends on hook implementation
                    // We'll need a way to get the last request stats from the hook
                }
                Err(e) => {
                    eprintln!("\nError: {}\n", e);
                }
            }
        }

        Ok(())
    }
}
```

---

## Files to Create/Modify

| File | Action | Description |
|------|--------|-------------|
| `src/hooks/mod.rs` | **CREATE** | New module for hooks |
| `src/hooks/token_cost.rs` | **CREATE** | TokenCostHook implementation |
| `src/config.rs` | **MODIFY** | Add cost_tracking config options |
| `src/lib.rs` | **MODIFY** | Integrate hook into build_agent and REPL |

---

## Key Implementation Notes

1. **Hook must implement `Clone`** - Use `Arc<Mutex<SessionStats>>` to share stats across clones
2. **Trait bounds for PromptHook** - Must implement: `Clone + WasmCompatSend + WasmCompatSync`
3. **Config backward compatibility** - Cost tracking defaults to `true` (enabled)
4. **Pricing table** - Start with common models, expand as needed:
   - `anthropic/claude-3.7-sonnet`: $3.00/m input, $15.00/m output
   - `anthropic/claude-3.5-sonnet`: $3.00/m input, $15.00/m output  
   - `google/gemini-2.0-flash-001`: ~$0.075/m input
5. **Display format** - Per the todo.md spec:
   ```
   [Tokens: {input} in / {output} out | Cost: ${cost} | Total: ${total}]
   ```

---

## Testing Checklist

- [ ] Test hook is called on each request
- [ ] Test token calculation accuracy with known prices
- [ ] Test cost calculation with known prices
- [ ] Test that /stats command displays cumulative stats
- [ ] Verify stats are reset when session restarts
- [ ] Test with multiple different models to verify pricing lookup

---

## User Stories (from todo.md)

As a user, I want to see token usage and API costs for my sessions so that I can:
1. Monitor spending
2. Understand the cost of different operations
3. Optimize prompts and tool usage