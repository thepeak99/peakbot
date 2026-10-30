import { useCallback, useEffect, useRef, useState } from "react";
import type { TabView } from "../convoTabs";

// Desktop-only strip of open conversations (issue #373). Purely
// presentational: the tab list, statuses and callbacks come from
// `useConvoTabs`. Styling lives in the `.ctab*` block of index.css.
//
// Tabs shrink browser-style: `.ctab { flex: 0 1 200px; min-width: 36px }`,
// so a few tabs sit at 200px and many shrink evenly down to the 36px floor,
// after which the strip scrolls. ‹ › arrows appear only when the tabs
// cannot fit at that floor; closing a tab with the mouse over the bar
// freezes the remaining tabs' widths so the next ✕ stays under the cursor.

export const MIN_TAB_PX = 36; // == .ctab { min-width } in index.css
export const TAB_GAP_PX = 2; // == .ctab-scroll { gap } in index.css

// Fixed chrome of the bar (kept in sync with the .ctab* CSS): padding, the
// "+" button (plus its margin), both arrows (always accounted for, even
// while hidden — the decision must not depend on the arrows' own state)
// and the bar's child gaps. Subtracting a constant, not the arrows' live
// offsetWidth, is what makes the overflow decision stateless and flicker-free.
const NAV_PADDING_PX = 8 * 2; // .ctabbar padding 0 8px
const PLUS_PX = 26 + 2; // .ctab-btn (26px) + .ctab-btn.plus margin-left
const ARROW_PX = 22 * 2; // .ctab-btn.arrow width, both of them
const BAR_GAPS_PX = 2 * 3; // .ctabbar gap 2px × up to 3 child gaps
const FIXED_CHROME_PX = NAV_PADDING_PX + PLUS_PX + ARROW_PX + BAR_GAPS_PX;

/** Which edges of a horizontally scrolling strip have hidden content.
 * 1px tolerance absorbs sub-pixel rounding. */
export function overflowEdges(
  scrollLeft: number,
  scrollWidth: number,
  clientWidth: number,
): { left: boolean; right: boolean } {
  return {
    left: scrollLeft > 1,
    right: scrollLeft + clientWidth < scrollWidth - 1,
  };
}

/** True iff the tabs cannot fit at their minimum width. Pure and stateless:
 * the same input always gives the same answer, so showing the arrows can
 * never flip it. Equality fits. */
export function tabsOverflow(o: {
  tabCount: number;
  minTabPx: number;
  gapPx: number;
  availPx: number;
}): boolean {
  const n = o.tabCount;
  if (n <= 0) return false;
  return n * o.minTabPx + (n - 1) * o.gapPx > o.availPx;
}

