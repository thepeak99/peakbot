//! Transient-error classification and backoff for the provider layer.
//!
//! `rig-core` strips the HTTP status from its error variants, folding any
//! non-2xx into `CompletionError::ProviderError(text)` / `ResponseError(text)`
//! (#111). With no status on the wire, transience is detected by:
//!   * `CompletionError::HttpError(_)` — always transient (transport-level)
//!   * `ProviderError(msg)` / `ResponseError(msg)` — transient iff `msg`
//!     matches a known marker (see `TRANSIENT_MESSAGE_MARKERS`).
//!
//! Substring matching is fragile — a localized or reworded rate-limit that
//! omits every marker classifies as *permanent* and the turn is lost. A
//! robust fix needs an upstream patch preserving the status code.

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

/// Whether a `CompletionError` is worth retrying. See module docs for why
/// we fall back to message-substring matching.
fn is_transient_completion_error(err: &CompletionError) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
