//! Reusable centered modal confirmation dialog.
//!
//! Originally the `Ctrl+C` quit-confirm overlay (a pair of bools on
//! `ReplUi`). Generalised on the second consumer (`/model`) per the
//! "extract-on-second-use" rule: a closure-based design would be
//! shorter today and a maintenance trap forever; a typed
//! [`ConfirmAction`] enum is grep-able and lets future contributors
//! enumerate every dialog by searching `ConfirmAction::`.
//!
//! ## Behaviour (locked from the original quit-confirm UX)
//! - **Default-deny.** New dialogs open with `yes_selected: false` so
//!   bare-Enter cancels.
//! - `y` / `Y` selects Yes. `n` / `N` selects No.
//! - `Left` / `Right` toggles between Yes / No.
//! - `Enter` confirms whichever button is currently selected.
//! - `Esc` always cancels.
//!
//! The View owns the dialog state and the input dispatch; the action
//! it produces on confirm is whatever the [`ConfirmAction`] variant
//! describes.
//!
//! ## Visual contract
//! Centered overlay, fixed 50×9 (was 50×9 for quit-confirm — kept
//! identical so existing snapshots stay byte-for-byte unchanged), with
//! warning glyph `⚠` (Class A, no VS16) flanking the title, the body
//! message, and a `[ Yes, … ]` / `[ No, … ]` button pair styled to
//! reflect `yes_selected`.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// What confirming the dialog actually *does*. Each variant carries
/// the data needed to dispatch the action — the View matches on this
/// to decide what `UiAction` to send (or what local state to mutate).
///
/// Add a variant whenever you add a new confirm-gated operation. The
/// match arms in `repl_impl.rs::handle_keyboard_input` (for the Enter
/// path) and any test helpers are the places to update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Quit the application (Ctrl+C). View flips `running = false`.
    Quit,
    /// Switch the active model and start a new conversation.
    /// View dispatches `UiAction::SwitchModel(alias)` on confirm.
    SwitchModel {
        /// Validated alias resolved against the model registry.
        alias: String,
        /// Pretty-printed `"<provider> · <wire id>"` for the dialog
        /// body — derived at the call site so the dialog widget never
        /// looks anything up.
        provider_descriptor: String,
    },
    /// Change the working directory and start a new conversation.
    /// View dispatches `UiAction::ChangeCwd(path)` on confirm.
    ChangeCwd {
        /// Canonicalised absolute path, already validated (exists + dir)
        /// at the call site.
        path: String,
    },
}

/// Centered modal confirmation dialog. View-owned state.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    /// One-line title shown between the warning glyphs.
    pub title: String,
    /// Body question (e.g. "Are you sure you want to quit PeakBot?").
    pub question: String,
    /// Label inside the Yes button (button label only — the brackets
    /// are added by the renderer when selected).
    pub yes_label: String,
    /// Label inside the No button.
    pub no_label: String,
    /// Whether Yes is currently the highlighted choice.
    /// **Default-deny**: dialogs open with this `false`.
    pub yes_selected: bool,
    /// What confirming actually does.
    pub action: ConfirmAction,
}

impl ConfirmDialog {
    /// Build the canonical Quit dialog (Ctrl+C). Identical wording and
    /// chrome to the pre-refactor quit-confirm so existing snapshot
    /// tests stay byte-for-byte unchanged.
    pub fn quit() -> Self {
        Self {
            title: "WAIT! DON'T LEAVE!".into(),
            question: "Are you sure you want to quit PeakBot?".into(),
            yes_label: "Yes, leave".into(),
            no_label: "No, stay".into(),
            yes_selected: false,
            action: ConfirmAction::Quit,
        }
    }

    /// Build a `/model` switch confirmation dialog.
    pub fn switch_model(alias: &str, provider_descriptor: &str) -> Self {
        Self {
            title: "SWITCH MODEL?".into(),
            question: format!("Switch to {alias}? Starts a new conversation."),
            yes_label: "Yes, switch".into(),
            no_label: "No, stay".into(),
            yes_selected: false,
            action: ConfirmAction::SwitchModel {
                alias: alias.to_string(),
                provider_descriptor: provider_descriptor.to_string(),
            },
        }
    }

    /// Build a `/cd` change-directory confirmation dialog.
    pub fn change_cwd(path: &str) -> Self {
        Self {
            title: "CHANGE DIRECTORY?".into(),
            question: format!("Switch to {path}? Starts a new conversation."),
            yes_label: "Yes, switch".into(),
            no_label: "No, stay".into(),
            yes_selected: false,
            action: ConfirmAction::ChangeCwd {
                path: path.to_string(),
            },
        }
    }

