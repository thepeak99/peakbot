use crate::config::FetchUrlConfig;
use crate::utils::strings::truncate_with_suffix;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;

const MAX_RESPONSE_CHARS: usize = 50_000;

#[derive(Debug, thiserror::Error)]
pub enum FetchUrlError {
    #[error("Request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("{0}")]
    InvalidArgs(String),
    #[error("POST is disabled; set `fetch_url.allow_post: true` in config.yaml")]
    PostDisabled,
}

#[derive(Deserialize)]
pub struct FetchUrlArgs {
    url: String,
    #[serde(default)]
    method: Method,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
}

#[derive(Deserialize, Default, Debug, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
enum Method {
    #[default]
    Get,
    Post,
}

#[derive(Debug, PartialEq)]
enum Request {
    Get,
    Post { body: String, content_type: String },
}

/// The single validation point: args + config → a concrete request, or a
/// coachable error. `call` never re-validates.
fn parse(args: FetchUrlArgs, allow_post: bool) -> Result<Request, FetchUrlError> {
    if args.url.is_empty() {
        return Err(FetchUrlError::InvalidUrl("URL cannot be empty".to_string()));
    }
    match args.method {
        Method::Get => {
            if args.body.is_some() || args.content_type.is_some() {
                return Err(FetchUrlError::InvalidArgs(
                    "body/content_type require method POST".to_string(),
                ));
            }
            Ok(Request::Get)
        }
        Method::Post => {
            if !allow_post {
                return Err(FetchUrlError::PostDisabled);
            }
            Ok(Request::Post {
                body: args.body.unwrap_or_default(),
                content_type: args
                    .content_type
                    .unwrap_or_else(|| "application/json".to_string()),
            })
        }
    }
}

#[derive(Default)]
pub struct FetchUrlTool {
    allow_post: bool,
}

impl FetchUrlTool {
    pub fn new(config: &FetchUrlConfig) -> Self {
        Self {
            allow_post: config.allow_post,
        }
    }
}

