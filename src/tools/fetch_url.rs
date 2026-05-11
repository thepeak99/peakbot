use crate::utils::strings::truncate_with_suffix;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

const MAX_RESPONSE_CHARS: usize = 50_000;

#[derive(Debug, thiserror::Error)]
pub enum FetchUrlError {
    #[error("Request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
}

#[derive(Deserialize)]
pub struct FetchUrlArgs {
    #[allow(dead_code)]
    thought: String,
    url: String,
}

#[derive(Serialize, Deserialize)]
pub struct FetchUrlTool;

impl Tool for FetchUrlTool {
    const NAME: &'static str = "fetch_url";
    type Error = FetchUrlError;
    type Args = FetchUrlArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "fetch_url".to_string(),
            description: "Fetch the content of a URL and return it as text. \
                Use this to retrieve web page content, API responses, or any HTTP GET request. \
                Returns the response body up to 50,000 characters (truncated if longer)."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    }
                },
                "required": ["thought", "url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "fetch_url",
            url = %args.url,
            "Starting fetch_url tool execution"
        );

        let start_time = std::time::Instant::now();

        // Validate URL
        if args.url.is_empty() {
            return Err(FetchUrlError::InvalidUrl("URL cannot be empty".to_string()));
        }

        // Make the HTTP request
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let response = client
            .get(&args.url)
            .header("User-Agent", "PeakBot/1.0")
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        let body = if body.len() > MAX_RESPONSE_CHARS {
            let total = body.len();
            truncate_with_suffix(
                &body,
                MAX_RESPONSE_CHARS,
                &format!("... [truncated, {total} total chars]"),
            )
        } else {
            body
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "fetch_url",
            url = %args.url,
            status_code = status.as_u16(),
            response_len = body.len(),
            duration_ms = start_time.elapsed().as_millis(),
            "Fetch URL completed successfully"
        );

        Ok(format!(
            "HTTP {} {}\n\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown"),
            body
        ))
    }
}
