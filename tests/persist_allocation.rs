//! Allocation-size regression test for `FileStorage::save`.
//!
//! Persisting a conversation must be O(1) in PEAK memory — no single
//! allocation proportional to the conversation size. The previous
//! implementation built a single ~60 MB `String` via
//! `serde_json::to_string_pretty` on every save, which exceeded glibc's
//! 32 MiB mmap threshold and triggered fresh non-main-arena heap
//! allocations on every persist. This test installs a recording global
//! allocator, builds a ~40 MiB synthetic conversation, and asserts that
//! the largest single allocation observed during `save` is < 1 MiB.
//!
//! The `#[global_allocator]` is scoped to THIS test binary — production
//! (`peakbot`) has no global allocator and stays that way.

#![cfg(test)]

use peakbot::{Conversation, ConversationStorage, FileStorage};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tempfile::TempDir;

/// Recording allocator: delegates to `System` while logging the LARGEST
/// single `alloc` / `realloc` size it sees while `armed` is set.
struct RecordingAllocator {
    inner: System,
    armed: AtomicBool,
    max: AtomicUsize,
}

unsafe impl GlobalAlloc for RecordingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.armed.load(Ordering::SeqCst) {
            self.max.fetch_max(layout.size(), Ordering::SeqCst);
        }
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.armed.load(Ordering::SeqCst) {
            self.max.fetch_max(new_size, Ordering::SeqCst);
        }
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: RecordingAllocator = RecordingAllocator {
    inner: System,
    armed: AtomicBool::new(false),
    max: AtomicUsize::new(0),
};

const ONE_MIB: usize = 1024 * 1024;
const NUM_TOOL_RESULTS: usize = 40;

/// ~40 MiB conversation: 40 `ToolResult` messages, each carrying a 1 MiB
/// ASCII blob. Mirrors the real shape (the 56 MB transcript is dominated
/// by a handful of base64 PNG tool results in the same size class).
fn build_large_conversation() -> Conversation {
    let mut conv = Conversation::new(
        "persist-allocation".into(),
        "openrouter".into(),
        "anthropic/claude-3.7-sonnet".into(),
        String::new(),
    );
    for i in 0..NUM_TOOL_RESULTS {
        let blob = "x".repeat(ONE_MIB);
        conv.add_tool_result(
            "view_image".into(),
            format!(r#"{{"path":"/tmp/img_{i}.png"}}"#),
            blob,
            Some(format!("call_{i}")),
        );
    }
    conv
}

#[test]
fn save_peak_allocation_is_constant_in_conversation_size() {
    let dir = TempDir::new().unwrap();
    let storage = FileStorage::new(dir.path().to_path_buf()).unwrap();

    // Build the conversation BEFORE arming the recorder — we want to
    // measure only what `save` itself allocates, not the 40 × 1 MiB
    // per-message String heap allocations done at construction time.
    let conv = build_large_conversation();
    assert_eq!(
        conv.messages.len(),
        NUM_TOOL_RESULTS,
        "fixture sanity: expected {NUM_TOOL_RESULTS} tool results",
    );

    ALLOC.max.store(0, Ordering::SeqCst);
    ALLOC.armed.store(true, Ordering::SeqCst);
    let save_result = storage.save(&conv);
    ALLOC.armed.store(false, Ordering::SeqCst);

    save_result.expect("save must succeed on a writable tempdir");
    let observed = ALLOC.max.load(Ordering::SeqCst);
    assert!(
        observed < ONE_MIB,
        "save allocated {observed} bytes in a single allocation (~{} MiB); \
         expected < 1 MiB. Persisting a conversation must be O(1) in PEAK \
         memory — no single allocation proportional to the conversation \
         size. Use a streaming writer (serde_json::to_writer_pretty) into \
         a fixed-capacity BufWriter<File> instead of building a full String \
         with to_string_pretty.",
        observed / ONE_MIB,
    );
}
