// Tests for `components/Sheet.tsx` — the bottom-sheet primitive (mobile
// redesign design §2.5, §2.7, §3, S1–S8).
//
// Contract under test:
//   * portal'd to `document.body` (the header's backdrop-blur would trap a
//     fixed child); backdrop `sheet-backdrop`; `role="dialog"`
//     `aria-modal="true"` `aria-label=<title>` `data-testid="sheet"`.
//   * Dismiss: Done (`sheet-done`), backdrop, Esc, OS Back (one
//     `useBackDismiss` entry; mounted = open, the parent renders conditionally).
//   * Detents (flag): opens at medium (55%), draggable grabber/header,
//     `data-detent` + `style.height`, `data-dragging` while dragging; the pure
//     math is sheetDrag.ts (P4) — here only the wiring.
//   * Non-detents: no grabber, no height, no detent, header not draggable;
//     `above` rides the sheet's top edge, outside the clipped panel.
//
// RED before implementation: `./Sheet` does not exist yet, so this file
// fails at import — the right reason (feature absent).
//
// jsdom: no `PointerEvent` guarantee and no layout; the drag contract is
// class/style values (the repo's layout contract). `performance.now` is
// spied for deterministic settle timing.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { act, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Sheet } from "./Sheet";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── environment ──────────────────────────────────────────────────────────

let originalInnerHeight: number;
let nowSpy: MockInstance<() => number>;

beforeEach(() => {
  // The §3 reference numbers use innerHeight 844 (medium 464, large 776).
  originalInnerHeight = window.innerHeight;
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 844 });
  nowSpy = vi.spyOn(performance, "now").mockReturnValue(0);
  window.history.replaceState(null, "", "/");
});

afterEach(async () => {
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value: originalInnerHeight,
  });
  nowSpy.mockRestore();
  vi.restoreAllMocks();
  // A release's history.back() delivers its popstate in a task (jsdom), and
  // the next test's mount must see a clean backStack — settle it here, like
  // the App-level back-stack tests do (App.mobileSheet settlePopstates).
  for (let i = 0; i < 2; i++) {
    await new Promise((r) => setTimeout(r, 0));
  }
  window.history.replaceState(null, "", "/");
});

// ─── harness ──────────────────────────────────────────────────────────────

/** The real usage: the parent renders the sheet conditionally; `onClose`
 * flips it off. */
function Harness({
  onClose,
  detents = false,
  above,
}: {
  onClose: () => void;
  detents?: boolean;
  above?: ReactNode;
}) {
  const [open, setOpen] = useState(true);
  return open ? (
    <Sheet
      title="Panels"
      onClose={() => {
        onClose();
        setOpen(false);
      }}
      detents={detents}
      above={above}
    >
      <div data-testid="sheet-body-marker">sheet body</div>
    </Sheet>
  ) : null;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: { onClose?: () => void; detents?: boolean; above?: ReactNode } = {}): Promise<{
  onClose: ReturnType<typeof vi.fn>;
}> {
  const onClose = vi.fn(props.onClose);
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Harness onClose={onClose} detents={props.detents} above={props.above} />);
  });
  return { onClose };
}

async function unmount(): Promise<void> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
}

/** The dialog if mounted, else null (for the "it is gone" assertions). */
const maybeDialog = (): HTMLElement | null =>
  document.body.querySelector<HTMLElement>('[data-testid="sheet"]');

const dialog = (): HTMLElement => {
  const el = document.body.querySelector<HTMLElement>('[data-testid="sheet"]');
  expect(el, "the sheet dialog must be in document.body").not.toBeNull();
  return el!;
};

/** A pointer event that keeps its clientY in jsdom (PointerEvent when the
 * engine has it, a MouseEvent with the same init otherwise). */
type PointerCtor = new (type: string, init?: Record<string, unknown>) => Event;
function pointer(type: "pointerdown" | "pointermove" | "pointerup" | "pointercancel", clientY: number): Event {
  const Ctor: PointerCtor =
    typeof PointerEvent !== "undefined"
      ? (PointerEvent as unknown as PointerCtor)
      : // Minimal polyfill, this file only: jsdom lacks PointerEvent.
        (class extends MouseEvent {} as unknown as PointerCtor);
  return new Ctor(type, { bubbles: true, cancelable: true, clientY, pointerId: 1 });
}

