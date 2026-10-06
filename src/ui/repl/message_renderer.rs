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

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::attachments::{Attachment, AttachmentKind, human_size};
use crate::ui::app_state::{ChatMessage, MessageRole};
use crate::vision::media_type_from_mime;

/// Convert a [`ChatMessage`] into styled [`Line`]s.
///
/// `width` is the terminal width (in cells) the renderer can lay out
/// against. Most renderers ignore it (`PlainRenderer` does); width-
/// sensitive renderers (e.g. `MarkdownRenderer` for tables) consume
/// it to size their output to the available pane.
///
/// `width` is hashed into the cache fingerprint
/// ([`crate::ui::repl::render_cache::ChatRenderCache`]), so a resize
/// invalidates rendered Lines automatically — renderers MUST be
/// deterministic in `(msg, width)`.
pub trait MessageRenderer: Send + Sync {
    /// Render a single message at the given pane width. Implementations
    /// must return owned (`'static`) lines suitable for caching across
    /// frames.
    fn render(&self, msg: &ChatMessage, width: u16) -> Vec<Line<'static>>;
}

/// The historical renderer: role prefix on the first line, remaining
/// `\n`-separated lines indented beneath. Bit-for-bit equivalent to the
/// previous `ReplUi::build_chat_message_lines` implementation.
#[derive(Default)]
pub struct PlainRenderer;

/// Format one attachment as a single-line annotation: images as
/// `[image: cat.png · PNG · 1.2 KB]`, everything else as `📄 spec.pdf · 2.3 MB`.
///
/// Public-in-crate because the cache tests exercise it directly.
pub(crate) fn format_attachment_line(a: &Attachment) -> String {
    let size = human_size(a.size);
    match a.kind {
        AttachmentKind::Image => match media_type_from_mime(&a.mime) {
            Some(media_type) => format!("[image: {} · {:?} · {}]", a.name, media_type, size),
            // I4 makes this unreachable for stored images; a hand-edited
            // conversation file still renders rather than panics.
            None => format!("[image: {} · {}]", a.name, size),
        },
        AttachmentKind::File => format!("📄 {} · {}", a.name, size),
    }
}

impl MessageRenderer for PlainRenderer {
    fn render(&self, msg: &ChatMessage, _width: u16) -> Vec<Line<'static>> {
        use crate::ui::app_state::MessageSource;
        // Non-human lanes get a distinct prefix + colour so the user can tell
        // at a glance who produced a turn. Sub-agent turns key on `source`
        // (any role — Agent/ToolCall/ToolResult), not on the User role.
        let lane_override = match &msg.source {
            MessageSource::Background { .. } if msg.role == MessageRole::User => {
                Some(("🛰 Background".to_string(), Color::LightBlue))
            }
            MessageSource::SubAgent { role } => Some((format!("🧩 {role}"), Color::LightCyan)),
            _ => None,
        };
        let (prefix, color) = match lane_override {
            Some((p, c)) => (p, c),
            None => {
                let (p, c) = match msg.role {
                    MessageRole::User => ("👤 User", Color::LightGreen),
                    MessageRole::Agent => ("🤖 Agent", Color::LightMagenta),
                    // VS16 stripped from "⚙️" → "⚙" — the base symbol U+2699 alone
                    // matches what `unicode-width` reports (1 cell) AND what every
                    // terminal advances by, so the column drift is gone.
                    // See `garbled.md` Class A.
                    MessageRole::System => ("⚙ System", Color::LightYellow),
                    MessageRole::ToolCall => ("🔧 Tool", Color::Cyan),
                    MessageRole::ToolResult => ("📋 Result", Color::Blue),
                    MessageRole::Summary => ("📝 Summary", Color::DarkGray),
                };
                (p.to_string(), c)
            }
        };

        let timestamp = msg.timestamp.format("%H:%M:%S").to_string();
        let content_lines: Vec<&str> = msg.content.split('\n').collect();

        let mut out = Vec::with_capacity(content_lines.len().max(1) + msg.attachments.len());

