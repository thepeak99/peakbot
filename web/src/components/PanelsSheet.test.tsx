// Tests for `components/PanelsSheet.tsx` — the phone's panels bottom sheet
// and its trigger button (mobile-redesign design §2.5, §2.7, PS1–PS6).
//
// Contract under test:
//   * `PanelsButton {count, onOpen}` — `panels-button`, aria-label
//     "Open panels", badge `panels-badge` with the count, absent at 0.
//   * `PanelsSheet {tabs, active, onActiveChange, onClose, running, onStop}`
//     — a Sheet titled "Panels" with detents; a `role="tablist"` pill strip
//     (`panels-pill` per tab, `data-tab`, `aria-selected`, `panels-pill-badge`
//     only when the tab's badge > 0); the body is a `role="tabpanel"` showing
//     the active tab's EXISTING content; an unknown `active` is an empty
//     body (one render before App's relocation effect); a running turn shows
//     the floating `panels-working` pill whose `panels-stop` button sends the
//     stop (via `onStop`) — the pill unmounts when `running` drops.
//
// RED before implementation: `./PanelsSheet` does not exist yet, so this
// file fails at import — the right reason (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { PanelsButton, PanelsSheet } from "./PanelsSheet";
import type { DrawerTab } from "./TabbedDrawer";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// useMediaQuery / the Sheet's matchMedia-free paths: jsdom has no
// matchMedia; the existing convention is a stub.
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
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
  vi.restoreAllMocks();
  window.history.replaceState(null, "", "/");
});

// ─── fixtures ─────────────────────────────────────────────────────────────

const TABS: DrawerTab[] = [
  { id: "session", label: "Session", content: <p data-testid="session-panel">session body</p>, badge: 0 },
  { id: "todo", label: "Todo", content: <p data-testid="todo-panel">todo body</p>, badge: 3 },
  { id: "bash", label: "Bash", content: <button data-testid="bash-panel">bash marker</button> },
];

// ─── mount plumbing ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(node: React.ReactNode): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(node);
  });
  return container;
}

afterEach(async () => {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
});

interface SheetProps {
  tabs: DrawerTab[];
  active: string;
  onActiveChange(id: string): void;
  onClose(): void;
  running: boolean;
  onStop(): void;
}

const baseProps: SheetProps = {
  tabs: TABS,
  active: "session",
  onActiveChange: () => {},
  onClose: () => {},
  running: false,
  onStop: () => {},
};

function mountSheet(over: Partial<SheetProps> = {}): Promise<HTMLDivElement> {
  return render(<PanelsSheet {...baseProps} {...over} />);
}

const pills = (): HTMLButtonElement[] =>
  Array.from(document.body.querySelectorAll<HTMLButtonElement>('[data-testid="panels-pill"]'));

// ─── PS1: the pill strip ──────────────────────────────────────────────────

describe("PanelsSheet — PS1: pills equal the tabs, in order", () => {
  it("one pill per tab, in the given order, inside a tablist labelled Panels", async () => {
    await mountSheet();
    const tablist = document.body.querySelector<HTMLElement>('[role="tablist"][aria-label="Panels"]');
    expect(tablist, "the pill strip is a tablist labelled Panels").not.toBeNull();
    expect(pills().map((p) => p.getAttribute("data-tab"))).toEqual(["session", "todo", "bash"]);
    for (const pill of pills()) {
      expect(pill.getAttribute("role")).toBe("tab");
    }
  });

  it("the sheet is a dialog titled Panels with a backdrop", async () => {
    await mountSheet();
    const dialog = document.body.querySelector<HTMLElement>('[data-testid="sheet"]');
    expect(dialog, "the sheet dialog renders").not.toBeNull();
    expect(dialog!.getAttribute("aria-label")).toBe("Panels");
    expect(document.body.querySelector('[data-testid="sheet-backdrop"]'), "backdrop").not.toBeNull();
  });
});

// ─── PS2: pill selection ──────────────────────────────────────────────────

describe("PanelsSheet — PS2: a pill tap drives onActiveChange and the body", () => {
  it("the active pill is aria-selected and its tab's content renders in the tabpanel", async () => {
    await mountSheet({ active: "bash" });
    const bashPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="bash"]')!;
    expect(bashPill.getAttribute("aria-selected")).toBe("true");
    const sessionPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="session"]')!;
    expect(sessionPill.getAttribute("aria-selected")).toBe("false");

    const panel = document.body.querySelector<HTMLElement>('[role="tabpanel"]');
    expect(panel, "the body is a tabpanel").not.toBeNull();
    expect(panel!.querySelector('[data-testid="bash-panel"]'), "the active tab's content").not.toBeNull();
    expect(panel!.querySelector('[data-testid="session-panel"]'), "inactive content absent").toBeNull();
  });

  it("tapping a pill calls onActiveChange with its id", async () => {
    const onActiveChange = vi.fn();
    await mountSheet({ onActiveChange });
    const todoPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="todo"]')!;
    await act(async () => {
      todoPill.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onActiveChange).toHaveBeenCalledTimes(1);
    expect(onActiveChange).toHaveBeenCalledWith("todo");
  });
});