    /// Toggle the selected button (Left/Right keys).
    pub fn toggle_selection(&mut self) {
        self.yes_selected = !self.yes_selected;
    }
}

/// Greedily wrap `text` to at most `width` display cells per line,
/// breaking on whitespace where possible and hard-breaking any single
/// run longer than `width` (a filesystem path has no spaces). Returns at
/// least one line so the caller always has a question row to render.
///
/// Width is approximated by `char` count — adequate for the ASCII-ish
/// questions and paths this dialog shows; the popup border gives a cell
/// of slack either way.
fn wrap_question(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_len = 0usize;

    let push_line = |lines: &mut Vec<String>, line: &mut String, line_len: &mut usize| {
        lines.push(std::mem::take(line));
        *line_len = 0;
    };

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        // A word wider than the line: flush, then hard-break it in chunks.
        if word_len > width {
            if line_len > 0 {
                push_line(&mut lines, &mut line, &mut line_len);
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            line = chunk;
            line_len = line.chars().count();
            continue;
        }
        // A space is needed before the word unless the line is empty.
        let needed = if line_len == 0 {
            word_len
        } else {
            line_len + 1 + word_len
        };
        if needed > width {
            push_line(&mut lines, &mut line, &mut line_len);
            line.push_str(word);
            line_len = word_len;
        } else {
            if line_len > 0 {
                line.push(' ');
                line_len += 1;
            }
            line.push_str(word);
            line_len += word_len;
        }
    }
    lines.push(line);
    lines
}

