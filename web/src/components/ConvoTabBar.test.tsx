// Tests for `components/ConvoTabBar.tsx` (design §5 + the mock's `.ctab*`
// markup, which the design says to port as-is):
//
//   export function ConvoTabBar(p: { tabs: TabView[]; onActivate(i: number): void;
//                                     onClose(i: number): void; onNew(): void }): JSX.Element;
//   export function overflowEdges(scrollLeft, scrollWidth, clientWidth):
//     { left: boolean; right: boolean };  // 1px tolerance
//
// Browser-style SHRINKING tabs (fix/tab-shrink) add to the contract:
//
//   export const MIN_TAB_PX = 36;   // == `.ctab { min-width }` in index.css
//   export const TAB_GAP_PX = 2;    // == `.ctab-scroll { gap }` in index.css
//   export function tabsOverflow(o: { tabCount: number; minTabPx: number;
//     gapPx: number; availPx: number }): boolean;
//     // true iff tabCount*minTabPx + (tabCount-1)*gapPx > availPx
//
//   Layout = [‹?][.ctab-scroll][›?][+] as direct children of nav.ctabbar.
//   ‹ › (aria-label + title "Scroll tabs left/right") are always rendered but
//   carry `hidden` unless tabsOverflow(...) says the tabs cannot fit at their
//   minimum width; ‹ is `disabled` at scrollLeft<=1, › at the far end (±1px).
//   No `.ctab-spacer`, no "N tabs" counter. Every tab has a `.icon` element.
//   Closing with the mouse over the bar pins the other tabs' widths
//   (inline `flex: 0 0 Npx`) until pointerleave / a tab is added.
//   The active tab is scrollIntoView'd on activate, add and RO resize.
//
// Tests changed for this feature are marked "(updated for fix/tab-shrink)".
//
// Contract:
//   - role="tablist"; each tab = role="tab", aria-selected, title = tooltip
//   - label = title || "New conversation"; untitled renders italic
//   - active tab: the `active` class; running → spinner; the ACTIVE tab's
//     running state is live (activeRunning), its unread is always off
//   - unread dot ONLY on inactive tabs
//   - × = button aria-label="Close tab" (stopPropagation: closing must not
//     activate); "+" = button aria-label="New conversation tab"
//   - fades shown iff overflowEdges says so; ‹ › arrows shown iff
//     tabsOverflow says so (was: iff overflowEdges); arrows scroll by
//     0.8*clientWidth
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

import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, useState, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  ConvoTabBar,
  overflowEdges,
  tabsOverflow,
  MIN_TAB_PX,
  TAB_GAP_PX,
} from "./ConvoTabBar";
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

// ─── jsdom layout + ResizeObserver stubs ─────────────────────────────────
//
// jsdom has no layout. Measurements the component may read are served from
// this mutable table: the bar (`nav.ctabbar`) and the strip (`.ctab-scroll`)
// widths via clientWidth, and each `.ctab`'s width via offsetWidth AND
// getBoundingClientRect (by its index among its siblings, so a measurement
// taken AFTER a tab is removed yields different numbers than one taken
// before — which is how the close-freeze "pre-close width" is pinned down).
// Per-element own properties (see setOverflow) still win over these.

const layout = {
  bar: 1000,
  strip: 940,
  tabW: ((): number => 120) as (i: number) => number,
};
const setBarWidth = (w: number) => {
  layout.bar = w;
  layout.strip = Math.max(0, w - 60);
};

const tabIndexOf = (el: Element): number =>
  el.parentElement ? [...el.parentElement.children].indexOf(el) : 0;

const saved: Record<string, PropertyDescriptor | undefined> = {};
function patch(proto: object, name: string, desc: PropertyDescriptor, key: string) {
  saved[key] = Object.getOwnPropertyDescriptor(proto, name);
  Object.defineProperty(proto, name, { configurable: true, ...desc });
}
function unpatch(proto: object, name: string, key: string) {
  const d = saved[key];
  if (d) Object.defineProperty(proto, name, d);
  else delete (proto as Record<string, unknown>)[name];
}

class MockResizeObserver {
  static instances: MockResizeObserver[] = [];
  targets: Element[] = [];
  disconnected = false;
  constructor(public cb: ResizeObserverCallback) {
    MockResizeObserver.instances.push(this);
  }
  observe(t: Element) {
    this.targets.push(t);
  }
  unobserve() {}
  disconnect() {
    this.disconnected = true;
  }
}
const realRO = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;

/** Fire every live ResizeObserver callback (entries are empty on purpose:
 * the component must re-read element metrics, not trust the entries). */
async function fireResize() {
  await act(async () => {
    for (const ro of MockResizeObserver.instances) {
      if (!ro.disconnected) ro.cb([], ro as unknown as ResizeObserver);
    }
  });
}

