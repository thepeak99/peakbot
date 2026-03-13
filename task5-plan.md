# Task 5 Plan: Ollama Support + Provider Independent Architecture

## Status: ✅ COMPLETED

**Implementation Date:** 2026-03-13

## Overview

Add support for local Ollama server while refactoring the codebase to be provider-independent. Currently, PeakBot is tightly coupled to OpenRouter - this task will make it work with multiple LLM providers.

## ✅ IMPLEMENTED

### 1. Config Refactoring (Phase 1)

- ✅ Created `ProviderType` enum (OpenRouter, Ollama)
- ✅ Created `ProviderConfig` enum with `#[serde(tag = "type", content = "config")]`
- ✅ Created `OpenRouterConfig` struct with api_key, model, max_tokens
- ✅ Created `OllamaConfig` struct with base_url, model, temperature, num_ctx
- ✅ Updated main `Config` struct with `provider` field
- ✅ Added environment variable support (PROVIDER JSON, OLLAMA_MODEL, etc.)
- ✅ Added backward compatibility for legacy YAML config (openrouter_api_key, etc.)

### 2. Provider Abstraction Layer (Phase 2)

- ✅ Created `src/providers/mod.rs` with:
  - `DynAgent` enum (runtime polymorphism for OpenRouter/Ollama agents)
  - `ProviderInfo` struct (name, model, supports_pricing)
  - `CostTracker` struct (unified cost tracking interface)
  - `create_provider()` factory function
- ✅ Both OpenRouter and Ollama agents can be created

### 3. Agent Building (Phase 4)

- ✅ Updated `main.rs` to use `create_provider()` factory
- ✅ Updated `AgentRunner` to use `DynAgent` instead of generic types
- ✅ Provider info displayed at startup

### 4. Backward Compatibility

- ✅ Legacy YAML config fields (`openrouter_api_key`, `openrouter_model`, etc.) still work
- ✅ Migration logic converts legacy format to new provider format
- ✅ Environment variables still work as before

### 5. MCP Tool Integration

- ✅ MCP tools are properly integrated with both OpenRouter and Ollama agents
- ✅ Tools are passed through `create_provider()` function
- ✅ Both providers use `.tools()` method to add dynamic MCP tools

### 6. System Prompt Integration

- ✅ System prompt is built before creating the provider
- ✅ System prompt is passed to the agent via `.preamble()` method
- ✅ Both OpenRouter and Ollama agents use the system prompt

### 7. Cost Tracking

- ✅ `TokenCostHook` is integrated with OpenRouter agent
- ✅ `CostTracker` provides unified interface for cost statistics
- ✅ Per-request and session stats are available for OpenRouter
- ✅ Ollama correctly reports no cost tracking (local model)

### 8. Documentation

- ✅ Updated `agents.md` with new provider configuration examples

---

## Summary of Changes

| Component | Before | After |
|-----------|--------|-------|
| Agent Type | Generic `Agent<M, P>` | `DynAgent` enum |
| Hook Type | `TokenCostHook` | `TokenCostHook` (OpenRouter) / `()` (Ollama) |
| Cost Tracking | Via stats reference | Via `CostTracker` wrapper |
| System Prompt | Built in `build_agent()` | Built in `main.rs`, passed to provider |
| MCP Tools | Combined in `build_agent()` | Passed to `create_provider()` |
| SearchTool | Always included | Conditional on SearXNG config |

---

## Original Plan (Reference)

### Current Architecture (Tightly Coupled to OpenRouter)

```
main.rs
  └── create_openrouter_client(&config)  ← Hard-coded to OpenRouter
        ↓
lib.rs
  ├── openrouter::Client               ← OpenRouter-specific
  ├── build_agent(&client, config, ...)
  │     └── Uses config.openrouter_*   ← OpenRouter-specific config fields
  └── TokenCostHook::new()             ← Uses OpenRouter pricing API
```

**Problems:**
1. Config fields are named `openrouter_api_key`, `openrouter_model`, etc.
2. `create_openrouter_client()` is hard-coded
3. Cost tracking queries OpenRouter API for pricing
4. No abstraction for provider-specific behavior

### Provider Differences

