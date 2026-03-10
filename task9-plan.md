# Task 9 Plan: Web Search Tool (Abstracted Backend)

## Overview

Implement a web search tool that uses a user-configured SearXNG instance under the hood. The key design principle is **abstraction**: users configure their search backend in config (SearXNG), but the model interacts with a simple, backend-agnostic web search interface.

**Design Goals:**
- **User-facing**: Users configure SearXNG in their config (they know what backend they're using)
- **Model-facing**: The model sees a simple `web_search` tool with just `query`, optional `category`, optional `site`, `num_results`, and `time_range` parameters
- **No SearXNG syntax**: The model doesn't need to know about `!images`, `!wp`, `:en`, `!!` lucky syntax, etc.
- **Future-proof**: Could swap SearXNG for another backend later without changing the tool interface

---

## Research Summary

> **Note:** This section documents SearXNG details for implementation reference only. The model never sees this complexity - it's abstracted away by the tool.

### SearXNG API Details (Internal Implementation)

**Endpoints:**
- `GET /search?q={query}&format=json` - Primary search endpoint
- `GET /?q={query}&format=json` - Alternative endpoint

**Required Parameters:**
- `q` - Search query (supports full search syntax)

**Optional Parameters:**
- `format` - Output format: `json`, `csv`, `rss` (must be enabled in instance settings!)
- `pageno` - Page number (default: 1)
- `language` - Language code (e.g., "en", "fr")
- `categories` - Comma-separated list of categories to search
- `engines` - Comma-separated list of specific engines to use
- `safesearch` - 0 (off), 1 (moderate), 2 (strict)
- `time_range` - `day`, `month`, `year` (for engines that support it)
- `image_proxy` - True/False to proxy images through SearXNG

**JSON Response Structure:**
```json
{
  "results": [
    {
      "title": "Result Title",
      "url": "https://example.com",
      "content": "Snippet text...",
      "engine": "google",
      "parsed_url": {
        "scheme": "https",
        "netloc": "example.com",
        "path": "/"
      },
      "img_src": "https://...",
      "thumbnail": "https://..."
    }
  ],
  "infoboxes": [],
  "suggestions": ["related query 1", "related query 2"],
  "answers": ["direct answer if available"],
  "number_of_results": 123,
  "query": "search terms",
  "page": 1
}
```

### Category Mapping (Internal)

SearXNG uses category prefixes (`!images`, `!map`, etc.). Our tool abstracts this - the model uses simple category names, and we map them internally:

| Model Parameter | SearXNG Category |
|-----------------|------------------|
| `"images"` | `images` |
| `"videos"` | `videos` |
| `"news"` | `news` |
| `"maps"` | `map` |
| `"music"` | `music` |
| `"science"` | `science` |

### Instance Requirements

**Critical:** The SearXNG instance must have JSON format enabled in `settings.yml`:
```yaml
search:
  formats:
    - html
    - json  # Must be enabled!
```

Many public instances disable JSON output. Users should either:
1. Run their own SearXNG instance
2. Use a public instance that has JSON enabled
3. Check https://searx.space for instance capabilities

### Rust Dependencies

No new crates required - use existing:
- `reqwest` (already in Cargo.toml for fetch_url)
- `serde` / `serde_json` (already available)

---

## Implementation Details

### 9.1 Add SearXNG Configuration

**File:** `src/config.rs`

#### 9.1.1 Add SearXngConfig Struct

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct SearXngConfig {
    /// Base URL of the SearXNG instance (e.g., "https://searx.example.com")
    pub base_url: String,
    
    /// Enable/disable search (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    
    /// Default maximum number of results to return (default: 10)
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

fn default_true() -> bool { true }
fn default_timeout() -> u64 { 30 }
fn default_max_results() -> u32 { 10 }
```

#### 9.1.2 Add to Config Struct

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    // ... existing fields ...
    
    /// SearXNG search configuration
    #[serde(default)]
    pub searxng: Option<SearXngConfig>,
}
```

#### 9.1.3 Add Helper Methods to Config

```rust
impl Config {
    /// Check if SearXNG is configured and enabled
    pub fn searxng_enabled(&self) -> bool {
        self.searxng
            .as_ref()
            .map(|c| c.enabled && !c.base_url.is_empty())
            .unwrap_or(false)
    }
    
    /// Get the SearXNG base URL
    pub fn searxng_base_url(&self) -> Option<String> {
        self.searxng.as_ref().map(|c| c.base_url.clone())
    }
}
```

#### 9.1.4 Add Environment Variable Support

In `Config::load()`:

```rust
// SEARXNG_BASE_URL
if let Ok(url) = std::env::var("SEARXNG_BASE_URL") {
    if !url.is_empty() {
        let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
            base_url: String::new(),
            enabled: true,
            timeout_seconds: 30,
            max_results: 10,
        });
        searxng.base_url = url;
    }
}

// SEARXNG_ENABLED
if let Ok(enabled) = std::env::var("SEARXNG_ENABLED") {
    if let Ok(enabled) = enabled.parse() {
        let searxng = config.searxng.get_or_insert_with(|| SearXngConfig {
            base_url: String::new(),
            enabled: true,
            timeout_seconds: 30,
            max_results: 10,
        });
        searxng.enabled = enabled;
    }
}

// SEARXNG_TIMEOUT
if let Ok(timeout) = std::env::var("SEARXNG_TIMEOUT") {
    if let Ok(timeout) = timeout.parse() {
        if let Some(searxng) = config.searxng.as_mut() {
            searxng.timeout_seconds = timeout;
        }
    }
}

// SEARXNG_MAX_RESULTS
if let Ok(max) = std::env::var("SEARXNG_MAX_RESULTS") {
    if let Ok(max) = max.parse() {
        if let Some(searxng) = config.searxng.as_mut() {
            searxng.max_results = max;
        }
    }
}
```

#### 9.1.5 Export from config module

```rust
pub use config::{Config, McpServerConfig, SearXngConfig};
```

---

### 9.2 Create Search Tool

**File:** `src/tools/search.rs` (NEW FILE)

#### 9.2.1 Module Structure

```rust
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_RESPONSE_CHARS: usize = 50_000;
const MAX_RESULTS_LIMIT: u32 = 20;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("SearXNG is not configured. Set SEARXNG_BASE_URL in config.")]
    NotConfigured,
    
    #[error("SearXNG is disabled in configuration.")]
    Disabled,
    
    #[error("Request failed: {0}")]
    Request(#[from] reqwest::Error),
    
    #[error("Failed to parse response: {0}")]
    ParseError(String),
    
    #[error("SearXNG instance does not have JSON format enabled. Please use a different instance or contact the administrator.")]
    JsonFormatDisabled,
    
    #[error("SearXNG instance returned an error: {0}")]
    InstanceError(String),
    
    #[error("No results found for query: {0}")]
    NoResults(String),
}
```

#### 9.2.2 Response Structs

```rust
/// SearXNG JSON response structure
#[derive(Debug, Deserialize)]
struct SearXngResponse {
    /// List of search results
    #[serde(default)]
    results: Vec<SearXngResult>,
    
    /// Original query
    query: String,
    
    /// Total number of results (if available)
    #[serde(default)]
    number_of_results: Option<u32>,
    
    /// Suggested queries (if available)
    #[serde(default)]
    suggestions: Option<Vec<String>>,
    
    /// Direct answers (if available)
    #[serde(default)]
    answers: Option<Vec<String>>,
    
    /// Current page number
    #[serde(default)]
    page: Option<u32>,
}

/// Individual search result
#[derive(Debug, Deserialize)]
struct SearXngResult {
    /// Result title
    title: String,
    
    /// Result URL
    url: String,
    
    /// Result snippet/content
    #[serde(default)]
    content: Option<String>,
    
    /// Source engine
    #[serde(default)]
    engine: Option<String>,
    
    /// Image source (for image results)
    #[serde(default)]
    img_src: Option<String>,
    
    /// Thumbnail URL
    #[serde(default)]
    thumbnail: Option<String>,
}
```

#### 9.2.3 Tool Arguments

**Design Decision:** The search tool abstracts away the underlying SearXNG backend from the model. Users configure SearXNG in their config, but the model sees a simple web search interface.

```rust
/// Arguments for the search tool
#[derive(Debug, Deserialize)]
pub struct SearchArgs {
    /// The search query (plain text, no special syntax required)
    query: String,
    
    /// Optional category to search in: "images", "videos", "news", "maps", "music", "science"
    /// Leave empty for general web search
    #[serde(default)]
    category: Option<String>,
    
    /// Optional site to filter results to (e.g., "github.com", "stackoverflow.com")
    #[serde(default)]
    site: Option<String>,
    
    /// Maximum number of results to return (default: 10, max: 20)
    #[serde(default = "default_num_results")]
    num_results: Option<u32>,
    
    /// Time range for results: "day", "month", "year"
    /// Only works with engines that support time-based search
    #[serde(default)]
    time_range: Option<String>,
}

fn default_num_results() -> Option<u32> { Some(10) }
```

#### 9.2.4 SearchTool Struct

```rust
/// The search tool for querying SearXNG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchTool {
    base_url: String,
    timeout_seconds: u64,
    default_max_results: u32,
}

impl SearchTool {
    pub fn new(config: &SearXngConfig) -> Self {
        Self {
            base_url: config.base_url.clone(),
            timeout_seconds: config.timeout_seconds,
            default_max_results: config.max_results,
        }
    }
}
```

#### 9.2.5 Tool Implementation

```rust
impl Tool for SearchTool {
    const NAME: &'static str = "web_search";
    type Error = SearchError;
    type Args = SearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Search the web. Provides a simple interface abstracting the underlying \
                search backend (SearXNG). Returns title, URL, and snippet for each result."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query (plain text, no special syntax required)"
                    },
                    "category": {
                        "type": "string",
                        "description": "Optional category to search in: images, videos, news, maps, music, science. Leave empty for general web search.",
                        "enum": ["images", "videos", "news", "maps", "music", "science"]
                    },
                    "site": {
                        "type": "string",
                        "description": "Optional site to filter results to (e.g., 'github.com', 'stackoverflow.com')"
                    },
                    "num_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 10, max: 20)",
                        "default": 10
                    },
                    "time_range": {
                        "type": "string",
                        "description": "Time range for results: day, month, year (only works with supported engines)",
                        "enum": ["day", "month", "year"]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate configuration
        if self.base_url.is_empty() {
            return Err(SearchError::NotConfigured);
        }

        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "web_search",
            query = %args.query,
            "Starting web_search tool execution"
        );

        let start_time = std::time::Instant::now();

        // Determine number of results
        let num_results = args.num_results.unwrap_or(self.default_max_results);
        let num_results = num_results.min(MAX_RESULTS_LIMIT);

        // Build URL with query parameters
        let mut url = format!("{}/search", self.base_url.trim_end_matches('/'));
        
        // Use Url for proper encoding
        let mut url_obj = reqwest::Url::parse(&url)
            .map_err(|e| SearchError::RequestError(format!("Invalid URL: {}", e)))?;
        
        // Build the query string - handle site filter
        let query_string = if let Some(ref site) = args.site {
            format!("site:{} {}", site, args.query)
        } else {
            args.query.clone()
        };
        
        url_obj.query_pairs_mut()
            .append_pair("q", &query_string)
            .append_pair("format", "json")
            .append_pair("pageno", "1");
        
        // Add optional parameters
        if let Some(ref category) = args.category {
            // Map our simplified category names to SearXNG category names
            let searxng_category = match category.as_str() {
                "images" => "images",
                "videos" => "videos",
                "news" => "news",
                "maps" => "map",
                "music" => "music",
                "science" => "science",
                _ => "general",
            };
            url_obj.query_pairs_mut().append_pair("categories", searxng_category);
        }
        
        if let Some(ref time_range) = args.time_range {
            url_obj.query_pairs_mut().append_pair("time_range", time_range);
        }

        // Make the HTTP request
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_seconds))
            .build()?;

        let response = client
            .get(url_obj.as_str())
            .header("User-Agent", "PeakBot/1.0 (Web Search)")
            .send()
            .await?;

        let status = response.status();
        
        // Check for specific error conditions
        if status.as_u16() == 403 {
            return Err(SearchError::JsonFormatDisabled);
        }
        
        if status.as_u16() == 429 {
            return Err(SearchError::InstanceError("Rate limited. Try again later.".to_string()));
        }

        if !status.is_success() {
            return Err(SearchError::InstanceError(format!(
                "HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            )));
        }

        // Parse JSON response
        let body = response.text().await?;
        let searxng_response: SearXngResponse = serde_json::from_str(&body)
            .map_err(|e| SearchError::ParseError(format!("{}: {}", e, &body[..body.len().min(500)])))?;

        // Format results
        let mut output = String::new();
        
        // Add query info
        output.push_str(&format!("Search query: {}\n", searxng_response.query));
        if let Some(total) = searxng_response.number_of_results {
            output.push_str(&format!("Total results: {}\n", total));
        }
        output.push_str("---\n\n");

        // Add suggestions if available
        if let Some(suggestions) = searxng_response.suggestions {
            if !suggestions.is_empty() {
                output.push_str("Suggestions: ");
                output.push_str(&suggestions.join(", "));
                output.push_str("\n\n---\n\n");
            }
        }

        // Add answers if available
        if let Some(answers) = searxng_response.answers {
            if !answers.is_empty() {
                output.push_str("Direct Answers:\n");
                for answer in &answers {
                    output.push_str(&format!("- {}\n", answer));
                }
                output.push_str("\n---\n\n");
            }
        }

        // Add results
        let results: Vec<_> = searxng_response.results.into_iter().take(num_results as usize).collect();
        
        if results.is_empty() {
            return Err(SearchError::NoResults(args.query));
        }

        output.push_str(&format!("Results ({}):\n\n", results.len()));
        
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!("{}. {}\n", i + 1, result.title));
            output.push_str(&format!("   URL: {}\n", result.url));
            
            if let Some(ref content) = result.content {
                // Truncate long snippets
                let snippet = if content.len() > 300 {
                    format!("{}...", &content[..300])
                } else {
                    content.clone()
                };
                output.push_str(&format!("   {}\n", snippet));
            }
            
            if let Some(ref engine) = result.engine {
                output.push_str(&format!("   [{}]\n", engine));
            }
            
            output.push_str("\n");
        }

        // Truncate if too long
        let output = if output.len() > MAX_RESPONSE_CHARS {
            format!("{}... [truncated]", &output[..MAX_RESPONSE_CHARS])
        } else {
            output
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "web_search",
            query = %args.query,
            num_results = results.len(),
            duration_ms = start_time.elapsed().as_millis(),
            "Web search completed successfully"
        );

        Ok(output)
    }
}
```

---

### 9.3 Update Tools Module

**File:** `src/tools/mod.rs`

```rust
pub mod search;
// ... existing mods ...

pub use search::{SearchTool, SearchError};
```

---

### 9.4 Add Search Tool to Agent

**File:** `src/lib.rs`

#### 9.4.1 Update imports

```rust
pub use tools::{
    BashTool, FetchUrlTool, FileEditTool, FileReadTool, ListDirectoryTool, 
    LoggingToolDyn, ThinkTool, SearchTool,
};
```

#### 9.4.2 Modify build_agent function

```rust
pub async fn build_agent<M, Ext>(
    client: &Client<Ext>,
    config: &Config,
    mcp_server_handles: &[McpServerHandle],
    skills: &SkillRegistry,
) -> Agent<M>
where
    M: CompletionModel<Client = Client<Ext>>,
    Ext: Capabilities<Completion = Capable<M>>,
{
    // ... existing code ...

    // Build the agent with all tools
    let mut agent_builder = client
        .agent(model_name)
        .preamble(&system_prompt)
        .max_tokens(config.openrouter_max_tokens)
        .default_max_turns(config.agent_max_turns)
        .tool(FileEditTool::default())
        .tool(FileReadTool)
        .tool(BashTool)
        .tool(ListDirectoryTool)
        .tool(FetchUrlTool)
        .tool(ThinkTool);

    // Conditionally add search tool if SearXNG is configured
    if config.searxng_enabled() {
        if let Some(searxng_config) = &config.searxng {
            agent_builder = agent_builder.tool(SearchTool::new(searxng_config));
            tracing::info!("SearXNG search enabled: {}", searxng_config.base_url);
        }
    }

    // Add MCP tools
    agent_builder.tools(mcp_tools).build()
}
```

#### 9.4.3 Update startup message in AgentRunner::run()

```rust
pub async fn run(&mut self) -> Result<()> {
    // ... existing startup output ...
    
    if self.config.searxng_enabled() {
        if let Some(ref searxng) = self.config.searxng {
            println!("SearXNG: {} (enabled)", searxng.base_url);
        }
    } else {
        println!("SearXNG: not configured");
    }
    
    // ... rest of function ...
}
```

---

### 9.5 Update System Prompt

**File:** `src/system_prompt.txt`

Add a single line to the Working Principles section:

```
## Working Principles

- Use the web_search tool to find current information, research topics, look up documentation, investigate anything you need to verify, or get up-to-date data. The tool uses your configured SearXNG instance and supports advanced search syntax:
  - Categories: !images, !videos, !news, !map, !music, !it, !science, !files, !social_media
  - Specific engines: !wp (Wikipedia), !ddg (DuckDuckGo), !go (Google), etc.
  - Language filters: :en, :fr, :de (or use :prefix in query)
  - Standard operators: "exact phrase", site:domain.com, -exclude term
```

---

### 9.6 Configuration Examples

#### YAML Configuration

```yaml
# config.yaml
openrouter_api_key: "your-api-key"
openrouter_model: "anthropic/claude-3.7-sonnet"

searxng:
  base_url: "https://searx.example.com"
  enabled: true
  timeout_seconds: 30
  max_results: 10
```

#### Environment Variables

```bash
export SEARXNG_BASE_URL="https://searx.example.com"
export SEARXNG_ENABLED="true"
export SEARXNG_TIMEOUT="30"
export SEARXNG_MAX_RESULTS="10"
```

---

### 9.7 Error Handling Details

| Error | Cause | User Message |
|-------|-------|--------------|
| `NotConfigured` | No base_url set | "SearXNG is not configured. Set SEARXNG_BASE_URL in config." |
| `Disabled` | enabled=false | "SearXNG is disabled in configuration." |
| `JsonFormatDisabled` | Instance has JSON disabled | "SearXNG instance does not have JSON format enabled. Please use a different instance or contact the administrator." |
| `RequestError` | Network/connection issues | "Request failed: {error details}" |
| `ParseError` | Malformed response | "Failed to parse response: {details}" |
| `InstanceError` | HTTP errors from instance | "SearXNG instance returned an error: {details}" |
| `NoResults` | Empty result set | "No results found for query: {query}" |

---

### 9.8 Testing Plan

#### 9.8.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_url_encoding() {
        // Test that special characters are properly encoded
    }
    
    #[test]
    fn test_category_param() {
        // Test category parameter is added to URL
    }
    
    #[test]
    fn test_max_results_limit() {
        // Test that num_results is capped at 20
    }
    
    #[test]
    fn test_response_parsing() {
        // Test parsing of mock JSON response
    }
    
    #[test]
    fn test_result_truncation() {
        // Test that long results are truncated
    }
}
```

#### 9.8.2 Integration Tests

- Test with a live SearXNG instance
- Test error handling for unreachable instance
- Test error handling for JSON-disabled instance
- Test various query types:
  - Simple text query
  - Category prefix: `!images sunset`
  - Engine prefix: `!wp rust`
  - Language filter: `:fr query`
  - Site filter: `site:github.com rust`
  - Exact phrase: `"exact match"`
  - Mixed: `!map !ddg paris restaurants`

#### 9.8.3 Manual Testing Commands

```bash
# Test basic search
web_search({query: "rust programming language"})

