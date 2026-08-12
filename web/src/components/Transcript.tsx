// The transcript pane: one scrolling section, virtualized rows, and the
// floating nav. Only the rows near the viewport are mounted, so a
// 5000-message conversation costs about what a 50-message one costs
// (issue #277).
//
// Resetting the transcript (conversation switch, view switch, truncation) is
// a *remount*: App keys this component by the epoch from `transcriptEpoch.ts`.
// A fresh mount means a fresh scroll element, a fresh virtualizer and a fresh
// pin state, with no ordering hazards — so nothing here needs to invalidate
// caches by hand.

import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  type Ref,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Message } from "./Message";
import { TranscriptNav } from "./TranscriptNav";
import { adaptMessage } from "../adapt";
import type { WireChatMessage } from "../state";
import { estimateRowHeight } from "../transcriptRows";
import { PIN_THRESHOLD_PX, useTranscriptScroll } from "../useTranscriptScroll";

export interface TranscriptHandle {
  /** Scroll to the newest message and resume following it. */
  jumpToLatest(): void;
}

interface Props {
  /** Already filtered by view, and never empty: App renders `EmptyTranscript`
   *  for the zero-message case, which makes "virtualizer over no rows"
   *  unrepresentable. */
  messages: WireChatMessage[];
  drawerOpen: boolean;
  /** React 19: `ref` is a plain prop, no `forwardRef` wrapper needed. */
  ref?: Ref<TranscriptHandle>;
}

export function Transcript({ messages, drawerOpen, ref }: Props) {
  // The hook counts the *filtered* messages, so "N new messages" is per-view.
  const { sectionRef, onScroll, pinned, unread, showTop, repin } =
    useTranscriptScroll(messages.length);

  const virtualizer = useVirtualizer<HTMLElement, HTMLDivElement>({
    count: messages.length,
    getScrollElement: () => sectionRef.current,
    estimateSize: (i) => estimateRowHeight(messages[i]),
    overscan: 3,
    // A chat grows at the tail: anchor to the end so appends and height
    // corrections above the viewport don't shift the text being read.
    anchorTo: "end",
    // Replaces the old scroll-to-bottom effect in App: the virtualizer
    // follows appends itself, instantly, and only when the viewport is
    // already within `scrollEndThreshold` of the end.
    followOnAppend: "auto",
    // Same slack as the pin state, so "following" and "pinned" agree.
    scrollEndThreshold: PIN_THRESHOLD_PX,
  });

  const jumpToLatest = useCallback(() => {
    // Instant, never smooth: a smooth scroll defeats end anchoring.
    virtualizer.scrollToEnd();
    repin();
  }, [virtualizer, repin]);

  useImperativeHandle(ref, () => ({ jumpToLatest }), [jumpToLatest]);

  useLayoutEffect(() => {
    // Land on the newest message before the first paint — history should
    // never flash from the top. Mount only; every later scroll belongs to
    // `followOnAppend` or the user.
    virtualizer.scrollToEnd();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- mount only, by design.
  }, []);

  // Read the pin state through a ref so the observer below survives every
  // pin/unpin flip instead of being torn down and rebuilt.
  const pinnedRef = useRef(pinned);
  useEffect(() => {
    pinnedRef.current = pinned;
  }, [pinned]);

  useEffect(() => {
    const el = sectionRef.current;
    if (!el) return;
    // Row heights depend on the width: text rewraps. Rows that are currently
    // unmounted keep the height they were measured at, so after a resize,
    // rotation or drawer toggle the total size is wrong until each row is
    // visited again. Throw those measurements away on a width change (height
    // changes are just the viewport growing — re-measuring on those would
    // fight the virtualizer's own measurement pass).
    let lastWidth = el.clientWidth;
    const observer = new ResizeObserver(() => {
      const width = el.clientWidth;
      if (width === lastWidth) return;
      lastWidth = width;
      virtualizer.measure();
      if (pinnedRef.current) virtualizer.scrollToEnd();
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [virtualizer, sectionRef]);

  return (
    // The nav buttons float over the transcript, so they live in this
    // positioned wrapper *outside* the scrolling <section> — absolute inside
    // it would scroll away with the content.
    <div className="relative flex min-h-0 flex-1 flex-col">
      <section
        ref={sectionRef}
        onScroll={onScroll}
        className="min-h-0 flex-1 overflow-y-auto mr-12 px-4 py-4 sm:px-6 md:px-8"
      >
        <div
          className="relative mx-auto w-full max-w-5xl"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualizer.getVirtualItems().map((vi) => (
            // key is the index: the transcript is append-only, so the index
            // *is* a stable identity. Do not "fix" this to a message id.
            <div
              key={vi.key}
              data-index={vi.index}
              // `data-index` and `measureElement` must stay on the same
              // element — the library reads the index off the DOM node.
              ref={virtualizer.measureElement}
              // Row spacing is padding, not margin: margins fall outside the
              // measured border box and the virtualizer would lose them.
              className="absolute left-0 top-0 w-full pb-3"
              style={{ transform: `translateY(${vi.start}px)` }}
            >
              <Message message={adaptMessage(messages[vi.index])} />
            </div>
          ))}
        </div>
      </section>

      <TranscriptNav
        pinned={pinned}
        unread={unread}
        showTop={showTop}
        onBottom={jumpToLatest}
        onTop={() => sectionRef.current?.scrollTo({ top: 0 })}
        drawerOpen={drawerOpen}
      />
    </div>
  );
}
