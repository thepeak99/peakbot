import { useCallback, useEffect, useRef, useState } from "react";
import type { TabView } from "../convoTabs";

// Desktop-only strip of open conversations (issue #373). Purely
// presentational: the tab list, statuses and callbacks come from
// `useConvoTabs`. Styling lives in the `.ctab*` block of index.css.

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
  const scrollRef = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ left: false, right: false });

  const measure = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const next = overflowEdges(el.scrollLeft, el.scrollWidth, el.clientWidth);
    setEdges((prev) =>
      prev.left === next.left && prev.right === next.right ? prev : next,
    );
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.addEventListener("scroll", measure);
    // Vertical wheel scrolls the strip sideways (needs a non-passive listener).
    const onWheel = (e: WheelEvent) => {
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
        el.scrollLeft += e.deltaY;
        e.preventDefault();
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    const ro =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(measure)
        : null;
    ro?.observe(el);
    return () => {
      el.removeEventListener("scroll", measure);
      el.removeEventListener("wheel", onWheel);
      ro?.disconnect();
    };
  }, [measure]);

  // Tabs came or went: re-measure, and keep the active tab in view.
  const activeKey = tabs.find((t) => t.active)?.key;
  useEffect(() => {
    measure();
    scrollRef.current
      ?.querySelector(".ctab.active")
      ?.scrollIntoView?.({ inline: "nearest", block: "nearest" });
  }, [tabs.length, activeKey, measure]);

  const scrollByPage = (dir: 1 | -1) => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * 0.8 * el.clientWidth, behavior: "smooth" });
  };

  const over = edges.left || edges.right;

  return (
    <nav
      className="ctabbar"
      role="tablist"
      aria-label="Conversations"
    >
      <button
        type="button"
        className="ctab-btn arrow"
        title="Scroll tabs left"
        hidden={!edges.left}
        onClick={() => scrollByPage(-1)}
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
            onClick={() => onActivate(i)}
            onKeyDown={(e) => {
              if (e.target !== e.currentTarget) return;
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onActivate(i);
              }
            }}
          >
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
                onClose(i);
              }}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        className="ctab-btn arrow"
        title="Scroll tabs right"
        hidden={!edges.right}
        onClick={() => scrollByPage(1)}
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
      <div className="ctab-spacer" />
      <span className="ctab-count" hidden={!over}>
        {tabs.length} tabs
      </span>
    </nav>
  );
}