async function drag(fromY: number, toY: number, upAtMs: number): Promise<void> {
  const start = dialog().querySelector<HTMLElement>('[data-testid="sheet-grabber"]')!;
  expect(start, "the grabber must render on a detents sheet").not.toBeNull();
  await act(async () => {
    nowSpy.mockReturnValue(0);
    start.dispatchEvent(pointer("pointerdown", fromY));
  });
  await act(async () => {
    start.dispatchEvent(pointer("pointermove", toY));
  });
  await act(async () => {
    nowSpy.mockReturnValue(upAtMs);
    start.dispatchEvent(pointer("pointerup", toY));
  });
}

// ─── S1: structure + portal ───────────────────────────────────────────────

describe("Sheet — S1: dialog contract and portal", () => {
  it("renders a role=dialog with the title as its accessible name", async () => {
    await mount();
    const el = dialog();
    expect(el.getAttribute("role")).toBe("dialog");
    expect(el.getAttribute("aria-modal")).toBe("true");
    expect(el.getAttribute("aria-label")).toBe("Panels");
    expect(el.getAttribute("tabindex")).toBe("-1");
    await unmount();
  });

  it("portals under document.body, not under the render container", async () => {
    await mount();
    expect(container!.querySelector('[data-testid="sheet"]'), "not in the container").toBeNull();
    expect(document.body.contains(dialog())).toBe(true);
    const backdrop = document.body.querySelector<HTMLElement>('[data-testid="sheet-backdrop"]');
    expect(backdrop, "the backdrop renders").not.toBeNull();
    expect(dialog().parentElement!.contains(backdrop!)).toBe(true);
    await unmount();
  });

  it("shows the title in the header and a Done button", async () => {
    await mount();
    const header = document.body.querySelector<HTMLElement>('[data-testid="sheet-header"]');
    expect(header, "the header renders").not.toBeNull();
    expect(header!.textContent).toContain("Panels");
    const done = document.body.querySelector<HTMLElement>('[data-testid="sheet-done"]');
    expect(done, "the Done button renders").not.toBeNull();
    expect(done!.textContent).toContain("Done");
    await unmount();
  });

  it("focuses the dialog on mount", async () => {
    await mount();
    expect(document.activeElement, "the dialog takes focus").toBe(dialog());
    await unmount();
  });
});

// ─── S2: dismiss paths ────────────────────────────────────────────────────

describe("Sheet — S2: Done, backdrop and Esc each close once", () => {
  it("the Done button calls onClose exactly once", async () => {
    const { onClose } = await mount();
    await act(async () => {
      document.body
        .querySelector<HTMLElement>('[data-testid="sheet-done"]')!
        .dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await unmount();
  });

  it("the backdrop calls onClose exactly once", async () => {
    const { onClose } = await mount();
    await act(async () => {
      document.body
        .querySelector<HTMLElement>('[data-testid="sheet-backdrop"]')!
        .dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await unmount();
  });

  it("Esc on the document calls onClose exactly once", async () => {
    const { onClose } = await mount();
    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
      );
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await unmount();
  });

  it("non-Esc keys do not close", async () => {
    const { onClose } = await mount();
    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true }));
    });
    expect(onClose).not.toHaveBeenCalled();
    await unmount();
  });
});

// ─── S3: OS Back ──────────────────────────────────────────────────────────

describe("Sheet — S3: OS Back closes without a second traversal", () => {
  it("mount pushes exactly one history entry", async () => {
    const pushSpy = vi.spyOn(window.history, "pushState");
    await mount();
    expect(pushSpy, "one marker at mount").toHaveBeenCalledTimes(1);
    pushSpy.mockRestore();
    await unmount();
  });

  it("a popstate calls onClose and the app issues no back() of its own", async () => {
    await mount();
    const backSpy = vi.spyOn(window.history, "back");
    const popped = new Promise<void>((resolve) => {
      window.addEventListener("popstate", () => resolve(), { once: true });
    });
    act(() => {
      window.history.back();
    });
    await act(async () => {
      await popped;
    });
    expect(maybeDialog(), "the sheet is gone").toBeNull();
    expect(
      backSpy.mock.calls.length,
      "only the user's Back traversed",
    ).toBe(1);
    backSpy.mockRestore();
    await unmount();
  });
});

