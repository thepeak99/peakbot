// Regression test for the row-overlap bug in `Transcript.tsx`.
//
// On the right path, `Transcript` renders N absolutely-positioned rows inside a
// virtualized container. The virtualizer learns each row's height two ways:
//
//   1. On mount, `ref={virtualizer.measureElement}` is invoked for every row.
//      With no ResizeObserver entry (entry is `void 0`), the default
//      `measureElement` option in @tanstack/virtual-core reads
//      `element.offsetHeight` — the row's real rendered height. That height
//      goes into the virtualizer's `itemSizeCache`.
//
//   2. While the row is in the DOM, a per-row ResizeObserver (created
//      internally by the virtualizer) keeps the cache fresh whenever the
//      row's border box actually changes — text rewrap, image load, etc.
//
// `Transcript.tsx` lines 87-106 also subscribes its OWN ResizeObserver to the
// scroll element so that a width change (drawer open/close, window resize,
// breakpoint flip) can invalidate stale measurements: a row that hasn't been
// seen since the width changed keeps the height it was measured at, so the
// total size is wrong until each row is visited again. The fix the code
// reaches for is `virtualizer.measure()`, which clears `itemSizeCache`
// wholesale (virtual-core index.js ~1111-1117).
//
// The bug: clearing the cache also clears the per-row sizes that were just
// measured at mount. The next render falls back to `estimateSize()` values.
// The per-row ResizeObservers don't fire because no row actually changed
// size (a drawer that overlays the transcript does not change the row's
// border box). So the rows stay at estimate values *permanently*. Estimates
// are far smaller than real content for any non-trivial message, so adjacent
// rows' `translateY` offsets are too close together and the rows visually
// overlap ("stuff mounts on top of each other").
//
// What this test asserts — and why it's bug-agnostic:
//
//   We don't reach into the virtualizer or assert which method it called.
//   We assert the observable layout: for every pair of adjacent rendered
//   rows (sorted by data-index), the next row's `translateY` is at least
//   the current row's `translateY` plus the current row's real height.
//   That is exactly the non-overlap invariant the virtualizer's positioning
//   must satisfy; if it fails, the rendered transcript is broken. We also
//   assert the container's total height covers the last row.
//
// jsdom has no layout, so we stub the measurements the virtualizer consults
// (`offsetHeight`, `getBoundingClientRect`, `clientWidth`/`clientHeight`)
// and install a controllable `ResizeObserver`. After mount we trigger the
// row observers to drive the library through its measurement path so each
// row adopts its "real" height. Then we change `clientWidth` and fire the
// scroll-element observer — the exact path `Transcript.tsx` subscribes to —
// and re-assert the same invariant. That second assertion is the one that
// fails today.

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Transcript } from "./Transcript";
import type { WireChatMessage } from "../state";
import { estimateRowHeight } from "../transcriptRows";

// React 19's `flushSync` (used by the virtualizer's wrapper inside
// `onChange`) reads `globalThis.IS_REACT_ACT_ENVIRONMENT` and warns when it's
// unset. Set it once so the console stays clean — the warning itself isn't
// the bug, but the test shouldn't be a sea of red.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── jsdom-shaping constants ──────────────────────────────────────────────
// Picked so 11 rows (~10x96 = 1060) comfortably overflow a 968px viewport —
// the same numbers captured in the broken-page evidence in the bug report.
const VIEWPORT_HEIGHT = 968;
const SECTION_WIDTH = 800;
// In a real layout, a row's rendered height depends on its wrapped content
// (text reflow, <details> expansion, code blocks). We pick a 3× multiplier
// over the seed estimate — far enough that the two are visibly distinct and
// small enough that adjacent estimate-based offsets can clearly overlap
// while the real offsets remain well-separated. This is the exact shape of
// the disagreement captured on the broken page.
const REAL_MULTIPLIER = 3;

interface StubState {
  /** All elements observed by any ResizeObserver — keyed by element. */
  observed: Map<Element, ResizeObserverCallback>;
  /** The single synthetic entry's contentRect, in px. */
  rect: { width: number; height: number };
  /** When true, the stub fires immediately after observe() (matches the
   *  ResizeObserver behaviour where the first observation reports the
   *  current border-box size). */
  fireOnObserve: boolean;
}