/** Elements scrollIntoView was called on, in order. */
let scrolledInto: Element[] = [];

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no scrollIntoView; the design calls it for the active tab.
  Element.prototype.scrollIntoView = vi.fn(function (this: Element) {
    scrolledInto.push(this);
  });
  // jsdom has no pointer capture; the hold-to-scroll arrows may use it.
  Element.prototype.setPointerCapture ??= vi.fn();
  Element.prototype.releasePointerCapture ??= vi.fn();

  patch(
    Element.prototype,
    "clientWidth",
    {
      get(this: Element) {
        if (this.classList.contains("ctabbar")) return layout.bar;
        if (this.classList.contains("ctab-scroll")) return layout.strip;
        return 0;
      },
    },
    "clientWidth",
  );
  patch(
    HTMLElement.prototype,
    "offsetWidth",
    {
      get(this: HTMLElement) {
        return this.classList.contains("ctab") ? layout.tabW(tabIndexOf(this)) : 0;
      },
    },
    "offsetWidth",
  );
  patch(
    Element.prototype,
    "getBoundingClientRect",
    {
      value(this: Element): DOMRect {
        const width = this.classList.contains("ctab") ? layout.tabW(tabIndexOf(this)) : 0;
        return {
          x: 0, y: 0, top: 0, left: 0, right: width, bottom: 0, width, height: 0,
          toJSON: () => ({}),
        } as DOMRect;
      },
    },
    "gbcr",
  );
});

afterAll(() => {
  unpatch(Element.prototype, "clientWidth", "clientWidth");
  unpatch(HTMLElement.prototype, "offsetWidth", "offsetWidth");
  unpatch(Element.prototype, "getBoundingClientRect", "gbcr");
});

beforeEach(() => {
  vi.clearAllMocks();
  scrolledInto = [];
  setBarWidth(1000);
  layout.tabW = () => 120;
  MockResizeObserver.instances = [];
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = MockResizeObserver;
});

afterEach(() => {
  vi.useRealTimers();
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = realRO;
});

/** Mount an arbitrary element (used by the stateful harness below). */
async function mountNode(node: ReactElement): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(node);
  });
  return container;
}

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

// ─── pointer / harness helpers ───────────────────────────────────────────

/** Dispatch a pointer-ish event. jsdom may lack PointerEvent, so a
 * MouseEvent is used with `pointerType`/`pointerId` stamped on. */
function firePointer(
  target: Element,
  type: string,
  o: { pointerType?: string; button?: number } = {},
): void {
  const ev = new MouseEvent(type, {
    bubbles: type !== "pointerenter" && type !== "pointerleave",
    cancelable: true,
    button: o.button ?? 0,
    relatedTarget: null,
  });
  Object.defineProperty(ev, "pointerType", { value: o.pointerType ?? "mouse" });
  Object.defineProperty(ev, "pointerId", { value: 1 });
  target.dispatchEvent(ev);
}

/** The pointer enters the bar. Both the native `pointerenter` and the
 * bubbling `pointerover` (what React's onPointerEnter derives from) fire,
 * so the test is agnostic to how the implementation listens. */
async function pointerIn(target: Element, pointerType = "mouse") {
  await act(async () => {
    firePointer(target, "pointerover", { pointerType });
    firePointer(target, "pointerenter", { pointerType });
  });
}
/** The pointer leaves the bar (native `pointerleave` + bubbling `pointerout`). */
async function pointerOut(target: Element, pointerType = "mouse") {
  await act(async () => {
    firePointer(target, "pointerout", { pointerType });
    firePointer(target, "pointerleave", { pointerType });
  });
}

interface HarnessApi {
  add(): void;
  activate(i: number): void;
}
let harness!: HarnessApi;

/** Minimal stateful parent: the bar is presentational, so closing /
 * adding / activating need a parent that actually changes `tabs`. */
function Harness({ initial }: { initial: TabView[] }) {
  const [tabs, setTabs] = useState(initial);
  harness = {
    add: () =>
      setTabs((t) => [
        ...t.map((x) => ({ ...x, active: false })),
        view({ key: `added${t.length}`, label: `Added ${t.length}`, active: true }),
      ]),
    activate: (i) => setTabs((t) => t.map((x, j) => ({ ...x, active: j === i }))),
  };
  return (
    <ConvoTabBar
      tabs={tabs}
      onActivate={harness.activate}
      onClose={(i) => setTabs((t) => t.filter((_, j) => j !== i))}
      onNew={() => {}}
    />
  );
}

const manyTabs = (n: number, active = 0): TabView[] =>
  Array.from({ length: n }, (_, i) =>
    view({
      key: `t${i}`,
      label: `Tab ${i + 1}`,
      tooltip: `Tab ${i + 1}`,
      active: i === active,
    }),
  );

/** Tear the current mount down early (for tests that mount more than once). */
function unmountNow() {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
}

