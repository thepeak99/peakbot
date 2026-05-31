use crate::utils::strings::truncate_with_suffix;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use spider::page::Page;
use spider_transformations::transformation::content::{
    ReturnFormat, TransformConfig, transform_content,
};

const MAX_RESPONSE_CHARS: usize = 50_000;

/// Default for the `markdown` arg: convert HTML to Markdown unless the caller
/// opts out. Kept as a free function so `#[serde(default = …)]` can name it.
fn default_markdown() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum FetchPageError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Failed to build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

#[derive(Deserialize)]
pub struct FetchPageArgs {
    #[allow(dead_code)]
    thought: String,
    url: String,
    #[serde(default = "default_markdown")]
    markdown: bool,
}

#[derive(Serialize, Deserialize)]
pub struct FetchPageTool;

impl Tool for FetchPageTool {
    const NAME: &'static str = "fetch_page";
    type Error = FetchPageError;
    type Args = FetchPageArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "fetch_page".to_string(),
            description: "Fetch a single web page and return its content, converted to clean \
                Markdown by default. Best for reading websites, articles, docs, and other \
                HTML pages — the markdown conversion strips boilerplate and makes the content \
                easy to read. For raw data such as JSON/REST APIs, XML, or plain-text \
                endpoints, prefer the `fetch_url` tool instead, which returns the body \
                verbatim. Output is truncated to 50,000 characters."
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
                        "description": "The URL of the web page to fetch."
                    },
                    "markdown": {
                        "type": "boolean",
                        "description": "Convert the page to Markdown (default: true). Set to false to get the raw HTML."
                    }
                },
                "required": ["thought", "url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target: "peakbot",
            tool_type = "fetch_page",
            url = %args.url,
            markdown = args.markdown,
            "Starting fetch_page tool execution"
        );

        let start_time = std::time::Instant::now();

        if args.url.is_empty() {
            return Err(FetchPageError::InvalidUrl(
                "URL cannot be empty".to_string(),
            ));
        }

        // Plain reqwest client (spider's `Client` aliases to `reqwest::Client`
        // under the `reqwest_rustls_tls` feature). 30s timeout + UA for parity
        // with `fetch_url`. `Page::new_page` does a one-shot fetch — no crawl.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("PeakBot/1.0")
            .build()?;

        let page = Page::new_page(&args.url, &client).await;
        let status = page.status_code;

        let content = if args.markdown {
            let conf = TransformConfig {
                return_format: ReturnFormat::Markdown,
                ..Default::default()
            };
            transform_content(&page, &conf, &None, &None, &None)
        } else {
            page.get_html()
        };

        let content = if content.len() > MAX_RESPONSE_CHARS {
            let total = content.len();
            truncate_with_suffix(
                &content,
                MAX_RESPONSE_CHARS,
                &format!("... [truncated, {total} total chars]"),
            )
        } else {
            content
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "fetch_page",
            url = %args.url,
            status_code = status.as_u16(),
            response_len = content.len(),
            duration_ms = start_time.elapsed().as_millis(),
            "Fetch page completed successfully"
        );

        Ok(format!(
            "HTTP {} {}\n\n{}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown"),
            content
        ))
    }
}
