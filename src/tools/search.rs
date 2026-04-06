use crate::SearXngConfig;
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

    #[error("Request error: {0}")]
    RequestError(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("SearXNG instance does not have JSON format enabled. Please use a different instance or contact the administrator.")]
    JsonFormatDisabled,

    #[error("SearXNG instance returned an error: {0}")]
    InstanceError(String),

    #[error("No results found for query: {0}")]
    NoResults(String),
}

/// SearXNG JSON response structure
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Arguments for the search tool
#[derive(Debug, Deserialize)]
pub struct SearchArgs {
    #[allow(dead_code)]
    thought: String,
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

impl Tool for SearchTool {
    const NAME: &'static str = "web_search";
    type Error = SearchError;
    type Args = SearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Search the web, news, maps, social media, and others".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
                    "query": {
                        "type": "string",
                        "description": "The search query"
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
                "required": ["thought", "query"]
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
        let url = format!("{}/search", self.base_url.trim_end_matches('/'));

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
            // Map our category names to SearXNG category names
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