const noop = { onActivate: () => {}, onClose: () => {}, onNew: () => {} };

const navOf = (el: HTMLElement): HTMLElement => q(el, "nav.ctabbar")!;
const arrowL = (el: HTMLElement) =>
  el.querySelector<HTMLButtonElement>('button[aria-label="Scroll tabs left"]');
const arrowR = (el: HTMLElement) =>
  el.querySelector<HTMLButtonElement>('button[aria-label="Scroll tabs right"]');
/** Strict lookup with a readable failure (instead of a null dereference). */
function mustArrow(el: HTMLElement, side: "left" | "right"): HTMLButtonElement {
  const b = side === "left" ? arrowL(el) : arrowR(el);
  if (!b) throw new Error(`expected a button[aria-label="Scroll tabs ${side}"] to render`);
  return b;
}
const plusOf = (el: HTMLElement) => q(el, 'button[aria-label="New conversation tab"]');
/** Rendered AND not `hidden`. */
const shown = (b: HTMLElement | null): boolean => b !== null && !b.hidden;

/** The bar's visible direct children, in DOM order, by role. */
function visibleLayout(el: HTMLElement): string[] {
  return [...navOf(el).children]
    .filter((c) => !(c as HTMLElement).hidden)
    .map((c) =>
      c.classList.contains("ctab-scroll")
        ? "strip"
        : (c.getAttribute("aria-label") ?? c.className),
    );
}

/** Inline-pinned width (px) from `style="flex: 0 0 Npx"`, else null. */
function pinnedPx(tab: HTMLElement): number | null {
  const m = /flex:\s*0\s+0\s+(\d+(?:\.\d+)?)px/.exec(tab.getAttribute("style") ?? "");
  if (m) return parseFloat(m[1]);
  if (
    tab.style.flexGrow === "0" &&
    tab.style.flexShrink === "0" &&
    /px$/.test(tab.style.flexBasis)
  ) {
    return parseFloat(tab.style.flexBasis);
  }
  return null;
}

// ─── tabsOverflow (pure) ─────────────────────────────────────────────────

describe("tabsOverflow", () => {
  const o = (tabCount: number, availPx: number, minTabPx = 36, gapPx = 2) => ({
    tabCount,
    minTabPx,
    gapPx,
    availPx,
  });

  it("is false for zero tabs, even with no space at all", () => {
    expect(tabsOverflow(o(0, 0))).toBe(false);
    expect(tabsOverflow(o(0, -50))).toBe(false);
  });

  it("is false for a negative tab count (malformed input must not report overflow)", () => {
    expect(tabsOverflow(o(-3, 0))).toBe(false);
  });

  it("one tab needs exactly minTabPx and no gap", () => {
    expect(tabsOverflow(o(1, 36))).toBe(false);
    expect(tabsOverflow(o(1, 35))).toBe(true);
    expect(tabsOverflow(o(1, 37))).toBe(false);
  });

  it("counts (n-1) gaps: two tabs need 2*min + 1*gap", () => {
    expect(tabsOverflow(o(2, 74))).toBe(false);
    expect(tabsOverflow(o(2, 73))).toBe(true);
  });

  it("is exactly Σ(min widths) + Σ(gaps) > avail — equality fits, one pixel less overflows", () => {
    for (const n of [3, 10, 25, 100]) {
      const floor = n * 36 + (n - 1) * 2;
      expect(tabsOverflow(o(n, floor)), `${n} tabs at the floor`).toBe(false);
      expect(tabsOverflow(o(n, floor - 1)), `${n} tabs 1px short`).toBe(true);
      expect(tabsOverflow(o(n, floor + 500)), `${n} tabs, lots of room`).toBe(false);
    }
  });

  it("honours custom minTabPx and gapPx", () => {
    // 4 * 50 + 3 * 10 = 230
    expect(tabsOverflow(o(4, 230, 50, 10))).toBe(false);
    expect(tabsOverflow(o(4, 229, 50, 10))).toBe(true);
    // zero gap
    expect(tabsOverflow(o(4, 200, 50, 0))).toBe(false);
    expect(tabsOverflow(o(4, 199, 50, 0))).toBe(true);
  });

  it("is true when there is no space at all (0 or negative) but there are tabs", () => {
    expect(tabsOverflow(o(1, 0))).toBe(true);
    expect(tabsOverflow(o(5, -10))).toBe(true);
  });

  it("handles a huge tab count without overflowing to nonsense", () => {
    expect(tabsOverflow(o(100_000, 1_000_000))).toBe(true);
    expect(tabsOverflow(o(100_000, 100_000 * 38))).toBe(false);
  });

  it("is stateless: the same input always gives the same answer", () => {
    const input = o(30, 600);
    const first = tabsOverflow(input);
    for (let i = 0; i < 5; i++) expect(tabsOverflow(input)).toBe(first);
  });
});

