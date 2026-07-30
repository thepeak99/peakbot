// Pin-aware transcript scrolling. The transcript follows the newest message
// only while the user is *pinned* to the bottom; scroll up to read history and
// new messages stop yanking the viewport (they're announced instead, see
// `TranscriptNav`).
//
// One piece of state carries both facts: `unpinnedAt` is the message count at
// the moment the user scrolled away, or `null` while pinned. That makes
// "pinned" and "how many messages arrived since" derivations rather than
// copies, so they cannot drift out of sync.

import { useCallback, useEffect, useRef, useState } from "react";

/** Distance from the bottom (px) still counted as "pinned". Slack enough to
 *  survive a half-line of overscroll and the composer's growth. */
const PIN_THRESHOLD_PX = 80;

/** How far down (px) before offering "back to top". */
const TOP_BUTTON_PX = 400;

/** The subset of a scroll container the pin decision needs — a real element
 *  satisfies it, and so does a plain object in tests. */
interface ScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export function isPinnedAt(m: ScrollMetrics): boolean {
  return m.scrollHeight - m.scrollTop - m.clientHeight < PIN_THRESHOLD_PX;
}

export function useTranscriptScroll(messageCount: number) {
  const sectionRef = useRef<HTMLElement>(null);
  const [unpinnedAt, setUnpinnedAt] = useState<number | null>(null);
  const [showTop, setShowTop] = useState(false);

  const pinned = unpinnedAt === null;
  // Clamp: a shorter transcript (conversation switch) must not count backwards.
  const unread =
    unpinnedAt === null ? 0 : Math.max(0, messageCount - unpinnedAt);

  /** Scrolls to the newest message and re-pins. One function, because doing
   *  either half alone leaves the UI lying about where the user is. */
  const scrollToBottom = useCallback(() => {
    setUnpinnedAt(null);
    sectionRef.current?.scrollTo({
      top: sectionRef.current.scrollHeight,
      behavior: "smooth",
    });
  }, []);

  const scrollToTop = useCallback(() => {
    sectionRef.current?.scrollTo({ top: 0, behavior: "smooth" });
  }, []);

  // Scroll events fire far faster than paint. Sample at most once per frame,
  // otherwise the state churn fights the smooth scroll it just started.
  const frame = useRef(0);
  const lastTop = useRef(0);
  const onScroll = useCallback(() => {
    if (frame.current) return;
    frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      const el = sectionRef.current;
      if (!el) return;
      const top = el.scrollTop;
      // Only scrolling *up* unpins. A smooth scroll toward the tail fires
      // events all the way down while still far from the bottom; treating
      // those as "the user left" would flash the button and, if the animation
      // were then superseded by a new message, silently kill auto-follow.
      // The 1px tolerance absorbs sub-pixel jitter at fractional zoom levels.
      const movedUp = top < lastTop.current - 1;
      lastTop.current = top;
      if (isPinnedAt(el)) setUnpinnedAt(null);
      else if (movedUp) setUnpinnedAt((prev) => prev ?? messageCount);
      setShowTop(top > TOP_BUTTON_PX);
    });
  }, [messageCount]);

  useEffect(() => () => cancelAnimationFrame(frame.current), []);

  return {
    sectionRef,
    onScroll,
    pinned,
    unread,
    showTop,
    scrollToBottom,
    scrollToTop,
  };
}