// ─── S4: UI close consumes the entry ──────────────────────────────────────

describe("Sheet — S4: a UI close then unmount issues exactly one back()", () => {
  it("consumes the marker with one traversal", async () => {
    const { onClose } = await mount();
    const backSpy = vi.spyOn(window.history, "back");
    // Close via the UI (Done), then unmount the (now null) harness.
    await act(async () => {
      document.body
        .querySelector<HTMLElement>('[data-testid="sheet-done"]')!
        .dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    const popped = new Promise<void>((resolve) => {
      const t = setTimeout(resolve, 250);
      window.addEventListener("popstate", () => {
        clearTimeout(t);
        resolve();
      });
    });
    await act(async () => {
      await popped;
    });
    expect(backSpy, "the release consumed the entry").toHaveBeenCalledTimes(1);
    backSpy.mockRestore();
    await unmount();
  });
});

// ─── S5: detents open at medium ───────────────────────────────────────────

describe("Sheet — S5: detents open at medium", () => {
  it("the dialog carries data-detent=medium and the medium height at 844", async () => {
    await mount({ detents: true });
    const el = dialog();
    expect(el.getAttribute("data-detent")).toBe("medium");
    expect(el.style.height).toBe("464px");
    expect(document.body.querySelector('[data-testid="sheet-grabber"]'), "grabber renders").not.toBeNull();
    await unmount();
  });
});

// ─── S6: the §3 drag cases ────────────────────────────────────────────────

describe("Sheet — S6: drag (the §3 reference cases, innerHeight 844)", () => {
  it("up: 400 → 100 gives 764px mid-drag; a slow release settles large (776px)", async () => {
    await mount({ detents: true });
    const start = dialog().querySelector<HTMLElement>('[data-testid="sheet-grabber"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      start.dispatchEvent(pointer("pointerdown", 400));
    });
    await act(async () => {
      start.dispatchEvent(pointer("pointermove", 100));
    });
    expect(dialog().style.height, "mid-drag height follows the finger").toBe("764px");
    expect(dialog().hasAttribute("data-dragging"), "data-dragging while dragging").toBe(true);
    await act(async () => {
      nowSpy.mockReturnValue(1000);
      start.dispatchEvent(pointer("pointerup", 100));
    });
    expect(dialog().getAttribute("data-detent")).toBe("large");
    expect(dialog().style.height).toBe("776px");
    expect(dialog().hasAttribute("data-dragging")).toBe(false);
    await unmount();
  });

  it("down to the bottom: the sheet closes (dialog gone, exactly one back)", async () => {
    const { onClose } = await mount({ detents: true });
    const backSpy = vi.spyOn(window.history, "back");
    const start = dialog().querySelector<HTMLElement>('[data-testid="sheet-grabber"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      start.dispatchEvent(pointer("pointerdown", 400));
    });
    // 400 → 1264: dy 864, h = 464 − 864 → clamped 0.
    await act(async () => {
      start.dispatchEvent(pointer("pointermove", 1264));
    });
    expect(dialog().style.height).toBe("0px");
    await act(async () => {
      nowSpy.mockReturnValue(1000);
      start.dispatchEvent(pointer("pointerup", 1264));
    });
    expect(onClose, "the release at h 0 closes").toHaveBeenCalledTimes(1);
    expect(maybeDialog(), "the dialog is gone").toBeNull();
    const popped = new Promise<void>((resolve) => {
      const t = setTimeout(resolve, 250);
      window.addEventListener("popstate", () => {
        clearTimeout(t);
        resolve();
      });
    });
    await act(async () => {
      await popped;
    });
    expect(backSpy, "the unmount's release issues exactly one back()").toHaveBeenCalledTimes(1);
    backSpy.mockRestore();
    await unmount();
  });

  it("a fast flick down (dy 40 in 50ms, vy 0.8) closes", async () => {
    const { onClose } = await mount({ detents: true });
    await drag(400, 440, 50);
    expect(onClose, "the flick closes").toHaveBeenCalledTimes(1);
    expect(maybeDialog(), "the dialog is gone").toBeNull();
    await unmount();
  });

  it("a small drag (dy 30 in 10ms) settles back to medium", async () => {
    await mount({ detents: true });
    await drag(400, 430, 10);
    expect(dialog().getAttribute("data-detent")).toBe("medium");
    expect(dialog().style.height).toBe("464px");
    await unmount();
  });

  it("a header drag works too (the header is a drag start)", async () => {
    await mount({ detents: true });
    const header = document.body.querySelector<HTMLElement>('[data-testid="sheet-header"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      header.dispatchEvent(pointer("pointerdown", 400));
    });
    await act(async () => {
      header.dispatchEvent(pointer("pointermove", 100));
    });
    expect(dialog().style.height).toBe("764px");
    await act(async () => {
      nowSpy.mockReturnValue(1000);
      header.dispatchEvent(pointer("pointerup", 100));
    });
    expect(dialog().getAttribute("data-detent")).toBe("large");
    await unmount();
  });

  it("pointercancel settles like a release (no crash, no close for a small drag)", async () => {
    await mount({ detents: true });
    const start = dialog().querySelector<HTMLElement>('[data-testid="sheet-grabber"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      start.dispatchEvent(pointer("pointerdown", 400));
    });
    await act(async () => {
      start.dispatchEvent(pointer("pointermove", 430));
    });
    await act(async () => {
      nowSpy.mockReturnValue(10);
      start.dispatchEvent(pointer("pointercancel", 430));
    });
    expect(dialog().getAttribute("data-detent"), "cancels settle, they don't crash").toBe("medium");
    await unmount();
  });
});

