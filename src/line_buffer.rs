//! The capped line ring a PTY reader thread fills, plus the child's
//! liveness. Plain data with no PTY dependency, so the bg registry that
//! stores it compiles in the Local edition too; `pty_runner` fills it.

use std::collections::VecDeque;

/// Maximum bytes per captured line before truncation kicks in. Protects
/// the LLM from log floods carrying a single multi-MB line.
pub const MAX_LINE_BYTES: usize = 4096;

/// Liveness status, updated by the reader thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyStatus {
    Running,
    Exited(i32),
}

impl PtyStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, PtyStatus::Running)
    }
}

/// Reader-thread-side state for a single process. Cheap to lock —
/// touches only Vec/usize fields, no IO, no async.
#[derive(Debug)]
pub struct LineBuffer {
    /// Line ring. Capped at `cap`; oldest evicted on overflow.
    pub lines: VecDeque<String>,
    /// Capacity of the ring (mirrors `SpawnParams::capture_cap`).
    pub cap: usize,
    /// Live status. Reader flips to `Exited` on EOF + `wait`.
    pub status: PtyStatus,
    /// `true` ⇒ buffer changed since the last consumer-driven clear.
    /// Set by the reader on every push and on exit; cleared by the
    /// consumer (e.g. `bg_processes::drain_outputs`).
    pub dirty: bool,
}

impl LineBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(1024)),
            cap,
            status: PtyStatus::Running,
            dirty: false,
        }
    }

    /// Append a line, honouring the cap. `cap == 0` discards the
    /// payload but still flags `dirty` so consumers can observe the
    /// "process produced something" signal (e.g. used for exit pings
    /// on capture-disabled bg processes).
    pub fn push_line(&mut self, line: String) {
        if self.cap == 0 {
            self.dirty = true;
            return;
        }
        let truncated = truncate_line(&line, MAX_LINE_BYTES);
        if self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(truncated);
        self.dirty = true;
    }
}

/// UTF-8-boundary-safe truncation with a `" … (truncated)"` suffix.
/// See the regression-pin notes in `memory.md`.
pub fn truncate_line(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let suffix = " … (truncated)";
    let target = max_bytes.saturating_sub(suffix.len());
    let mut cut = target.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}", &s[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_line_short_input_passes_through() {
        assert_eq!(truncate_line("hi", MAX_LINE_BYTES), "hi");
    }

    #[test]
    fn truncate_line_long_input_gets_truncated_marker() {
        let s = "a".repeat(MAX_LINE_BYTES + 100);
        let t = truncate_line(&s, MAX_LINE_BYTES);
        assert!(t.ends_with(" … (truncated)"));
        assert!(t.len() <= MAX_LINE_BYTES);
    }

    #[test]
    fn truncate_line_respects_utf8_boundaries() {
        // 4-byte emoji + 1-byte prefix shifts boundaries by 1 so a
        // naive byte cut would land mid-codepoint.
        let s = format!("a{}", "🦀".repeat(2000));
        let t = truncate_line(&s, MAX_LINE_BYTES);
        assert!(t.is_char_boundary(t.len() - " … (truncated)".len()));
    }

    #[test]
    fn line_buffer_evicts_oldest_on_overflow() {
        let mut b = LineBuffer::new(3);
        for i in 0..5 {
            b.push_line(format!("line {i}"));
        }
        assert_eq!(b.lines.len(), 3);
        assert_eq!(b.lines[0], "line 2");
        assert_eq!(b.lines[2], "line 4");
        assert!(b.dirty);
    }

    #[test]
    fn line_buffer_capacity_zero_drops_payload_but_marks_dirty() {
        let mut b = LineBuffer::new(0);
        b.push_line("anything".into());
        assert!(b.lines.is_empty());
        assert!(b.dirty);
    }
}
