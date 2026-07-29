//! Turn a failed delegation into a summarised handoff for the orchestrator.
//!
//! A sub-agent that dies mid-task used to surface as a stringified error, which
//! told the orchestrator nothing about the work already done — it would either
//! re-delegate the identical task (and hit the identical wall) or give up. Here
//! the failure is classified, the sub-agent's own transcript is summarised by
//! the role's model, and the result comes back as a *successful* tool result
//! carrying that summary plus one line of guidance.
//!
//! The single exception is a user `/stop`, which stays an error so the whole
//! turn unwinds as before.
//!
//! Only failures that genuinely end a delegation reach `classify`: unknown
//! tool calls and tool errors are fed back to the sub-agent mid-loop (see
//! `SessionHook::on_invalid_tool_call` and rig's tool executor), and transient
//! wire errors are retried in `DelegateTool::call`.

use crate::config::ProviderConfig;
use crate::providers::create_compaction_model;
use crate::utils::strings::truncate_with_suffix;
use rig_core::completion::PromptError;
use rig_core::completion::message::{AssistantContent, Message, ToolResultContent, UserContent};
use std::collections::HashMap;

/// Error strings (and the header's `{error}`) are capped at this many bytes.
const ERROR_CAP: usize = 300;
/// Hard cap on the rendered summary in the final result.
const SUMMARY_CAP: usize = 4 * 1024;
/// Cap on the last-assistant-text fallback used when the summariser is unusable.
const FALLBACK_CAP: usize = 1500;

/// Above this, the transcript is elided front/back before summarisation.
const TRANSCRIPT_CAP: usize = 48 * 1024;
/// Kept from the front on elision — enough to preserve the original task brief.
const HEAD_CAP: usize = 12 * 1024;
/// Kept from the back on elision — where the sub-agent actually stopped.
const TAIL_CAP: usize = 36 * 1024;

const USER_TEXT_CAP: usize = 2000;
const TOOL_RESULT_CAP: usize = 500;
const ASSISTANT_TEXT_CAP: usize = 1000;
const TOOL_ARGS_CAP: usize = 200;

const SUMMARY_PROMPT: &str = "A sub-agent was interrupted before it finished the task shown at the top of the transcript below. Write a handoff briefing for the orchestrator that delegated it. No preamble, no apology — just:\n\n1. What it actually did: files touched, commands run, searches made.\n2. What it found or concluded: concrete facts, names, paths, numbers.\n3. Exactly where it stopped, and what remains unfinished.\n\nBe specific and dense. 200 words maximum. If the transcript shows nothing was accomplished, say exactly that in one line.\n\n--- SUB-AGENT TRANSCRIPT ---\n";

const CONTEXT_GUIDANCE: &str = "[The task was NOT completed. Your call: re-delegate only the remaining slice, split it across several smaller delegations, pick a different role, or finish it yourself. Re-sending this same task will hit the same limit again.]";
const FAILED_GUIDANCE: &str = "[The task was NOT completed. Your call: if that error looks transient, re-delegate as-is; otherwise narrow the task, split it across several delegations, or pick a different role.]";
const NO_SUMMARY: &str = "No summary of its work is recoverable.";

/// What a failed delegation becomes. Total over `PromptError`.
pub(crate) enum Handoff {
    /// User pressed stop — propagate as an error; the turn unwinds.
    Abort(PromptError),
    /// The budget gate fired.
    ContextExceeded { history: Vec<Message> },
    /// Anything else.
    Failed {
        error: String,
        history: Vec<Message>,
    },
}

/// Which banner the result carries.
enum Header {
    ContextExceeded,
    Failed { error: String },
}

impl Header {
    /// Short class name for the log line.
    fn class(&self) -> &'static str {
        match self {
            Header::ContextExceeded => "context-exceeded",
            Header::Failed { .. } => "failed",
        }
    }
}

