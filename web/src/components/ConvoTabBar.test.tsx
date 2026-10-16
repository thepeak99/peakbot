// Tests for `components/ConvoTabBar.tsx` (design §5 + the mock's `.ctab*`
// markup, which the design says to port as-is):
//
//   export function ConvoTabBar(p: { tabs: TabView[]; onActivate(i: number): void;
//                                     onClose(i: number): void; onNew(): void }): JSX.Element;
//   export function overflowEdges(scrollLeft, scrollWidth, clientWidth):
//     { left: boolean; right: boolean };  // 1px tolerance
//
// Contract:
//   - role="tablist"; each tab = role="tab", aria-selected, title = tooltip
//   - label = title || "New conversation"; untitled renders italic
//   - active tab: the `active` class; running → spinner; the ACTIVE tab's
//     running state is live (activeRunning), its unread is always off
//   - unread dot ONLY on inactive tabs
//   - × = button aria-label="Close tab" (stopPropagation: closing must not
//     activate); "+" = button aria-label="New conversation tab"
//   - fades, ‹ › arrows and the "N tabs" counter shown iff overflowEdges
//     says so; arrows scroll by 0.8*clientWidth
//
// RED before implementation: `./ConvoTabBar` does not exist yet, so this
// file fails at import — the right reason (feature absent). The type-only
// import of `TabView` is erased at runtime.
//
// Harness modeled on TopBar.test.tsx (createRoot/act off react-dom/client;
// jsdom auto-enabled for `src/components/**` by vitest.config.ts). jsdom has
// no layout, so the overflow tests drive the component the way the page
// would: set the scroll container's metrics, then fire its scroll event
// (the design's "scroll listener"). `scrollIntoView` is stubbed because
// jsdom does not implement it and the design calls it unguarded.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConvoTabBar, overflowEdges } from "./ConvoTabBar";
import type { TabView } from "../convoTabs";

// ─── fixtures ────────────────────────────────────────────────────────────

const view = (over: Partial<TabView> = {}): TabView => ({
  key: "k",
  label: "Label",
  tooltip: "Label",
  untitled: false,
  active: false,
  running: false,
  unread: false,
  ...over,
});

interface Props {
  tabs: TabView[];
  onActivate: (i: number) => void;
  onClose: (i: number) => void;
  onNew: () => void;
}

// ─── mount helper ────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: Props): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(
      <ConvoTabBar
        tabs={props.tabs}
        onActivate={props.onActivate}
        onClose={props.onClose}
        onNew={props.onNew}
      />,
    );
  });
  return container;
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no scrollIntoView; the design calls it for the active tab.
  Element.prototype.scrollIntoView = vi.fn();
});

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
});

const tabsOf = (el: HTMLElement): HTMLElement[] => [
  ...el.querySelectorAll<HTMLElement>('[role="tab"]'),
];

/** Typed querySelector for elements we click / read `.hidden` from. */
const q = (el: ParentNode, sel: string): HTMLElement | null =>
  el.querySelector<HTMLElement>(sel);

// ─── rendering ───────────────────────────────────────────────────────────

