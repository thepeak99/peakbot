//! Helper functions for testing Ratatui widgets with TestBackend.

use ratatui::{Terminal, backend::TestBackend, widgets::Widget};

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