| Feature | OpenRouter | Ollama |
|---------|------------|--------|
| API Key | Required | Not needed |
| Base URL | openrouter.ai | localhost:11434 (configurable) |
| Tool Format | OpenAI-compatible | Ollama-native `ToolDefinition` |
| Token Pricing | Available via API | Not available (local) |
| Function Calling | Supported | Varies by model |

### Available Providers in rig-core (v0.31+)

The rig-core library natively supports many providers:
- OpenRouter, OpenAI, Anthropic, Google Gemini, Cohere, Groq
- **Ollama** (local models)
- DeepSeek, xAI, Perplexity, Together AI, Azure, etc.

## Implementation Plan

### Phase 1: Refactor Config for Provider Independence

**1.1 Create Provider Config Enum**
```rust
// src/config.rs
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    #[default]
    OpenRouter,
    Ollama,
    // Future: OpenAI, Anthropic, etc.
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", content = "config")]
pub enum ProviderConfig {
    #[serde(rename = "openrouter")]
    OpenRouter(OpenRouterConfig),
    #[serde(rename = "ollama")]
    Ollama(OllamaConfig),
}
```

**1.2 Define Provider-Specific Configs**
```rust
#[derive(Debug, Deserialize, Clone)]
pub struct OpenRouterConfig {
    pub api_key: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OllamaConfig {
    /// Base URL (default: http://localhost:11434)
    #[serde(default = "default_ollama_url")]
    pub base_url: String,
    /// Model name (e.g., "llama3", "qwen2.5:14b", "mistral")
    pub model: String,
    /// Temperature setting (optional)
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Number of context tokens (optional)
    #[serde(default)]
    pub num_ctx: Option<usize>,
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
```

**1.3 Update Main Config Struct**
```rust
pub struct Config {
    /// LLM Provider configuration
    pub provider: ProviderConfig,
    
    // ... rest unchanged (mcp_servers, searxng, cost_tracking, context)
}
```

**1.4 Add Environment Variable Support**
```bash
# Provider config via JSON (new format)
export PROVIDER='{"type":"openrouter","config":{"api_key":"sk-...","model":"anthropic/claude-3.7-sonnet"}}'
# or
export PROVIDER='{"type":"ollama","config":{"base_url":"http://localhost:11434","model":"llama3"}}'
```

### Phase 2: Create Provider Abstraction Layer

**2.1 Create Provider Module** (`src/providers/mod.rs`)
```rust
pub mod openrouter;
pub mod ollama;

use rig::client::{Client, Capabilities, Capable};
use rig::completion::CompletionModel;
use anyhow::Result;

pub trait LLMProvider {
    fn name(&self) -> &str;
    fn create_client(&self) -> Result<Client<Self::Ext>>;
    fn get_model_name(&self) -> &str;
    fn get_max_tokens(&self) -> u64;
    fn get_temperature(&self) -> Option<f32>;
    fn supports_pricing(&self) -> bool;
}

// Implement for OpenRouter
impl LLMProvider for OpenRouterProvider { ... }

// Implement for Ollama  
impl LLMProvider for OllamaProvider { ... }
```

**2.2 Create Factory Function**
```rust
// src/providers/mod.rs
pub fn create_provider(config: &Config) -> Result<Box<dyn LLMProvider>> {
    match &config.provider {
        ProviderConfig::OpenRouter(c) => Ok(Box::new(OpenRouterProvider::new(c)?)),
        ProviderConfig::Ollama(c) => Ok(Box::new(OllamaProvider::new(c)?)),
    }
}
```

### Phase 3: Handle Tool Definition Format Differences

**3.1 Create Tool Definition Adapter**

Ollama uses a different tool definition format than OpenAI/OpenRouter:
```rust
// OpenAI format (used by OpenRouter)
{
  "type": "function",
  "function": {
    "name": "tool_name",
    "description": "...",
    "parameters": { ... }
  }
}

// Ollama format
{
  "type": "function",
  "function": {
    "name": "tool_name", 
    "parameters": { ... }
  }
  // Note: Ollama doesn't use "description" in the same way
}
```

**3.2 Implement Conversion**
```rust
// src/providers/tool_converter.rs
pub struct ToolConverter;

impl ToolConverter {
    /// Convert OpenAI-style tool definitions to Ollama format
    pub fn to_ollama(tools: Vec<ToolDefinition>) -> Vec<OllamaTool> {
        // Transform tool definitions
    }
    
    /// Convert Ollama responses to OpenAI-style for compatibility
    pub fn from_ollama(response: OllamaResponse) -> CompletionResponse {
        // Transform responses
    }
}
```