describe("ConvoTabBar — rendering", () => {
  it("renders a tablist with one role=tab per view, in order", async () => {
    const el = await mount({
      tabs: [
        view({ key: "a", label: "Fix login", active: true }),
        view({ key: "b", label: "Refactor" }),
        view({ key: "new:2", label: "New conversation", untitled: true }),
      ],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    expect(el.querySelector('[role="tablist"]')).not.toBeNull();
    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(3);
    expect(tabs.map((t) => t.textContent)).toEqual([
      expect.stringContaining("Fix login"),
      expect.stringContaining("Refactor"),
      expect.stringContaining("New conversation"),
    ]);
  });

  it("marks the active tab with aria-selected and the active class, the rest without", async () => {
    const el = await mount({
      tabs: [view({ key: "a", active: true }), view({ key: "b" }), view({ key: "c" })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    const [a, b, c] = tabsOf(el);
    expect(a.getAttribute("aria-selected")).toBe("true");
    expect(b.getAttribute("aria-selected")).toBe("false");
    expect(c.getAttribute("aria-selected")).toBe("false");
    expect(a.classList.contains("active")).toBe(true);
    expect(b.classList.contains("active")).toBe(false);
  });

  it("sets the tab title attribute to the tooltip", async () => {
    const el = await mount({
      tabs: [view({ key: "a", label: "Short", tooltip: "A much longer tooltip" })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    expect(tabsOf(el)[0].getAttribute("title")).toBe("A much longer tooltip");
  });

  it("renders an untitled tab with the italic (untitled) title style", async () => {
    const el = await mount({
      tabs: [view({ key: "new:0", label: "New conversation", untitled: true, active: true })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    const title = tabsOf(el)[0].querySelector(".title");
    expect(title, "a .title element must render").not.toBeNull();
    expect(title!.classList.contains("untitled")).toBe(true);
    expect(title!.textContent).toBe("New conversation");
  });
});

// ─── status indicators ───────────────────────────────────────────────────

describe("ConvoTabBar — running spinner and unread dot", () => {
  it("shows the spinner on a running tab and none on idle tabs", async () => {
    const el = await mount({
      tabs: [
        view({ key: "a", active: true, running: true }),
        view({ key: "b", running: false }),
        view({ key: "c", running: true }),
      ],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    const [a, b, c] = tabsOf(el);
    expect(a.querySelector(".spin"), "active running tab: spinner").not.toBeNull();
    expect(b.querySelector(".spin"), "idle tab: no spinner").toBeNull();
    expect(c.querySelector(".spin"), "background running tab: spinner").not.toBeNull();
  });

  it("shows the unread dot on an inactive tab with unread", async () => {
    const el = await mount({
      tabs: [view({ key: "a", active: true }), view({ key: "b", unread: true })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    const [a, b] = tabsOf(el);
    expect(a.querySelector(".unread")).toBeNull();
    expect(b.querySelector(".unread"), "inactive unread tab: dot").not.toBeNull();
  });

  it("shows no unread dot on an inactive tab without unread", async () => {
    const el = await mount({
      tabs: [view({ key: "a", active: true }), view({ key: "b" })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    expect(tabsOf(el)[1].querySelector(".unread")).toBeNull();
  });

  it("never shows the unread dot on the active tab, even if the view says unread", async () => {
    // Defensive: the active tab's unread is always off (tabViews enforces
    // it); the component must not render a dot on the active tab anyway.
    const el = await mount({
      tabs: [view({ key: "a", active: true, unread: true, running: true })],
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    expect(tabsOf(el)[0].querySelector(".unread")).toBeNull();
  });
});

// ─── interactions ────────────────────────────────────────────────────────

describe("ConvoTabBar — interactions", () => {
  it("clicking a tab calls onActivate with its index (and not onClose)", async () => {
    const onActivate = vi.fn();
    const onClose = vi.fn();
    const el = await mount({
      tabs: [view({ key: "a", active: true }), view({ key: "b" }), view({ key: "c" })],
      onActivate,
      onClose,
      onNew: () => {},
    });

    await act(async () => {
      tabsOf(el)[2].click();
    });

    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onActivate).toHaveBeenCalledWith(2);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("renders a Close tab button per tab; clicking it calls onClose and NOT onActivate", async () => {
    const onActivate = vi.fn();
    const onClose = vi.fn();
    const el = await mount({
      tabs: [view({ key: "a", active: true }), view({ key: "b" })],
      onActivate,
      onClose,
      onNew: () => {},
    });

    const tabs = tabsOf(el);
    for (const t of tabs) {
      expect(
        t.querySelector('button[aria-label="Close tab"]'),
        "every tab needs a close button",
      ).not.toBeNull();
    }

    await act(async () => {
      q(tabs[1], 'button[aria-label="Close tab"]')!.click();
    });

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledWith(1);
    // stopPropagation: the close click must not bubble into a tab activate.
    expect(onActivate).not.toHaveBeenCalled();
  });

  it("renders a + button (aria-label 'New conversation tab') that calls onNew", async () => {
    const onNew = vi.fn();
    const el = await mount({
      tabs: [view({ key: "a", active: true })],
      onActivate: () => {},
      onClose: () => {},
      onNew,
    });

    const plus = q(el, 'button[aria-label="New conversation tab"]');
    expect(plus, "the + button must render").not.toBeNull();

    await act(async () => {
      plus!.click();
    });

    expect(onNew).toHaveBeenCalledTimes(1);
  });
});

// ─── overflowEdges (pure) ────────────────────────────────────────────────

describe("overflowEdges", () => {
  it("is false on both edges when the content fits", () => {
    expect(overflowEdges(0, 100, 100)).toEqual({ left: false, right: false });
    expect(overflowEdges(0, 99, 100)).toEqual({ left: false, right: false });
  });

  it("tolerates a 1px sub-pixel overflow", () => {
    expect(overflowEdges(0, 101, 100)).toEqual({ left: false, right: false });
  });

  it("flags the right edge at the start of an overflowing strip", () => {
    expect(overflowEdges(0, 200, 100)).toEqual({ left: false, right: true });
  });

  it("flags the left edge at the end of an overflowing strip", () => {
    expect(overflowEdges(100, 200, 100)).toEqual({ left: true, right: false });
  });

  it("flags both edges in the middle", () => {
    expect(overflowEdges(50, 300, 100)).toEqual({ left: true, right: true });
  });

  it("applies the 1px tolerance on the left edge too", () => {
    expect(overflowEdges(1, 200, 100).left).toBe(false);
    expect(overflowEdges(2, 200, 100).left).toBe(true);
  });
});

// ─── overflow UI: fades, arrows, counter ─────────────────────────────────
//
// jsdom has no layout, so the overflow state is driven the way the page
// drives it: set the scroll container's metrics, then fire its scroll event
// (the design's "scroll listener" — the ResizeObserver path is guarded by
// `typeof ResizeObserver` and is a no-op in jsdom).

/** Set scroll metrics on the `.ctab-scroll` container and fire a scroll event. */
async function setOverflow(
  el: HTMLDivElement,
  m: { scrollLeft: number; scrollWidth: number; clientWidth: number },
): Promise<HTMLElement> {
  const sc = q(el, ".ctab-scroll");
  expect(sc, "a .ctab-scroll container must render").not.toBeNull();
  Object.defineProperty(sc!, "scrollWidth", { value: m.scrollWidth, configurable: true });
  Object.defineProperty(sc!, "clientWidth", { value: m.clientWidth, configurable: true });
  Object.defineProperty(sc!, "scrollLeft", {
    value: m.scrollLeft,
    configurable: true,
    writable: true,
  });
  await act(async () => {
    sc!.dispatchEvent(new Event("scroll"));
  });
  return sc!;
}

describe("ConvoTabBar — overflow UI", () => {
  const manyTabs = (n: number): TabView[] =>
    Array.from({ length: n }, (_, i) =>
      view({
        key: `t${i}`,
        label: `Tab ${i + 1}`,
        tooltip: `Tab ${i + 1}`,
        active: i === 0,
      }),
    );

  it("shows no counter and no arrows when the strip fits", async () => {
    const el = await mount({
      tabs: manyTabs(3),
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    const count = q(el, ".ctab-count");
    expect(count?.hidden ?? true, "no counter when it fits").toBe(true);
    const leftArrow = q(el, 'button[title="Scroll tabs left"]');
    const rightArrow = q(el, 'button[title="Scroll tabs right"]');
    expect(leftArrow?.hidden ?? true).toBe(true);
    expect(rightArrow?.hidden ?? true).toBe(true);
  });

  it("shows the 'N tabs' counter and the right arrow when the strip overflows", async () => {
    const el = await mount({
      tabs: manyTabs(12),
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });
    // Start of a long strip: everything past 500px is to the right.
    const sc = await setOverflow(el, { scrollLeft: 0, scrollWidth: 2000, clientWidth: 500 });

    const count = q(el, ".ctab-count");
    expect(count, "the counter must render on overflow").not.toBeNull();
    expect(count!.hidden).toBe(false);
    expect(count!.textContent).toContain("12 tabs");

    const rightArrow = q(el, 'button[title="Scroll tabs right"]');
    expect(rightArrow, "the › arrow must show").not.toBeNull();
    expect(rightArrow!.hidden).toBe(false);
    const leftArrow = q(el, 'button[title="Scroll tabs left"]');
    expect(leftArrow?.hidden ?? true, "no ‹ at the start").toBe(true);

    // The right fade applies to the scroll container.
    expect(sc.classList.contains("fade-r")).toBe(true);
    expect(sc.classList.contains("fade-l")).toBe(false);
  });

  it("shows both arrows and the counter in the middle of the strip", async () => {
    const el = await mount({
      tabs: manyTabs(12),
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });
    const sc = await setOverflow(el, { scrollLeft: 700, scrollWidth: 2000, clientWidth: 500 });

    expect(q(el, 'button[title="Scroll tabs left"]')?.hidden ?? true).toBe(false);
    expect(q(el, 'button[title="Scroll tabs right"]')?.hidden ?? true).toBe(false);
    expect(sc.classList.contains("fade-l")).toBe(true);
    expect(sc.classList.contains("fade-r")).toBe(true);
  });

  it("arrows scroll the strip by 0.8 * clientWidth (scrollBy delta or scrollTo absolute)", async () => {
    const el = await mount({
      tabs: manyTabs(12),
      onActivate: () => {},
      onClose: () => {},
      onNew: () => {},
    });

    /** Stub both scroll APIs and report the net scrollLeft change an arrow
     * click causes, whichever the implementation uses (the design says only
     * "scroll by 0.8*clientWidth"). */
    async function clickArrow(
      title: string,
      startLeft: number,
    ): Promise<number | null> {
      const sc = await setOverflow(el, {
        scrollLeft: startLeft,
        scrollWidth: 2000,
        clientWidth: 500,
      });
      const scrollBy = vi.fn();
      const scrollTo = vi.fn();
      Object.assign(sc, { scrollBy, scrollTo });
      await act(async () => {
        q(el, `button[title="${title}"]`)!.click();
      });
      const leftOf = (arg: unknown): number | null =>
        typeof arg === "number"
          ? arg
          : typeof arg === "object" && arg !== null && "left" in arg
            ? (arg as { left: number }).left
            : null;
      if (scrollBy.mock.calls.length) return leftOf(scrollBy.mock.calls[0][0]);
      if (scrollTo.mock.calls.length) {
        const abs = leftOf(scrollTo.mock.calls[0][0]);
        return abs === null ? null : abs - startLeft;
      }
      return null;
    }

    expect(
      await clickArrow("Scroll tabs right", 0),
      "the › arrow must scroll right by 0.8 * clientWidth",
    ).toBe(400);
    expect(
      await clickArrow("Scroll tabs left", 1500),
      "the ‹ arrow must scroll left by 0.8 * clientWidth",
    ).toBe(-400);
  });
});
