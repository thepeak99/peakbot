//! Message rendering seam.
//!
//! `MessageRenderer` is the single extension point for how a
//! [`ChatMessage`] becomes a list of styled [`Line`]s ready to be shown in
//! the TUI. Today there is exactly one implementation — [`PlainRenderer`] —
//! which reproduces the historical `ReplUi::build_chat_message_lines`
//! behaviour verbatim (role-prefixed plain text, one line per `\n`).
//!
//! A future `MarkdownRenderer` can slot in here without touching
//! [`ChatRenderCache`](crate::ui::repl::render_cache::ChatRenderCache) or
//! [`ReplUi`](crate::ui::repl::ReplUi). See `slow-messages.md` §4.2.
//!
//! # Line ownership
//!
//! Rendered lines are owned (`Line<'static>`) so the cache can hold them
//! across frames. `PlainRenderer` allocates `String`s from the source
//! message; the cost is paid once, not 20×/second. Width-dependent
//! word-wrapping is handled downstream by the cache + ratatui; renderers
//! MUST NOT try to wrap.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::ui::app_state::{ChatMessage, MessageRole};

/// Convert a [`ChatMessage`] into styled [`Line`]s, width-independent.
pub trait MessageRenderer: Send + Sync {
    /// Render a single message. Implementations must return owned
    /// (`'static`) lines suitable for caching across frames.
    fn render(&self, msg: &ChatMessage) -> Vec<Line<'static>>;
}

/// The historical renderer: role prefix on the first line, remaining
/// `\n`-separated lines indented beneath. Bit-for-bit equivalent to the
/// previous `ReplUi::build_chat_message_lines` implementation.
pub struct PlainRenderer;

impl MessageRenderer for PlainRenderer {
    fn render(&self, msg: &ChatMessage) -> Vec<Line<'static>> {
        let (prefix, color) = match msg.role {
            MessageRole::User => ("👤 User", Color::LightGreen),
            MessageRole::Agent => ("🤖 Agent", Color::LightMagenta),
            MessageRole::System => ("⚙️ System", Color::LightYellow),
            MessageRole::ToolCall => ("🔧 Tool", Color::Cyan),
            MessageRole::ToolResult => ("📋 Result", Color::Blue),
            MessageRole::Summary => ("📝 Summary", Color::DarkGray),
        };

        let timestamp = msg.timestamp.format("%H:%M:%S").to_string();
        let content_lines: Vec<&str> = msg.content.split('\n').collect();

        let mut out = Vec::with_capacity(content_lines.len().max(1));
        for (i, content_line) in content_lines.iter().enumerate() {
            let line_content: Vec<Span<'static>> = if i == 0 {
                vec![
                    Span::raw("["),
                    Span::styled(
                        timestamp.clone(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw("] "),
                    Span::styled(prefix.to_string(), Style::default().fg(color)),
                    Span::raw(": "),
                    Span::raw(content_line.to_string()),
                ]
            } else {
                vec![Span::raw(content_line.to_string())]
            };
            out.push(Line::from(line_content));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_renderer_single_line_user_message() {
        let msg = ChatMessage::user("hello".to_string());
        let lines = PlainRenderer.render(&msg);
        assert_eq!(lines.len(), 1, "single-line content yields one Line");
    }

    #[test]
    fn plain_renderer_multiline_content_splits_on_newlines() {
        let msg = ChatMessage::agent("one\ntwo\nthree".to_string());
        let lines = PlainRenderer.render(&msg);
        assert_eq!(lines.len(), 3, "three content lines yield three Lines");
    }

    #[test]
    fn plain_renderer_empty_content_still_emits_one_line() {
        // `"".split('\n')` yields one empty segment — we keep that as the
        // header-bearing line so an empty message still shows its prefix.
        let msg = ChatMessage::user(String::new());
        let lines = PlainRenderer.render(&msg);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn plain_renderer_all_roles_render_without_panic() {
        for role in [
            MessageRole::User,
            MessageRole::Agent,
            MessageRole::System,
            MessageRole::ToolCall,
            MessageRole::ToolResult,
            MessageRole::Summary,
        ] {
            let mut msg = ChatMessage::user("body".to_string());
            msg.role = role;
            let _ = PlainRenderer.render(&msg);
        }
    }
}