/// Render any [`ConfirmDialog`] as a centered overlay.
///
/// Visual layout matches the original `render_quit_confirm` exactly so
/// the existing 80×24 / 120×40 / 60×15 quit-confirm snapshots stay
/// stable across the refactor.
pub fn render_confirm_dialog(f: &mut Frame, area: Rect, dialog: &ConfirmDialog) {
    let popup_width: u16 = 50;
    // Wrap the question to the popup's inner width so a long path (or any
    // long question) never gets clipped at the border. Short questions
    // (quit, /model) wrap to exactly one line — geometry and snapshots are
    // unchanged; only long questions grow the popup vertically.
    let inner_width = (popup_width as usize).saturating_sub(2);
    let q_lines = wrap_question(&dialog.question, inner_width);
    // Base height (9) held one question line; add a row per extra line.
    let popup_height = 9u16.saturating_add(q_lines.len().saturating_sub(1) as u16);
    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, popup_area);

    // Both buttons get the same fixed display width so the centered
    // pair lines up regardless of which is selected. Width was chosen
    // to fit "Yes, leave " / "No, stay   " (the longest of the
    // original quit-confirm labels), padded to 14 cells. That width
    // also accommodates "Yes, switch " / "No, stay    " for /model.
    let pad_to = |s: &str, width: usize| -> String {
        let len = s.chars().count();
        if len >= width {
            s.to_string()
        } else {
            let mut out = String::with_capacity(width);
            out.push_str(s);
            out.extend(std::iter::repeat_n(' ', width - len));
            out
        }
    };

    const BTN_INNER: usize = 12;
    const BTN_TOTAL_WIDTH: usize = 14 + 3 + 14; // "[ X ]" + sep + "[ Y ]"

    let selected_style = Style::default().fg(Color::Yellow).bg(Color::DarkGray);
    let unselected_style = Style::default().fg(Color::White);

    let (yes_btn, yes_style) = if dialog.yes_selected {
        (format!("[ {} ]", dialog.yes_label), selected_style)
    } else {
        (
            format!("  {}  ", pad_to(&dialog.yes_label, BTN_INNER - 1)),
            unselected_style,
        )
    };
    let (no_btn, no_style) = if !dialog.yes_selected {
        (format!("[ {} ]", dialog.no_label), selected_style)
    } else {
        (
            format!("  {}  ", pad_to(&dialog.no_label, BTN_INNER - 1)),
            unselected_style,
        )
    };

    let btn_left_padding = (popup_width as usize).saturating_sub(BTN_TOTAL_WIDTH) / 2;

    // Centered warning line. VS16 stripped from `⚠` so the cell width
    // is 1 each — total visible width: 2*1 + 2 spaces + title-len + 2
    // spaces + 2*1. Indent computed to centre the whole thing.
    let warning_inner_width = 2 + 2 + dialog.title.chars().count() + 2 + 2;
    let warning_indent = (popup_width as usize).saturating_sub(warning_inner_width) / 2;
    let warning = format!("{}⚠  {}  ⚠", " ".repeat(warning_indent), dialog.title);

    // Centered question — one centered `Line` per wrapped row so a long
    // path stacks inside the popup instead of clipping at the border.
    let question_lines: Vec<Line> = q_lines
        .iter()
        .map(|q| {
            let indent = (popup_width as usize).saturating_sub(q.chars().count()) / 2;
            Line::from(vec![Span::raw(format!("{}{}", " ".repeat(indent), q))])
        })
        .collect();

    let hint = "      ←/→ to switch  ·  Enter to confirm  ·  Esc to cancel";
    let btn_padding = " ".repeat(btn_left_padding);

    let mut content = vec![
        Line::from(vec![Span::raw("")]),
        Line::from(vec![Span::styled(
            warning,
            Style::default().fg(Color::LightRed),
        )]),
        Line::from(vec![Span::raw("")]),
    ];
    content.extend(question_lines);
    content.extend([
        Line::from(vec![Span::raw("")]),
        Line::from(vec![
            Span::raw(btn_padding),
            Span::styled(yes_btn, yes_style),
            Span::raw("   "),
            Span::styled(no_btn, no_style),
        ]),
        Line::from(vec![Span::raw("")]),
        Line::from(vec![Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        )]),
        Line::from(vec![Span::raw("")]),
    ]);

    let paragraph = Paragraph::new(content)
        .style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightCyan)),
        );

    f.render_widget(paragraph, popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_dialog_carries_quit_action() {
        let d = ConfirmDialog::quit();
        assert_eq!(d.action, ConfirmAction::Quit);
        assert!(!d.yes_selected, "default-deny: opens with No");
    }

    #[test]
    fn switch_model_dialog_carries_alias() {
        let d = ConfirmDialog::switch_model("oai-gpt4", "openai · gpt-4o");
        match &d.action {
            ConfirmAction::SwitchModel {
                alias,
                provider_descriptor,
            } => {
                assert_eq!(alias, "oai-gpt4");
                assert_eq!(provider_descriptor, "openai · gpt-4o");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(!d.yes_selected, "default-deny");
    }

    #[test]
    fn change_cwd_dialog_carries_path() {
        let d = ConfirmDialog::change_cwd("/tmp/project");
        match &d.action {
            ConfirmAction::ChangeCwd { path } => assert_eq!(path, "/tmp/project"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(!d.yes_selected, "default-deny");
    }

    #[test]
    fn toggle_flips_selection() {
        let mut d = ConfirmDialog::quit();
        assert!(!d.yes_selected);
        d.toggle_selection();
        assert!(d.yes_selected);
        d.toggle_selection();
        assert!(!d.yes_selected);
    }

    #[test]
    fn wrap_question_short_stays_one_line() {
        // Existing quit/model questions fit — must stay a single line so
        // popup geometry (and the golden snapshots) are unchanged.
        let q = "Are you sure you want to quit PeakBot?";
        let lines = wrap_question(q, 48);
        assert_eq!(lines, vec![q.to_string()]);
    }

    #[test]
    fn wrap_question_hard_breaks_long_path() {
        // A long path has no spaces — it must hard-break rather than
        // overflow a single line.
        let q = "Switch to /aaaa/bbbb/cccc/dddd/eeee/ffff/gggg/hhhh/iiii/jjjj/kkkk? \
                 Starts a new conversation.";
        let lines = wrap_question(q, 48);
        assert!(lines.len() > 1, "long question must wrap: {lines:?}");
        for l in &lines {
            assert!(
                l.chars().count() <= 48,
                "no line exceeds the inner width: {l:?}"
            );
        }
    }

    #[test]
    fn render_long_cwd_question_is_not_clipped() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // The whole point of ticket #145 feedback: a long /cd path must
        // appear in full inside the dialog, not truncated at the border.
        let path = "/very/long/directory/path/that/would/overflow/the/fixed/popup/leafdir";
        let dialog = ConfirmDialog::change_cwd(path);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_confirm_dialog(f, f.area(), &dialog))
            .unwrap();
        let rendered = format!("{:?}", terminal.backend().buffer());

        // Every path segment must survive the wrap — including the leaf,
        // which the old fixed-width single-line render clipped away.
        for segment in ["very", "overflow", "leafdir"] {
            assert!(
                rendered.contains(segment),
                "path segment `{segment}` was clipped:\n{rendered}"
            );
        }
    }
}