describe("tab-width constants", () => {
  it("MIN_TAB_PX is 36 and TAB_GAP_PX is 2 (kept in sync with index.css by convoTabBar.css.test.ts)", () => {
    expect(MIN_TAB_PX).toBe(36);
    expect(TAB_GAP_PX).toBe(2);
  });
});

// ─── overflow UI: fades, arrows ──────────────────────────────────────────
//
// jsdom has no layout. Two independent signals drive the UI:
//   * ARROWS (shown / hidden): the stateless tabsOverflow predicate, fed
//     from the bar's clientWidth (`layout.bar`) and the tab count;
//     re-evaluated when the ResizeObserver fires.
//   * FADES + disabled-ness: the strip's scroll metrics. We set them and
//     fire the strip's `scroll` event, as the page would.

/** Set scroll metrics on the `.ctab-scroll` container and fire a scroll event. */
async function setOverflow(
  el: HTMLElement,
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

describe("ConvoTabBar — overflow UI (updated for fix/tab-shrink)", () => {
  it("shows no arrows when the tabs fit at minimum width (updated: no counter)", async () => {
    setBarWidth(1000);
    const el = await mount({ tabs: manyTabs(3), ...noop });

    expect(shown(arrowL(el)), "‹ must not show when the tabs fit").toBe(false);
    expect(shown(arrowR(el)), "› must not show when the tabs fit").toBe(false);
  });

  it("shows the right arrow (not the left) at the start of an overflowing strip (updated: no counter)", async () => {
    setBarWidth(300); // 12 tabs need >= 454px even at minimum width
    const el = await mount({ tabs: manyTabs(12), ...noop });
    // Start of a long strip: everything past 500px is to the right.
    const sc = await setOverflow(el, { scrollLeft: 0, scrollWidth: 2000, clientWidth: 500 });

    // Arrows are always rendered as a pair; only the disabled state differs.
    expect(shown(arrowR(el)), "the › arrow must show").toBe(true);
    expect(shown(arrowL(el)), "‹ shows too (as a disabled button) on overflow").toBe(true);
    expect(mustArrow(el, "left").disabled, "‹ is disabled at the start").toBe(true);
    expect(mustArrow(el, "right").disabled, "› is enabled at the start").toBe(false);

    // The right fade applies to the scroll container.
    expect(sc.classList.contains("fade-r")).toBe(true);
    expect(sc.classList.contains("fade-l")).toBe(false);
  });

  it("shows both arrows enabled, and both fades, in the middle of the strip (updated: no counter)", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 700, scrollWidth: 2000, clientWidth: 500 });

    expect(shown(arrowL(el))).toBe(true);
    expect(shown(arrowR(el))).toBe(true);
    expect(mustArrow(el, "left").disabled).toBe(false);
    expect(mustArrow(el, "right").disabled).toBe(false);
    expect(sc.classList.contains("fade-l")).toBe(true);
    expect(sc.classList.contains("fade-r")).toBe(true);
  });

  it("arrows have both the title and the aria-label 'Scroll tabs left/right'", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    expect(arrowL(el)).not.toBeNull();
    expect(arrowR(el)).not.toBeNull();
    expect(mustArrow(el, "left").getAttribute("title")).toBe("Scroll tabs left");
    expect(mustArrow(el, "right").getAttribute("title")).toBe("Scroll tabs right");
    expect(mustArrow(el, "left").textContent).toBe("‹");
    expect(mustArrow(el, "right").textContent).toBe("›");
  });

  it("decides arrows by the min-width predicate, NOT by the strip's scroll metrics", async () => {
    setBarWidth(1000);
    const el = await mount({ tabs: manyTabs(3), ...noop });
    // The strip reports a lot of hidden content, but 3 tabs fit at 36px.
    await setOverflow(el, { scrollLeft: 0, scrollWidth: 2000, clientWidth: 500 });

    expect(shown(arrowL(el))).toBe(false);
    expect(shown(arrowR(el))).toBe(false);
  });

  it("shows arrows for many tabs in a narrow bar and none in a wide one (30 tabs)", async () => {
    setBarWidth(600); // 30*36 + 29*2 = 1138 > 600
    const narrow = await mount({ tabs: manyTabs(30), ...noop });
    expect(shown(arrowL(narrow))).toBe(true);
    expect(shown(arrowR(narrow))).toBe(true);
    unmountNow();

    setBarWidth(1400); // 1138 < 1400 - chrome
    const wide = await mount({ tabs: manyTabs(30), ...noop });
    expect(shown(arrowL(wide))).toBe(false);
    expect(shown(arrowR(wide))).toBe(false);
  });

  it("re-evaluates the arrows when the ResizeObserver fires, both ways, with no hysteresis", async () => {
    setBarWidth(1000);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    expect(shown(arrowR(el)), "fits at 1000px").toBe(false);

    setBarWidth(300);
    await fireResize();
    expect(shown(arrowL(el)), "300px: ‹ appears").toBe(true);
    expect(shown(arrowR(el)), "300px: › appears").toBe(true);

    setBarWidth(1000);
    await fireResize();
    expect(shown(arrowL(el)), "back to 1000px: ‹ gone").toBe(false);
    expect(shown(arrowR(el)), "back to 1000px: › gone").toBe(false);

    // Firing again with no change must not flip anything (stateless).
    await fireResize();
    expect(shown(arrowR(el))).toBe(false);
  });

  it("re-evaluates the arrows when tabs are added (the count crosses the threshold)", async () => {
    setBarWidth(500); // 1 tab fits; 40 tabs need 1518px at minimum width
    const el = await mount({ tabs: [view({ key: "only", active: true })], ...noop });
    expect(shown(arrowR(el))).toBe(false);

    await act(async () => {
      root!.render(<ConvoTabBar tabs={manyTabs(40)} {...noop} />);
    });
    expect(shown(arrowR(el))).toBe(true);

    await act(async () => {
      root!.render(<ConvoTabBar tabs={manyTabs(2)} {...noop} />);
    });
    expect(shown(arrowR(el))).toBe(false);
  });

  it("observes the bar or the strip with a ResizeObserver", async () => {
    const el = await mount({ tabs: manyTabs(3), ...noop });
    const nav = navOf(el);
    const strip = q(el, ".ctab-scroll")!;
    const observed = MockResizeObserver.instances.flatMap((r) => r.targets);
    expect(observed.some((t) => t === nav || t === strip)).toBe(true);
  });

  it("disables ‹ at scrollLeft 0 and enables › (updated: new disabled-state contract)", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    await setOverflow(el, { scrollLeft: 0, scrollWidth: 2000, clientWidth: 500 });
    expect(mustArrow(el, "left").disabled).toBe(true);
    expect(mustArrow(el, "right").disabled).toBe(false);
  });

  it("disables › at the max scroll (scrollWidth - clientWidth), with 1px tolerance, and enables ‹", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });

    await setOverflow(el, { scrollLeft: 1500, scrollWidth: 2000, clientWidth: 500 });
    expect(mustArrow(el, "right").disabled, "exactly at max").toBe(true);
    expect(mustArrow(el, "left").disabled).toBe(false);

    await setOverflow(el, { scrollLeft: 1499, scrollWidth: 2000, clientWidth: 500 });
    expect(mustArrow(el, "right").disabled, "1px short of max is still the end").toBe(true);

    await setOverflow(el, { scrollLeft: 1498, scrollWidth: 2000, clientWidth: 500 });
    expect(mustArrow(el, "right").disabled, "2px short of max is not the end").toBe(false);
  });

  it("enables both arrows strictly between the ends", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    await setOverflow(el, { scrollLeft: 400, scrollWidth: 2000, clientWidth: 500 });
    expect(mustArrow(el, "left").disabled).toBe(false);
    expect(mustArrow(el, "right").disabled).toBe(false);
  });

  it("puts the fade classes only on the side(s) with hidden tabs", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 1500, scrollWidth: 2000, clientWidth: 500 });
    expect(sc.classList.contains("fade-l")).toBe(true);
    expect(sc.classList.contains("fade-r")).toBe(false);
  });

  it("arrows scroll the strip by 0.8 * clientWidth via smooth scrollBy (updated: scrollBy only, smooth)", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });

    async function clickArrow(
      label: "Scroll tabs left" | "Scroll tabs right",
      startLeft: number,
    ) {
      const sc = await setOverflow(el, {
        scrollLeft: startLeft,
        scrollWidth: 2000,
        clientWidth: 500,
      });
      const scrollBy = vi.fn();
      Object.assign(sc, { scrollBy });
      await act(async () => {
        mustArrow(el, label === "Scroll tabs left" ? "left" : "right").click();
      });
      return scrollBy;
    }

    const right = await clickArrow("Scroll tabs right", 0);
    expect(right, "› must call scrollBy exactly once").toHaveBeenCalledTimes(1);
    expect(right).toHaveBeenCalledWith({ left: 400, behavior: "smooth" });

    const left = await clickArrow("Scroll tabs left", 1500);
    expect(left, "‹ must call scrollBy exactly once").toHaveBeenCalledTimes(1);
    expect(left).toHaveBeenCalledWith({ left: -400, behavior: "smooth" });
  });

  it("scales the arrow step with the strip's clientWidth", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 100, scrollWidth: 5000, clientWidth: 250 });
    const scrollBy = vi.fn();
    Object.assign(sc, { scrollBy });
    await act(async () => {
      mustArrow(el, "right").click();
    });
    expect(scrollBy).toHaveBeenCalledWith({ left: 200, behavior: "smooth" });
  });

  for (const [label, sign] of [
    ["Scroll tabs right", 1],
    ["Scroll tabs left", -1],
  ] as const) {
    it(`holding "${label}" keeps scrolling until the pointer is released`, async () => {
      setBarWidth(300);
      const el = await mount({ tabs: manyTabs(12), ...noop });
      const sc = await setOverflow(el, { scrollLeft: 1000, scrollWidth: 4000, clientWidth: 500 });
      const scrollBy = vi.fn();
      Object.assign(sc, { scrollBy });
      const btn = mustArrow(el, sign > 0 ? "right" : "left");

      vi.useFakeTimers({
        toFake: [
          "setTimeout", "clearTimeout", "setInterval", "clearInterval",
          "requestAnimationFrame", "cancelAnimationFrame", "Date", "performance",
        ],
      });
      act(() => {
        firePointer(btn, "pointerdown");
      });
      vi.advanceTimersByTime(1000);

      expect(
        scrollBy.mock.calls.length,
        "a held arrow must repeat scrollBy while the pointer stays down",
      ).toBeGreaterThanOrEqual(2);
      for (const [arg] of scrollBy.mock.calls) {
        expect(Math.sign(arg.left), "every step goes the arrow's way").toBe(sign);
        expect(arg.behavior).toBe("smooth");
      }

      act(() => {
        firePointer(btn, "pointerup");
      });
      const settled = scrollBy.mock.calls.length;
      vi.advanceTimersByTime(2000);
      expect(scrollBy.mock.calls.length, "no more steps after pointerup").toBe(settled);
    });
  }

  it("pointercancel also stops a held arrow", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 0, scrollWidth: 4000, clientWidth: 500 });
    const scrollBy = vi.fn();
    Object.assign(sc, { scrollBy });
    const btn = mustArrow(el, "right");

    vi.useFakeTimers({
      toFake: [
        "setTimeout", "clearTimeout", "setInterval", "clearInterval",
        "requestAnimationFrame", "cancelAnimationFrame", "Date", "performance",
      ],
    });
    act(() => {
      firePointer(btn, "pointerdown");
    });
    vi.advanceTimersByTime(600);
    expect(scrollBy.mock.calls.length).toBeGreaterThanOrEqual(1);
    act(() => {
      firePointer(btn, "pointercancel");
    });
    const settled = scrollBy.mock.calls.length;
    vi.advanceTimersByTime(2000);
    expect(scrollBy.mock.calls.length).toBe(settled);
  });
});