/// Classify a failed delegation. `snapshot` (the sub-agent hook's last
/// pre-request history) is used only when the error carries no usable history —
/// rig builds `PromptCancelled` with an empty one in some paths.
pub(crate) fn classify(err: PromptError, snapshot: Vec<Message>) -> Handoff {
    match err {
        PromptError::PromptCancelled {
            chat_history,
            reason,
        } => {
            if reason == "stop" {
                return Handoff::Abort(PromptError::PromptCancelled {
                    chat_history,
                    reason,
                });
            }
            if reason == "subagent-context" {
                return Handoff::ContextExceeded {
                    history: pick(chat_history, snapshot),
                };
            }
            Handoff::Failed {
                error: format!("cancelled: {reason}"),
                history: pick(chat_history, snapshot),
            }
        }
        PromptError::MaxTurnsError {
            max_turns,
            chat_history,
            ..
        } => Handoff::Failed {
            error: format!("reached its max turn limit ({max_turns})"),
            history: pick(*chat_history, snapshot),
        },
        // Only reachable for a hookless (Ollama) sub-agent: everywhere else
        // `SessionHook::on_invalid_tool_call` skips the call with a synthetic
        // "unknown tool" result and the sub-agent self-corrects (#223).
        PromptError::UnknownToolCall {
            tool_name,
            chat_history,
            ..
        } => Handoff::Failed {
            error: format!("called an unknown tool `{tool_name}`"),
            history: pick(*chat_history, snapshot),
        },
        // These variants carry no history at all — the snapshot is all there is.
        // Never sniff the provider's error text; the class is the signal.
        // Transient ones were already retried in `DelegateTool::call`, so one
        // arriving here means the budget is spent or the failure is permanent.
        PromptError::CompletionError(e) => Handoff::Failed {
            error: cap_error(&e.to_string()),
            history: snapshot,
        },
        // rig hands a failing tool's message back to the model as a tool result
        // and keeps looping, so the agentic loop never raises this — kept for
        // totality over `PromptError`.
        PromptError::ToolError(e) => Handoff::Failed {
            error: cap_error(&e.to_string()),
            history: snapshot,
        },
        PromptError::ToolServerError(e) => Handoff::Failed {
            error: cap_error(&e.to_string()),
            history: snapshot,
        },
    }
}

/// The error's history if it has one, else the hook's snapshot.
fn pick(from_error: Vec<Message>, snapshot: Vec<Message>) -> Vec<Message> {
    if from_error.is_empty() {
        snapshot
    } else {
        from_error
    }
}

/// Idempotent: re-capping an already-capped string reproduces it exactly, so
/// `classify` and `format_result` can both apply it without stacking ellipses.
fn cap_error(s: &str) -> String {
    truncate_with_suffix(s, ERROR_CAP, "…")
}

/// Render the sub-agent's history as a bounded plain-text transcript for the
/// summariser. Never exceeds ~48 KiB.
fn render_transcript(history: &[Message]) -> String {
    // Tool results name only a call id; the label comes from the earlier call.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut blocks: Vec<String> = Vec::new();

    for msg in history {
        match msg {
            Message::System { .. } => {}
            Message::User { content } => {
                for c in content.iter() {
                    match c {
                        UserContent::Text(t) => {
                            blocks.push(format!(
                                "User: {}",
                                truncate_with_suffix(&t.text, USER_TEXT_CAP, "…")
                            ));
                        }
                        UserContent::ToolResult(tr) => {
                            let name = tr
                                .call_id
                                .as_ref()
                                .and_then(|id| tool_names.get(id))
                                .or_else(|| tool_names.get(&tr.id))
                                .map(String::as_str)
                                .unwrap_or("tool");
                            let text = tool_result_text(&tr.content);
                            blocks.push(format!(
                                "Tool [{name}] returned: {}",
                                truncate_with_suffix(&text, TOOL_RESULT_CAP, "…")
                            ));
                        }
                        UserContent::Image(_)
                        | UserContent::Audio(_)
                        | UserContent::Video(_)
                        | UserContent::Document(_) => {}
                    }
                }
            }
            Message::Assistant { content, .. } => {
                for c in content.iter() {
                    match c {
                        AssistantContent::Text(t) => {
                            blocks.push(format!(
                                "Assistant: {}",
                                truncate_with_suffix(&t.text, ASSISTANT_TEXT_CAP, "…")
                            ));
                        }
                        AssistantContent::ToolCall(tc) => {
                            let name = &tc.function.name;
                            tool_names.insert(tc.id.clone(), name.clone());
                            if let Some(call_id) = &tc.call_id {
                                tool_names.insert(call_id.clone(), name.clone());
                            }
                            let args = tc.function.arguments.to_string();
                            blocks.push(format!(
                                "Assistant called {name}({})",
                                truncate_with_suffix(&args, TOOL_ARGS_CAP, "…")
                            ));
                        }
                        AssistantContent::Reasoning(_) | AssistantContent::Image(_) => {}
                    }
                }
            }
        }
    }

    elide(&blocks)
}

