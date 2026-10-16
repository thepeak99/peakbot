// Hook-level tests for the Focus-mode preference (plan-focus-mode.md §5
// case 26, §6.1) — written FIRST, expected to be RED until
// `web/src/useFocusMode.ts` exists (the import fails to resolve, so this
// file fails at collection for the right reason).
//
// Contract: useFocusMode() → { focus: boolean; toggle: () => void }
//   - initial focus = readFocusPref(localStorage["peakbot-focus"]),
//     i.e. ON by default, OFF only when the stored value is "off"
//   - the hook is the single source of truth (called once in App; TopBar
//     gets the value via props) — so the hook must PERSIST every state
//     change to localStorage key "peakbot-focus" as "on"/"off"
//
// Lives under src/components/ so vitest runs it in jsdom (the hook needs
// localStorage + a real render). The pure readFocusPref cases live in
// src/focus.test.ts (node env).

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useFocusMode } from "../useFocusMode";

const KEY = "peakbot-focus";

// Minimal probe: renders the hook's state and a button wired to toggle.
function Probe() {
  const { focus, toggle } = useFocusMode();
  return (
    <div>
      <span data-focus-value>{String(focus)}</span>
      <button type="button" onClick={toggle}>
        toggle
      </button>
    </div>
  );
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  // Each test starts from a fresh browser profile.
  localStorage.clear();
});

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Probe />);
  });
  return container;
}

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

// ─── tests ────────────────────────────────────────────────────────────────

describe("useFocusMode", () => {
  it("defaults to focus ON when nothing is stored (fresh browser)", async () => {
    const el = await mount();

    expect(el.querySelector("[data-focus-value]")!.textContent).toBe("true");
    // The locked sketch persists on mount too (useEffect over [focus]),
    // so a fresh browser ends up with an explicit "on" in storage.
    expect(localStorage.getItem(KEY)).toBe("on");
  });

  it("toggling persists 'off' and flips the state to false", async () => {
    const el = await mount();
    expect(el.querySelector("[data-focus-value]")!.textContent).toBe("true");

    const button = el.querySelector("button") as HTMLButtonElement;
    await act(async () => {
      button.click();
    });

    expect(el.querySelector("[data-focus-value]")!.textContent).toBe("false");
    expect(localStorage.getItem(KEY)).toBe("off");

    // Toggling back persists "on" again.
    await act(async () => {
      button.click();
    });
    expect(el.querySelector("[data-focus-value]")!.textContent).toBe("true");
    expect(localStorage.getItem(KEY)).toBe("on");
  });

  it("starts OFF when the stored preference is 'off' (preference kept across reloads)", async () => {
    localStorage.setItem(KEY, "off");
    const el = await mount();

    expect(el.querySelector("[data-focus-value]")!.textContent).toBe("false");
  });
});
