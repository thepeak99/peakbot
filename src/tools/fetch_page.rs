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

/// Wall clock on a single `Page::new_page` await. Sits just above the client's
/// own `REQUEST_TIMEOUT` so reqwest's timeout — which yields a real status and
/// a real error — wins in every normal case; this fires only when the future is
/// genuinely stuck. Spider builds its own client, so neither `REQUEST_TIMEOUT`
/// nor `crate::http::TIMEOUTS` reaches its fetches (postmortem 0.16.1).
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(35);

/// One bounded fetch. `None` means the future was cancelled by its budget —
/// dropping it tears the connection down via reqwest's `Drop`.
async fn fetch_once(url: &str, client: &reqwest::Client, budget: Duration) -> Option<Page> {
    tokio::time::timeout(budget, Page::new_page(url, client))
        .await
        .ok()
}

/// Worst-case wall clock of a whole `fetch_page` call: every attempt times out
/// plus every capped backoff. Derived, never hard-coded, so the coherence test
/// against `DEFAULT_TOOL_BUDGET` cannot drift. Derived specification: its only
/// consumers are the coherence tests here and in `time_budget`.
#[cfg(test)]
pub(crate) fn worst_case_duration() -> Duration {
    ATTEMPT_TIMEOUT * (MAX_RETRIES + 1) + (BACKOFF_CAP + Duration::from_millis(250)) * MAX_RETRIES
}

/// A cancelled attempt, phrased for the model: the canonical timeout wording
/// plus the one fact it needs to route around the problem — which URL, and
/// that the host is the thing that never answered.
fn fetch_timeout_result(url: &str) -> String {
    format!(
        "{}\n\nURL: {url}\nThe host accepted the request and never answered \
         (bad TLS or a black-holed connection).",
        crate::tools::time_budget::timeout_message("fetch_page", ATTEMPT_TIMEOUT)
    )
}

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

