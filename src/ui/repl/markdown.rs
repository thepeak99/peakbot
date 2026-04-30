//! Markdown rendering for chat messages.
//!
//! [`MarkdownRenderer`] is the width-aware [`MessageRenderer`] used for
//! agent replies in the live REPL. Non-agent roles fall back to
//! [`PlainRenderer`] verbatim (user input, tool output, system banners
//! must round-trip without parsing — see `markdown-render.md` § Scope).
//!
//! # Header line
//!
//! For agent messages, the `[HH:MM:SS] 🤖 Agent:` prefix gets its **own**
//! `Line`. The plain renderer glues it onto the first content line, but
//! markdown bodies can start with a block-level element (heading, code
//! block, table) that can't share a row with a prefix span.
//!
//! # Width
//!
//! Width is consumed by:
//! - **Tables** — column widths shrink to fit; cells wrap with proper
//!   border continuation.
//! - **Fenced code rules** — top/bottom rules extend to the pane edge.
//!
//! Headers, emphasis, inline code, and paragraph text are width-oblivious.
//!
//! # Streaming agent messages
//!
//! Half-streamed input (e.g. `**bo` mid-stream) parses as literal text.
//! That's fine — pulldown-cmark is forgiving, and the cache will re-parse
//! once the closing `**` arrives because `content_len` flips the
//! fingerprint.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::app_state::{ChatMessage, MessageRole};
use crate::ui::repl::message_renderer::{MessageRenderer, PlainRenderer};

/// Display width of a string in terminal cells.
///
/// `.chars().count()` returns the number of Unicode scalar values, which
/// is wrong for *layout* (emojis are usually 2 cells, CJK is 2, combining
/// marks are 0, ZWJ sequences collapse multiple code points into one
/// glyph). Use this everywhere column widths or padding are computed —
/// ratatui itself uses the same crate to lay out spans, so our maths
/// agrees with what actually gets drawn.
fn cells(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a single character in cells.
fn cell_width_of_char(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Renders agent messages as styled markdown; everything else falls
/// through to [`PlainRenderer`].
#[derive(Default)]
pub struct MarkdownRenderer {
    fallback: PlainRenderer,
}

impl MessageRenderer for MarkdownRenderer {
    fn render(&self, msg: &ChatMessage, width: u16) -> Vec<Line<'static>> {
        if msg.role != MessageRole::Agent {
            return self.fallback.render(msg, width);
        }
        render_agent_markdown(msg, width)
    }
}

// ─── Style palette ────────────────────────────────────────────────────

fn heading_style(level: HeadingLevel) -> Style {
    let color = match level {
        HeadingLevel::H1 => Color::LightCyan,
        HeadingLevel::H2 => Color::LightMagenta,
        HeadingLevel::H3 => Color::LightYellow,
        _ => Color::Gray,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn inline_code_style() -> Style {
    Style::default()
        .fg(Color::LightYellow)
        .add_modifier(Modifier::DIM)
}

fn code_block_style() -> Style {
    Style::default().fg(Color::LightYellow)
}

fn code_rule_style() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
}

fn table_border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

// ─── Agent prefix line ────────────────────────────────────────────────

/// Build the timestamp + role prefix as a standalone `Line`. Matches the
/// PlainRenderer palette so the visual change is "the prefix moved to its
/// own row", nothing more.
fn agent_prefix_line(msg: &ChatMessage) -> Line<'static> {
    let timestamp = msg.timestamp.format("%H:%M:%S").to_string();
    Line::from(vec![
        Span::raw("["),
        Span::styled(timestamp, Style::default().fg(Color::Gray)),
        Span::raw("] "),
        Span::styled(
            "🤖 Agent".to_string(),
            Style::default().fg(Color::LightMagenta),
        ),
        Span::raw(":"),
    ])
}

// ─── Top-level entry ──────────────────────────────────────────────────

fn render_agent_markdown(msg: &ChatMessage, width: u16) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(agent_prefix_line(msg));

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(&msg.content, opts);
    let mut state = MarkdownState::new(width);
    for ev in parser {
        state.handle(ev);
    }
    state.finish_into(&mut out);
    out
}

// ─── State machine ────────────────────────────────────────────────────

