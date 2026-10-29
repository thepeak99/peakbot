//! Helper functions for testing Ratatui widgets with TestBackend.

use ratatui::{Terminal, backend::TestBackend, widgets::Widget};
use regex::Regex;

/// Render a widget to a TestBackend terminal and return it for assertions.
pub fn render_widget<W: Widget>(widget: W, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("Failed to create terminal");
    let _ = terminal.draw(|f| {
        f.render_widget(widget, f.area());
    });
    terminal
}

/// Convert TestBackend buffer to lines for inspection.
pub fn buffer_to_lines(backend: &TestBackend) -> Vec<String> {
    // Access the buffer content - TestBackend stores it internally
    // We need to use Display trait or reconstruct lines manually
    let buf = backend.buffer();
    let width = buf.area.width as usize;
    let content = buf.content();

    content
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

/// Redact a TUI banner version (`v0.25.0-rc.1` → `vX.Y.Z`) for snapshots.
/// The replacement is padded to the match's length: the banner lives in a
/// fixed-width box, so a length-preserving placeholder keeps the rendered
/// line (padding, right border) identical for stable and prerelease versions.
pub fn redact_version(text: &str) -> String {
    static VERSION: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = VERSION.get_or_init(|| Regex::new(r"v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?").unwrap());
    re.replace_all(text, |m: &regex::Captures| {
        format!("vX.Y.Z{}", " ".repeat(m[0].len() - 6))
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_version_preserves_length() {
        // Shortest match (v0.0.0) equals the placeholder — never negative padding.
        assert_eq!(redact_version("v0.0.0"), "vX.Y.Z");
        assert_eq!(redact_version("v0.25.0"), "vX.Y.Z ");
        // Prerelease and build-metadata versions must not shift line width.
        assert_eq!(redact_version("v0.25.0-rc.1"), "vX.Y.Z      ");
        assert_eq!(redact_version("v1.2.3+build.5"), "vX.Y.Z        ");
        assert_eq!(redact_version("no version here"), "no version here");
    }
}