/// Concatenate a tool result's text parts; image parts have nothing to say here.
fn tool_result_text(content: &rig_core::one_or_many::OneOrMany<ToolResultContent>) -> String {
    let parts: Vec<&str> = content
        .iter()
        .filter_map(|c| match c {
            ToolResultContent::Text(t) => Some(t.text.as_str()),
            ToolResultContent::Image(_) => None,
        })
        .collect();
    parts.join("\n")
}

/// Join blocks, dropping whole blocks from the middle when the transcript is
/// over budget. Blocks are never split — a half-rendered tool call reads as
/// corruption to the summariser.
fn elide(blocks: &[String]) -> String {
    let joined = blocks.join("\n");
    if joined.len() <= TRANSCRIPT_CAP {
        return joined;
    }

    let mut head_end = 0;
    let mut head_bytes = 0;
    for b in blocks {
        if head_bytes + b.len() > HEAD_CAP {
            break;
        }
        head_bytes += b.len() + 1;
        head_end += 1;
    }

    let mut tail_start = blocks.len();
    let mut tail_bytes = 0;
    while tail_start > head_end {
        let b = &blocks[tail_start - 1];
        if tail_bytes + b.len() > TAIL_CAP {
            break;
        }
        tail_bytes += b.len() + 1;
        tail_start -= 1;
    }

    let elided = tail_start - head_end;
    if elided == 0 {
        return joined;
    }

    let mut parts: Vec<String> = Vec::new();
    if head_end > 0 {
        parts.push(blocks[..head_end].join("\n"));
    }
    parts.push(format!("[... {elided} earlier steps elided ...]"));
    if tail_start < blocks.len() {
        parts.push(blocks[tail_start..].join("\n"));
    }
    parts.join("\n")
}

/// Whether the history holds anything worth summarising: assistant text or a
/// tool result. Whitespace-only assistant text is not work.
fn has_work(history: &[Message]) -> bool {
    history.iter().any(|m| match m {
        Message::System { .. } => false,
        Message::User { content } => content
            .iter()
            .any(|c| matches!(c, UserContent::ToolResult(_))),
        Message::Assistant { content, .. } => content.iter().any(|c| match c {
            AssistantContent::Text(t) => !t.text.trim().is_empty(),
            _ => false,
        }),
    })
}

/// The most recent non-empty assistant text block — the fallback when the
/// summariser is unavailable or unusable.
fn last_assistant_text(history: &[Message]) -> Option<String> {
    history.iter().rev().find_map(|m| match m {
        Message::Assistant { content, .. } => {
            // `OneOrMany`'s iterator is forward-only, so fold to the last hit.
            content.iter().fold(None, |acc, c| match c {
                AssistantContent::Text(t) if !t.text.trim().is_empty() => Some(t.text.clone()),
                _ => acc,
            })
        }
        _ => None,
    })
}

/// The wire format the orchestrator sees as the `delegate` tool result.
fn format_result(role: &str, header: Header, summary: Option<&str>) -> String {
    let (banner, guidance) = match &header {
        Header::ContextExceeded => (
            format!(
                "[delegate:{role}] INTERRUPTED — the subagent context exceeded its max threshold."
            ),
            CONTEXT_GUIDANCE,
        ),
        Header::Failed { error } => (
            format!(
                "[delegate:{role}] INTERRUPTED — error: {}",
                cap_error(error)
            ),
            FAILED_GUIDANCE,
        ),
    };

    let body = match summary {
        Some(s) => format!(
            "Here is a summary of what it was doing:\n\n{}",
            truncate_with_suffix(s, SUMMARY_CAP, "…")
        ),
        None => NO_SUMMARY.to_string(),
    };

    format!("{banner}\n\n{body}\n\n{guidance}")
}

