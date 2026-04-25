//! Per-message render cache for O(viewport) chat rendering.
//!
//! # Why
//!
//! The old render pipeline rebuilt every `Line`/`Span` from every
//! `ChatMessage` on every 50 ms tick and then asked ratatui to word-wrap
//! the whole transcript — three times per frame. At 500 messages that was
//! ~10 ms of pure word-wrap per frame, scaling linearly in history size.
//! See `slow-messages.md` for the measurements.
//!
//! # What
//!
//! `ChatRenderCache` memoises two things:
//!
//! 1. **Rendered lines** per message, `Arc<Vec<Line<'static>>>`, width-
//!    independent. Only rebuilt when a message's fingerprint changes.
//! 2. **Wrapped heights** per message at the current terminal width.
//!    Recomputed only when width or rendered lines change.
//!
//! A prefix-sum array over the per-message wrapped heights lets us map
//! a scroll offset to the containing message in `O(log N)` and emit only
//! the `Line`s that cover the viewport. Per-frame work becomes
//! proportional to viewport size, not transcript size.
//!
//! # Invariants
//!
//! - `fingerprints`, `rendered`, `wrapped_counts` are always the same
//!   length and index-aligned with the `ChatMessage` slice the cache was
//!   last synced against.
//! - `prefix_sums.len() == wrapped_counts.len() + 1`, with
//!   `prefix_sums[0] == 0` and `prefix_sums[i+1] == prefix_sums[i] +
//!   wrapped_counts[i] as u32`.
//! - `wrap_width == 0` iff the cache has never been synced.

use std::sync::Arc;

use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};

use crate::ui::app_state::{ChatMessage, MessageRole};
use crate::ui::repl::message_renderer::MessageRenderer;

/// Cheap equality check for detecting when a message has been edited or
/// streamed. Content hashing would be correct but wasteful on the hot
/// path; in practice messages are either pushed whole (user, tool) or
/// appended-to (streaming agent) — either way the byte length changes.
///
/// `compacted` is included because compaction changes what the renderer
/// outputs even when role + length don't.
///
/// `attachments_len` invalidates the cache when an image is added or
/// removed — the renderer emits one extra line per attachment, so row
/// counts change even if `content` doesn't.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    role: MessageRole,
    content_len: usize,
    compacted: bool,
    attachments_len: usize,
}

impl Fingerprint {
    fn of(msg: &ChatMessage) -> Self {
        Self {
            role: msg.role,
            content_len: msg.content.len(),
            compacted: msg.compacted,
            attachments_len: msg.attachments.len(),
        }
    }
}

/// Pre-rendered message lines. Wrapped in [`Arc`] so the cache can hand
/// the same vector to a ratatui [`Paragraph`] across frames without
/// copying the outer `Vec`.
struct RenderedMessage {
    lines: Arc<Vec<Line<'static>>>,
}

/// Memoised rendering state for a chat transcript.
pub struct ChatRenderCache {
    renderer: Box<dyn MessageRenderer>,

    // Per-message state (index-aligned with `ChatState::messages`).
    fingerprints: Vec<Fingerprint>,
    rendered: Vec<RenderedMessage>,
    wrapped_counts: Vec<u16>,

    // Width-scoped aggregates.
    wrap_width: u16,
    /// `prefix_sums[i]` = total wrapped lines of messages `[0..i]`.
    /// `prefix_sums[N]` = grand total. Invariant: length = N + 1.
    prefix_sums: Vec<u32>,
}

/// The subset of rendered lines that covers a given viewport, plus the
/// inner scroll offset to pass to [`Paragraph::scroll`].
pub struct WindowView {
    /// Concatenated lines covering the viewport, starting at message
    /// boundaries. The first message's full body is included even if only
    /// its tail is visible — `inner_scroll` crops the partial lead-in.
    pub lines: Vec<Line<'static>>,
    /// Number of wrapped lines to skip inside `lines` before the viewport
    /// starts. Always `< wrapped_counts[first_visible_message]`.
    pub inner_scroll: u16,
}

impl ChatRenderCache {
    pub fn new(renderer: Box<dyn MessageRenderer>) -> Self {
        Self {
            renderer,
            fingerprints: Vec::new(),
            rendered: Vec::new(),
            wrapped_counts: Vec::new(),
            wrap_width: 0,
            prefix_sums: vec![0],
        }
    }

