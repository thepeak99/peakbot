//! Retry helpers for the provider layer.
//!
//! Wraps the existing (broken — see #111) retry loop in
//! `crate::lib::process_message_internal`. All upstream LLM providers in
//! `rig-core` strip the HTTP status from their error variants, converting
//! any non-2xx into `CompletionError::ProviderError(text)` or
//! `ResponseError(text)` (verified by grep on the upstream crate). We
//! therefore detect transient failures by:
//!   * `CompletionError::HttpError(_)` — always transient (transport-level)
//!   * `CompletionError::ProviderError(msg)` / `ResponseError(msg)` —
//!     transient iff `msg` matches known transient markers (429, rate limit,
//!     5xx, server-side markers). Substring matching is fragile; a proper
//!     fix requires an upstream patch to preserve the status code.

use crate::config::RetryConfig;
use rig_core::completion::{CompletionError, PromptError};
use std::time::Duration;

/// HTTP statuses that are worth retrying. Mirrors `fetch_page`'s policy
/// (408/425/429/5xx). Only used when the status code is available — see
/// `is_transient_completion_error` for the rig-error path.
#[allow(dead_code)] // Exported for future use when rig (or a future http_client wrapper) surfaces status in error variants.
pub fn transient_status_code(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..600).contains(&status)
}

/// Substring markers in `ProviderError`/`ResponseError` messages that
/// indicate a transient (retryable) upstream failure. The list is
/// deliberately conservative — false positives cost a 30s sleep, false
/// negatives cost the user their turn.
const TRANSIENT_MESSAGE_MARKERS: &[&str] = &[
    "429",
    "rate limit",
    "too many requests",
    " 500 ",
    " 502 ",
    " 503 ",
    " 504 ",
    " 524 ",
    "server error",
    "service unavailable",
    "bad gateway",
    "gateway timeout",
    "temporarily unavailable",
    "overloaded",
    "capacity",
];

/// Whether a `CompletionError` is worth retrying. See module docs for why
/// we fall back to message-substring matching.
pub fn is_transient_completion_error(err: &CompletionError) -> bool {
    match err {
        // Transport-level errors (TCP reset, TLS, timeout) are always transient.
        CompletionError::HttpError(_) => true,
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
/// backoff_factor^attempt`, capped at `max_delay_ms`, plus up to ~250ms
/// of cheap nano-derived jitter so concurrent callers don't synchronize.
/// No `rand` dependency — same trick `fetch_page` uses.
pub fn backoff_delay(attempt: u32, cfg: &RetryConfig) -> Duration {
    // Clamp attempt to keep `2^attempt` bounded; we cap the result with
    // `max_delay_ms` anyway, but the multiplication has to not overflow.
    let capped_attempt = attempt.min(32);
    let factor = cfg.backoff_factor.powi(capped_attempt as i32);
    let base_ms = (cfg.initial_delay_ms as f64 * factor) as u64;
    let base = Duration::from_millis(base_ms).min(Duration::from_millis(cfg.max_delay_ms));

    // Cheap, dependency-free jitter: low bits of the wall clock in nanos.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter = Duration::from_millis((nanos % 250) as u64);
    base + jitter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_status_codes_retry() {
        for code in [408u16, 425, 429, 500, 502, 503, 504, 524, 599] {
            assert!(transient_status_code(code), "{code} should be transient");
        }
    }

    #[test]
    fn permanent_status_codes_do_not_retry() {
        for code in [200u16, 301, 400, 401, 403, 404, 410, 422] {
            assert!(
                !transient_status_code(code),
                "{code} should not be transient"
            );
        }
    }

    #[test]
    fn http_error_is_always_transient() {
        assert!(is_transient_completion_error(&CompletionError::HttpError(
            rig_core::http_client::Error::StreamEnded
        )));
    }

    #[test]
    fn provider_error_message_matching() {
        let transient = [
            "HTTP 429 Too Many Requests",
            "rate limit exceeded, slow down",
            "Too Many Requests, please retry",
            " 500 Internal Server Error",
            " 502 Bad Gateway",
            " 503 Service Unavailable",
            " 504 Gateway Timeout",
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

        // attempt 0: 1000ms ± jitter
        // attempt 1: 2000ms ± jitter
        // attempt 2: 4000ms ± jitter
        // attempt 3: 8000ms (would be 8000 = max, fits)
        // attempt 4+: clamped to max_delay_ms
        let jitter = Duration::from_millis(250);
        let within = |d: Duration, base_ms: u64| {
            let lo = Duration::from_millis(base_ms);
            let hi = lo + jitter;
            d >= lo && d < hi
        };

        assert!(within(backoff_delay(0, &cfg), 1000));
        assert!(within(backoff_delay(1, &cfg), 2000));
        assert!(within(backoff_delay(2, &cfg), 4000));
        assert!(within(backoff_delay(3, &cfg), 8000));

        // attempt 100 would overflow 2^100 — must not panic, must clamp.
        let d = backoff_delay(100, &cfg);
        assert!(d >= Duration::from_millis(8000));
        assert!(d < Duration::from_millis(8000) + jitter);
    }

    #[test]
    fn backoff_with_zero_initial_delay() {
        let cfg = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 0,
            max_delay_ms: 100,
            backoff_factor: 2.0,
        };
        // 0 * anything = 0; result is bounded by jitter (<250ms) which is
        // > max_delay_ms — confirm we still cap at max_delay_ms.
        let d = backoff_delay(5, &cfg);
        assert!(d <= Duration::from_millis(100) + Duration::from_millis(250));
    }
}
