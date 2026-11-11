//! Anthropic `stop_reason: "refusal"`: the provider's verdict on a payload,
//! surfaced as a readable, never-retried error instead of rig's generic
//! empty-response error.

use bytes::Bytes;
use rig_core::completion::{CompletionError, PromptError};
use rig_core::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig_core::wasm_compat::WasmCompatSend;
use serde::Deserialize;
use std::future::Future;

/// An Anthropic `stop_reason: "refusal"` response. `Display` is the user-facing sentence.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refusal(String);

/// Lenient envelope for `Refusal::parse`: everything a refusal may lack is
/// optional, and `content`'s elements are ignored — only its emptiness matters.
#[derive(Deserialize)]
struct Envelope {
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<serde::de::IgnoredAny>,
    stop_details: Option<StopDetails>,
}

#[derive(Deserialize)]
struct StopDetails {
    category: Option<String>,
    explanation: Option<String>,
}

impl Refusal {
    /// `Some` only for a JSON body with `stop_reason == "refusal"` and empty `content`.
    pub(crate) fn parse(body: &[u8]) -> Option<Self> {
        let env: Envelope = serde_json::from_slice(body).ok()?;
        if env.stop_reason.as_deref() != Some("refusal") || !env.content.is_empty() {
            return None;
        }
        // A blank string is the provider's way of saying "absent".
        let present = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
        let (category, explanation) = match env.stop_details {
            Some(d) => (present(d.category), present(d.explanation)),
            None => (None, None),
        };
        let mut text = String::from("The model refused this request");
        if let Some(c) = category {
            text.push_str(&format!(" ({c})"));
        }
        match explanation {
            Some(e) => text.push_str(&format!(": {e}")),
            None => text.push_str(" — the provider gave no explanation."),
        }
        Some(Self(text))
    }

    /// The refusal behind a prompt error, if that is what it is.
    pub fn of(err: &PromptError) -> Option<&Refusal> {
        match err {
            PromptError::CompletionError(CompletionError::HttpError(
                http_client::Error::Instance(e),
            )) => e.downcast_ref(),
            _ => None,
        }
    }
}

/// reqwest client for Anthropic that reports a refusal as an error instead of an empty reply.
#[derive(Clone, Debug)]
pub struct RefusalAwareHttp(reqwest::Client);

/// The only constructor — `crate::http::client()` keeps the shared TLS and
/// timeout policy from being bypassed.
impl Default for RefusalAwareHttp {
    fn default() -> Self {
        Self(crate::http::client())
    }
}

impl HttpClientExt for RefusalAwareHttp {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes>,
        T: WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let sent = self.0.send::<T, Bytes>(req);
        async move {
            let (parts, body) = sent.await?.into_parts();
            let bytes = body.await?;
            if let Some(refusal) = Refusal::parse(&bytes) {
                return Err(http_client::Error::Instance(Box::new(refusal)));
            }
            let body: LazyBody<U> = Box::pin(async move { Ok(U::from(bytes)) });
            Ok(Response::from_parts(parts, body))
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        self.0.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes> + WasmCompatSend,
    {
        self.0.send_streaming(req)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Fixtures plus a loopback fake of the Anthropic Messages endpoint,
    //! shared by the refusal, retry and main-loop tests. The real rig
    //! Anthropic path must see the wire bytes — the in-crate
    //! `MockCompletionModel` sits above that conversion and cannot express a
    //! refusal.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        Router,
        extract::State,
        http::{HeaderValue, StatusCode, header},
        routing::post,
    };

    /// Live refusal capture: empty `content`, `stop_reason: "refusal"`,
    /// `stop_details` with both category and explanation.
    pub const REFUSAL_BODY: &str = r#"{"id":"msg_x","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"reasoning_extraction","explanation":"This request was blocked as it seems to violate ... Terms of Service restrictions on reverse engineering or duplicating model outputs. ..."},"usage":{"input_tokens":4,"output_tokens":0,"cache_creation_input_tokens":15595,"cache_read_input_tokens":0}}"#;

    /// A normal 200 body: one text block, `end_turn`.
    pub const OK_BODY: &str = r#"{"id":"msg_y","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":4,"output_tokens":1}}"#;

    /// The `Refusal` Display text for `REFUSAL_BODY` — the sentence the main
    /// loop prints behind a `❌ ` prefix.
    pub const REFUSAL_SENTENCE: &str = "The model refused this request (reasoning_extraction): This request was blocked as it seems to violate ... Terms of Service restrictions on reverse engineering or duplicating model outputs. ...";

    #[derive(Clone)]
    struct FakeAnthropic {
        body: &'static str,
        hits: Arc<AtomicUsize>,
    }

    async fn messages_route(
        State(srv): State<FakeAnthropic>,
    ) -> (
        StatusCode,
        [(axum::http::HeaderName, HeaderValue); 1],
        &'static str,
    ) {
        srv.hits.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            srv.body,
        )
    }