    /// Reconcile the cache against the current chat messages at the given
    /// terminal width. Only mutated messages are re-rendered; only mutated
    /// (or width-invalidated) rows are re-wrapped. Returns `true` if
    /// anything changed in the cache.
    pub fn sync(&mut self, messages: &[ChatMessage], width: u16) -> bool {
        if width == 0 {
            // No usable width yet (e.g. before first frame). Nothing to do.
            return false;
        }

        let mut dirty = false;

        // 1) Width change: rendered Lines stay valid, wrapped counts do not.
        if width != self.wrap_width {
            self.wrap_width = width;
            // Reset all wrapped counts; they'll be recomputed in step 3.
            for c in &mut self.wrapped_counts {
                *c = 0;
            }
            dirty = true;
        }

        // 2) Truncation (e.g. after `/clear`).
        if messages.len() < self.rendered.len() {
            self.rendered.truncate(messages.len());
            self.fingerprints.truncate(messages.len());
            self.wrapped_counts.truncate(messages.len());
            dirty = true;
        }

        // 3) Walk messages, rendering any whose fingerprint changed or
        //    who don't exist in the cache yet. Appends are the common
        //    case (new user/agent message), so this is O(1) per frame
        //    in steady state.
        for (i, msg) in messages.iter().enumerate() {
            let fp = Fingerprint::of(msg);
            let needs_render = self.fingerprints.get(i).is_none_or(|old| *old != fp);

            if needs_render {
                let lines = self.renderer.render(msg);
                let rendered = RenderedMessage {
                    lines: Arc::new(lines),
                };
                if i < self.rendered.len() {
                    self.rendered[i] = rendered;
                    self.fingerprints[i] = fp;
                    self.wrapped_counts[i] = 0;
                } else {
                    self.rendered.push(rendered);
                    self.fingerprints.push(fp);
                    self.wrapped_counts.push(0);
                }
                dirty = true;
            }
        }

        // 4) Recompute wrapped counts for any row marked invalid (0).
        //    After a pure append this touches exactly one row.
        if dirty {
            for i in 0..self.rendered.len() {
                if self.wrapped_counts[i] == 0 {
                    self.wrapped_counts[i] = wrap_height(&self.rendered[i].lines, width);
                }
            }
            self.rebuild_prefix_sums();
        }

        dirty
    }

    fn rebuild_prefix_sums(&mut self) {
        self.prefix_sums.clear();
        self.prefix_sums.reserve(self.wrapped_counts.len() + 1);
        self.prefix_sums.push(0);
        let mut acc: u32 = 0;
        for &c in &self.wrapped_counts {
            acc = acc.saturating_add(c as u32);
            self.prefix_sums.push(acc);
        }
    }

    /// Total wrapped height across all messages at the current width.
    pub fn total_height(&self) -> u32 {
        *self.prefix_sums.last().unwrap_or(&0)
    }

    /// Are we synced against any messages yet? Used by callers to decide
    /// whether to render a welcome banner instead of an empty viewport.
    pub fn is_empty(&self) -> bool {
        self.rendered.is_empty()
    }

    /// Compute the lines needed to render the viewport
    /// `[scroll, scroll + viewport_h)`.
    ///
    /// Behaviour at the edges:
    /// - If `scroll` is past `total_height`, falls back to the last line
    ///   (mirrors `Paragraph::scroll`'s clamp-at-end behaviour).
    /// - If no messages are cached, returns an empty view.
    pub fn window(&self, scroll: u32, viewport_h: u16) -> WindowView {
        if self.rendered.is_empty() || viewport_h == 0 {
            return WindowView {
                lines: Vec::new(),
                inner_scroll: 0,
            };
        }

        let total = self.total_height();
        let scroll = scroll.min(total.saturating_sub(1));

        // Find the message that contains `scroll`. Message `i` covers the
        // half-open range `[prefix_sums[i], prefix_sums[i+1])`, so we want
        // the largest `i` such that `prefix_sums[i] <= scroll`.
        //
        // `partition_point(|&s| s <= scroll)` returns the first index where
        // the predicate flips to false — i.e. `prefix_sums[idx] > scroll`.
        // The containing message is therefore `idx - 1`.
        let first = self
            .prefix_sums
            .partition_point(|&s| s <= scroll)
            .saturating_sub(1);
        let first = first.min(self.rendered.len() - 1);

        let inner_scroll = (scroll - self.prefix_sums[first]) as u16;

        // Walk forward until we have enough wrapped lines to fill the
        // viewport (plus the inner_scroll we will clip off).
        let target = inner_scroll as u32 + viewport_h as u32;
        let mut collected: u32 = 0;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut i = first;
        while i < self.rendered.len() && collected < target {
            // Clone the pre-built Lines into this frame's vector.
            // The Spans inside use Cow-backed strings, so this is cheap.
            lines.extend(self.rendered[i].lines.iter().cloned());
            collected = collected.saturating_add(self.wrapped_counts[i] as u32);
            i += 1;
        }

        WindowView {
            lines,
            inner_scroll,
        }
    }
}