/// What kind of block we're currently emitting.
enum Mode {
    Body,
    /// Inside a fenced or indented code block.
    CodeBlock,
    /// Collecting cells for a pending table. Flushed on `End(Table)`.
    Table {
        rows: Vec<Vec<String>>,
        alignments: Vec<Alignment>,
        in_header: bool,
        current_cell: String,
    },
}

struct MarkdownState {
    width: u16,
    lines: Vec<Line<'static>>,
    /// Spans accumulated for the in-progress line.
    current_spans: Vec<Span<'static>>,
    /// Style modifiers stacked by `Start(Strong | Emphasis)` events.
    /// Effective style = base ⊕ each frame's mods. Headings push a
    /// fully-styled frame instead.
    style_stack: Vec<Style>,
    mode: Mode,
}

impl MarkdownState {
    fn new(width: u16) -> Self {
        Self {
            width,
            lines: Vec::new(),
            current_spans: Vec::new(),
            style_stack: Vec::new(),
            mode: Mode::Body,
        }
    }

    /// Compose the active style by folding the stack from the bottom up.
    /// Last frame wins on conflicting fg colours; modifiers OR together.
    fn current_style(&self) -> Style {
        let mut s = Style::default();
        for frame in &self.style_stack {
            if let Some(fg) = frame.fg {
                s = s.fg(fg);
            }
            s = s.add_modifier(frame.add_modifier);
        }
        s
    }

    /// End the current line, push it to output, and start a fresh empty
    /// span buffer.
    fn finish_line(&mut self) {
        let spans = std::mem::take(&mut self.current_spans);
        self.lines.push(Line::from(spans));
    }

    /// Push a styled span onto the in-progress line.
    fn push_span(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        self.current_spans.push(Span::styled(text, style));
    }

    fn push_text(&mut self, text: &str) {
        let style = self.current_style();
        self.push_span(text.to_string(), style);
    }

    /// Force a paragraph break: end the current line if non-empty, then
    /// emit a blank separator line.
    fn paragraph_break(&mut self) {
        if !self.current_spans.is_empty() {
            self.finish_line();
        }
        // Avoid stacking blanks if the last emitted line is already empty.
        if !matches!(self.lines.last(), Some(line) if line.spans.is_empty()) {
            self.lines.push(Line::from(""));
        }
    }

    /// Flush any in-progress line and any leftover state into `out`.
    fn finish_into(mut self, out: &mut Vec<Line<'static>>) {
        if !self.current_spans.is_empty() {
            self.finish_line();
        }
        // Trim a single trailing blank line if present (paragraphs always
        // emit one, but the message itself shouldn't end with empty space).
        if matches!(self.lines.last(), Some(l) if l.spans.is_empty()) {
            self.lines.pop();
        }
        out.extend(self.lines);
    }

    fn handle(&mut self, ev: Event<'_>) {
        // Table mode hijacks most events — handle it first to keep the
        // body branch readable.
        if let Mode::Table { .. } = &self.mode {
            self.handle_table_event(ev);
            return;
        }
        if matches!(self.mode, Mode::CodeBlock) {
            self.handle_codeblock_event(ev);
            return;
        }
        self.handle_body_event(ev);
    }

