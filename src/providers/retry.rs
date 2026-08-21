//! Transient-error classification and backoff for the provider layer.
//!
//! `rig_core::http_client::Error` carries a real, typed HTTP status
//! (`InvalidStatusCode` / `InvalidStatusCodeWithMessage`) for every non-2xx
//! response, so `CompletionError::HttpError` is classified by inspecting that
//! status via `is_retryable_status` — not assumed transient. Only
//! `ProviderError(text)` / `ResponseError(text)` arrive as bare strings (a
//! provider-level error envelope, not a transport failure), so those still
//! fall back to substring matching against `TRANSIENT_MESSAGE_MARKERS`.
//!
//! Substring matching is fragile — a localized or reworded rate-limit that
//! omits every marker classifies as *permanent* and the turn is lost. That
//! fallback is out of scope here; it needs provider-by-provider evidence.

use crate::config::RetryConfig;
use rig_core::completion::{CompletionError, PromptError};
use std::time::Duration;

/// Substring markers in `ProviderError`/`ResponseError` messages that
/// indicate a transient (retryable) upstream failure. Deliberately short:
/// the reliable signal is `HttpError` (transport-level, always transient);
/// this list is a best-effort fallback for the case where rig has folded an
/// HTTP status into a string. Every marker is a maintenance liability, so we
/// keep only the high-confidence ones and let false negatives fail the turn
/// rather than guess at every vendor's prose.
const TRANSIENT_MESSAGE_MARKERS: &[&str] = &[
    "429",
    "rate limit",
    "500",
    "502",
    "503",
    "504",
    "overloaded",
];

/// Statuses worth another attempt: the request was fine, the server or the
/// moment was not. Everything else 4xx is a permanent contract violation —
/// the payload is identical on every retry, so retrying only delays the
/// error and buries the message.
fn is_retryable_status(status: http::StatusCode) -> bool {
    status == http::StatusCode::REQUEST_TIMEOUT // 408
        || status == http::StatusCode::TOO_MANY_REQUESTS // 429
        || status.is_server_error() // 5xx, incl. Anthropic's 529
}

/// Classify `rig_core::http_client::Error`. Exhaustive, no wildcard: a rig
/// upgrade that adds a variant must fail this build, not silently default.
fn is_transient_http_error(err: &rig_core::http_client::Error) -> bool {
    use rig_core::http_client::Error as E;
    match err {
        // A real response with a real status — ask the status.
        E::InvalidStatusCode(s) | E::InvalidStatusCodeWithMessage(s, _) => is_retryable_status(*s),
        // Transport: connection reset, TLS, DNS, timeout, truncated body.
        E::Instance(_) | E::StreamEnded => true,
        // We built or read the request/response wrong. Deterministic.
        E::Protocol(_) | E::InvalidHeaderValue(_) | E::NoHeaders | E::InvalidContentType(_) => {
            false
        }
    }
}

/// Whether a `CompletionError` is worth retrying. See module docs for why
/// we fall back to message-substring matching.
fn is_transient_completion_error(err: &CompletionError) -> bool {
    match err {
        CompletionError::HttpError(e) => is_transient_http_error(e),
        // rig providers strip the status into the message; substring-match.
        CompletionError::ProviderError(msg) | CompletionError::ResponseError(msg) => {
            let lower = msg.to_ascii_lowercase();
            TRANSIENT_MESSAGE_MARKERS.iter().any(|m| lower.contains(m))
        }
        // JSON / URL / request-building errors are deterministic and won't
        // change on retry.
        CompletionError::JsonError(_)
        | CompletionError::UrlError(_)
        | CompletionError::RequestError(_) => false,
    }
}

/// Whether a `PromptError` is worth retrying. The agentic loop's
/// non-transient variants (`MaxTurnsError`, `UnknownToolCall`, `ToolError`,
/// `PromptCancelled`) all map to false here.
pub fn is_transient_prompt_error(err: &PromptError) -> bool {
    match err {
        PromptError::CompletionError(c) => is_transient_completion_error(c),
        PromptError::PromptCancelled { .. }
        | PromptError::MaxTurnsError { .. }
        | PromptError::UnknownToolCall { .. }
        | PromptError::ToolError(_)
        | PromptError::ToolServerError(_) => false,
    }
}

/// Backoff for the given attempt (0-indexed): `initial_delay_ms *
/// backoff_factor^attempt`, capped at `max_delay_ms`. No jitter: this is a
/// single interactive agent retrying its own turn, not a fleet of clients
/// that could stampede a recovering upstream — there's no herd to spread out.
pub fn backoff_delay(attempt: u32, cfg: &RetryConfig) -> Duration {
    // Clamp attempt to keep `2^attempt` bounded; we cap the result with
    // `max_delay_ms` anyway, but the multiplication has to not overflow.
    let capped_attempt = attempt.min(32);
    let factor = cfg.backoff_factor.powi(capped_attempt as i32);
    let base_ms = (cfg.initial_delay_ms as f64 * factor) as u64;
    Duration::from_millis(base_ms).min(Duration::from_millis(cfg.max_delay_ms))
}