let stub: StubState;

beforeEach(() => {
  stub = {
    observed: new Map(),
    rect: { width: SECTION_WIDTH, height: 0 },
    fireOnObserve: true,
  };

  // Install a controllable ResizeObserver. The library reaches for
  // `globalThis.ResizeObserver` (via `targetWindow.ResizeObserver`) — we
  // install it on `window` so all observers created inside the component
  // tree come through us.
  const win = window as unknown as {
    ResizeObserver: typeof ResizeObserver;
  };
  win.ResizeObserver = class FakeResizeObserver {
    private cb: ResizeObserverCallback;
    constructor(cb: ResizeObserverCallback) {
      this.cb = cb;
    }
    observe(target: Element): void {
      stub.observed.set(target, this.cb);
      if (stub.fireOnObserve) {
        // Fire asynchronously to mirror the real browser, which delivers
        // observations in a microtask after observe(). The test flushes
        // these via the `flushResizeObservers` helper below.
        queueMicrotask(() => this.fire(target));
      }
    }
    unobserve(target: Element): void {
      stub.observed.delete(target);
    }
    disconnect(): void {
      // No-op: the per-row observer and the Transcript-internal observer
      // each get disconnected individually in their own teardown.
    }
    /** Test helper: synchronously fire the callback for one target with the
     *  rect appropriate to *that* target. Mirrors the structure of a real
     *  ResizeObserverEntry: a `target` and a `borderBoxSize` so the
     *  virtualizer's `measureElement` path (which reads `entry.borderBoxSize`)
     *  sees the same shape it would in Chromium. */
    fire(target: Element): void {
      const cb = stub.observed.get(target);
      if (!cb) return;
      // Pick the height to report for this element. Rows get their real
      // height (read via the data-index → global lookup); the scroll
      // element gets the viewport; everything else gets the stub's
      // catch-all rect. Without this per-element lookup the rows would
      // be reported as `height: 0` on the first synchronous fire after
      // observe(), and the virtualizer's `itemSizeCache` would end up
      // filled with zeros — masking exactly the bug we're testing for.
      let width: number;
      let height: number;
      const tag = (target as HTMLElement).tagName;
      if (tag === "SECTION") {
        width = globalThis.__sectionWidth ?? SECTION_WIDTH;
        height = VIEWPORT_HEIGHT;
      } else {
        const idx = target.getAttribute?.("data-index");
        if (idx !== null && idx !== undefined && globalThis.__realRowHeight) {
          height = globalThis.__realRowHeight(Number(idx)) ?? 0;
        } else {
          height = stub.rect.height;
        }
        width = SECTION_WIDTH;
      }
      const entry = {
        target,
        contentRect: {
          x: 0,
          y: 0,
          width,
          height,
          top: 0,
          left: 0,
          right: width,
          bottom: height,
        },
        borderBoxSize: [{ inlineSize: width, blockSize: height }],
        contentBoxSize: [{ inlineSize: width, blockSize: height }],
        devicePixelContentBoxSize: [{ inlineSize: width, blockSize: height }],
      };
      // ResizeObserver callbacks are invoked with a list of entries.
      cb([entry as unknown as ResizeObserverEntry], this as unknown as ResizeObserver);
    }
  } as unknown as typeof ResizeObserver;

  // Stub HTMLElement.prototype.offsetHeight — the fallback path the
  // virtualizer takes when `measureElement` is called with `void 0` for
  // the entry (the mount-time path, see `react-virtual` `measureElement`
  // in src/index.tsx and virtual-core's `measureElement` default at
  // index.js:127-146). Reading `data-index` is enough to map back to the
  // message we want a height for. Note these are defined on
  // `HTMLElement.prototype` in jsdom (not `Element.prototype`), so the
  // overrides have to land there too.
  //
  // The section element is identified by `tagName === "SECTION"` rather
  // than a cached global — caching requires picking the section out of
  // the DOM, but the first call to `getRect(scrollElement)` happens
  // *during* `_willUpdate`, which is itself the first time we see the
  // section. Tagging by element kind keeps the stub order-independent.
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: Element): number {
      const tag = (this as HTMLElement).tagName;
      if (tag === "SECTION") return VIEWPORT_HEIGHT;
      const idx = this.getAttribute?.("data-index");
      if (idx !== null && idx !== undefined && globalThis.__realRowHeight) {
        const h = globalThis.__realRowHeight(Number(idx));
        if (h !== undefined) return h;
      }
      return 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get(this: Element): number {
      const tag = (this as HTMLElement).tagName;
      if (tag === "SECTION") return globalThis.__sectionWidth;
      return SECTION_WIDTH;
    },
  });

  // Stub getBoundingClientRect — used by the virtualizer's `getRect` and by
  // the row measureElement path's borderBoxSize fallback (real browsers
  // report via borderBoxSize; in jsdom we have to make it consistent).
  Element.prototype.getBoundingClientRect = function (): DOMRect {
    const tag = (this as HTMLElement).tagName;
    if (tag === "SECTION") {
      return {
        x: 0,
        y: 0,
        width: globalThis.__sectionWidth,
        height: VIEWPORT_HEIGHT,
        top: 0,
        left: 0,
        right: globalThis.__sectionWidth,
        bottom: VIEWPORT_HEIGHT,
        toJSON() {
          return this;
        },
      } as DOMRect;
    }
    const idx = this.getAttribute?.("data-index");
    if (idx !== null && idx !== undefined && globalThis.__realRowHeight) {
      const h = globalThis.__realRowHeight(Number(idx));
      if (h !== undefined) {
        return {
          x: 0,
          y: 0,
          width: SECTION_WIDTH,
          height: h,
          top: 0,
          left: 0,
          right: SECTION_WIDTH,
          bottom: h,
          toJSON() {
            return this;
          },
        } as DOMRect;
      }
    }
    return {
      x: 0,
      y: 0,
      width: 0,
      height: 0,
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      toJSON() {
        return this;
      },
    } as DOMRect;
  };

  // Stub clientWidth/clientHeight on HTMLElement.prototype (jsdom defines
  // them there, not on Element.prototype) so the virtualizer sees a
  // non-zero viewport (jsdom defaults to 0 → no rows mounted → empty
  // transcript). The Transcript component reads `clientWidth` from inside
  // its own ResizeObserver too, so this same getter serves both call
  // sites. We define the getter on HTMLElement.prototype but honour a
  // per-element override (the section) so the width-change trigger below
  // can mutate it. Tag-based identification keeps the stub
  // order-independent (see the offsetHeight note above).
  const clientHeightGetter = {
    configurable: true,
    get(this: Element): number {
      const tag = (this as HTMLElement).tagName;
      if (tag === "SECTION") return VIEWPORT_HEIGHT;
      // Per-row clientHeight doesn't matter for the bug — return 0.
      return 0;
    },
  };
  Object.defineProperty(HTMLElement.prototype, "clientHeight", clientHeightGetter);

  const clientWidthGetter = {
    configurable: true,
    get(this: Element): number {
      const tag = (this as HTMLElement).tagName;
      // The section's width is the value the Transcript-internal
      // ResizeObserver compares against `lastWidth`. We track it via a
      // mutable variable so the test can flip it.
      if (tag === "SECTION") return globalThis.__sectionWidth;
      return SECTION_WIDTH;
    },
  };
  Object.defineProperty(HTMLElement.prototype, "clientWidth", clientWidthGetter);

  // jsdom does not implement `scrollTo` on HTMLElement — but the virtualizer
  // calls it on every `scrollToIndex`/`scrollToEnd` to actually move the
  // viewport. Without a stub those calls throw and the layout effect chain
  // fails silently, which (in our setup) prevents the rows from mounting.
  // The stub is a no-op: the virtualizer treats it as "I asked the browser
  // to scroll", and the test reads positions directly off `data-index` /
  // `transform: translateY(...)` rather than via `scrollTop`. We also stub
  // `scrollTop` so reads return 0 (jsdom defaults to 0 anyway; this is
  // belt-and-braces).
  HTMLElement.prototype.scrollTo = function scrollTo(): void {
    // No-op: see comment above.
  };
  Object.defineProperty(HTMLElement.prototype, "scrollTop", {
    configurable: true,
    get(this: Element): number {
      return 0;
    },
    set(this: Element, _value: number): void {
      // No-op: see comment above.
      void _value;
    },
  });
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
    configurable: true,
    get(this: Element): number {
      return VIEWPORT_HEIGHT;
    },
  });
});