/// Compute the wrapped height of a block of `Line`s at `width` using
/// ratatui's own word-wrapper. Run per-message (small input), then cached
/// — so this never becomes the hot path.
fn wrap_height(lines: &[Line<'static>], width: u16) -> u16 {
    if width == 0 || lines.is_empty() {
        return lines.len() as u16;
    }
    let p = Paragraph::new(Text::from(lines.to_vec())).wrap(Wrap { trim: true });
    // `line_count` returns usize; cap at u16::MAX to keep the arithmetic
    // in our prefix-sum domain safe. Pathological messages > 65k lines
    // are a non-issue in practice.
    p.line_count(width).min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::ChatMessage;
    use crate::ui::repl::message_renderer::PlainRenderer;

    fn cache() -> ChatRenderCache {
        ChatRenderCache::new(Box::new(PlainRenderer))
    }

    #[test]
    fn fresh_cache_is_empty() {
        let c = cache();
        assert!(c.is_empty());
        assert_eq!(c.total_height(), 0);
    }

    #[test]
    fn sync_appends_populate_cache() {
        let mut c = cache();
        let msgs = vec![
            ChatMessage::user("hello".into()),
            ChatMessage::agent("world".into()),
        ];
        assert!(c.sync(&msgs, 80));
        assert!(!c.is_empty());
        assert!(c.total_height() > 0);
    }

    #[test]
    fn sync_with_no_change_is_idempotent() {
        let mut c = cache();
        let msgs = vec![ChatMessage::user("hi".into())];
        c.sync(&msgs, 80);
        let h1 = c.total_height();
        let changed = c.sync(&msgs, 80);
        assert!(!changed, "identical sync must report not-dirty");
        assert_eq!(c.total_height(), h1);
    }

    #[test]
    fn sync_truncation_shrinks_cache() {
        let mut c = cache();
        let msgs = vec![
            ChatMessage::user("a".into()),
            ChatMessage::user("b".into()),
            ChatMessage::user("c".into()),
        ];
        c.sync(&msgs, 80);
        let full = c.total_height();

        let shorter = msgs[..1].to_vec();
        assert!(c.sync(&shorter, 80));
        assert!(
            c.total_height() < full,
            "truncation must shrink cumulative height"
        );
    }

    #[test]
    fn sync_streaming_tail_updates_last_message_only() {
        let mut c = cache();
        let msgs = vec![
            ChatMessage::user("short".into()),
            ChatMessage::agent("in-progress".into()),
        ];
        c.sync(&msgs, 80);
        let h_before = c.total_height();

        // Simulate the streaming agent appending text: same index, longer content.
        let mut grown = msgs.clone();
        grown[1] = ChatMessage::agent("in-progress and now much much longer".into());
        assert!(c.sync(&grown, 80));
        assert!(c.total_height() >= h_before);
    }

    #[test]
    fn sync_width_change_preserves_message_count() {
        let mut c = cache();
        let msgs: Vec<ChatMessage> = (0..5)
            .map(|i| ChatMessage::agent(format!("msg {i} with enough words to wrap")))
            .collect();
        c.sync(&msgs, 40);
        let narrow_total = c.total_height();

        assert!(c.sync(&msgs, 120));
        let wide_total = c.total_height();

        // Wider terminal → fewer wrapped lines, same number of messages.
        assert!(
            wide_total < narrow_total,
            "wider width must yield fewer wrapped lines (narrow={narrow_total}, wide={wide_total})"
        );
    }

    #[test]
    fn window_scroll_zero_starts_at_top() {
        let mut c = cache();
        let msgs: Vec<ChatMessage> =
            (0..10).map(|i| ChatMessage::user(format!("m{i}"))).collect();
        c.sync(&msgs, 80);

        let view = c.window(0, 5);
        assert_eq!(view.inner_scroll, 0);
        assert!(!view.lines.is_empty());
    }

    #[test]
    fn window_scroll_past_end_clamps() {
        let mut c = cache();
        let msgs: Vec<ChatMessage> =
            (0..10).map(|i| ChatMessage::user(format!("m{i}"))).collect();
        c.sync(&msgs, 80);

        // Ask for a viewport beyond total height.
        let view = c.window(10_000, 5);
        assert!(!view.lines.is_empty(), "clamped scroll still returns lines");
    }

    #[test]
    fn window_inner_scroll_lands_inside_first_visible_message() {
        let mut c = cache();
        // One tall message followed by many short ones; scroll into the middle of the tall one.
        let big = "line\n".repeat(20).trim_end().to_string();
        let msgs = vec![
            ChatMessage::agent(big),
            ChatMessage::user("short".into()),
        ];
        c.sync(&msgs, 80);

        // Scroll to line 5 — should still be inside the first message.
        let view = c.window(5, 3);
        assert_eq!(view.inner_scroll, 5, "inner_scroll is offset into first visible msg");
        assert!(!view.lines.is_empty());
    }

    #[test]
    fn empty_cache_window_returns_empty() {
        let c = cache();
        let view = c.window(0, 10);
        assert!(view.lines.is_empty());
        assert_eq!(view.inner_scroll, 0);
    }

    #[test]
    fn zero_viewport_returns_empty() {
        let mut c = cache();
        c.sync(&[ChatMessage::user("x".into())], 80);
        let view = c.window(0, 0);
        assert!(view.lines.is_empty());
    }

    #[test]
    fn zero_width_sync_is_noop() {
        let mut c = cache();
        let msgs = vec![ChatMessage::user("hi".into())];
        assert!(
            !c.sync(&msgs, 0),
            "zero-width sync must not dirty (nothing to wrap)"
        );
        assert!(c.is_empty(), "zero-width sync must not populate cache");
    }

    #[test]
    fn prefix_sums_match_wrapped_counts() {
        let mut c = cache();
        let msgs: Vec<ChatMessage> = (0..8)
            .map(|i| ChatMessage::agent(format!("msg {i}")))
            .collect();
        c.sync(&msgs, 80);

        // Manually verify prefix_sums invariant.
        let mut expected: u32 = 0;
        assert_eq!(c.prefix_sums[0], 0);
        for (i, &count) in c.wrapped_counts.iter().enumerate() {
            expected += count as u32;
            assert_eq!(
                c.prefix_sums[i + 1], expected,
                "prefix_sums[{}] must equal cumulative wrapped_counts",
                i + 1
            );
        }
    }

    #[test]
    fn total_height_agrees_with_naive_paragraph_line_count() {
        // Property test: the cache's total_height must match what a
        // naïve `Paragraph::new(all_lines).line_count(w)` would report,
        // otherwise the scrollbar and viewport windowing drift apart.
        let mut c = cache();
        let msgs: Vec<ChatMessage> = (0..20)
            .map(|i| ChatMessage::agent(format!(
                "Message #{i} — lorem ipsum dolor sit amet consectetur adipiscing elit"
            )))
            .collect();
        let width = 50u16;
        c.sync(&msgs, width);

        // Build the naïve paragraph the old path would have produced.
        let mut all_lines: Vec<Line<'static>> = Vec::new();
        for msg in &msgs {
            all_lines.extend(PlainRenderer.render(msg));
        }
        let naive = Paragraph::new(Text::from(all_lines)).wrap(Wrap { trim: true });
        let naive_height = naive.line_count(width) as u32;

        // The cache sums per-message wrapped heights; ratatui's line_count
        // wraps the whole paragraph at once. For `Wrap { trim: true }` and
        // our simple inputs these must agree — otherwise the scrollbar
        // lies about content size.
        assert_eq!(
            c.total_height(),
            naive_height,
            "cache total must match naïve paragraph wrap"
        );
    }

    #[test]
    fn fingerprint_changes_when_attachments_are_added() {
        use crate::vision::{ImageAttachment, ImageSource};
        use rig::completion::message::ImageMediaType;

        let text_only = ChatMessage::user("same text".to_string());
        let with_image = ChatMessage::user_with_attachments(
            "same text".to_string(),
            vec![ImageAttachment {
                display_name: "cat.png".into(),
                source: ImageSource::Base64 {
                    bytes: vec![1, 2, 3],
                    media_type: ImageMediaType::PNG,
                },
                detail: None,
            }],
        );

        // Content bytes are identical, but the attachment flips the
        // fingerprint — without this, adding an image wouldn't invalidate
        // the cache and the `[image: …]` line would never appear.
        assert!(
            Fingerprint::of(&text_only) != Fingerprint::of(&with_image),
            "adding an attachment must change the fingerprint"
        );
    }
}