# Test image search
web_search({query: "!images sunset", num_results: 5})

# Test with category parameter
web_search({query: "latest news", category: "news"})

# Test site search
web_search({query: "site:github.com rig framework"})

# Test language filter
web_search({query: "meteo", language: "fr"})

# Test safe search
web_search({query: "test", safe_search: 2})
```

---

### 9.9 Documentation Updates

#### Update `agents.md`

Add a section on SearXNG configuration:

```markdown
## Web Search (SearXNG)

PeakBot supports web search via a user-configured SearXNG instance.

### Configuration

```yaml
# config.yaml
searxng:
  base_url: "https://searx.example.com"  # Your SearXNG instance
  enabled: true
  timeout_seconds: 30
  max_results: 10
```

Or via environment variables:
```bash
export SEARXNG_BASE_URL="https://searx.example.com"
export SEARXNG_ENABLED="true"
```

### Search Syntax

The search tool supports all SearXNG syntax:

| Syntax | Example | Description |
|--------|---------|-------------|
| `!category` | `!images cat` | Search in specific category |
| `!engine` | `!wp rust` | Use specific search engine |
| `:lang` | `:fr query` | Filter by language |
| `site:` | `site:github.com query` | Limit to domain |
| `"exact"` | `"exact phrase"` | Exact phrase match |
| `-exclude` | `query -term` | Exclude term |
| `!!` | `!! query` | Auto-redirect to first result |