// ─── PS3: badges ──────────────────────────────────────────────────────────

describe("PanelsSheet — PS3: pill badges only when > 0", () => {
  it("a tab with badge 3 shows the badge; tabs without one do not", async () => {
    await mountSheet();
    const todoPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="todo"]')!;
    const badge = todoPill.querySelector<HTMLElement>('[data-testid="panels-pill-badge"]');
    expect(badge, "the todo pill shows its badge").not.toBeNull();
    expect(badge!.textContent).toBe("3");
    const sessionPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="session"]')!;
    expect(
      sessionPill.querySelector('[data-testid="panels-pill-badge"]'),
      "badge 0 is hidden",
    ).toBeNull();
    const bashPill = document.body.querySelector<HTMLElement>('[data-testid="panels-pill"][data-tab="bash"]')!;
    expect(
      bashPill.querySelector('[data-testid="panels-pill-badge"]'),
      "absent badge is hidden",
    ).toBeNull();
  });
});

// ─── PS4: the floating working pill ───────────────────────────────────────

describe("PanelsSheet — PS4: the working pill follows `running`", () => {
  it("running=true shows 'working…' and Stop calls onStop once", async () => {
    const onStop = vi.fn();
    await mountSheet({ running: true, onStop });
    const working = document.body.querySelector<HTMLElement>('[data-testid="panels-working"]');
    expect(working, "the working pill renders").not.toBeNull();
    expect(working!.textContent).toContain("working…");
    const stop = document.body.querySelector<HTMLElement>('[data-testid="panels-stop"]');
    expect(stop, "the Stop button renders").not.toBeNull();
    expect(stop!.getAttribute("aria-label")).toBe("Stop");
    await act(async () => {
      stop!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("running=false removes the pill", async () => {
    await mountSheet({ running: false });
    expect(document.body.querySelector('[data-testid="panels-working"]')).toBeNull();
    expect(document.body.querySelector('[data-testid="panels-stop"]')).toBeNull();
  });

  it("the pill unmounts live when running drops (the sheet stays)", async () => {
    await render(<PanelsSheet {...baseProps} running={true} onStop={() => {}} />);
    expect(document.body.querySelector('[data-testid="panels-working"]'), "pill at first").not.toBeNull();
    await act(async () => {
      root!.render(<PanelsSheet {...baseProps} running={false} onStop={() => {}} />);
    });
    expect(document.body.querySelector('[data-testid="panels-working"]'), "pill gone").toBeNull();
    expect(document.body.querySelector('[data-testid="sheet"]'), "the sheet itself stays").not.toBeNull();
  });
});

// ─── PS5: an unknown active tab ───────────────────────────────────────────

describe("PanelsSheet — PS5: an unknown active is an empty body, not a crash", () => {
  it("renders an empty tabpanel for a tab that is not in `tabs`", async () => {
    await mountSheet({ active: "gone" });
    expect(() => document.body.textContent).not.toThrow();
    const panel = document.body.querySelector<HTMLElement>('[role="tabpanel"]');
    expect(panel, "the tabpanel renders").not.toBeNull();
    expect(
      panel!.querySelector('[data-testid="session-panel"], [data-testid="todo-panel"], [data-testid="bash-panel"]'),
      "no tab content renders",
    ).toBeNull();
  });
});

// ─── PS6: PanelsButton ────────────────────────────────────────────────────

describe("PanelsButton — PS6", () => {
  it("is labelled 'Open panels', shows its badge, and calls onOpen", async () => {
    const onOpen = vi.fn();
    const el = await render(<PanelsButton count={4} onOpen={onOpen} />);
    const btn = el.querySelector<HTMLElement>('[data-testid="panels-button"]');
    expect(btn, "the button renders").not.toBeNull();
    expect(btn!.getAttribute("aria-label")).toBe("Open panels");
    const badge = el.querySelector<HTMLElement>('[data-testid="panels-badge"]');
    expect(badge, "the badge renders").not.toBeNull();
    expect(badge!.textContent).toBe("4");
    await act(async () => {
      btn!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("hides the badge at 0", async () => {
    const el = await render(<PanelsButton count={0} onOpen={() => {}} />);
    expect(el.querySelector('[data-testid="panels-badge"]'), "no badge at 0").toBeNull();
    expect(el.querySelector('[data-testid="panels-button"]'), "the button itself stays").not.toBeNull();
  });
});