### Phase 4: Update Agent Building

**4.1 Refactor build_agent()**
```rust
// Before (OpenRouter specific)
pub async fn build_agent<M, Ext>(
    client: &Client<Ext>,
    config: &Config,
    ...
) -> Result<(Agent<M, TokenCostHook>, Arc<Mutex<SessionStats>>)>

// After (Provider agnostic)
pub async fn build_agent(
    provider: &dyn LLMProvider,
    config: &Config,
    ...
) -> Result<(Agent<impl CompletionModel, TokenCostHook>, Arc<Mutex<SessionStats>>)>
```

**4.2 Update main.rs**
```rust
#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    
    // Create provider based on config
    let provider = create_provider(&config)?;
    
    // Build client from provider
    let client = provider.create_client()?;
    
    // Build agent (now uses provider-agnostic interface)
    let (agent, stats) = build_agent(&*provider, &config, &mcp_servers, &skills).await?;
    
    // Run REPL
    let mut runner = AgentRunner::new(agent, config, skills, stats, provider.name().to_string());
    runner.run().await?;
}
```

### Phase 5: Update Cost Tracking

**5.1 Make Cost Tracking Optional**
```rust
impl TokenCostHook {
    pub fn new(model_name: String, pricing: ModelPricing) -> Self { ... }
    
    pub fn noop(model_name: String) -> Self {
        Self::new(model_name, ModelPricing {
            input_per_token: 0.0,
            output_per_token: 0.0,
        })
    }
}
```

**5.2 Disable for Ollama**
```rust
let hook = if config.provider.supports_pricing() && config.cost_tracking {
    TokenCostHook::new(
        provider.get_model_name(),
        fetch_model_pricing(&api_key, &model_name).await?
    )
} else {
    TokenCostHook::noop(provider.get_model_name())
};
```

### Phase 6: Update REPL and Messages

**6.1 Add Provider Info to Startup**
```rust
println!("PeakBot v{} - Provider: {}", env!("CARGO_PKG_VERSION"), provider_name);
```

**6.2 Update Error Messages**
- "OpenRouter API key not configured" → "No LLM provider configured"

### Phase 7: Testing

**7.1 Test Ollama Connection**
- Test connection to local Ollama instance
- Handle "Ollama not running" gracefully

**7.2 Test Tool Calling**
- Test with models that support function calling (e.g., llama3, qwen2.5)
- Document which models support tools

**7.3 Test Provider Switching**
- Verify config can switch between OpenRouter and Ollama

## Backward Compatibility

*Not needed - we will remove the old config format entirely and migrate directly to the new provider-based configuration.*

## Example Configs

### OpenRouter Example
```yaml
provider:
  type: openrouter
  config:
    api_key: sk-or-v1-xxx
    model: anthropic/claude-3.7-sonnet
    max_tokens: 4096

mcp_servers:
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/home"]
```

### Ollama Example
```yaml
provider:
  type: ollama
  config:
    base_url: http://localhost:11434
    model: llama3
    temperature: 0.7
    num_ctx: 4096

mcp_servers:
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/home"]
```

## Files to Modify

| File | Changes |
|------|---------|
| `src/config.rs` | Add ProviderConfig, OllamaConfig, replace openrouter_* fields |
| `src/lib.rs` | Create provider module, refactor build_agent, remove create_openrouter_client |
| `src/main.rs` | Use new provider factory |
| `src/providers/mod.rs` | New: Provider trait and factory |
| `src/providers/openrouter.rs` | New: OpenRouter provider impl |
| `src/providers/ollama.rs` | New: Ollama provider impl |
| `src/providers/tool_converter.rs` | New: Tool format conversion |
| `agents.md` | Update with new provider config examples |

## Dependencies

No new dependencies needed - rig-core already has Ollama support built-in.

## Estimated Complexity

- **Refactoring**: Medium (config, factory pattern)
- **Ollama integration**: Low (rig-core has native support)
- **Tool conversion**: Medium (handle format differences)
- **Testing**: Medium (need Ollama running for full tests)

## Related Tasks

- Task 3 (Token Hook): Already done - will integrate with new provider system
- Task 4 (Logging Hooks): Uses PromptHook - compatible with new architecture