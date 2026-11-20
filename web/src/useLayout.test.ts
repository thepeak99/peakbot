// @vitest-environment jsdom
// layoutOf matrix (N1) + useLayout hook (N2) — the single layout decision seam.
//
// N1: layoutOf(shell, phoneBrowserViewport) must return "phone" only when the
// shell is "mobile" or the shell is "browser" AND the phone viewport query
// matches; otherwise "desktop". The desktop shell is ALWAYS desktop regardless
// of viewport, which fixes the narrow-desktop-app overflow bug.
//
// N2: useLayout() must subscribe to PHONE_BROWSER_QUERY once, mirror the
// result onto document.documentElement.dataset.layout via useLayoutEffect,
// clean up by deleting the attribute, and never subscribe to
// "(min-width: 1024px)" (that query is retired by this design).
//
// RED before implementation: useLayout.ts does not exist yet, so these fail
// on the missing module (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";

// ─── N1: layoutOf matrix ─────────────────────────────────────────────────

// Dynamic import so the file fails on the missing module, not on a broken
// import of the whole file — the existing module-surface pattern.
type LayoutModule = {
  layoutOf?: (shell: string, phone: boolean) => string;
  PHONE_BROWSER_QUERY?: string;
  useLayout?: () => string;
};

async function loadLayoutModule(): Promise<{
  layoutOf: (shell: string, phone: boolean) => string;
  PHONE_BROWSER_QUERY: string;
  useLayout: () => string;
}> {
  const mod = (await import("./useLayout")) as LayoutModule;
  if (typeof mod.layoutOf !== "function") {
    throw new Error("useLayout.ts does not export layoutOf() yet");
  }
  if (typeof mod.PHONE_BROWSER_QUERY !== "string") {
    throw new Error("useLayout.ts does not export PHONE_BROWSER_QUERY yet");
  }
  if (typeof mod.useLayout !== "function") {
    throw new Error("useLayout.ts does not export useLayout() yet");
  }
  return {
    layoutOf: mod.layoutOf,
    PHONE_BROWSER_QUERY: mod.PHONE_BROWSER_QUERY,
    useLayout: mod.useLayout,
  };
}

describe("layoutOf — the layout decision matrix (N1)", () => {
  let layoutOf: (shell: string, phone: boolean) => string;

  beforeAll(async () => {
    layoutOf = (await loadLayoutModule()).layoutOf;
  });

  it.each([
    ["mobile shell, phone viewport true", "mobile", true, "phone"],
    ["mobile shell, phone viewport false", "mobile", false, "phone"],
    ["desktop shell, phone viewport true", "desktop", true, "desktop"],
    ["desktop shell, phone viewport false", "desktop", false, "desktop"],
    ["browser shell, phone viewport true", "browser", true, "phone"],
    ["browser shell, phone viewport false", "browser", false, "desktop"],
  ] as const)(
    "layoutOf(%j, %j) → %j: %s",
    (_label, shell, phone, expected) => {
      expect(layoutOf(shell, phone)).toBe(expected);
    },
  );
});

// ─── N2: useLayout hook ──────────────────────────────────────────────────

// Controllable matchMedia — captures every query the hook subscribes to so
// N2 can assert the hook uses PHONE_BROWSER_QUERY and never "(min-width: 1024px)".
interface FakeMql {
  query: string;
  matches: boolean;
  listeners: Set<(e: MediaQueryListEvent) => void>;
}
let mqls: FakeMql[] = [];

function installMatchMedia(): void {
  mqls = [];
  vi.stubGlobal("matchMedia", (query: string) => {
    const mql: FakeMql = { query, matches: false, listeners: new Set() };
    mqls.push(mql);
    return {
      get matches() {
        return mql.matches;
      },
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: (_: string, l: (e: MediaQueryListEvent) => void) => mql.listeners.add(l),
      removeEventListener: (_: string, l: (e: MediaQueryListEvent) => void) => mql.listeners.delete(l),
      dispatchEvent: () => false,
    } as unknown as MediaQueryList;
  });
}