/// Build the orchestrator-facing result for a failed delegation. Absorbing by
/// design: every internal failure degrades to a less informative result, never
/// to an error — a failed *summary* must not turn into a failed *turn*.
pub(crate) async fn build(role: &str, h: Handoff, cfg: &ProviderConfig) -> String {
    let (header, history) = match h {
        // The caller unwinds `Abort` as an error; degrade rather than panic if
        // one ever reaches here.
        Handoff::Abort(e) => (
            Header::Failed {
                error: e.to_string(),
            },
            Vec::new(),
        ),
        Handoff::ContextExceeded { history } => (Header::ContextExceeded, history),
        Handoff::Failed { error, history } => (Header::Failed { error }, history),
    };

    if history.is_empty() || !has_work(&history) {
        tracing::warn!(
            role,
            class = header.class(),
            summarised = false,
            "Sub-agent delegation interrupted with no summarisable work"
        );
        return format_result(role, header, None);
    }

    let transcript = render_transcript(&history);
    // The role's own model, not the global compaction alias — the alias may
    // point at a different provider that cannot serve this role's credentials.
    let summary = match create_compaction_model(cfg, None) {
        Ok(model) => match model
            .summarize(&format!("{SUMMARY_PROMPT}{transcript}"))
            .await
        {
            Ok(s) if !s.trim().is_empty() => Some(s),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(role, error = %e, "Sub-agent handoff summarisation failed");
                None
            }
        },
        Err(e) => {
            tracing::warn!(role, error = %e, "Could not build a summariser for the sub-agent handoff");
            None
        }
    };

    let summarised = summary.is_some();
    let text = summary.or_else(|| {
        last_assistant_text(&history).map(|t| truncate_with_suffix(&t, FALLBACK_CAP, "…"))
    });

    tracing::warn!(
        role,
        class = header.class(),
        summarised,
        "Sub-agent delegation interrupted; handing summary back to the orchestrator"
    );
    format_result(role, header, text.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::OneOrMany;
    use rig_core::completion::CompletionError;
    use rig_core::completion::message::{Text, ToolCall, ToolFunction, ToolResult};
    use rig_core::tool::ToolSetError;

    fn user_text(t: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: t.to_string(),
                additional_params: None,
            })),
        }
    }

    fn assistant_text(t: &str) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: t.to_string(),
                additional_params: None,
            })),
        }
    }

    fn assistant_call(id: &str, name: &str, args: serde_json::Value) -> Message {
        Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
                id: id.to_string(),
                call_id: None,
                function: ToolFunction {
                    name: name.to_string(),
                    arguments: args,
                },
                signature: None,
                additional_params: None,
            })),
        }
    }

    fn tool_result(id: &str, text: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                id: id.to_string(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::Text(Text {
                    text: text.to_string(),
                    additional_params: None,
                })),
            })),
        }
    }

    // ── classify ────────────────────────────────────────────────────────────

    /// `/stop` must stay an error so the whole turn unwinds — it is the one
    /// failure the orchestrator must not "recover" from.
    #[test]
    fn classify_stop_aborts() {
        let err = PromptError::PromptCancelled {
            chat_history: vec![assistant_text("work")],
            reason: "stop".to_string(),
        };
        match classify(err, vec![]) {
            Handoff::Abort(PromptError::PromptCancelled { reason, .. }) => {
                assert_eq!(reason, "stop");
            }
            _ => panic!("stop must classify as Abort"),
        }
    }

    /// The gate's own reason maps to the context header, carrying the history
    /// rig built for the cancellation.
    #[test]
    fn classify_subagent_context_is_context_exceeded() {
        let err = PromptError::PromptCancelled {
            chat_history: vec![assistant_text("did a thing")],
            reason: "subagent-context".to_string(),
        };
        match classify(err, vec![]) {
            Handoff::ContextExceeded { history } => assert_eq!(history.len(), 1),
            _ => panic!("subagent-context must classify as ContextExceeded"),
        }
    }

    /// Any other cancellation reason is a plain failure naming the reason.
    #[test]
    fn classify_other_cancel_is_failed_with_reason() {
        let err = PromptError::PromptCancelled {
            chat_history: vec![assistant_text("x")],
            reason: "compact".to_string(),
        };
        match classify(err, vec![]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "cancelled: compact");
                assert_eq!(history.len(), 1);
            }
            _ => panic!("unknown cancel reason must classify as Failed"),
        }
    }

    /// rig sometimes cancels with an EMPTY history — the hook's snapshot is the
    /// only thing standing between that and a content-free handoff.
    #[test]
    fn classify_empty_error_history_falls_back_to_snapshot() {
        let err = PromptError::PromptCancelled {
            chat_history: vec![],
            reason: "subagent-context".to_string(),
        };
        let snapshot = vec![user_text("task"), assistant_text("progress")];
        match classify(err, snapshot) {
            Handoff::ContextExceeded { history } => assert_eq!(history.len(), 2),
            _ => panic!("expected ContextExceeded"),
        }
    }

    #[test]
    fn classify_max_turns_is_failed_with_limit() {
        let err = PromptError::MaxTurnsError {
            max_turns: 42,
            chat_history: Box::new(vec![assistant_text("looping")]),
            prompt: Box::new(user_text("task")),
        };
        match classify(err, vec![]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "reached its max turn limit (42)");
                assert_eq!(history.len(), 1);
            }
            _ => panic!("MaxTurnsError must classify as Failed"),
        }
    }

    /// Residual path only: a hooked sub-agent recovers from a hallucinated
    /// tool name in-loop (`SessionHook::on_invalid_tool_call`), so this arm
    /// speaks for the hookless (Ollama) lane — where the delegation really is
    /// over and the orchestrator needs the summary.
    #[test]
    fn classify_unknown_tool_call_is_failed_with_tool_name() {
        let err = PromptError::UnknownToolCall {
            tool_name: "teleport".to_string(),
            available_tools: vec!["bash".to_string()],
            allowed_tools: vec!["bash".to_string()],
            chat_history: Box::new(vec![assistant_text("hm")]),
        };
        match classify(err, vec![]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "called an unknown tool `teleport`");
                assert_eq!(history.len(), 1);
            }
            _ => panic!("UnknownToolCall must classify as Failed"),
        }
    }

    /// `CompletionError` carries no history, so the snapshot is all there is —
    /// and its message is capped, never substring-sniffed. Reaching `classify`
    /// at all means `DelegateTool::call` already spent its retry budget (or
    /// the error was permanent); the class is the signal, so this arm doesn't
    /// re-judge transience.
    #[test]
    fn classify_completion_error_uses_snapshot_and_caps_message() {
        let long = "x".repeat(5000);
        let err = PromptError::CompletionError(CompletionError::ProviderError(long));
        match classify(err, vec![assistant_text("snapshotted")]) {
            Handoff::Failed { error, history } => {
                assert!(
                    error.len() <= ERROR_CAP + "…".len(),
                    "provider error must be capped, got {} bytes",
                    error.len()
                );
                assert!(error.ends_with('…'), "a cut error must be marked");
                assert_eq!(history.len(), 1);
            }
            _ => panic!("CompletionError must classify as Failed"),
        }
    }

    /// Defensive arm: rig returns a failing tool's message to the model as a
    /// tool result and keeps looping, so the agentic loop never raises this.
    /// Kept total anyway — if some future path does, the orchestrator gets a
    /// summary rather than a stringified error.
    #[test]
    fn classify_tool_error_uses_snapshot() {
        let err = PromptError::ToolError(ToolSetError::ToolNotFoundError("nope".to_string()));
        match classify(err, vec![assistant_text("snapshotted")]) {
            Handoff::Failed { error, history } => {
                assert!(error.contains("nope"));
                assert_eq!(history.len(), 1);
            }
            _ => panic!("ToolError must classify as Failed"),
        }
    }

    /// ToolServerError is its own match arm in `classify` — without a test,
    /// a typo or wrong variant in the arm is invisible. The message must come
    /// from `e.to_string()` (capped) and the history from the snapshot (these
    /// variants carry none of their own).
    #[test]
    fn classify_tool_server_error_uses_snapshot_and_caps_message() {
        let long = "s".repeat(800);
        let inner = rig_core::tool::server::ToolServerError::ToolsetError(
            ToolSetError::ToolNotFoundError(long.clone()),
        );
        let err = PromptError::ToolServerError(Box::new(inner));
        match classify(err, vec![assistant_text("snapshotted")]) {
            Handoff::Failed { error, history } => {
                assert!(
                    error.len() <= ERROR_CAP + "…".len(),
                    "tool server error must be capped, got {} bytes",
                    error.len()
                );
                assert!(error.ends_with('…'), "a cut error must be marked");
                assert_eq!(history.len(), 1, "snapshot is the only history");
            }
            _ => panic!("ToolServerError must classify as Failed"),
        }
    }

    /// The spec requires an empty `chat_history` to fall back to the snapshot
    /// *wherever the error's history is used* (table rows 2–5). Only
    /// `subagent-context` is covered for `PromptCancelled` so far — exercise
    /// the other three arms so a future regression can't drop the fallback
    /// from any of them without breaking a test.
    #[test]
    fn classify_empty_history_falls_back_for_other_prompt_cancelled_reasons() {
        let err = PromptError::PromptCancelled {
            chat_history: vec![],
            reason: "compact".to_string(),
        };
        match classify(err, vec![assistant_text("from snapshot")]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "cancelled: compact");
                assert_eq!(history.len(), 1, "snapshot must be used");
            }
            _ => panic!("empty chat_history + other reason must classify as Failed"),
        }
    }

    #[test]
    fn classify_empty_history_falls_back_for_max_turns() {
        let err = PromptError::MaxTurnsError {
            max_turns: 7,
            chat_history: Box::new(vec![]),
            prompt: Box::new(user_text("task")),
        };
        match classify(err, vec![assistant_text("from snapshot")]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "reached its max turn limit (7)");
                assert_eq!(history.len(), 1, "snapshot must be used");
            }
            _ => panic!("empty chat_history + MaxTurnsError must classify as Failed"),
        }
    }

    #[test]
    fn classify_empty_history_falls_back_for_unknown_tool_call() {
        let err = PromptError::UnknownToolCall {
            tool_name: "ghost".to_string(),
            available_tools: vec!["bash".to_string()],
            allowed_tools: vec!["bash".to_string()],
            chat_history: Box::new(vec![]),
        };
        match classify(err, vec![assistant_text("from snapshot")]) {
            Handoff::Failed { error, history } => {
                assert_eq!(error, "called an unknown tool `ghost`");
                assert_eq!(history.len(), 1, "snapshot must be used");
            }
            _ => panic!("empty chat_history + UnknownToolCall must classify as Failed"),
        }
    }

    /// Capping twice must not stack ellipses — `classify` and `format_result`
    /// both cap the same string.
    #[test]
    fn cap_error_is_idempotent() {
        let once = cap_error(&"é".repeat(400));
        assert_eq!(cap_error(&once), once);
    }

    // ── render_transcript ───────────────────────────────────────────────────

    /// A tool result is labelled with the *name* from the matching earlier
    /// call; an orphan result degrades to the generic label.
    #[test]
    fn render_transcript_labels_tool_results() {
        let history = vec![
            user_text("do the thing"),
            assistant_call("c1", "bash", serde_json::json!({"command": "ls"})),
            tool_result("c1", "a.rs\nb.rs"),
            tool_result("unmatched", "orphan"),
            assistant_text("found two files"),
        ];
        let out = render_transcript(&history);
        assert!(out.contains("User: do the thing"));
        assert!(out.contains(r#"Assistant called bash({"command":"ls"})"#));
        assert!(out.contains("Tool [bash] returned: a.rs\nb.rs"));
        assert!(out.contains("Tool [tool] returned: orphan"));
        assert!(out.contains("Assistant: found two files"));
    }

    /// Non-text blocks carry nothing the summariser can use and must not leak
    /// base64 payloads into the prompt.
    #[test]
    fn render_transcript_skips_system_and_non_text() {
        let history = vec![
            Message::System {
                content: "system preamble".to_string(),
            },
            assistant_text("visible"),
        ];
        let out = render_transcript(&history);
        assert!(!out.contains("system preamble"));
        assert_eq!(out, "Assistant: visible");
    }

    /// Each block type is capped independently so one huge tool result cannot
    /// crowd out the rest of the transcript.
    #[test]
    fn render_transcript_caps_each_block_type() {
        let history = vec![
            user_text(&"u".repeat(9000)),
            assistant_call(
                "c1",
                "bash",
                serde_json::json!({"command": "z".repeat(9000)}),
            ),
            tool_result("c1", &"t".repeat(9000)),
            assistant_text(&"a".repeat(9000)),
        ];
        let rendered = render_transcript(&history);
        let blocks: Vec<&str> = rendered.split('\n').collect();
        assert_eq!(blocks.len(), 4);
        assert!(blocks[0].len() <= "User: ".len() + USER_TEXT_CAP + 3);
        assert!(blocks[0].starts_with("User: uuu"));
        assert!(blocks[1].len() <= "Assistant called bash()".len() + TOOL_ARGS_CAP + 3);
        assert!(blocks[2].len() <= "Tool [bash] returned: ".len() + TOOL_RESULT_CAP + 3);
        assert!(blocks[3].len() <= "Assistant: ".len() + ASSISTANT_TEXT_CAP + 3);
    }

    /// Under the cap nothing is dropped and no elision marker appears.
    #[test]
    fn render_transcript_no_elision_when_small() {
        let history = vec![user_text("a"), assistant_text("b")];
        assert_eq!(render_transcript(&history), "User: a\nAssistant: b");
    }

    /// Over the cap: whole blocks survive at both ends, the middle is dropped,
    /// and the marker counts exactly the dropped blocks.
    #[test]
    fn render_transcript_elides_middle_keeping_head_and_tail() {
        // Each block renders as ~1 KiB, so ~100 KiB total — well over the cap.
        let mut history = vec![user_text(&"h".repeat(1000))];
        for _ in 0..99 {
            history.push(assistant_text(&"m".repeat(1000)));
        }
        history.push(assistant_text("FINAL BLOCK"));

        let out = render_transcript(&history);
        assert!(
            out.len() <= TRANSCRIPT_CAP,
            "elided transcript must fit the cap"
        );
        assert!(out.starts_with("User: hhh"), "the task brief must survive");
        assert!(
            out.ends_with("Assistant: FINAL BLOCK"),
            "the tail must survive"
        );

        let total = history.len();
        let kept = out
            .lines()
            .filter(|l| l.starts_with("User: ") || l.starts_with("Assistant: "))
            .count();
        let marker = format!("[... {} earlier steps elided ...]", total - kept);
        assert!(
            out.contains(&marker),
            "marker must count exactly the dropped blocks; expected {marker:?} in:\n{}",
            &out[..200.min(out.len())]
        );
        // Blocks are never split.
        for line in out.lines() {
            assert!(
                line.starts_with("User: ")
                    || line.starts_with("Assistant: ")
                    || line.starts_with("[... "),
                "unexpected partial block: {line:?}"
            );
        }
    }

    // ── has_work ────────────────────────────────────────────────────────────

    #[test]
    fn has_work_detects_assistant_text_and_tool_results() {
        assert!(has_work(&[assistant_text("something")]));
        assert!(has_work(&[tool_result("c1", "output")]));
    }

    #[test]
    fn has_work_false_without_progress() {
        assert!(!has_work(&[]));
        assert!(!has_work(&[user_text("just the task")]));
        assert!(
            !has_work(&[assistant_text("   \n ")]),
            "whitespace-only assistant text is not work"
        );
        assert!(!has_work(&[assistant_call(
            "c1",
            "bash",
            serde_json::json!({})
        )]));
    }

    // ── last_assistant_text ─────────────────────────────────────────────────

    #[test]
    fn last_assistant_text_picks_the_most_recent_non_empty() {
        let history = vec![
            assistant_text("first"),
            tool_result("c1", "out"),
            assistant_text("second"),
            assistant_text("  "),
        ];
        assert_eq!(last_assistant_text(&history).as_deref(), Some("second"));
    }

    #[test]
    fn last_assistant_text_none_without_assistant_text() {
        assert_eq!(last_assistant_text(&[]), None);
        assert_eq!(last_assistant_text(&[user_text("task")]), None);
    }

    // ── format_result ───────────────────────────────────────────────────────

    #[test]
    fn format_result_context_exceeded_with_summary() {
        let out = format_result("reviewer", Header::ContextExceeded, Some("Read 3 files."));
        assert_eq!(
            out,
            "[delegate:reviewer] INTERRUPTED — the subagent context exceeded its max threshold.\n\
             \n\
             Here is a summary of what it was doing:\n\
             \n\
             Read 3 files.\n\
             \n\
             [The task was NOT completed. Your call: re-delegate only the remaining slice, split it across several smaller delegations, pick a different role, or finish it yourself. Re-sending this same task will hit the same limit again.]"
        );
    }

    #[test]
    fn format_result_context_exceeded_without_summary() {
        let out = format_result("reviewer", Header::ContextExceeded, None);
        assert_eq!(
            out,
            "[delegate:reviewer] INTERRUPTED — the subagent context exceeded its max threshold.\n\
             \n\
             No summary of its work is recoverable.\n\
             \n\
             [The task was NOT completed. Your call: re-delegate only the remaining slice, split it across several smaller delegations, pick a different role, or finish it yourself. Re-sending this same task will hit the same limit again.]"
        );
    }

    #[test]
    fn format_result_failed_with_summary() {
        let out = format_result(
            "coder",
            Header::Failed {
                error: "reached its max turn limit (30)".to_string(),
            },
            Some("Edited src/main.rs."),
        );
        assert_eq!(
            out,
            "[delegate:coder] INTERRUPTED — error: reached its max turn limit (30)\n\
             \n\
             Here is a summary of what it was doing:\n\
             \n\
             Edited src/main.rs.\n\
             \n\
             [The task was NOT completed. Your call: if that error looks transient, re-delegate as-is; otherwise narrow the task, split it across several delegations, or pick a different role.]"
        );
    }

    #[test]
    fn format_result_failed_without_summary() {
        let out = format_result(
            "coder",
            Header::Failed {
                error: "ProviderError: 502".to_string(),
            },
            None,
        );
        assert_eq!(
            out,
            "[delegate:coder] INTERRUPTED — error: ProviderError: 502\n\
             \n\
             No summary of its work is recoverable.\n\
             \n\
             [The task was NOT completed. Your call: if that error looks transient, re-delegate as-is; otherwise narrow the task, split it across several delegations, or pick a different role.]"
        );
    }

    /// A runaway summary must not blow the orchestrator's context — the very
    /// thing this feature exists to protect.
    #[test]
    fn format_result_caps_summary_and_error() {
        let out = format_result(
            "coder",
            Header::Failed {
                error: "e".repeat(5000),
            },
            Some(&"s".repeat(20_000)),
        );
        assert!(
            out.len() < ERROR_CAP + SUMMARY_CAP + 1000,
            "both summary and error must be capped, got {} bytes",
            out.len()
        );
    }

    // ── build (delegate-timeout salvage contract) ──────────────────────────

    /// A delegation that exceeded its 30-min wall-clock budget must come
    /// back to the orchestrator as an `INTERRUPTED` handoff with a summary
    /// of what the sub-agent accomplished, NOT as an opaque "timed out".
    /// In the production incident this is the exact path that would have
    /// saved the 150 lost tool calls (see §6.4).
    ///
    /// The summariser is forced to fail (no API key) so `build` falls
    /// through to the `last_assistant_text` fallback; that fallback is the
    /// one path the test exercises — it is the worst case the orchestrator
    /// can rely on, and it is exactly what the postmortem needed.
    #[tokio::test]
    async fn timeout_handoff_is_rendered_as_an_interrupted_result() {
        use crate::config::{OpenRouterConfig, ProviderConfig};

        let history = vec![
            user_text("the original task brief"),
            assistant_text("in-progress work the sub-agent did — must survive"),
        ];

        let cfg = ProviderConfig::OpenRouter(OpenRouterConfig {
            api_key: None, // forces create_compaction_model → Err → fallback path
            model: "any/model".to_string(),
            max_tokens: 1024,
            vision: None,
        });

        let out = build(
            "reviewer",
            Handoff::Failed {
                error: "exceeded its 1800s wall-clock budget and was cancelled".to_string(),
                history,
            },
            &cfg,
        )
        .await;

        // The wire shape the orchestrator reads — pinned so a future edit to
        // the banner or guidance changes the test, not the model's vocabulary.
        assert!(
            out.starts_with(
                "[delegate:reviewer] INTERRUPTED — error: exceeded its 1800s wall-clock budget and was cancelled"
            ),
            "banner must name the role, the INTERRUPTED class, and the budget error verbatim; got:\n{out}"
        );
        // The fallback path surfaces the most recent assistant text so the
        // orchestrator knows what the dead sub-agent had actually done.
        assert!(
            out.contains("in-progress work the sub-agent did — must survive"),
            "fallback assistant text must appear in the rendered handoff; got:\n{out}"
        );
        // Standard failure guidance — same as every other non-stop failure.
        assert!(
            out.ends_with(FAILED_GUIDANCE),
            "rendered handoff must end with FAILED_GUIDANCE so the orchestrator knows how to react; got:\n{out}"
        );
    }
}