/// How long to wait before re-running a failed prompt, or `None` when the
/// error is permanent or the retry budget is spent. Same policy the main run
/// loop applies inline (`process_message_internal`), reused by the sub-agent
/// path so a delegation survives the blips a top-level turn survives.
pub fn next_retry_delay(err: &PromptError, attempt: u32, cfg: &RetryConfig) -> Option<Duration> {
    if !is_transient_prompt_error(err) || attempt >= cfg.max_retries {
        return None;
    }
    Some(backoff_delay(attempt, cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_errors_are_transient() {
        assert!(is_transient_completion_error(&CompletionError::HttpError(
            rig_core::http_client::Error::StreamEnded
        )));
    }

    #[test]
    fn provider_error_message_matching() {
        let transient = [
            "HTTP 429 Too Many Requests",
            "rate limit exceeded, slow down",
            "status:500 Internal Server Error",
            "502 Bad Gateway",
            "503 Service Unavailable",
            "504 Gateway Timeout",
            "The server is overloaded, try again",
        ];
        for msg in transient {
            let e = CompletionError::ProviderError(msg.to_string());
            assert!(
                is_transient_completion_error(&e),
                "{msg:?} should be transient"
            );
        }

        let permanent = [
            "Invalid API key",
            "Model not found",
            "Context length exceeded",
            "Bad request: missing field",
            "Forbidden: quota exhausted permanently",
        ];
        for msg in permanent {
            let e = CompletionError::ProviderError(msg.to_string());
            assert!(
                !is_transient_completion_error(&e),
                "{msg:?} should not be transient"
            );
        }
    }

    #[test]
    fn json_and_url_errors_are_not_transient() {
        // Construct minimal errors just to exercise the variant arms.
        let json = CompletionError::JsonError(serde_json::from_str::<u32>("\"x\"").unwrap_err());
        assert!(!is_transient_completion_error(&json));
    }

    #[test]
    fn prompt_error_variants_classified() {
        // MaxTurnsError -> not transient (deterministic, won't change on retry).
        let max_turns = PromptError::MaxTurnsError {
            max_turns: 50,
            chat_history: Box::new(vec![]),
            prompt: Box::new(rig_core::completion::message::Message::from("x")),
        };
        assert!(!is_transient_prompt_error(&max_turns));

        // ProviderError with transient marker -> transient.
        let transient = PromptError::CompletionError(CompletionError::ProviderError(
            "429 Too Many Requests".to_string(),
        ));
        assert!(is_transient_prompt_error(&transient));

        // ProviderError without marker -> not transient.
        let permanent = PromptError::CompletionError(CompletionError::ProviderError(
            "401 Unauthorized".to_string(),
        ));
        assert!(!is_transient_prompt_error(&permanent));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let cfg = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 8000,
            backoff_factor: 2.0,
        };

        let ms = |n| Duration::from_millis(n);
        assert_eq!(backoff_delay(0, &cfg), ms(1000));
        assert_eq!(backoff_delay(1, &cfg), ms(2000));
        assert_eq!(backoff_delay(2, &cfg), ms(4000));
        assert_eq!(backoff_delay(3, &cfg), ms(8000)); // == max, fits
        assert_eq!(backoff_delay(4, &cfg), ms(8000)); // clamped to max

        // attempt 100 would overflow 2^100 — must not panic, must clamp.
        assert_eq!(backoff_delay(100, &cfg), ms(8000));
    }

    #[test]
    fn next_retry_delay_honours_transience_and_budget() {
        let cfg = RetryConfig {
            max_retries: 2,
            initial_delay_ms: 100,
            max_delay_ms: 1000,
            backoff_factor: 2.0,
        };
        let transient = PromptError::CompletionError(CompletionError::ProviderError(
            "503 Service Unavailable".to_string(),
        ));
        let permanent = PromptError::CompletionError(CompletionError::ProviderError(
            "Invalid API key".to_string(),
        ));

        assert_eq!(
            next_retry_delay(&transient, 0, &cfg),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            next_retry_delay(&transient, 1, &cfg),
            Some(Duration::from_millis(200))
        );
        // Budget spent: attempt 2 would be the third try with max_retries = 2.
        assert_eq!(next_retry_delay(&transient, 2, &cfg), None);
        // Permanent errors never retry, however much budget is left.
        assert_eq!(next_retry_delay(&permanent, 0, &cfg), None);
    }

    #[test]
    fn next_retry_delay_with_retries_disabled() {
        let cfg = RetryConfig {
            max_retries: 0,
            ..RetryConfig::default()
        };
        let transient = PromptError::CompletionError(CompletionError::HttpError(
            rig_core::http_client::Error::StreamEnded,
        ));
        assert_eq!(next_retry_delay(&transient, 0, &cfg), None);
    }

    #[test]
    fn backoff_with_zero_initial_delay() {
        let cfg = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 0,
            max_delay_ms: 100,
            backoff_factor: 2.0,
        };
        // 0 * anything = 0 — no delay, no panic.
        assert_eq!(backoff_delay(5, &cfg), Duration::from_millis(0));
    }

    /// rig's `http_client::Error` carries a real, typed status
    /// (`InvalidStatusCode`/`InvalidStatusCodeWithMessage`) — the module doc's
    /// claim that rig "strips the HTTP status" is false. Today the
    /// `HttpError(_) => true` arm ignores that status entirely, so a permanent
    /// 4xx is retried three times with an identical payload — the production
    /// incident this test names: a 400 "image exceeds 10 MB maximum" retried
    /// for ~64s before failing with a generic "max retries exceeded" that
    /// discarded the endpoint's own explanation.
    #[test]
    fn hard_4xx_is_not_transient() {
        use http::StatusCode;
        use rig_core::http_client::Error;

        let body = "messages.2.content.0.tool_result.content.0.image.source.base64: \
                     image exceeds 10 MB maximum: 11663068 bytes > 10485760 bytes";

        for code in [400u16, 401, 403, 404, 413, 422] {
            let status = StatusCode::from_u16(code).unwrap();
            let msg = if code == 400 {
                body.to_string()
            } else {
                format!("{code} error")
            };
            let e = CompletionError::HttpError(Error::InvalidStatusCodeWithMessage(status, msg));
            assert!(
                !is_transient_completion_error(&e),
                "HTTP {code} must not be transient — it is a permanent contract \
                 violation, retrying sends the identical payload again"
            );
        }
    }

    /// Guard against over-correcting defect 2: the statuses that genuinely
    /// mean "try again" must stay transient, through both status-carrying
    /// variants rig can produce. 529 is Anthropic's overloaded code, folded
    /// into `is_server_error()` alongside the standard 5xx range.
    #[test]
    fn retryable_statuses_stay_transient() {
        use http::StatusCode;
        use rig_core::http_client::Error;

        for code in [408u16, 429, 500, 502, 503, 504, 529] {
            let status = StatusCode::from_u16(code).unwrap();

            let bare = CompletionError::HttpError(Error::InvalidStatusCode(status));
            assert!(
                is_transient_completion_error(&bare),
                "HTTP {code} (InvalidStatusCode) must stay transient"
            );

            let with_msg = CompletionError::HttpError(Error::InvalidStatusCodeWithMessage(
                status,
                "retry me".to_string(),
            ));
            assert!(
                is_transient_completion_error(&with_msg),
                "HTTP {code} (InvalidStatusCodeWithMessage) must stay transient"
            );
        }
    }

    /// Non-status `http_client::Error` variants split into two buckets:
    /// genuine transport failures (connection reset, truncated stream) are
    /// worth retrying, while errors from PeakBot/rig building or reading the
    /// request/response wrong are deterministic and won't change on retry.
    #[test]
    fn transport_errors_stay_transient() {
        use rig_core::http_client::Error;

        let stream_ended = CompletionError::HttpError(Error::StreamEnded);
        assert!(is_transient_completion_error(&stream_ended));

        let instance = CompletionError::HttpError(Error::Instance(
            std::io::Error::other("connection reset").into(),
        ));
        assert!(is_transient_completion_error(&instance));

        let no_headers = CompletionError::HttpError(Error::NoHeaders);
        assert!(!is_transient_completion_error(&no_headers));

        let bad_header_value =
            http::HeaderValue::from_bytes(b"\n").expect_err("newline is not a legal header byte");
        let invalid_header =
            CompletionError::HttpError(Error::InvalidHeaderValue(bad_header_value));
        assert!(!is_transient_completion_error(&invalid_header));
    }

    /// End-to-end pin of the user-visible fix: a 400 must stop the retry loop
    /// immediately (attempt 0, full budget) rather than spending the retry
    /// budget on an identical doomed payload. This is the "~64s → ~0s" change.
    #[test]
    fn next_retry_delay_returns_none_for_a_400() {
        use http::StatusCode;
        use rig_core::http_client::Error;

        let cfg = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 8000,
            backoff_factor: 2.0,
        };
        let body = "image exceeds 10 MB maximum: 11663068 bytes > 10485760 bytes";
        let err = PromptError::CompletionError(CompletionError::HttpError(
            Error::InvalidStatusCodeWithMessage(StatusCode::BAD_REQUEST, body.to_string()),
        ));

        assert_eq!(next_retry_delay(&err, 0, &cfg), None);
    }
}
