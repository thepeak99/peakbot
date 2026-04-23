//! Chat render performance tests.
//!
//! Proves (and later guards) that building the chat-history Paragraph does
//! not scale linearly with transcript length on the per-frame hot path.
//!
//! Two tests:
//!
//! - `build_chat_history_scales_reasonably`: exercises the current naïve
//!   pipeline (`build_chat_history_paragraph` + `line_count`) on increasing
//!   history sizes and just prints the numbers. Marked `#[ignore]` so it
//!   doesn't slow the default test run; opt in with
//!   `cargo test --release --test integration -- --ignored --nocapture chat_render`.
//!
//! - `build_chat_history_is_not_quadratic`: guard rail for the eventual
//!   cached path. Fails if the n=500 frame takes more than 50 ms after
//!   PR #2 lands. Today it is expected to pass; it mostly prevents
//!   regressions.
//!
//! See `slow-messages.md` for the full design.

use std::time::{Duration, Instant};

use peakbot::ui::ChatMessage;
use peakbot::ui::app_state::ChatState;
use peakbot::ui::repl::message_renderer::PlainRenderer;
use peakbot::ui::repl::render_cache::ChatRenderCache;
use peakbot::ui::repl::repl_impl::ReplUi;

/// Build a `ChatState` with `n` agent messages of roughly 1 KB each —
/// representative of a long tool-using conversation.
fn build_history(n: usize) -> ChatState {
    let mut chat = ChatState::new();
    let body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                Sed do eiusmod tempor incididunt ut labore et dolore magna \
                aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
                ullamco laboris nisi ut aliquip ex ea commodo consequat. \
                Duis aute irure dolor in reprehenderit in voluptate velit \
                esse cillum dolore eu fugiat nulla pariatur. Excepteur sint \
                occaecat cupidatat non proident, sunt in culpa qui officia \
                deserunt mollit anim id est laborum.";
    for i in 0..n {
        chat.messages
            .push(ChatMessage::agent(format!("Message #{i}\n{body}")));
    }
    chat
}

/// Simulate one frame of rendering work on the chat history paragraph.
///
/// This mirrors the hot path in `ReplUi::render`:
///   - build the paragraph from all messages
///   - call `line_count` to compute height for the layout
///
/// It deliberately skips the actual `render_widget` (which would need a
/// backend) because that does its *own* wrap pass — so the real app
/// does strictly more work than this function measures. Any slowdown
/// here is a lower bound on the real per-frame cost.
fn one_frame_of_work(chat: &ChatState, width: u16) -> usize {
    let paragraph = ReplUi::build_chat_history_paragraph(chat);
    paragraph.line_count(width)
}

fn time_frames(chat: &ChatState, width: u16, frames: usize) -> Duration {
    // Warm up once to avoid counting allocator first-touch costs.
    let _ = one_frame_of_work(chat, width);

    let start = Instant::now();
    for _ in 0..frames {
        let _ = one_frame_of_work(chat, width);
    }
    start.elapsed()
}

#[test]
#[ignore = "performance probe — run with --release --ignored --nocapture"]
fn chat_render_scales_reasonably() {
    const WIDTH: u16 = 100;
    const FRAMES: usize = 20; // one second of real-time ticks

    println!("\n=== naïve path (the bug): rebuild + wrap whole history per frame ===");
    println!("  msgs  |  total   |  per-frame");
    println!("  ------+----------+-----------");
    for &n in &[10usize, 50, 100, 250, 500, 1000] {
        let chat = build_history(n);
        let elapsed = time_frames(&chat, WIDTH, FRAMES);
        let per_frame = elapsed / FRAMES as u32;
        println!(
            "  {:>4}  |  {:>7.2?}  |  {:>8.2?}",
            n, elapsed, per_frame
        );
    }

    println!("\n=== cached path: sync + window via ChatRenderCache ===");
    println!("  msgs  |  sync_1  |  sync_N  |  window   (steady-state frame ≈ window)");
    println!("  ------+----------+----------+----------");
    for &n in &[10usize, 50, 100, 250, 500, 1000] {
        let chat = build_history(n);
        let mut cache = ChatRenderCache::new(Box::new(PlainRenderer));

        // First sync: pays the full O(N) cost to populate the cache. This
        // happens exactly once per (conversation, width) combo.
        let t = Instant::now();
        cache.sync(&chat.messages, WIDTH);
        let first_sync = t.elapsed();

        // Subsequent syncs on an unchanged transcript: should be O(N) for
        // the fingerprint comparison, but no renders or wraps happen.
        let t = Instant::now();
        for _ in 0..FRAMES {
            cache.sync(&chat.messages, WIDTH);
        }
        let steady_sync = t.elapsed() / FRAMES as u32;

        // Windowing: the per-frame hot path. Should be O(viewport),
        // independent of n.
        let t = Instant::now();
        for _ in 0..FRAMES {
            let _ = cache.window(0, 40);
        }
        let window = t.elapsed() / FRAMES as u32;

        println!(
            "  {:>4}  |  {:>7.2?} |  {:>7.2?} |  {:>7.2?}",
            n, first_sync, steady_sync, window
        );
    }
}