// ─── S7: Done is not a drag start ─────────────────────────────────────────

describe("Sheet — S7: a pointerdown on Done does not start a drag", () => {
  it("pressing Done drags nothing and releases without closing", async () => {
    const { onClose } = await mount({ detents: true });
    const done = document.body.querySelector<HTMLElement>('[data-testid="sheet-done"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      done.dispatchEvent(pointer("pointerdown", 400));
    });
    expect(dialog().hasAttribute("data-dragging"), "no drag from a button").toBe(false);
    await act(async () => {
      done.dispatchEvent(pointer("pointermove", 100));
    });
    expect(dialog().style.height, "the height is untouched").toBe("464px");
    await act(async () => {
      nowSpy.mockReturnValue(1000);
      done.dispatchEvent(pointer("pointerup", 100));
    });
    expect(onClose, "no accidental close").not.toHaveBeenCalled();
    expect(dialog().getAttribute("data-detent")).toBe("medium");
    await unmount();
  });
});

// ─── S8: non-detents ──────────────────────────────────────────────────────

describe("Sheet — S8: non-detents sheets", () => {
  it("have no grabber, no height and no detent", async () => {
    await mount();
    const el = dialog();
    expect(document.body.querySelector('[data-testid="sheet-grabber"]')).toBeNull();
    expect(el.style.height, "content height, not a detent").toBe("");
    expect(el.hasAttribute("data-detent")).toBe(false);
    await unmount();
  });

  it("a pointerdown on the header does nothing", async () => {
    await mount();
    const header = document.body.querySelector<HTMLElement>('[data-testid="sheet-header"]')!;
    await act(async () => {
      nowSpy.mockReturnValue(0);
      header.dispatchEvent(pointer("pointerdown", 400));
    });
    const el = dialog();
    expect(el.hasAttribute("data-dragging")).toBe(false);
    expect(el.style.height).toBe("");
    await act(async () => {
      header.dispatchEvent(pointer("pointermove", 100));
      header.dispatchEvent(pointer("pointerup", 100));
    });
    expect(el.style.height).toBe("");
    await unmount();
  });

  it("above renders outside the clipped panel, riding the sheet's top edge", async () => {
    await mount({ above: <div data-testid="above-marker">pill</div> });
    const el = dialog();
    const above = el.querySelector<HTMLElement>('[data-testid="above-marker"]')!;
    expect(above, "the above slot renders").not.toBeNull();
    // The above wrapper is a direct child of the dialog, a sibling of the
    // panel — never inside it (the panel is overflow-hidden).
    expect(above.parentElement!.parentElement).toBe(el);
    const header = el.querySelector<HTMLElement>('[data-testid="sheet-header"]')!;
    expect(header.contains(above), "the panel does not clip the above slot").toBe(false);
    await unmount();
  });
});
