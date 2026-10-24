// Tests for `components/TabbedDrawer.tsx` — pointer-events layering.
//
// The drawer <aside> is `fixed right-0 top-14 bottom-14` and is as wide as
// handle rail + 288px body even while closed (the body is only slid/hidden
// off-canvas), so its invisible box swallowed taps meant for the Composer's
// Stop button on phones. Contract:
//   - <aside>                 → `pointer-events-none` (the box itself is inert)
//   - handle rail (tablist)   → `pointer-events-auto` (handles stay tappable)
//   - body (tabpanel), open   → `pointer-events-auto` (content stays usable)
// Class names ARE the layout contract (jsdom has no layout engine).

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { TabbedDrawer, type DrawerTab } from "./TabbedDrawer";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// `useMediaQuery` needs `window.matchMedia`, which jsdom lacks.
let originalMatchMedia: typeof window.matchMedia | undefined;
beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  window.matchMedia = (media: string): MediaQueryList =>
    ({
      matches: false,
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
});

const tabs: DrawerTab[] = [
  { id: "a", label: "Alpha", content: <button data-testid="inner">inside</button> },
  { id: "b", label: "Beta", content: <p>beta body</p> },
];

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(
  active: string | null,
  onActiveChange: (a: string | null) => void = () => {},
): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<TabbedDrawer tabs={tabs} active={active} onActiveChange={onActiveChange} />);
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
});

const has = (el: Element | null, cls: string): boolean => !!el && el.classList.contains(cls);

describe("TabbedDrawer — pointer-events layering (mobile)", () => {
  it("closed: the <aside> is pointer-events-none and the handle rail is pointer-events-auto", async () => {
    const el = await mount(null);
    expect(has(el.querySelector("aside"), "pointer-events-none")).toBe(true);
    expect(has(el.querySelector('[role="tablist"]'), "pointer-events-auto")).toBe(true);
  });

  it("open: the <aside> is still pointer-events-none and the handle rail still pointer-events-auto", async () => {
    const el = await mount("a");
    expect(has(el.querySelector("aside"), "pointer-events-none")).toBe(true);
    expect(has(el.querySelector('[role="tablist"]'), "pointer-events-auto")).toBe(true);
  });

  it("open: the drawer body (tabpanel) is pointer-events-auto so its content stays interactive", async () => {
    const el = await mount("a");
    const body = el.querySelector('[role="tabpanel"]');
    expect(body).not.toBeNull();
    expect(has(body, "pointer-events-auto")).toBe(true);
    // The tab content really is inside that body.
    expect(body!.querySelector('[data-testid="inner"]')).not.toBeNull();
  });

  it("regression: clicking a handle still toggles the tab via onActiveChange", async () => {
    const calls: Array<string | null> = [];
    const el = await mount(null, (a) => calls.push(a));
    const tab = Array.from(el.querySelectorAll<HTMLButtonElement>('[role="tab"]')).find((t) =>
      (t.textContent ?? "").includes("Alpha"),
    )!;
    await act(async () => {
      tab.click();
    });
    expect(calls).toEqual(["a"]);
  });
});