afterEach(() => {
  // Clean up our HTMLElement.prototype patches so other tests aren't
  // affected. `delete` on own-configurable getters works; the prototype
  // chain picks up the original (jsdom) implementations.
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .offsetHeight;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .offsetWidth;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .clientHeight;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .clientWidth;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .scrollTop;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .scrollHeight;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)
    .scrollTo;
  delete (Element.prototype as unknown as Record<string, unknown>)
    .getBoundingClientRect;
  delete (globalThis as unknown as Record<string, unknown>).__realRowHeight;
  delete (globalThis as unknown as Record<string, unknown>).__sectionEl;
  delete (globalThis as unknown as Record<string, unknown>).__sectionWidth;
  // Tear down any container we left in document.body.
  document.body.innerHTML = "";
});

// Extend the global scope with the small surface area our Element stubs
// reach for. Typed locally so TypeScript is happy; the assignment uses
// `globalThis` so the prototype getters above can read it.
declare global {
  var __realRowHeight: ((idx: number) => number) | undefined;
  var __sectionEl: HTMLElement | undefined;
  var __sectionWidth: number;
}

// ─── test fixtures ─────────────────────────────────────────────────────────

/** Build a single `WireChatMessage` of a known content length. Only `role`,
 *  `content` and `timestamp` are required on the wire type; the rest is
 *  optional and ignored by both `adaptMessage` and `estimateRowHeight`. */