// ─── layout: [‹?][strip][›?][+], no spacer, no counter ───────────────────

describe("ConvoTabBar — layout and chrome", () => {
  it("renders + immediately after the strip when the arrows are not shown", async () => {
    setBarWidth(1000);
    const el = await mount({ tabs: manyTabs(3), ...noop });
    expect(visibleLayout(el)).toEqual(["strip", "New conversation tab"]);
  });

  it("renders [‹][strip][›][+] in that order when the tabs overflow", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    expect(visibleLayout(el)).toEqual([
      "Scroll tabs left",
      "strip",
      "Scroll tabs right",
      "New conversation tab",
    ]);
  });

  it("keeps + as the LAST child of the bar, after the strip and ›, even when the arrows are hidden", async () => {
    for (const w of [1000, 300]) {
      setBarWidth(w);
      const el = await mount({ tabs: manyTabs(12), ...noop });
      const nav = navOf(el);
      expect(nav.lastElementChild, `bar ${w}px`).toBe(plusOf(el));
      const strip = q(el, ".ctab-scroll")!;
      expect(strip.compareDocumentPosition(plusOf(el)!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(mustArrow(el, "right").compareDocumentPosition(plusOf(el)!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(mustArrow(el, "left").compareDocumentPosition(strip) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      unmountNow();
    }
  });

  it("always renders + and it works: zero tabs, one tab, and a huge overflowing strip", async () => {
    for (const n of [0, 1, 80]) {
      setBarWidth(300);
      const onNew = vi.fn();
      const el = await mount({
        tabs: manyTabs(n),
        onActivate: () => {},
        onClose: () => {},
        onNew,
      });
      const plus = plusOf(el);
      expect(plus, `${n} tabs: + must render`).not.toBeNull();
      expect(plus!.hidden, `${n} tabs: + must not be hidden`).toBe(false);
      await act(async () => {
        plus!.click();
      });
      expect(onNew, `${n} tabs: + calls onNew`).toHaveBeenCalledTimes(1);
      unmountNow();
    }
  });

  it("has no .ctab-spacer element", async () => {
    for (const w of [1000, 300]) {
      setBarWidth(w);
      const el = await mount({ tabs: manyTabs(12), ...noop });
      expect(q(el, ".ctab-spacer"), `bar ${w}px`).toBeNull();
      unmountNow();
    }
  });

  it("has no 'N tabs' counter element or text, whether or not the tabs overflow", async () => {
    for (const w of [1000, 300]) {
      setBarWidth(w);
      const el = await mount({ tabs: manyTabs(12), ...noop });
      expect(q(el, ".ctab-count"), `bar ${w}px: no counter element`).toBeNull();
      expect(navOf(el).textContent, `bar ${w}px: no counter text`).not.toMatch(/\d+\s+tabs?\b/i);
      unmountNow();
    }
  });

  it("keeps the strip as the scroll container (.ctab-scroll) holding every tab", async () => {
    const el = await mount({ tabs: manyTabs(5), ...noop });
    const strip = q(el, ".ctab-scroll")!;
    expect(strip.querySelectorAll('[role="tab"]')).toHaveLength(5);
    expect(strip.parentElement).toBe(navOf(el));
  });
});

// ─── tab anatomy: icon + ✕ ───────────────────────────────────────────────

describe("ConvoTabBar — tab icon and close button", () => {
  it("gives every tab a .icon element (so something shows when the title is hidden)", async () => {
    const el = await mount({
      tabs: [
        view({ key: "a", label: "Fix login", active: true }),
        view({ key: "b", label: "Refactor" }),
        view({ key: "c", label: "New conversation", untitled: true }),
        view({ key: "d", label: "Running", running: true }),
        view({ key: "e", label: "Unread", unread: true }),
      ],
      ...noop,
    });
    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(5);
    for (const t of tabs) {
      expect(t.querySelector(".icon"), `"${t.textContent}" needs an .icon`).not.toBeNull();
    }
  });

  it("keeps the title element in the DOM (CSS container queries hide it, not React)", async () => {
    const el = await mount({ tabs: manyTabs(40), ...noop });
    for (const t of tabsOf(el)) expect(t.querySelector(".title")).not.toBeNull();
  });

  it("keeps the ✕ on the ACTIVE tab, visible (not hidden), even in a crowded bar", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(40, 7), ...noop });
    const active = tabsOf(el)[7];
    expect(active.classList.contains("active")).toBe(true);
    const x = active.querySelector<HTMLElement>('button[aria-label="Close tab"]');
    expect(x, "active tab keeps its ✕").not.toBeNull();
    expect(x!.hidden).toBe(false);
    expect(x!.textContent).toBe("×");
  });

  it("activating a crowded tab and closing the active tab both still route to the right index", async () => {
    setBarWidth(300);
    const onClose = vi.fn();
    const onActivate = vi.fn();
    const el = await mount({ tabs: manyTabs(40, 7), onActivate, onClose, onNew: () => {} });
    await act(async () => {
      q(tabsOf(el)[7], 'button[aria-label="Close tab"]')!.click();
    });
    expect(onClose).toHaveBeenCalledWith(7);
    expect(onActivate).not.toHaveBeenCalled();
  });
});

// ─── scrolling the active tab into view ──────────────────────────────────

describe("ConvoTabBar — active tab scrolled into view", () => {
  const five = () => manyTabs(5, 0);

  it("scrolls the active tab into view on mount", async () => {
    const el = await mount({ tabs: manyTabs(5, 2), ...noop });
    expect(scrolledInto.at(-1)).toBe(tabsOf(el)[2]);
  });

  it("scrolls the newly ACTIVATED tab into view", async () => {
    const el = await mountNode(<Harness initial={five()} />);
    scrolledInto = [];
    await act(async () => {
      harness.activate(3);
    });
    expect(scrolledInto.length).toBeGreaterThanOrEqual(1);
    expect(scrolledInto.at(-1)).toBe(tabsOf(el)[3]);
    expect(tabsOf(el)[3].classList.contains("active")).toBe(true);
  });

  it("scrolls a newly ADDED (and active) tab into view", async () => {
    const el = await mountNode(<Harness initial={five()} />);
    scrolledInto = [];
    await act(async () => {
      harness.add();
    });
    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(6);
    expect(scrolledInto.at(-1)).toBe(tabs[5]);
  });

  it("scrolls the active tab into view again when the ResizeObserver fires", async () => {
    const el = await mount({ tabs: manyTabs(5, 4), ...noop });
    scrolledInto = [];
    await fireResize();
    expect(scrolledInto.length, "a resize must re-reveal the active tab").toBeGreaterThanOrEqual(1);
    expect(scrolledInto.at(-1)).toBe(tabsOf(el)[4]);
  });

  it("does not throw when the bar resizes with no active tab", async () => {
    await mount({ tabs: [view({ key: "a" }), view({ key: "b" })], ...noop });
    await expect(fireResize()).resolves.toBeUndefined();
  });
});

// ─── close-freeze ────────────────────────────────────────────────────────

describe("ConvoTabBar — close-freeze (Chrome-style)", () => {
  const closeBtn = (t: HTMLElement) => q(t, 'button[aria-label="Close tab"]')!;

  it("pins every remaining tab at its PRE-close width when closing with the mouse over the bar", async () => {
    const widths = [150, 140, 130, 120, 110];
    layout.tabW = (i) => widths[i] ?? 100;
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    await pointerIn(navOf(el));

    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });

    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(4);
    // Pre-close widths of the survivors: tabs 0,1,3,4 → 150,140,120,110.
    expect(tabs.map(pinnedPx)).toEqual([150, 140, 120, 110]);
  });

  it("releases the pins on pointerleave of the bar", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    const nav = navOf(el);
    await pointerIn(nav);
    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });
    expect(tabsOf(el).every((t) => pinnedPx(t) !== null), "frozen while hovering").toBe(true);

    await pointerOut(nav);

    for (const t of tabsOf(el)) {
      expect(pinnedPx(t), "pins must be cleared on pointerleave").toBeNull();
      expect(t.getAttribute("style") ?? "").not.toMatch(/flex/);
    }
  });

  it("releases the pins when the number of tabs increases", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    await pointerIn(navOf(el));
    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });
    expect(tabsOf(el).every((t) => pinnedPx(t) !== null)).toBe(true);

    await act(async () => {
      harness.add();
    });

    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(5);
    for (const t of tabs) expect(pinnedPx(t), "adding a tab un-freezes").toBeNull();
  });

  it("stays frozen across several closes in one hover", async () => {
    const el = await mountNode(<Harness initial={manyTabs(6, 0)} />);
    await pointerIn(navOf(el));

    await act(async () => {
      closeBtn(tabsOf(el)[3]).click();
    });
    await act(async () => {
      closeBtn(tabsOf(el)[3]).click();
    });

    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(4);
    expect(tabs.map(pinnedPx)).toEqual([120, 120, 120, 120]);
  });

  it("can freeze again after a release (second hover, second close)", async () => {
    const el = await mountNode(<Harness initial={manyTabs(6, 0)} />);
    const nav = navOf(el);
    await pointerIn(nav);
    await act(async () => {
      closeBtn(tabsOf(el)[3]).click();
    });
    await pointerOut(nav);
    expect(tabsOf(el).every((t) => pinnedPx(t) === null)).toBe(true);

    layout.tabW = () => 90;
    await pointerIn(nav);
    await act(async () => {
      closeBtn(tabsOf(el)[3]).click();
    });
    expect(tabsOf(el).map(pinnedPx)).toEqual([90, 90, 90, 90]);
  });

  it("does NOT pin when the pointer was never over the bar (keyboard / programmatic close)", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });
    const tabs = tabsOf(el);
    expect(tabs).toHaveLength(4);
    for (const t of tabs) expect(pinnedPx(t)).toBeNull();
  });

  it("does NOT pin for a touch pointer (only the mouse)", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    await pointerIn(navOf(el), "touch");
    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });
    for (const t of tabsOf(el)) expect(pinnedPx(t)).toBeNull();
  });

  it("does NOT pin when the pointer already left before the close", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    const nav = navOf(el);
    await pointerIn(nav);
    await pointerOut(nav);
    await act(async () => {
      closeBtn(tabsOf(el)[2]).click();
    });
    for (const t of tabsOf(el)) expect(pinnedPx(t)).toBeNull();
  });

  it("leaves tabs un-pinned before any close, even while hovering", async () => {
    const el = await mountNode(<Harness initial={manyTabs(5, 0)} />);
    await pointerIn(navOf(el));
    for (const t of tabsOf(el)) expect(pinnedPx(t)).toBeNull();
  });
});

// ─── wheel ───────────────────────────────────────────────────────────────

describe("ConvoTabBar — wheel scrolls the strip sideways", () => {
  const wheel = (target: Element, deltaX: number, deltaY: number): WheelEvent => {
    const ev = new WheelEvent("wheel", { deltaX, deltaY, bubbles: true, cancelable: true });
    target.dispatchEvent(ev);
    return ev;
  };

  it("a vertical wheel adds deltaY to scrollLeft and prevents the page scroll", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 50, scrollWidth: 2000, clientWidth: 500 });
    const ev = wheel(sc, 0, 100);
    expect((sc as HTMLElement).scrollLeft).toBe(150);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("a mostly-horizontal wheel is left to the browser", async () => {
    setBarWidth(300);
    const el = await mount({ tabs: manyTabs(12), ...noop });
    const sc = await setOverflow(el, { scrollLeft: 50, scrollWidth: 2000, clientWidth: 500 });
    const ev = wheel(sc, 100, 10);
    expect((sc as HTMLElement).scrollLeft).toBe(50);
    expect(ev.defaultPrevented).toBe(false);
  });
});