        // Attachment preamble: one dim-cyan line per attached image, rendered
        // *before* the content. The first attachment line carries the
        // role prefix so users can see "who said it"; subsequent lines are
        // indented continuation style. If there are no attachments, the
        // behaviour is bit-identical to the historical renderer.
        let attach_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM);
        for (i, att) in msg.attachments.iter().enumerate() {
            let line = format_attachment_line(att);
            let spans: Vec<Span<'static>> = if i == 0 {
                vec![
                    Span::raw("["),
                    Span::styled(timestamp.clone(), Style::default().fg(Color::Gray)),
                    Span::raw("] "),
                    Span::styled(prefix.to_string(), Style::default().fg(color)),
                    Span::raw(": "),
                    Span::styled(line, attach_style),
                ]
            } else {
                vec![Span::styled(line, attach_style)]
            };
            out.push(Line::from(spans));
        }

        let attachments_present = !msg.attachments.is_empty();
        for (i, content_line) in content_lines.iter().enumerate() {
            // Normalise at the renderer boundary: strip VS16 and friends
            // from any text that flows in from outside our codebase
            // (LLM output, tool results, user input). See
            // `crate::ui::emoji_normalize` and `garbled.md`. Internal
            // role prefixes are normalised at the literal site, not here.
            let safe_content = crate::ui::emoji_normalize::normalize_for_terminal(content_line);
            let line_content: Vec<Span<'static>> = if i == 0 && !attachments_present {
                vec![
                    Span::raw("["),
                    Span::styled(timestamp.clone(), Style::default().fg(Color::Gray)),
                    Span::raw("] "),
                    Span::styled(prefix.to_string(), Style::default().fg(color)),
                    Span::raw(": "),
                    Span::raw(safe_content.into_owned()),
                ]
            } else {
                // Either:
                // - attachments already emitted the prefix line, so all
                //   content lines are continuation style; or
                // - this is the 2nd+ content line of a text-only message.
                vec![Span::raw(safe_content.into_owned())]
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
        let lines = PlainRenderer.render(&msg, 80);
        assert_eq!(lines.len(), 1, "single-line content yields one Line");
    }

    #[test]
    fn plain_renderer_multiline_content_splits_on_newlines() {
        let msg = ChatMessage::agent("one\ntwo\nthree".to_string());
        let lines = PlainRenderer.render(&msg, 80);
        assert_eq!(lines.len(), 3, "three content lines yield three Lines");
    }

    #[test]
    fn plain_renderer_empty_content_still_emits_one_line() {
        // `"".split('\n')` yields one empty segment — we keep that as the
        // header-bearing line so an empty message still shows its prefix.
        let msg = ChatMessage::user(String::new());
        let lines = PlainRenderer.render(&msg, 80);
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
            let _ = PlainRenderer.render(&msg, 80);
        }
    }

    // ── Attachment rendering (T5: `format_attachment_line(&Attachment)`) ──
    //
    // RED: `crate::attachments::{Attachment, AttachmentKind}` does not exist
    // yet, and `format_attachment_line` still takes the old
    // the old `vision::ImageAttachment`. These tests target the
    // locked design §7-T5 contract:
    //   Image → `[image: cat.png · PNG · 1.2 KB]`  (label = `media_type_from_mime` Debug)
    //   File  → `📄 spec.pdf · 2.3 MB`
    // with sizes from `attachments::human_size` (base 1024, 1 decimal).

    fn attachment(
        name: &str,
        mime: &str,
        size: u64,
        kind: crate::attachments::AttachmentKind,
    ) -> crate::attachments::Attachment {
        use crate::attachments::Attachment;
        Attachment {
            id: uuid::Uuid::new_v4(),
            convo: uuid::Uuid::new_v4(),
            name: name.to_string(),
            mime: mime.to_string(),
            size,
            kind,
        }
    }

    #[test]
    fn format_attachment_line_for_image() {
        let a = attachment(
            "cat.png",
            "image/png",
            1234,
            crate::attachments::AttachmentKind::Image,
        );
        assert_eq!(
            format_attachment_line(&a),
            "[image: cat.png · PNG · 1.2 KB]"
        );
    }

    #[test]
    fn format_attachment_line_for_file() {
        let a = attachment(
            "spec.pdf",
            "application/pdf",
            2411724,
            crate::attachments::AttachmentKind::File,
        );
        assert_eq!(format_attachment_line(&a), "📄 spec.pdf · 2.3 MB");
    }

    #[test]
    fn plain_renderer_prepends_attachment_line_before_content() {
        let msg = ChatMessage::user_with_attachments(
            "what's this?".to_string(),
            vec![attachment(
                "cat.png",
                "image/png",
                234 * 1024,
                crate::attachments::AttachmentKind::Image,
            )],
        );
        let lines = PlainRenderer.render(&msg, 80);
        assert_eq!(lines.len(), 2, "one attachment line + one content line");
        // First line has the role prefix AND the attachment tag.
        let first = format!("{:?}", lines[0]);
        assert!(
            first.contains("cat.png") && first.contains("User"),
            "first line should carry both role prefix and attachment: {first}"
        );
    }

    #[test]
    fn plain_renderer_emits_one_line_per_attachment() {
        let msg = ChatMessage::user_with_attachments(
            "two attachments".to_string(),
            vec![
                attachment(
                    "a.png",
                    "image/png",
                    100,
                    crate::attachments::AttachmentKind::Image,
                ),
                attachment(
                    "b.pdf",
                    "application/pdf",
                    200,
                    crate::attachments::AttachmentKind::File,
                ),
            ],
        );
        let lines = PlainRenderer.render(&msg, 80);
        assert_eq!(lines.len(), 3, "two attachment lines + one content line");
    }
}