/// Build a one-shot HTTP client with the given user-agent. TLS comes from the
/// shared webpki-root config (see crate::http) so this works on Android too.
fn build_client(user_agent: &str) -> Result<reqwest::Client, reqwest::Error> {
    crate::http::client_builder()
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
        // `page.status_code`, and a pathological host makes it never return at
        // all, so every attempt is bounded by `ATTEMPT_TIMEOUT`. Retry policy
        // (no chrome):
        //   * transient status (408/425/429/5xx)  → retry with backoff
        //   * 403 Forbidden                        → retry ONCE with a browser UA
        //   * a cancelled attempt                  → terminal, never retried
        //   * anything else (2xx/3xx/4xx-permanent) → done
        //
        // Note: spider's `anti_bot_tech`/`waf_check` fields are populated only
        // by its chrome fetcher, never by this plain HTTP path — so there's no
        // WAF detection to act on here. The browser-UA swap is the one cheap
        // defense available without a real browser.
        //
        // A hung attempt is terminal because a host that swallows the request
        // whole will not answer 3s later; retrying a hang only multiplies it.
        let Some(mut page) = fetch_once(&args.url, &client, ATTEMPT_TIMEOUT).await else {
            return Ok(fetch_timeout_result(&args.url));
        };
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
            let Some(next) = fetch_once(&args.url, &client, ATTEMPT_TIMEOUT).await else {
                return Ok(fetch_timeout_result(&args.url));
            };
            page = next;
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

    // ── per-attempt deadline (A) ────────────────────────────────────────────
    //
    // These tests pin the regression-test contract for the postmortem:
    // `fetch_page → spider → Page::new_page` wedged >1 h because nothing
    // armed a wall-clock deadline above the spider future. The fix extracts
    // `fetch_once(url, client, budget) -> Option<Page>` so a hung attempt
    // returns `None` at the budget, NOT after the upstream eventually times
    // out. The function does not exist yet — these tests are RED.

    /// The regression test for the incident. The mechanism: a host accepts the
    /// connection, then never sends a byte (bad TLS, NXDOMAIN, swallowed
    /// request — all shape the same way). With the bounded `fetch_once`, a
    /// hung attempt returns `None` inside the budget; without it, the future
    /// hangs forever. Assert both the outcome (`None`) AND the wall-clock
    /// bound (must come back within seconds, NOT hang for minutes).
    #[tokio::test]
    async fn fetch_once_bounds_a_server_that_accepts_and_never_responds() {
        // Loopback listener that accepts and never writes — the exact shape
        // of the pathological host, with the network swapped for a socket we
        // own. Pattern from src/http.rs:96-124.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _acceptor = std::thread::spawn(move || {
            // Hold the accepted socket open; never write a byte.
            let _held = listener.accept();
            std::thread::sleep(std::time::Duration::from_secs(30));
        });

        let client = build_client(DEFAULT_UA).expect("test client builds");
        let started = std::time::Instant::now();
        let result = fetch_once(
            &format!("http://{addr}/"),
            &client,
            Duration::from_millis(300),
        )
        .await;
        let elapsed = started.elapsed();

        assert!(
            result.is_none(),
            "hung attempt must return None, not hang: got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "hung attempt must return within seconds, not minutes; took {elapsed:?}"
        );
    }

    /// The derived worst-case wall clock for a whole `fetch_page` call:
    /// every attempt timed out plus every capped backoff. Per design §5.3,
    /// it is `(ATTEMPT_TIMEOUT * (MAX_RETRIES + 1)) +
    /// ((BACKOFF_CAP + 250ms) * MAX_RETRIES)`. Pinned as the literal
    /// arithmetic so a future tweak to any single constant breaks this test
    /// loudly with the exact value that drifted.
    #[test]
    fn worst_case_duration_is_the_sum_of_attempts_and_backoffs() {
        // ATTEMPT_TIMEOUT = 35s × 4 attempts (initial + 3 retries)
        // + (BACKOFF_CAP + 250ms) × 3 backoffs
        let expected = Duration::from_secs(35) * (MAX_RETRIES + 1)
            + (BACKOFF_CAP + Duration::from_millis(250)) * MAX_RETRIES;
        assert_eq!(worst_case_duration(), expected);
        // And the whole tool must fit inside the default decorator budget,
        // else the generic backstop cuts the informative per-attempt message
        // before fetch_page's own bound fires.
        assert!(
            worst_case_duration() < crate::tools::time_budget::DEFAULT_TOOL_BUDGET,
            "fetch_page worst case ({:?}) must fit inside the default tool budget ({:?})",
            worst_case_duration(),
            crate::tools::time_budget::DEFAULT_TOOL_BUDGET
        );
    }

    /// Design §9.2 Q3: the per-attempt deadline must stay strictly above the
    /// client's own request timeout, so reqwest's informative timeout wins in
    /// the normal case and this one only catches a genuinely stuck future.
    #[test]
    fn attempt_timeout_sits_above_the_request_timeout() {
        assert!(
            ATTEMPT_TIMEOUT > REQUEST_TIMEOUT,
            "ATTEMPT_TIMEOUT ({ATTEMPT_TIMEOUT:?}) must exceed REQUEST_TIMEOUT ({REQUEST_TIMEOUT:?})"
        );
    }

    /// The timeout-result helper must produce text the model can act on:
    /// the canonical ⏱ TIMEOUT marker, the tool name (so the model knows
    /// which call was cut), and the URL verbatim (so it can route around the
    /// problem). The design §4.2 pins this exact shape.
    #[test]
    fn timeout_result_names_the_url_and_is_ok() {
        let url = "https://qsafeprotocol.io/";
        let out = fetch_timeout_result(url);

        assert!(
            out.contains("⏱ TIMEOUT"),
            "canonical timeout marker missing: {out}"
        );
        assert!(
            out.contains("fetch_page"),
            "tool name missing — the model can't tell which call was cut: {out}"
        );
        assert!(
            out.contains(url),
            "URL must be echoed verbatim so the model can route around it: {out}"
        );
    }

    /// The original incident, end-to-end against the actual pathological host.
    /// Marked `#[ignore]` — CI has no network egress. Run locally with
    /// `cargo test -- --ignored pathological_host_returns_within_the_budget`
    /// to confirm the fix on the real network shape.
    #[ignore = "needs network; run locally with --ignored"]
    #[tokio::test]
    async fn pathological_host_returns_within_the_budget() {
        let url = "https://qsafeprotocol.io/";
        let started = std::time::Instant::now();
        let result = FetchPageTool
            .call(FetchPageArgs {
                url: url.to_string(),
                markdown: true,
            })
            .await;
        let elapsed = started.elapsed();

        // The whole call must come back within worst-case + a generous slack
        // for any final wiring (transformer, panel). If it doesn't, the budget
        // didn't fire and we have a hang again.
        assert!(
            elapsed < worst_case_duration() + Duration::from_secs(10),
            "postmortem host must return inside the bounded budget; took {elapsed:?}"
        );
        assert!(
            result.is_ok(),
            "result must be Ok(timeout message), not an Err; got {result:?}"
        );
    }
}