function msg(content: string, role: WireChatMessage["role"] = "agent"): WireChatMessage {
  return { role, content, timestamp: "2026-01-01T00:00:00Z" };
}

/** A varied transcript: short user line, short agent line, a long agent
 *  message, a tool call, a tool result with a multi-line body, a thinking
 *  block, and so on. The exact content is unimportant; what matters is
 *  that each message has a different `estimateRowHeight`, so the bug
 *  surfaces as a visible disagreement between the two layouts. */
function fixtureMessages(): WireChatMessage[] {
  return [
    msg("hi", "user"),
    msg("hello, how can I help?"),
    msg("a".repeat(180)),
    msg("b".repeat(450)),
    msg("c".repeat(900)),
    msg("ls -la /tmp", "toolcall"),
    msg("file1\nfile2\nfile3\nfile4\nfile5", "toolresult"),
    msg("d".repeat(1200)),
    msg("e".repeat(60)),
    msg("summary of the above", "summary"),
    msg("ok", "user"),
  ];
}

// ─── helpers ───────────────────────────────────────────────────────────────

/** Read the rendered layout of every row the virtualizer mounted: the
 *  `translateY` it asked React to write and the real height we'd report
 *  for that row. Sorted by `data-index` so the test can compare adjacent
 *  rows in the natural reading order. */
interface RowLayout {
  index: number;
  translateY: number;
  realHeight: number;
}

function readLayout(realHeights: number[]): RowLayout[] {
  const rows: RowLayout[] = [];
  for (const el of Array.from(document.querySelectorAll("[data-index]"))) {
    const idx = Number(el.getAttribute("data-index"));
    const style = (el as HTMLElement).style.transform;
    // The Transcript writes `transform: translateY(<start>px)` verbatim.
    const m = /translateY\(([0-9.-]+)px\)/.exec(style);
    if (!m) continue;
    rows.push({
      index: idx,
      translateY: Number(m[1]),
      realHeight: realHeights[idx],
    });
  }
  rows.sort((a, b) => a.index - b.index);
  return rows;
}

/** Find the scroll element the Transcript component creates and remember it
 *  on the global so the clientWidth/Height getters can special-case it. */
function rememberSection(): HTMLElement {
  const section = document.querySelector("section");
  if (!section) throw new Error("Transcript did not render a <section>");
  globalThis.__sectionEl = section as HTMLElement;
  return section as HTMLElement;
}

