use crate::utils::strings::truncate_with_suffix;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use spider::page::Page;
use spider_transformations::transformation::content::{
    ReturnFormat, TransformConfig, transform_content,
};
use std::time::Duration;

const MAX_RESPONSE_CHARS: usize = 50_000;

/// Number of retries after the first attempt. The URL is fetched up to
/// `1 + MAX_RETRIES` times in total.
const MAX_RETRIES: u32 = 3;

/// Base unit for exponential backoff: `BACKOFF_BASE * 2^attempt`, capped at
/// `BACKOFF_CAP`. With base 3s the delays are ~3s, 6s, then capped at 8s
/// (plus jitter).
const BACKOFF_BASE: Duration = Duration::from_secs(3);

/// Upper bound on a single backoff sleep, so a high retry count can't stall
/// the tool for minutes.
const BACKOFF_CAP: Duration = Duration::from_secs(8);

/// Honest default user-agent for the first attempt. On a 403/429 retry we swap
/// to `BROWSER_UA` since some sites serve content only to browser-shaped UAs.
const DEFAULT_UA: &str = "PeakBot/1.0";

/// Realistic desktop-browser user-agent used on retry when a site blocks the
/// honest UA. No chrome involved — this is just the request header.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// HTTP request timeout, shared by both the default and browser-UA clients.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether an HTTP status is worth retrying. We retry only *transient* codes:
/// `408 Request Timeout`, `425 Too Early`, `429 Too Many Requests`, and any
/// `5xx`. `403 Forbidden` is handled separately (retry once with a browser UA).
/// Permanent client errors (`400`, `401`, `404`, …) never change on retry, so
/// we don't waste round-trips on them.
fn is_transient(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error()
}

/// Backoff for the given attempt (0-indexed): `BACKOFF_BASE * 2^attempt`,
/// capped, with up to ~250ms of cheap nano-derived jitter so concurrent
/// callers don't synchronize their retries. No `rand` dependency.
fn backoff_with_jitter(attempt: u32) -> Duration {
    let exp = BACKOFF_BASE.saturating_mul(1u32 << attempt.min(16));
    let base = exp.min(BACKOFF_CAP);
    // Cheap, dependency-free jitter: low bits of the wall clock in nanos.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = Duration::from_millis((nanos % 250) as u64);
    base + jitter
}

/// Build a one-shot HTTP client with the given user-agent. Spider's `Client`
/// aliases to `reqwest::Client` under the `reqwest_rustls_tls` feature.
fn build_client(user_agent: &str) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(user_agent)
        .build()
}

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
                verbatim. If the page returns a transient error (e.g. 429 \
                rate-limit, a temporary 5xx, or a 403 that only serves to \
                browsers), it is retried automatically with exponential \
                backoff (a 403 is retried once with a browser user-agent). \
                Output is truncated to 50,000 characters."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL of the web page to fetch."
                    },
                    "markdown": {
                        "type": "boolean",
                        "description": "Convert the page to Markdown (default: true). Set to false to get the raw HTML."
                    }
                },
                "required": ["url"]
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
        // under the `reqwest_rustls_tls` feature). `Page::new_page` does a
        // one-shot fetch — no crawl. Start honest with `DEFAULT_UA`.
        let mut client = build_client(DEFAULT_UA)?;
        let mut browser_ua_tried = false;

        // `Page::new_page` never returns `Err` — HTTP failures land in
        // `page.status_code`. Retry policy (no chrome):
        //   * transient status (408/425/429/5xx)  → retry with backoff
        //   * 403 Forbidden                        → retry ONCE with a browser UA
        //   * anything else (2xx/3xx/4xx-permanent) → done
        //
        // Note: spider's `anti_bot_tech`/`waf_check` fields are populated only
        // by its chrome fetcher, never by this plain HTTP path — so there's no
        // WAF detection to act on here. The browser-UA swap is the one cheap
        // defense available without a real browser.
        let mut page = Page::new_page(&args.url, &client).await;
        for attempt in 0..MAX_RETRIES {
            let status = page.status_code;

            let retry = if status.as_u16() == 403 && !browser_ua_tried {
                // Some sites 403 a non-browser UA. Swap once and rebuild.
                browser_ua_tried = true;
                client = build_client(BROWSER_UA)?;
                true
            } else {
                is_transient(status)
            };

            if !retry {
                break;
            }

            let delay = backoff_with_jitter(attempt);
            tracing::warn!(
                target: "peakbot",
                tool_type = "fetch_page",
                url = %args.url,
                status_code = status.as_u16(),
                attempt = attempt + 1,
                max_retries = MAX_RETRIES,
                backoff_ms = delay.as_millis(),
                browser_ua = browser_ua_tried,
                "fetch_page retrying"
            );
            tokio::time::sleep(delay).await;
            page = Page::new_page(&args.url, &client).await;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn transient_statuses_retry() {
        for code in [408u16, 425, 429, 500, 502, 503, 504] {
            let s = StatusCode::from_u16(code).unwrap();
            assert!(is_transient(s), "{code} should be transient");
        }
    }

    #[test]
    fn permanent_statuses_do_not_retry() {
        // 403 is handled separately (browser-UA swap), so it must NOT be
        // classified as transient here, alongside the other permanent codes.
        for code in [200u16, 301, 400, 401, 403, 404, 410] {
            let s = StatusCode::from_u16(code).unwrap();
            assert!(!is_transient(s), "{code} should not be transient");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        // Jitter is additive in [0, 250ms), so each delay sits in
        // [exp_base, exp_base + 250ms). BACKOFF_BASE = 3s → 3s, 6s, then
        // clamped to the 8s cap.
        let jit = Duration::from_millis(250);
        let within = |d: Duration, base: Duration| d >= base && d < base + jit;
        assert!(within(backoff_with_jitter(0), Duration::from_secs(3)));
        assert!(within(backoff_with_jitter(1), Duration::from_secs(6)));
        // Attempt 2 would be 12s but is clamped to the 8s cap.
        assert!(within(backoff_with_jitter(2), BACKOFF_CAP));
        // Large attempt is clamped to the cap, never overflows.
        assert!(within(backoff_with_jitter(30), BACKOFF_CAP));
    }
}