### Categories

- `!general` - General web search
- `!images` - Image search
- `!videos` - Video search
- `!news` - News search
- `!map` - Maps and locations
- `!music` - Music and lyrics
- `!it` - IT/dev resources
- `!science` - Scientific papers
- `!files` - File downloads
- `!social_media` - Social media

### Running Your Own Instance

To run a personal SearXNG instance:

1. Install SearXNG: https://docs.searxng.org/
2. Enable JSON format in `settings.yml`:
   ```yaml
   search:
     formats:
       - html
       - json
   ```
3. Configure the base URL in PeakBot

---

## File Changes Summary

| File | Change Type | Description |
|------|-------------|-------------|
| `src/config.rs` | Modify | Add `SearXngConfig` struct, add to `Config`, add env var support, add helper methods |
| `src/tools/search.rs` | New | Complete search tool implementation |
| `src/tools/mod.rs` | Modify | Export `SearchTool` and `SearchError` |
| `src/lib.rs` | Modify | Import `SearchTool`, conditionally add to agent, update startup message |
| `src/system_prompt.txt` | Modify | Add search tool usage to Working Principles |
| `agents.md` | Modify | Add SearXNG configuration documentation |

---

## Dependencies

No new dependencies required. Uses existing:
- `reqwest` - HTTP client (already in Cargo.toml)
- `serde` / `serde_json` - JSON parsing (already in Cargo.toml)
- `thiserror` - Error handling (already in Cargo.toml)

---

## Edge Cases Handled

1. **Instance has JSON disabled** → Returns helpful error suggesting different instance
2. **Query with special characters** → Proper URL encoding via `reqwest::Url`
3. **Very long query** → Passed as-is to instance (instance handles limits)
4. **Network timeout** → Configurable timeout with clear error
5. **Empty results** → Returns "No results found" error
6. **Suggestions in response** → Displayed to user
7. **Answers in response** → Displayed as direct answers
8. **Rate limiting (429)** → Returns retry suggestion
9. **Result truncation** → Truncates output at 50,000 chars
10. **Missing optional fields** → Uses defaults, handles Option fields gracefully

---

## Notes

- The search tool name is `web_search` (not just `search`) to avoid potential conflicts
- The tool accepts the full query as-is - model can use any SearXNG syntax without special handling
- Category can be specified either as a parameter OR in the query using `!prefix`
- The tool is conditionally added to the agent - if no SearXNG is configured, the tool isn't available
- The instance URL should NOT have a trailing slash