    fn handle_body_event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Text(t) => self.push_text(&t),
            Event::Code(t) => {
                let style = inline_code_style();
                // Wrap inline code with thin spaces so it's visually
                // distinct even without a background colour.
                self.push_span(format!(" {t} "), style);
            }
            Event::SoftBreak | Event::HardBreak => self.finish_line(),
            Event::Rule => {
                if !self.current_spans.is_empty() {
                    self.finish_line();
                }
                let rule_w = self.width.max(4) as usize;
                let rule = "─".repeat(rule_w);
                self.lines
                    .push(Line::from(Span::styled(rule, table_border_style())));
            }
            // HTML, footnotes, math etc. — render the raw text so users
            // at least see what the model produced.
            Event::Html(t) | Event::InlineHtml(t) => self.push_text(&t),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
            Event::InlineMath(t) | Event::DisplayMath(t) => self.push_text(&t),
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => { /* lines accumulate naturally */ }
            Tag::Heading { level, .. } => {
                if !self.current_spans.is_empty() {
                    self.finish_line();
                }
                self.style_stack.push(heading_style(level));
            }
            Tag::Strong => self
                .style_stack
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Emphasis => self
                .style_stack
                .push(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strikethrough => self
                .style_stack
                .push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::CodeBlock(kind) => {
                if !self.current_spans.is_empty() {
                    self.finish_line();
                }
                let lang = match kind {
                    CodeBlockKind::Fenced(s) if !s.is_empty() => format!(" {s} "),
                    _ => String::new(),
                };
                self.lines.push(self.code_rule_top(&lang));
                self.mode = Mode::CodeBlock;
            }
            Tag::Table(alignments) => {
                self.mode = Mode::Table {
                    rows: Vec::new(),
                    alignments,
                    in_header: false,
                    current_cell: String::new(),
                };
            }
            Tag::Link { .. } => self
                .style_stack
                .push(Style::default().add_modifier(Modifier::UNDERLINED)),
            // Tags we deliberately don't style in v1 (lists, blockquotes,
            // images, etc.). Their inner Text events still fire and become
            // plain spans, so users see the content rather than nothing.
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.paragraph_break(),
            TagEnd::Heading(_) => {
                if !self.current_spans.is_empty() {
                    self.finish_line();
                }
                self.style_stack.pop();
                // Blank line after headings for visual breathing room.
                self.lines.push(Line::from(""));
            }
            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => {
                self.style_stack.pop();
            }
            // CodeBlock / Table ends are handled in their dedicated
            // handlers (we're never in body-mode when those fire).
            _ => {}
        }
    }

    // ─── Code blocks ──────────────────────────────────────────────────

    fn handle_codeblock_event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Text(t) => {
                // pulldown-cmark may deliver the whole block as one Text
                // or as multiple chunks. Either way, split on '\n' and
                // emit one Line per code line, styled uniformly.
                let style = code_block_style();
                let mut chunks = t.split('\n').peekable();
                while let Some(seg) = chunks.next() {
                    if !seg.is_empty() {
                        self.current_spans
                            .push(Span::styled(seg.to_string(), style));
                    }
                    // Every '\n' boundary closes the current line. The
                    // trailing-newline chunk produces an empty seg + no
                    // more iterations, which finalises the last code line.
                    if chunks.peek().is_some() {
                        self.finish_line();
                    }
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if !self.current_spans.is_empty() {
                    self.finish_line();
                }
                self.lines.push(self.code_rule_bottom());
                self.mode = Mode::Body;
            }
            // Other events inside a code block shouldn't really happen,
            // but ignore them defensively.
            _ => {}
        }
    }

    fn code_rule_top(&self, lang: &str) -> Line<'static> {
        let total = self.width.max(4) as usize;
        // ┌─ rust ─────…─┐
        let lang_width = cells(lang);
        // 2 corners + min 2 dashes + lang + at least 1 dash on each side
        let dashes_remaining = total
            .saturating_sub(2) // corners
            .saturating_sub(lang_width)
            .saturating_sub(2); // one dash on each side of lang
        let left_dashes = "─".repeat(2);
        let right_dashes = "─".repeat(dashes_remaining);
        let rule = format!("┌{left_dashes}{lang}{right_dashes}┐");
        Line::from(Span::styled(rule, code_rule_style()))
    }

    fn code_rule_bottom(&self) -> Line<'static> {
        let total = self.width.max(4) as usize;
        let dashes = "─".repeat(total.saturating_sub(2));
        let rule = format!("└{dashes}┘");
        Line::from(Span::styled(rule, code_rule_style()))
    }

    // ─── Tables ───────────────────────────────────────────────────────

    fn handle_table_event(&mut self, ev: Event<'_>) {
        let Mode::Table {
            rows,
            in_header,
            current_cell,
            ..
        } = &mut self.mode
        else {
            unreachable!("handle_table_event called outside Table mode");
        };
        match ev {
            Event::Start(Tag::TableHead) => {
                *in_header = true;
                rows.push(Vec::new());
            }
            Event::Start(Tag::TableRow) => {
                rows.push(Vec::new());
            }
            Event::Start(Tag::TableCell) => {
                current_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                let cell = std::mem::take(current_cell);
                if let Some(row) = rows.last_mut() {
                    row.push(cell);
                }
            }
            Event::End(TagEnd::TableHead) => {
                *in_header = false;
            }
            Event::End(TagEnd::TableRow) => { /* row already in rows */ }
            Event::End(TagEnd::Table) => {
                let (rows_owned, alignments) = match std::mem::replace(&mut self.mode, Mode::Body) {
                    Mode::Table {
                        rows, alignments, ..
                    } => (rows, alignments),
                    _ => unreachable!(),
                };
                self.emit_table(rows_owned, alignments);
            }
            // Inline events inside a cell append to the cell buffer. We
            // intentionally drop styling for table cells in v1 (cells are
            // plain text), so bold/italic/links flatten to their literal
            // text. Styling cells would make column-width math depend on
            // span composition, which is more complexity than the v1 spec
            // calls for.
            Event::Text(t) => current_cell.push_str(&t),
            Event::Code(t) => current_cell.push_str(&t),
            Event::SoftBreak | Event::HardBreak => current_cell.push(' '),
            _ => {}
        }
    }

    fn emit_table(&mut self, rows: Vec<Vec<String>>, alignments: Vec<Alignment>) {
        if rows.is_empty() {
            return;
        }
        let n_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if n_cols == 0 {
            return;
        }
        if !self.current_spans.is_empty() {
            self.finish_line();
        }

        // Pad short rows so column indexing is uniform.
        let mut rows = rows;
        for row in &mut rows {
            while row.len() < n_cols {
                row.push(String::new());
            }
        }

        // Step 1: natural column widths (max display cells per column).
        // Using cells, not code-point count, so emojis (2 cells) and
        // CJK (2 cells) reserve enough room for the right border.
        let natural: Vec<usize> = (0..n_cols)
            .map(|c| rows.iter().map(|r| cells(&r[c])).max().unwrap_or(0))
            .collect();

        // Step 2: shrink to fit the available pane.
        // Layout: │ c0 │ c1 │ … │  →  borders = n_cols + 1, padding = 2 per col.
        // Total used = sum(col_w) + 2*n_cols (padding) + n_cols+1 (borders).
        let chrome = 2 * n_cols + (n_cols + 1);
        let avail = (self.width as usize).saturating_sub(chrome);
        let col_widths = shrink_widths(&natural, avail);

        // Step 3: emit the box.
        let aligns = pad_alignments(alignments, n_cols);
        let border = table_border_style();

        self.lines
            .push(table_rule_line(&col_widths, "┌", "┬", "┐", border));

        // Header (first row) and separator.
        if let Some(header) = rows.first() {
            for line in render_table_row(header, &col_widths, &aligns, true, border) {
                self.lines.push(line);
            }
            self.lines
                .push(table_rule_line(&col_widths, "├", "┼", "┤", border));
        }

        // Body rows.
        for row in rows.iter().skip(1) {
            for line in render_table_row(row, &col_widths, &aligns, false, border) {
                self.lines.push(line);
            }
        }

        self.lines
            .push(table_rule_line(&col_widths, "└", "┴", "┘", border));
    }
}