/** Flip the phone viewport query and fire its listeners. */
async function setPhoneViewport(on: boolean): Promise<void> {
  await act(async () => {
    for (const m of mqls) {
      if (!m.query.includes("max-width: 1023px")) continue;
      m.matches = on;
      for (const l of m.listeners) l({ matches: on, media: m.query } as MediaQueryListEvent);
    }
  });
}

import { useLayout } from "./useLayout";

// Manual hook rendering via a wrapper component (no @testing-library/react).
// The wrapper stores the hook's return value on document.body.dataset.layout
// so we can read it from the test.
let hookResult = "unknown";
function LayoutTestWrapper() {
  hookResult = useLayout();
  return null;
}

async function renderHookWrapper(): Promise<{ unmount: () => void }> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(LayoutTestWrapper));
  });
  return {
    unmount: () => {
      act(() => root.unmount());
      el.remove();
    },
  };
}

describe("useLayout hook (N2)", () => {
  beforeAll(() => {
    (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
      .IS_REACT_ACT_ENVIRONMENT = true;
  });

  beforeEach(() => {
    installMatchMedia();
    document.documentElement.removeAttribute("data-layout");
    // Default: browser shell, phone viewport false
    delete (globalThis as unknown as { peakbotShell?: unknown }).peakbotShell;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    document.documentElement.removeAttribute("data-layout");
    hookResult = "unknown";
  });

  it("exports PHONE_BROWSER_QUERY matching '(max-width: 1023px) and (pointer: coarse)'", async () => {
    const { PHONE_BROWSER_QUERY } = await loadLayoutModule();
    expect(PHONE_BROWSER_QUERY).toBe("(max-width: 1023px) and (pointer: coarse)");
  });

  it("sets html[data-layout='desktop'] for browser with phone query false", async () => {
    const { unmount } = await renderHookWrapper();
    expect(hookResult).toBe("desktop");
    expect(document.documentElement.dataset.layout).toBe("desktop");
    unmount();
  });

  it("sets html[data-layout='phone'] for browser with phone query true", async () => {
    const { unmount } = await renderHookWrapper();
    expect(hookResult).toBe("desktop"); // initial: phone query false

    await setPhoneViewport(true);
    expect(hookResult).toBe("phone");
    expect(document.documentElement.dataset.layout).toBe("phone");
    unmount();
  });

  it("flips from phone to desktop when the viewport widens", async () => {
    const { unmount } = await renderHookWrapper();

    await setPhoneViewport(true);
    expect(hookResult).toBe("phone");

    await setPhoneViewport(false);
    expect(hookResult).toBe("desktop");
    expect(document.documentElement.dataset.layout).toBe("desktop");
    unmount();
  });

  it("sets phone layout for mobile shell regardless of viewport", async () => {
    (globalThis as unknown as { peakbotShell?: unknown }).peakbotShell = "mobile";
    const { unmount } = await renderHookWrapper();
    expect(hookResult).toBe("phone");
    expect(document.documentElement.dataset.layout).toBe("phone");
    unmount();
  });

  it("sets desktop layout for desktop shell regardless of viewport", async () => {
    (globalThis as unknown as { peakbotShell?: unknown }).peakbotShell = "desktop";
    const { unmount } = await renderHookWrapper();
    expect(hookResult).toBe("desktop");
    expect(document.documentElement.dataset.layout).toBe("desktop");
    unmount();
  });

  it("unmount removes the data-layout attribute", async () => {
    const { unmount } = await renderHookWrapper();
    expect(document.documentElement.dataset.layout).not.toBeUndefined();
    unmount();
    expect(document.documentElement.dataset.layout).toBeUndefined();
  });

  it("subscribes to PHONE_BROWSER_QUERY, never to '(min-width: 1024px)'", async () => {
    const { PHONE_BROWSER_QUERY } = await loadLayoutModule();
    await renderHookWrapper();

    const queries = mqls.map((m) => m.query);
    expect(queries).toContain(PHONE_BROWSER_QUERY);
    expect(queries).not.toContain("(min-width: 1024px)");
  });
});