export function ConvoTabBar({
  tabs,
  onActivate,
  onClose,
  onNew,
}: {
  tabs: TabView[];
  onActivate: (i: number) => void;
  onClose: (i: number) => void;
  onNew: () => void;
}) {
  const navRef = useRef<HTMLElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ left: false, right: false });
  const [atEnds, setAtEnds] = useState({ left: true, right: true });
  const [over, setOver] = useState(false);
  // Close-freeze (Chrome-style): a mouse over the bar pins the surviving
  // tabs' pre-close widths so nothing reflows under the cursor.
  const mouseOverRef = useRef(false);
  const pinsRef = useRef(new Map<string, number>());
  const [frozen, setFrozen] = useState(false);
  // Live button refs so the hold interval can read disabled-ness.
  const arrowBtnsRef = useRef<{
    left: HTMLButtonElement | null;
    right: HTMLButtonElement | null;
  }>({ left: null, right: null });
  const holdRef = useRef<{ dir: 1 | -1; timer: number | null }>({
    dir: 1,
    timer: null,
  });

  const unfreeze = useCallback(() => {
    pinsRef.current.clear();
    setFrozen(false);
  }, []);

  /** Re-read the bar's width and re-decide the arrows (stateless predicate),
   * plus the arrows' disabled-ness from the strip's scroll metrics. Runs on
   * mount, on every tab-count change and on every ResizeObserver callback
   * (which ignores its entries and re-reads the metrics). */
  const recompute = useCallback(() => {
    const nav = navRef.current;
    const strip = scrollRef.current;
    if (!nav) return;
    const avail = nav.clientWidth - FIXED_CHROME_PX;
    setOver(
      tabsOverflow({
        tabCount: tabs.length,
        minTabPx: MIN_TAB_PX,
        gapPx: TAB_GAP_PX,
        availPx: avail,
      }),
    );
    if (strip) {
      setAtEnds((prev) => {
        const left = strip.scrollLeft <= 1;
        const right =
          strip.scrollLeft >= strip.scrollWidth - strip.clientWidth - 1;
        return prev.left === left && prev.right === right
          ? prev
          : { left, right };
      });
    }
  }, [tabs.length]);

  const onScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setEdges(overflowEdges(el.scrollLeft, el.scrollWidth, el.clientWidth));
    setAtEnds((prev) => {
      const left = el.scrollLeft <= 1;
      const right = el.scrollLeft >= el.scrollWidth - el.clientWidth - 1;
      return prev.left === left && prev.right === right ? prev : { left, right };
    });
  }, []);

  const revealActive = useCallback(() => {
    // Optional-chained: there may be no active tab (all closed).
    scrollRef.current
      ?.querySelector(".ctab.active")
      ?.scrollIntoView?.({ inline: "nearest", block: "nearest" });
  }, []);

  useEffect(() => {
    const strip = scrollRef.current;
    if (!strip) return;
    strip.addEventListener("scroll", onScroll);
    // Vertical wheel scrolls the strip sideways (needs a non-passive listener).
    const onWheel = (e: WheelEvent) => {
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
        strip.scrollLeft += e.deltaY;
        e.preventDefault();
      }
    };
    strip.addEventListener("wheel", onWheel, { passive: false });
    const ro =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => {
            // Entries are ignored on purpose: re-read the metrics so the
            // decision can't depend on stale sizes.
            recompute();
            revealActive();
          })
        : null;
    ro?.observe(strip);
    if (navRef.current) ro?.observe(navRef.current);
    return () => {
      strip.removeEventListener("scroll", onScroll);
      strip.removeEventListener("wheel", onWheel);
      ro?.disconnect();
    };
  }, [onScroll, recompute, revealActive]);

  // Tabs came or went: re-decide the arrows, and keep the active tab in view.
  const activeKey = tabs.find((t) => t.active)?.key;
  useEffect(() => {
    recompute();
    revealActive();
  }, [recompute, activeKey, revealActive]);

  // A tab was ADDED: the strip re-flows, so drop any close-freeze.
  const prevCountRef = useRef(tabs.length);
  useEffect(() => {
    if (tabs.length > prevCountRef.current) unfreeze();
    prevCountRef.current = tabs.length;
  }, [tabs.length, unfreeze]);

  // ── close-freeze ────────────────────────────────────────────────────
  const handlePointerIn = (e: React.PointerEvent) => {
    if (e.pointerType === "mouse") mouseOverRef.current = true;
  };
  const handlePointerOut = (e: React.PointerEvent) => {
    // pointerout also fires when moving between children; only a real
    // departure (relatedTarget outside the bar) ends the hover.
    const nav = navRef.current;
    const rt = e.relatedTarget;
    if (nav && rt instanceof Node && nav.contains(rt)) return;
    mouseOverRef.current = false;
    unfreeze();
  };

  /** Close a tab. With a mouse over the bar, first pin the surviving tabs'
   * widths as measured BEFORE the removal, so nothing reflows under the
   * cursor. Already-pinned widths persist across further closes. */
  const handleTabClose = (i: number) => {
    const nav = navRef.current;
    if (mouseOverRef.current && nav) {
      const pins = pinsRef.current;
      if (pins.size === 0) {
        const els = nav.querySelectorAll<HTMLElement>(".ctab");
        els.forEach((el, idx) => {
          if (idx === i) return;
          const w = el.getBoundingClientRect().width;
          pins.set(tabs[idx].key, w);
        });
      }
      setFrozen(true);
    }
    onClose(i);
  };

  // ── arrows ──────────────────────────────────────────────────────────
  const stepScroll = (dir: 1 | -1) => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * 0.8 * el.clientWidth, behavior: "smooth" });
  };
  const stopHold = useCallback(() => {
    if (holdRef.current.timer !== null) {
      clearInterval(holdRef.current.timer);
      holdRef.current.timer = null;
    }
  }, []);
  const onArrowDown = (dir: 1 | -1) => (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault(); // one step per gesture: also suppresses the synthetic click
    const btn = e.currentTarget;
    try {
      btn.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture — holding still works */
    }
    stopHold();
    holdRef.current.dir = dir;
    stepScroll(dir);
    holdRef.current.timer = window.setInterval(() => {
      const b = arrowBtnsRef.current[dir === 1 ? "right" : "left"];
      if (b?.disabled) {
        stopHold();
        return;
      }
      stepScroll(dir);
    }, 90);
  };

  return (
    <nav
      ref={navRef}
      className="ctabbar"
      role="tablist"
      aria-label="Conversations"
      onPointerEnter={handlePointerIn}
      onPointerOver={handlePointerIn}
      onPointerLeave={handlePointerOut}
      onPointerOut={handlePointerOut}
    >
      <button
        ref={(el) => {
          arrowBtnsRef.current.left = el;
        }}
        type="button"
        className="ctab-btn arrow"
        title="Scroll tabs left"
        aria-label="Scroll tabs left"
        hidden={!over}
        disabled={atEnds.left}
        onClick={() => stepScroll(-1)}
        onPointerDown={onArrowDown(-1)}
        onPointerUp={stopHold}
        onPointerCancel={stopHold}
      >
        ‹
      </button>
      <div
        ref={scrollRef}
        className={`ctab-scroll${edges.left ? " fade-l" : ""}${edges.right ? " fade-r" : ""}`}
      >
        {tabs.map((t, i) => (
          <div
            key={t.key}
            role="tab"
            tabIndex={0}
            aria-selected={t.active}
            title={t.tooltip}
            className={`ctab${t.active ? " active" : ""}`}
            style={
              frozen && pinsRef.current.has(t.key)
                ? { flex: `0 0 ${pinsRef.current.get(t.key)}px` }
                : undefined
            }
            onClick={() => onActivate(i)}
            onKeyDown={(e) => {
              if (e.target !== e.currentTarget) return;
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onActivate(i);
              }
            }}
          >
            <span className="icon" aria-hidden="true" />
            {t.running ? (
              <span className="spin" title="Agent running" />
            ) : t.unread && !t.active ? (
              <span className="unread" title="New reply" />
            ) : null}
            <span className={`title${t.untitled ? " untitled" : ""}`}>
              {t.label}
            </span>
            <button
              type="button"
              className="x"
              aria-label="Close tab"
              title="Close tab"
              onClick={(e) => {
                e.stopPropagation();
                handleTabClose(i);
              }}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <button
        ref={(el) => {
          arrowBtnsRef.current.right = el;
        }}
        type="button"
        className="ctab-btn arrow"
        title="Scroll tabs right"
        aria-label="Scroll tabs right"
        hidden={!over}
        disabled={atEnds.right}
        onClick={() => stepScroll(1)}
        onPointerDown={onArrowDown(1)}
        onPointerUp={stopHold}
        onPointerCancel={stopHold}
      >
        ›
      </button>
      <button
        type="button"
        className="ctab-btn plus"
        title="New conversation"
        aria-label="New conversation tab"
        onClick={onNew}
      >
        +
      </button>
    </nav>
  );
}