/// Regression guard: one tick of chat-render work at 500 messages must
/// fit inside the 50 ms tick budget in `ReplUi::run`. "One tick" means
/// what the app actually does every 50 ms *after* the cache has been
/// populated — a `sync` call (fingerprint-compare, no new renders on an
/// unchanged transcript) plus a viewport `window`. That's the real hot
/// path the fix in `slow-messages.md` PR2 was designed to make O(viewport).
///
/// Cold-sync cost (first render of a loaded conversation) is paid once
/// and is not a per-tick concern; it's not measured here.
///
/// Budget is deliberately generous (the cached path is microseconds, not
/// milliseconds). This guard fires only if someone re-introduces O(N)
/// work on the per-frame path.
#[test]
fn chat_render_one_frame_under_budget_at_500_messages() {
    const WIDTH: u16 = 100;
    const VIEWPORT_H: u16 = 40;
    const BUDGET: Duration = Duration::from_millis(50);

    let chat = build_history(500);
    let mut cache = ChatRenderCache::new(Box::new(PlainRenderer));

    // Prime the cache. This is the one-time cost paid when a conversation
    // first loads; subsequent ticks walk the same messages unchanged.
    cache.sync(&chat.messages, WIDTH);

    // One steady-state tick: re-sync (fingerprint scan, zero renders) and
    // pull a viewport window.
    let start = Instant::now();
    cache.sync(&chat.messages, WIDTH);
    let _ = cache.window(0, VIEWPORT_H);
    let elapsed = start.elapsed();

    assert!(
        elapsed < BUDGET,
        "one steady-state tick on 500 messages took {:?}, exceeding \
         the {:?} tick budget. The per-frame path has regressed to O(N). \
         See slow-messages.md.",
        elapsed,
        BUDGET
    );
}

/// Strict regression guard on the cached hot path: the steady-state
/// per-frame cost must be well under 1 ms at 1000 messages. This is the
/// property the cache was written to ensure.
///
/// If this test ever fails, something has re-introduced O(N) work on the
/// per-frame path — read `slow-messages.md` and find it.
#[test]
fn cached_chat_render_steady_state_is_under_1ms_at_1000_messages() {
    const WIDTH: u16 = 100;
    const VIEWPORT_H: u16 = 40;
    const BUDGET: Duration = Duration::from_millis(1);

    let chat = build_history(1000);
    let mut cache = ChatRenderCache::new(Box::new(PlainRenderer));

    // Prime the cache once.
    cache.sync(&chat.messages, WIDTH);

    // Steady-state frame: no new messages, just re-sync + window.
    // Average across 100 iterations to damp out nanosecond-scale jitter.
    let start = Instant::now();
    for _ in 0..100 {
        cache.sync(&chat.messages, WIDTH);
        let _ = cache.window(0, VIEWPORT_H);
    }
    let per_frame = start.elapsed() / 100;

    assert!(
        per_frame < BUDGET,
        "cached steady-state frame on 1000 messages took {:?} \
         (budget {:?}). This suggests the per-frame path has regressed \
         to O(history). See slow-messages.md.",
        per_frame,
        BUDGET
    );
}