impl Tool for FetchUrlTool {
    const NAME: &'static str = "fetch_url";
    type Error = FetchUrlError;
    type Args = FetchUrlArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        // With POST disabled the schema and description stay byte-identical
        // to the GET-only tool, so existing model behaviour cannot drift.
        let (description, parameters) = if self.allow_post {
            (
                "Fetch the content of a URL and return it as text. \
                    Use this to retrieve web page content, API responses, or any HTTP GET or POST request. \
                    Returns the response body up to 50,000 characters (truncated if longer)."
                    .to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to fetch"
                        },
                        "method": {
                            "type": "string",
                            "enum": ["GET", "POST"],
                            "description": "HTTP method (default GET)"
                        },
                        "body": {
                            "type": "string",
                            "description": "Request body, POST only"
                        },
                        "content_type": {
                            "type": "string",
                            "description": "Content-Type for the POST body (default application/json)"
                        }
                    },
                    "required": ["url"]
                }),
            )
        } else {
            (
                "Fetch the content of a URL and return it as text. \
                    Use this to retrieve web page content, API responses, or any HTTP GET request. \
                    Returns the response body up to 50,000 characters (truncated if longer)."
                    .to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to fetch"
                        }
                    },
                    "required": ["url"]
                }),
            )
        };
        ToolDefinition {
            name: "fetch_url".to_string(),
            description,
            parameters,
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let url = args.url.clone();
        let request = parse(args, self.allow_post)?;

        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "fetch_url",
            url = %url,
            method = match &request {
                Request::Get => "GET",
                Request::Post { .. } => "POST",
            },
            "Starting fetch_url tool execution"
        );

        let start_time = std::time::Instant::now();

        // Make the HTTP request
        let client = crate::http::client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        // No retries: POST isn't idempotent, and reqwest turns a POST into a GET on 301/302/303.
        let request = match &request {
            Request::Get => client.get(&url),
            Request::Post { body, content_type } => client
                .post(&url)
                .header("Content-Type", content_type)
                .body(body.clone()),
        };
        let response = request.header("User-Agent", "PeakBot/1.0").send().await?;

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
            url = %url,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FetchUrlConfig;
    use crate::tools::ThoughtGate;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    // RED: written against the locked fetch_url-POST spec. `FetchUrlTool::new`,
    // `parse`, `Request`, and the `InvalidArgs`/`PostDisabled` variants do not
    // exist yet — every test here fails to compile until they do.

    fn args(value: serde_json::Value) -> FetchUrlArgs {
        serde_json::from_value(value).expect("test args deserialize")
    }

    // ── parse ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_empty_url_is_invalid_url() {
        let err = parse(args(json!({ "url": "" })), true).expect_err("empty url must fail");
        assert!(
            matches!(err, FetchUrlError::InvalidUrl(_)),
            "expected InvalidUrl, got: {err:?}"
        );
    }

    #[test]
    fn parse_plain_get_is_get_request() {
        assert_eq!(
            parse(args(json!({ "url": "http://x" })), false).expect("plain GET parses"),
            Request::Get
        );
    }

    #[test]
    fn parse_get_with_body_is_invalid_args_mentioning_post() {
        let err = parse(args(json!({ "url": "http://x", "body": "data" })), true)
            .expect_err("GET with body must fail");
        match err {
            FetchUrlError::InvalidArgs(msg) => assert!(
                msg.to_ascii_lowercase().contains("post"),
                "the message must point the model at POST, got: {msg}"
            ),
            other => panic!("expected InvalidArgs, got: {other:?}"),
        }
    }

    #[test]
    fn parse_get_with_content_type_is_invalid_args_mentioning_post() {
        let err = parse(
            args(json!({ "url": "http://x", "content_type": "text/plain" })),
            true,
        )
        .expect_err("GET with content_type must fail");
        match err {
            FetchUrlError::InvalidArgs(msg) => assert!(
                msg.to_ascii_lowercase().contains("post"),
                "the message must point the model at POST, got: {msg}"
            ),
            other => panic!("expected InvalidArgs, got: {other:?}"),
        }
    }

    #[test]
    fn parse_post_without_allow_post_is_post_disabled() {
        let err = parse(args(json!({ "url": "http://x", "method": "POST" })), false)
            .expect_err("POST must be refused when disabled");
        assert!(
            matches!(err, FetchUrlError::PostDisabled),
            "expected PostDisabled, got: {err:?}"
        );
        // The operator needs the exact config knob in the error text.
        assert!(
            err.to_string().contains("fetch_url.allow_post"),
            "message must name the config key, got: {err}"
        );
    }

    #[test]
    fn parse_post_with_allow_post_builds_post_request() {
        let explicit = parse(
            args(json!({
                "url": "http://x",
                "method": "POST",
                "body": "{\"a\":1}",
                "content_type": "text/plain"
            })),
            true,
        )
        .expect("POST parses when allowed");
        assert_eq!(
            explicit,
            Request::Post {
                body: "{\"a\":1}".to_string(),
                content_type: "text/plain".to_string(),
            }
        );

        // Absent body / content_type fall back to "" / application/json.
        let defaulted = parse(args(json!({ "url": "http://x", "method": "POST" })), true)
            .expect("POST without body parses when allowed");
        assert_eq!(
            defaulted,
            Request::Post {
                body: String::new(),
                content_type: "application/json".to_string(),
            }
        );
    }

    #[test]
    fn args_with_put_method_is_a_serde_error() {
        let result: Result<FetchUrlArgs, _> =
            serde_json::from_value(json!({ "url": "http://x", "method": "PUT" }));
        assert!(
            result.is_err(),
            "PUT is not a supported method and must not deserialize"
        );
    }

    // ── schema ─────────────────────────────────────────────────────────────

    /// Pinned from the pre-POST `definition()`: with `allow_post: false` the
    /// wire schema must stay byte-for-byte today's, so existing model
    /// behaviour cannot drift. The description literal is copied verbatim
    /// (same continuation style) from the current `definition()`.
    #[tokio::test]
    async fn definition_without_allow_post_is_the_pinned_schema() {
        let def = FetchUrlTool::default().definition(String::new()).await;
        assert_eq!(def.name, "fetch_url");
        assert_eq!(
            def.description,
            "Fetch the content of a URL and return it as text. \
                Use this to retrieve web page content, API responses, or any HTTP GET request. \
                Returns the response body up to 50,000 characters (truncated if longer)."
        );
        assert_eq!(
            def.parameters,
            json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    }
                },
                "required": ["url"]
            })
        );
    }

    #[tokio::test]
    async fn definition_with_allow_post_exposes_method_body_and_content_type() {
        let def = FetchUrlTool::new(&FetchUrlConfig { allow_post: true })
            .definition(String::new())
            .await;
        let props = def.parameters["properties"]
            .as_object()
            .expect("properties object");
        for key in ["url", "method", "body", "content_type"] {
            assert!(props.contains_key(key), "missing property {key}");
        }
        assert_eq!(props["method"]["type"], "string");
        assert_eq!(props["method"]["enum"], json!(["GET", "POST"]));
        assert_eq!(props["body"]["type"], "string");
        assert_eq!(props["content_type"]["type"], "string");
        assert_eq!(def.parameters["required"], json!(["url"]));
        assert!(
            def.description.to_ascii_lowercase().contains("post"),
            "description must mention POST: {}",
            def.description
        );
    }

    #[tokio::test]
    async fn thought_gate_injects_thought_into_allow_post_schema() {
        let gate = ThoughtGate::wrap(Box::new(FetchUrlTool::new(&FetchUrlConfig {
            allow_post: true,
        })));
        // Fully qualified: `FetchUrlTool` also implements `ToolDyn` via the
        // rig blanket impl, so a bare `.definition()` would be ambiguous.
        let def = rig_core::tool::ToolDyn::definition(&gate, String::new()).await;
        let required = def.parameters["required"]
            .as_array()
            .expect("required array");
        assert!(
            required.iter().any(|v| v == "thought"),
            "thought must be injected as required: {required:?}"
        );
    }

    // ── call() round-trips against a loopback server ───────────────────────

    /// One-shot loopback HTTP server: reads the request (headers until the
    /// `\r\n\r\n` boundary, then `Content-Length` body bytes), replies
    /// `HTTP/1.1 201 Created` with body `ok`, and hands the captured
    /// (headers, body) back over a oneshot.
    async fn loopback_once() -> (String, oneshot::Receiver<(String, String)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // The body may arrive in the same read as the headers, so scan
            // for the boundary instead of checking the buffer tail.
            let mut header_end = None;
            while header_end.is_none() {
                let n = socket.read(&mut chunk).await.expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
            }
            let header_end = header_end.expect("header/body boundary");
            let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            let mut body = buf[header_end..].to_vec();
            while body.len() < content_length {
                let n = socket.read(&mut chunk).await.expect("read body");
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(content_length);
            let _ = socket
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok")
                .await;
            let _ = tx.send((headers, String::from_utf8_lossy(&body).into_owned()));
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    async fn post_round_trip_sends_body_and_default_content_type() {
        let (base, rx) = loopback_once().await;
        let tool = FetchUrlTool::new(&FetchUrlConfig { allow_post: true });
        let out = tool
            .call(args(json!({
                "url": format!("{base}/x"),
                "method": "POST",
                "body": "{\"a\":1}"
            })))
            .await
            .expect("POST round trip succeeds");

        let (headers, body) = rx.await.expect("server captured the request");
        let request_line = headers.lines().next().unwrap_or("");
        assert!(
            request_line.starts_with("POST /x"),
            "request line must be POST /x, got: {request_line}"
        );
        let content_type = headers
            .lines()
            .find(|l| l.trim().to_ascii_lowercase().starts_with("content-type:"))
            .map(|l| {
                l.trim()
                    .split_once(':')
                    .map(|(_, value)| value)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        assert_eq!(
            content_type, "application/json",
            "absent content_type must default to application/json"
        );
        assert_eq!(body, "{\"a\":1}", "the body must arrive verbatim");
        assert!(
            out.starts_with("HTTP 201 Created\n\nok"),
            "output format is unchanged, got: {out}"
        );
    }

    #[tokio::test]
    async fn get_round_trip_sends_get_request_line() {
        let (base, rx) = loopback_once().await;
        let tool = FetchUrlTool::new(&FetchUrlConfig { allow_post: true });
        let out = tool
            .call(args(json!({ "url": format!("{base}/x") })))
            .await
            .expect("GET round trip succeeds");

        let (headers, _) = rx.await.expect("server captured the request");
        let request_line = headers.lines().next().unwrap_or("");
        assert!(
            request_line.starts_with("GET /x"),
            "request line must be GET /x, got: {request_line}"
        );
        assert!(
            out.starts_with("HTTP 201 Created\n\nok"),
            "output format is unchanged, got: {out}"
        );
    }

    #[tokio::test]
    async fn call_post_with_allow_post_disabled_is_post_disabled() {
        let tool = FetchUrlTool::default();
        let err = tool
            .call(args(
                json!({ "url": "http://127.0.0.1:9/x", "method": "POST" }),
            ))
            .await
            .expect_err("POST must be refused before any request is sent");
        assert!(
            matches!(err, FetchUrlError::PostDisabled),
            "expected PostDisabled, got: {err:?}"
        );
    }
}