/** Drain microtasks (the ResizeObserver stub queues its initial fire via
 *  `queueMicrotask`) and then a few animation frames, because `useEffect`
 *  callbacks run in a microtask and React 19 batches state updates behind
 *  rAF in some paths. After this returns, all effects and observer fires
 *  triggered by `act` should have settled. */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise<void>((resolve) => {
      // Two microtask ticks plus one rAF — enough for React's effect
      // queue, the stub's microtask-fired observer, and a possible rAF
      // scheduled by `useTranscriptScroll`'s onScroll.
      queueMicrotask(() =>
        queueMicrotask(() => requestAnimationFrame(() => resolve())),
      );
    });
  });
}

// ─── tests ─────────────────────────────────────────────────────────────────

describe("Transcript — virtualizer layout under scroll-element width change", () => {
  it("renders every visible row at non-overlapping translateY positions, both before and after a width-triggered measure()", async () => {
    const messages = fixtureMessages();
    const realHeights = messages.map((m) => estimateRowHeight(m) * REAL_MULTIPLIER);
    // Real height must differ from the seed estimate — otherwise the test
    // cannot distinguish a correct measurement from a stuck estimate.
    for (let i = 0; i < messages.length; i++) {
      expect(realHeights[i]).toBeGreaterThan(estimateRowHeight(messages[i]));
    }
    globalThis.__realRowHeight = (i) => realHeights[i];
    globalThis.__sectionWidth = SECTION_WIDTH;

    const container = document.createElement("div");
    container.style.width = `${SECTION_WIDTH}px`;
    container.style.height = `${VIEWPORT_HEIGHT}px`;
    document.body.appendChild(container);

    let root: Root | undefined;
    try {
      await act(async () => {
        root = createRoot(container);
        root.render(
          <Transcript messages={messages} drawerOpen={false} />,
        );
      });
    } catch (e) {
      console.error("Error during initial render:", e);
      throw e;
    }
    const section = rememberSection();

    // The component's effect chain fires a few microtasks deep: a single
    // layout-effect pass (which sets up the scroll-element ResizeObserver
    // and the per-row observers), the React 19 wrapper's flushSync inside
    // onChange, and the virtualizer's reconcileScroll via rAF. Wait for a
    // pair of microtask + rAF cycles so all of those settle.
    await act(async () => {
      await new Promise<void>((resolve) => {
        queueMicrotask(() =>
          queueMicrotask(() => requestAnimationFrame(() => resolve())),
        );
      });
    });
    await act(async () => {
      await new Promise<void>((resolve) => {
        queueMicrotask(() =>
          queueMicrotask(() => requestAnimationFrame(() => resolve())),
        );
      });
    });
    await flush();

    // ── Pre-trigger invariant (should PASS today) ────────────────────────
    // After mount + ResizeObserver firing, every row's translateY must be
    // at least the previous row's translateY plus the previous row's real
    // height. If the virtualizer did its job — measured each row's border
    // box — this holds.
    const preLayout = readLayout(realHeights);
    expect(preLayout.length).toBeGreaterThan(1);

    // Total container height must cover the last row. The Transcript sets
    // it via `virtualizer.getTotalSize()`. With real heights in the cache,
    // it should reach past the bottom of the last row. `element.style.height`
    // returns the *value* (e.g. "4026px"), not the full declaration, so the
    // regex matches just the number.
    const inner = container.querySelector("section > div") as HTMLElement | null;
    expect(inner).not.toBeNull();
    const preTotal = Number(/^([0-9.]+)px$/.exec(inner!.style.height)?.[1] ?? "0");
    const lastPre = preLayout[preLayout.length - 1];
    expect(preTotal).toBeGreaterThanOrEqual(lastPre.translateY + lastPre.realHeight);

    for (let i = 1; i < preLayout.length; i++) {
      const prev = preLayout[i - 1];
      const cur = preLayout[i];
      expect(
        cur.translateY,
        `pre-trigger: row ${cur.index} (translateY=${cur.translateY}) overlaps row ${prev.index} ` +
          `(translateY=${prev.translateY}, height=${prev.realHeight}); minimum required ` +
          `translateY for row ${cur.index} is ${prev.translateY + prev.realHeight}`,
      ).toBeGreaterThanOrEqual(prev.translateY + prev.realHeight);
    }

    // ── Trigger ──────────────────────────────────────────────────────────
    // Simulate the bug's reproducer: the right-hand drawer opens. The
    // scroll element's content width shrinks. The Transcript-internal
    // ResizeObserver sees the width change, calls `virtualizer.measure()`,
    // which clears `itemSizeCache`. We need to:
    //
    //   1. Change `clientWidth` on the section so the Transcript's
    //      `width === lastWidth` guard releases and `virtualizer.measure()`
    //      actually runs.
    //   2. Fire the section's ResizeObserver callback with the new rect so
    //      the Transcript-internal observer's callback runs.
    //   3. NOT fire any per-row ResizeObserver — the rows haven't changed
    //      size in real life (the drawer overlays, it doesn't reflow the
    //      rows), so the virtualizer's own measurement path will not be
    //      re-triggered. That's the whole point of the bug.
    globalThis.__sectionWidth = SECTION_WIDTH - 256; // drawer stole 256px
    await act(async () => {
      // Synthesize the callback delivery as if the browser had observed
      // the width change. We hand-craft an entry because the trigger
      // path is the *Transcript's* observer, not the library's — it
      // doesn't read the entry's contentRect, only `el.clientWidth`.
      const cb = stub.observed.get(section);
      // The Transcript's observer only reads `el.clientWidth`; the entry
      // shape doesn't matter. But the library's scroll-element observer
      // (a separate one) also exists if it fires — guard against both
      // being absent.
      if (cb) {
        const entry = {
          target: section,
          contentRect: {
            x: 0,
            y: 0,
            width: SECTION_WIDTH - 256,
            height: VIEWPORT_HEIGHT,
            top: 0,
            left: 0,
            right: SECTION_WIDTH - 256,
            bottom: VIEWPORT_HEIGHT,
          },
          borderBoxSize: [
            {
              inlineSize: SECTION_WIDTH - 256,
              blockSize: VIEWPORT_HEIGHT,
            },
          ],
          contentBoxSize: [
            {
              inlineSize: SECTION_WIDTH - 256,
              blockSize: VIEWPORT_HEIGHT,
            },
          ],
          devicePixelContentBoxSize: [
            {
              inlineSize: SECTION_WIDTH - 256,
              blockSize: VIEWPORT_HEIGHT,
            },
          ],
        };
        cb([entry as unknown as ResizeObserverEntry], {} as ResizeObserver);
      }
    });
    await flush();

    // ── Post-trigger invariant (expected to FAIL today) ─────────────────
    // After the width change + virtualizer.measure(), the same non-overlap
    // property must hold. On a healthy implementation it does. On the
    // current implementation it does not: rows are at estimate-based
    // offsets and the next row's translateY is less than the current
    // row's translateY + real height.
    const postLayout = readLayout(realHeights);
    expect(postLayout.length).toBeGreaterThan(1);

    const innerPost = container.querySelector("section > div") as HTMLElement | null;
    expect(innerPost).not.toBeNull();
    const postTotal = parseFloat(innerPost!.style.height) || 0;
    const lastPost = postLayout[postLayout.length - 1];
    expect(postTotal).toBeGreaterThanOrEqual(
      lastPost.translateY + lastPost.realHeight,
    );

    for (let i = 1; i < postLayout.length; i++) {
      const prev = postLayout[i - 1];
      const cur = postLayout[i];
      expect(
        cur.translateY,
        `post-trigger: row ${cur.index} (translateY=${cur.translateY}) overlaps row ${prev.index} ` +
          `(translateY=${prev.translateY}, height=${prev.realHeight}); minimum required ` +
          `translateY for row ${cur.index} is ${prev.translateY + prev.realHeight}`,
      ).toBeGreaterThanOrEqual(prev.translateY + prev.realHeight);
    }

    // Sanity: the rows' DOM elements should still be present (no remount
    // happened) so a passing fix would have to drive measurement through
    // the rows themselves, not by remounting.
    expect(document.querySelectorAll("[data-index]").length).toBe(preLayout.length);

    root?.unmount();
  });
});