// ─── Free-standing helpers (table rendering) ─────────────────────────

/// Pad `alignments` to `n_cols` with `Alignment::None`; defensive against
/// pulldown-cmark giving us fewer entries than the data calls for.
fn pad_alignments(mut a: Vec<Alignment>, n_cols: usize) -> Vec<Alignment> {
    while a.len() < n_cols {
        a.push(Alignment::None);
    }
    a
}

/// Compute final per-column widths that fit within `avail` cells, keeping
/// columns at their natural width when possible and shrinking the
/// widest first when not.
fn shrink_widths(natural: &[usize], avail: usize) -> Vec<usize> {
    let total: usize = natural.iter().sum();
    if total <= avail || avail == 0 {
        // Either fits, or terminal so narrow we can't make sensible
        // choices — return naturals and let ratatui's wrap handle it.
        return natural.to_vec();
    }
    let mut widths = natural.to_vec();
    // Greedy: shrink the widest column by 1 until total fits, with a
    // minimum width of 3 (room for "x…"). If everything is at minimum
    // and we still don't fit, give up — the table will overflow but the
    // layout stays consistent.
    let min_w = 3usize;
    loop {
        let cur: usize = widths.iter().sum();
        if cur <= avail {
            break;
        }
        let Some((idx, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > min_w)
            .max_by_key(|(_, w)| **w)
        else {
            break; // all at minimum
        };
        widths[idx] -= 1;
    }
    widths
}

/// Pad/clip `text` to exactly `width` *display cells*, applying alignment.
/// All maths is in cells, not code points, so emojis and CJK don't shove
/// the right border one column off.
fn align_cell(text: &str, width: usize, align: Alignment) -> String {
    let cur = cells(text);
    if cur == width {
        return text.to_string();
    }
    if cur < width {
        let pad = width - cur;
        return match align {
            Alignment::Right => format!("{}{}", " ".repeat(pad), text),
            Alignment::Center => {
                let l = pad / 2;
                let r = pad - l;
                format!("{}{}{}", " ".repeat(l), text, " ".repeat(r))
            }
            Alignment::Left | Alignment::None => format!("{}{}", text, " ".repeat(pad)),
        };
    }
    // Overflow: walk chars summing display cells until we'd exceed
    // `width - 1`, then append `…` (1 cell). May land short by one cell
    // if the rejected char is double-width — pad with a trailing space
    // so the total is exactly `width` cells.
    if width >= 1 {
        let mut acc = String::new();
        let mut acc_w = 0usize;
        let limit = width - 1;
        for ch in text.chars() {
            let cw = cell_width_of_char(ch);
            if acc_w + cw > limit {
                break;
            }
            acc.push(ch);
            acc_w += cw;
        }
        let pad = width.saturating_sub(acc_w + 1);
        format!("{acc}…{}", " ".repeat(pad))
    } else {
        String::new()
    }
}

/// Wrap `text` into chunks at most `width` *display cells* wide. A
/// rudimentary word-wrap; falls back to char-wrap when a single word is
/// wider than the column. All accounting is cell-based so wide chars
/// don't burst out of their column.
fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;

    for word in text.split_whitespace() {
        let wlen = cells(word);
        if wlen > width {
            // Flush whatever we have, then char-wrap the long word in cells.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            let mut buf = String::new();
            let mut buf_w = 0usize;
            for ch in word.chars() {
                let cw = cell_width_of_char(ch);
                if buf_w + cw > width {
                    lines.push(std::mem::take(&mut buf));
                    buf_w = 0;
                }
                buf.push(ch);
                buf_w += cw;
            }
            if !buf.is_empty() {
                cur = buf;
                cur_w = buf_w;
            }
            continue;
        }
        let need = if cur.is_empty() {
            wlen
        } else {
            cur_w + 1 + wlen
        };
        if need > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_w = wlen;
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += wlen;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Render a single logical table row, possibly wrapping cell contents
/// across multiple visual rows so the box stays aligned.
fn render_table_row(
    row: &[String],
    col_widths: &[usize],
    aligns: &[Alignment],
    is_header: bool,
    border_style: Style,
) -> Vec<Line<'static>> {
    let n = col_widths.len();
    // Wrap each cell to its column width.
    let wrapped: Vec<Vec<String>> = row
        .iter()
        .zip(col_widths.iter())
        .map(|(text, w)| wrap_cell(text, *w))
        .collect();
    let max_h = wrapped.iter().map(|c| c.len()).max().unwrap_or(1);

    let cell_style = if is_header {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(max_h);
    for vis_row in 0..max_h {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(2 * n + 1);
        spans.push(Span::styled("│".to_string(), border_style));
        for c in 0..n {
            let cell_text = wrapped
                .get(c)
                .and_then(|cell_lines| cell_lines.get(vis_row))
                .cloned()
                .unwrap_or_default();
            let aligned = align_cell(&cell_text, col_widths[c], aligns[c]);
            spans.push(Span::styled(format!(" {aligned} "), cell_style));
            spans.push(Span::styled("│".to_string(), border_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Render one of the `┌─┬─┐ / ├─┼─┤ / └─┴─┘` rule lines.
fn table_rule_line(
    col_widths: &[usize],
    left: &str,
    mid: &str,
    right: &str,
    style: Style,
) -> Line<'static> {
    let mut s = String::new();
    s.push_str(left);
    for (i, w) in col_widths.iter().enumerate() {
        // 2 padding cells on each side of content
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < col_widths.len() {
            s.push_str(mid);
        }
    }
    s.push_str(right);
    Line::from(Span::styled(s, style))
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::ChatMessage;

    fn render(content: &str, width: u16) -> Vec<Line<'static>> {
        let msg = ChatMessage::agent(content.to_string());
        MarkdownRenderer::default().render(&msg, width)
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn render_to_text(content: &str, width: u16) -> String {
        render(content, width)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ─── Role gating ──────────────────────────────────────────────────

    #[test]
    fn non_agent_role_falls_back_to_plain() {
        let mr = MarkdownRenderer::default();
        let msg = ChatMessage::user("**not bold**".to_string());
        let md_lines = mr.render(&msg, 80);
        let plain_lines = PlainRenderer.render(&msg, 80);
        // Agent-only parsing — user text must round-trip literally.
        assert_eq!(md_lines.len(), plain_lines.len());
        let md_text: String = md_lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            md_text.contains("**not bold**"),
            "user content must round-trip literally: {md_text}"
        );
    }

    #[test]
    fn tool_result_role_falls_back_to_plain() {
        let mr = MarkdownRenderer::default();
        let mut msg = ChatMessage::agent("output | with | pipes".to_string());
        msg.role = MessageRole::ToolResult;
        let md_lines = mr.render(&msg, 80);
        let plain_lines = PlainRenderer.render(&msg, 80);
        assert_eq!(md_lines.len(), plain_lines.len());
    }

    // ─── Prefix line ──────────────────────────────────────────────────

    #[test]
    fn agent_message_emits_prefix_on_its_own_line() {
        let lines = render("hello", 80);
        // Line 0 should be the prefix; line 1+ the body.
        assert!(line_text(&lines[0]).contains("Agent"));
        assert!(!line_text(&lines[0]).contains("hello"));
        assert!(lines.iter().skip(1).any(|l| line_text(l).contains("hello")));
    }

    #[test]
    fn empty_agent_message_still_emits_prefix() {
        let lines = render("", 80);
        assert_eq!(lines.len(), 1, "empty body → just the prefix line");
        assert!(line_text(&lines[0]).contains("Agent"));
    }

    // ─── Headers ──────────────────────────────────────────────────────

    #[test]
    fn h1_emits_bold_styled_line() {
        let lines = render("# Title", 80);
        let title_line = lines
            .iter()
            .find(|l| line_text(l).contains("Title"))
            .expect("title line must exist");
        let has_bold = title_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold, "h1 must have BOLD modifier");
    }

    #[test]
    fn h1_h2_h3_all_render() {
        let text = render_to_text("# A\n\n## B\n\n### C", 80);
        assert!(text.contains("A") && text.contains("B") && text.contains("C"));
    }

    // ─── Emphasis ─────────────────────────────────────────────────────

    #[test]
    fn bold_applies_bold_modifier() {
        let lines = render("**bold word**", 80);
        let body = lines.iter().skip(1).flat_map(|l| l.spans.iter());
        let has_bold = body.into_iter().any(|s| {
            s.content.as_ref().contains("bold word")
                && s.style.add_modifier.contains(Modifier::BOLD)
        });
        assert!(has_bold, "**bold word** must yield a BOLD span");
    }

    #[test]
    fn italic_applies_italic_modifier() {
        let lines = render("*italic*", 80);
        let body = lines.iter().skip(1).flat_map(|l| l.spans.iter());
        let has_italic = body.into_iter().any(|s| {
            s.content.as_ref().contains("italic") && s.style.add_modifier.contains(Modifier::ITALIC)
        });
        assert!(has_italic, "*italic* must yield an ITALIC span");
    }

    #[test]
    fn bold_italic_combines_modifiers() {
        let lines = render("***both***", 80);
        let body = lines.iter().skip(1).flat_map(|l| l.spans.iter());
        let has_both = body.into_iter().any(|s| {
            s.content.as_ref().contains("both")
                && s.style.add_modifier.contains(Modifier::BOLD)
                && s.style.add_modifier.contains(Modifier::ITALIC)
        });
        assert!(has_both, "***both*** must yield BOLD+ITALIC");
    }

    // ─── Inline code ──────────────────────────────────────────────────

    #[test]
    fn inline_code_styled_and_backticks_stripped() {
        let lines = render("look at `foo()`", 80);
        let body_text: String = lines
            .iter()
            .skip(1)
            .map(line_text)
            .collect::<Vec<_>>()
            .join("");
        assert!(
            body_text.contains("foo()"),
            "inline code text preserved: {body_text}"
        );
        assert!(
            !body_text.contains("`"),
            "backticks must be stripped: {body_text}"
        );
    }

    // ─── Fenced code blocks ───────────────────────────────────────────

    #[test]
    fn fenced_code_emits_top_and_bottom_rules() {
        let text = render_to_text("```rust\nfn x() {}\n```", 40);
        assert!(text.contains("┌"), "top rule missing: {text}");
        assert!(text.contains("┐"), "top right corner missing: {text}");
        assert!(text.contains("└"), "bottom rule missing: {text}");
        assert!(text.contains("┘"), "bottom right corner missing: {text}");
        assert!(text.contains("fn x() {}"), "code body missing: {text}");
    }

    #[test]
    fn fenced_code_rule_extends_to_pane_width() {
        let lines = render("```\nx\n```", 40);
        let rule = lines
            .iter()
            .find(|l| line_text(l).starts_with("┌"))
            .expect("top rule must exist");
        // The rule should span the full pane width (40 *display cells*).
        assert_eq!(cells(&line_text(rule)), 40);
    }

    // ─── Tables ───────────────────────────────────────────────────────

    #[test]
    fn simple_table_renders_with_box_chars() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |";
        let text = render_to_text(md, 80);
        // Top rule, header row, separator, body × 2, bottom rule.
        assert!(text.contains("┌"), "top rule");
        assert!(text.contains("┬"));
        assert!(text.contains("├"), "header separator");
        assert!(text.contains("┼"));
        assert!(text.contains("└"), "bottom rule");
        assert!(text.contains("┴"));
        for cell in ["A", "B", "1", "2", "3", "4"] {
            assert!(text.contains(cell), "missing cell {cell}: {text}");
        }
    }

    #[test]
    fn table_header_row_is_bold() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let lines = render(md, 80);
        // First content row inside the box is the header — find the line
        // containing "A" and check at least one span is bold.
        let header_line = lines
            .iter()
            .find(|l| line_text(l).contains('A') && line_text(l).contains('B'))
            .expect("header line");
        let any_bold = header_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(any_bold, "header row must have a BOLD span");
    }

    #[test]
    fn narrow_table_wraps_cells() {
        // Force a narrow pane that can't fit the natural widths so cells
        // must wrap. Long single-column content guarantees overflow.
        let md = "| Header |\n|---|\n| this is a much longer cell that will need wrapping |";
        let lines = render(md, 20);
        // Count visual rows inside the box (between top and bottom rule).
        let n_box_lines = lines
            .iter()
            .filter(|l| {
                let t = line_text(l);
                t.starts_with("│") || t.starts_with("┌") || t.starts_with("└") || t.starts_with("├")
            })
            .count();
        // Top + header(≥1) + sep + body(≥2 due to wrap) + bottom = at least 6.
        assert!(
            n_box_lines >= 6,
            "narrow table must wrap body cell: got {n_box_lines} box lines"
        );
    }

    #[test]
    fn alignment_right_pads_left() {
        // pulldown-cmark parses `---:` as right-aligned.
        let md = "| n |\n|---:|\n| 7 |";
        let text = render_to_text(md, 80);
        // The "7" should be right-aligned: spaces before it, then "7", then trailing space + border.
        // Cell layout: │␣<padding>7␣│. Find the body line.
        let body_line = text
            .lines()
            .find(|l| l.contains('7') && !l.contains("7 "))
            .or_else(|| text.lines().find(|l| l.contains('7')))
            .expect("body line with 7");
        // For right alignment the '7' should have at least one ' ' before it inside the cell.
        assert!(
            body_line.contains(" 7"),
            "right-aligned cell: {body_line:?}"
        );
    }

    // ─── Multi-paragraph ──────────────────────────────────────────────

    #[test]
    fn multi_paragraph_separated_by_blank_line() {
        let lines = render("first\n\nsecond", 80);
        // Find indices of "first" and "second".
        let first_idx = lines
            .iter()
            .position(|l| line_text(l).contains("first"))
            .unwrap();
        let second_idx = lines
            .iter()
            .position(|l| line_text(l).contains("second"))
            .unwrap();
        assert!(second_idx > first_idx + 1, "must have blank line between");
        assert!(
            lines[first_idx + 1].spans.is_empty(),
            "intermediate line is blank"
        );
    }

    // ─── Width-accuracy regressions ───────────────────────────────────
    //
    // These two tests pin the bugs reported in chat 2026-04-30:
    //   1. Emojis / wide chars in a table broke alignment because we
    //      sized columns with `.chars().count()` (code points) instead
    //      of terminal display cells.
    //   2. Code blocks rendered too-wide for the chat pane in normal
    //      mode (borders + scrollbar present) because the renderer's
    //      width was passed without subtracting the scrollbar column.
    //      The renderer-side fix is to honour the width it's *given*;
    //      the call-site fix lives in `repl_impl.rs`. This test pins
    //      the renderer-side contract: rules MUST equal the requested
    //      width, exactly.

    /// Display width: every visible glyph contributes its terminal-cell
    /// count (1 for ASCII, 2 for CJK / most emojis). This is the same
    /// metric ratatui uses for layout, so renderer-side measurements
    /// must match.
    fn cells(s: &str) -> usize {
        use unicode_width::UnicodeWidthStr;
        UnicodeWidthStr::width(s)
    }

    #[test]
    fn fenced_code_top_rule_width_matches_requested_width() {
        // Renderer was told `width = 40`. The drawn rule must be 40
        // *display cells* — not 41, not 39. This is the contract the
        // chat-pane sizing logic relies on.
        for w in [20u16, 40, 79, 80, 120] {
            let lines = render("```\nx\n```", w);
            let rule = lines
                .iter()
                .find(|l| line_text(l).starts_with("┌"))
                .expect("top rule must exist");
            let drawn = cells(&line_text(rule));
            assert_eq!(
                drawn, w as usize,
                "top rule must be exactly {w} cells, got {drawn}"
            );
        }
    }

    #[test]
    fn fenced_code_bottom_rule_width_matches_requested_width() {
        for w in [20u16, 40, 79, 80, 120] {
            let lines = render("```\nx\n```", w);
            let rule = lines
                .iter()
                .find(|l| line_text(l).starts_with("└"))
                .expect("bottom rule must exist");
            let drawn = cells(&line_text(rule));
            assert_eq!(
                drawn, w as usize,
                "bottom rule must be exactly {w} cells, got {drawn}"
            );
        }
    }

    #[test]
    fn table_with_emojis_keeps_columns_aligned() {
        // The emoji column is 2 cells wide per glyph; the previous
        // implementation sized it by code-point count, undercounting
        // by the difference and shoving the right border one cell off.
        let md = "| Icon | Name |\n|---|---|\n| 🦀 | crab |\n| 🚀 | rocket |";
        let lines = render(md, 80);
        // Every box-drawing line (top, header, separator, body×2,
        // bottom — 6 total) must have identical *display width*.
        let box_lines: Vec<String> = lines
            .iter()
            .map(line_text)
            .filter(|t| {
                t.starts_with("┌") || t.starts_with("├") || t.starts_with("└") || t.starts_with("│")
            })
            .collect();
        assert!(
            box_lines.len() >= 6,
            "expected ≥6 box lines, got {}",
            box_lines.len()
        );
        let widths: Vec<usize> = box_lines.iter().map(|l| cells(l)).collect();
        let first = widths[0];
        for (i, w) in widths.iter().enumerate() {
            assert_eq!(
                *w, first,
                "row {i} width drift: line {:?} = {w} cells, expected {first}",
                box_lines[i]
            );
        }
    }

    #[test]
    fn table_with_cjk_keeps_columns_aligned() {
        let md = "| 名前 | 値 |\n|---|---|\n| 日本語 | テスト |\n| short | x |";
        let lines = render(md, 80);
        let box_lines: Vec<String> = lines
            .iter()
            .map(line_text)
            .filter(|t| {
                t.starts_with("┌") || t.starts_with("├") || t.starts_with("└") || t.starts_with("│")
            })
            .collect();
        let widths: Vec<usize> = box_lines.iter().map(|l| cells(l)).collect();
        let first = widths[0];
        for (i, w) in widths.iter().enumerate() {
            assert_eq!(
                *w, first,
                "CJK row {i} width drift: line {:?} = {w} cells, expected {first}",
                box_lines[i]
            );
        }
    }
}