    /// Serve `body` at `POST /v1/messages` (rig appends that path to the
    /// base URL) on a random loopback port, counting hits.
    pub async fn serve(body: &'static str) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/messages", post(messages_route))
            .with_state(FakeAnthropic {
                body,
                hits: hits.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind");
        let base_url = format!("http://{}", listener.local_addr().expect("bound address"));
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (base_url, hits)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::config::{AnthropicCaching, AnthropicConfig, ProviderConfig};
    use crate::providers::create_compaction_model;
    use crate::providers::retry::is_transient_prompt_error;

    use super::Refusal;
    use super::test_support;

    fn anthropic_config(base_url: String) -> ProviderConfig {
        ProviderConfig::Anthropic(AnthropicConfig {
            api_key: Some("k".to_string()),
            base_url,
            model: "claude-opus-5-5".to_string(),
            max_tokens: 64,
            prompt_caching: AnthropicCaching::Off,
            vision: None,
            preserve_reasoning: None,
            display_reasoning: None,
            max_image_base64_bytes: 5 * 1024 * 1024,
        })
    }

    /// The user-visible fix, end-to-end on the real rig Anthropic path: a
    /// refusal must surface the provider's reason, stay a hard (non-transient)
    /// error, and cost exactly one wire request.
    #[tokio::test]
    async fn refusal_surfaces_category_and_explanation_and_is_not_retried() {
        let (base_url, hits) = test_support::serve(test_support::REFUSAL_BODY).await;
        let cfg = anthropic_config(base_url);
        let model = create_compaction_model(&cfg, None).expect("compaction model builds");
        let err = model
            .summarize("x")
            .await
            .expect_err("a refusal is an error, not a reply");
        let msg = err.to_string();
        assert!(
            msg.contains(
                "The model refused this request (reasoning_extraction): \
                          This request was blocked as it seems to violate"
            ),
            "the provider's reason must be readable; got: {msg}"
        );
        assert!(
            !msg.contains("Response contained no message"),
            "the generic empty-response string must not survive; got: {msg}"
        );
        assert!(
            !is_transient_prompt_error(&err),
            "a refusal is the provider's verdict on this payload — identical on every retry"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a refusal must cost exactly one wire request"
        );
    }

    /// Guard for the other half of the seam: a normal 200 body must pass
    /// through the refusal-aware client byte-for-byte and come back as the
    /// plain reply rig parses today.
    #[tokio::test]
    async fn normal_response_passes_through() {
        let (base_url, hits) = test_support::serve(test_support::OK_BODY).await;
        let cfg = anthropic_config(base_url);
        let model = create_compaction_model(&cfg, None).expect("compaction model builds");
        let reply = model
            .summarize("x")
            .await
            .expect("a normal 200 body is a normal reply");
        assert_eq!(reply, "hello");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "exactly one wire request");
    }

    /// The sentence is built from two independent parts: ` ({category})` when
    /// present, then `: {explanation}` when present or the fixed
    /// no-explanation tail. Every missing-details shape must land on its exact
    /// string — this is what the user reads.
    #[test]
    fn refusal_text_covers_missing_details() {
        let cases = [
            (test_support::REFUSAL_BODY, test_support::REFUSAL_SENTENCE),
            (
                r#"{"id":"msg_x","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":null},"usage":{"input_tokens":4,"output_tokens":0}}"#,
                "The model refused this request (cyber) — the provider gave no explanation.",
            ),
            (
                r#"{"id":"msg_x","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":null,"explanation":"Because."},"usage":{"input_tokens":4,"output_tokens":0}}"#,
                "The model refused this request: Because.",
            ),
            (
                r#"{"id":"msg_x","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"refusal","stop_details":null,"usage":{"input_tokens":4,"output_tokens":0}}"#,
                "The model refused this request — the provider gave no explanation.",
            ),
            (
                r#"{"id":"msg_x","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"refusal","usage":{"input_tokens":4,"output_tokens":0}}"#,
                "The model refused this request — the provider gave no explanation.",
            ),
        ];
        for (body, expected) in cases {
            let refusal = Refusal::parse(body.as_bytes()).unwrap_or_else(|| panic!("body: {body}"));
            assert_eq!(refusal.to_string(), expected, "body: {body}");
        }
    }

    /// Only `stop_reason == "refusal"` with empty `content` is a refusal.
    /// Everything else — other stop reasons, error envelopes, non-JSON,
    /// empty bodies — must be left untouched for rig.
    #[test]
    fn non_refusals_are_not_parsed() {
        let bodies: &[&[u8]] = &[
            test_support::OK_BODY.as_bytes(),
            // A tool_use reply: non-empty content, different stop reason.
            br#"{"id":"msg_t","type":"message","role":"assistant","model":"m","content":[{"type":"tool_use","id":"call_1","name":"bash","input":{"command":"ls"}}],"stop_reason":"tool_use","usage":{"input_tokens":4,"output_tokens":1}}"#,
            // Empty content, but the stop reason is max_tokens, not refusal.
            br#"{"id":"msg_m","type":"message","role":"assistant","model":"m","content":[],"stop_reason":"max_tokens","usage":{"input_tokens":4,"output_tokens":64}}"#,
            // The provider's error envelope, not a message at all.
            br#"{"type":"error","error":{"type":"overloaded_error","message":"x"}}"#,
            b"not json",
            b"",
        ];
        for body in bodies {
            assert!(Refusal::parse(body).is_none(), "not a refusal: {body:?}");
        }
    }

    /// A refusal that still carries content keeps its current behaviour: rig
    /// returns that content as a normal reply, so `parse` must not intercept
    /// it (Design B changes the empty-response outcome only).
    #[test]
    fn refusal_with_content_is_left_to_rig() {
        let body = br#"{"id":"msg_z","type":"message","role":"assistant","model":"m","content":[{"type":"text","text":"partial"}],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":"x"},"usage":{"input_tokens":4,"output_tokens":1}}"#;
        assert!(
            Refusal::parse(body).is_none(),
            "non-empty content is left to rig: {body:?}"
        );
    }
}
