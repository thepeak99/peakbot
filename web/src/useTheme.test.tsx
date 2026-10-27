// @vitest-environment jsdom
//
// useTheme + desktop theme reporting (design §5 item 7).
// Written BEFORE the code (TDD).
//
// Contract: the existing useTheme rule (stored `peakbot-theme` else the OS
// `prefers-color-scheme`; dark default) is UNCHANGED; the new behaviour is
// that WHILE THE PANE RUNS IN THE DESKTOP APP (isDesktop()), every theme
// change — the initial one on mount and each toggle — is reported to the
// desktop via `setDesktopTheme(theme)` (the `theme_set` IPC seam in
// ./desktop). In a browser the seam is never touched; a rejecting seam
// must not raise an unhandled error (the `.catch(() => {})` in the hook's
// effect).
//
// Harness: plain createRoot/act (project convention, no
// testing-library). jsdom via the per-file environment header (this file
// sits in `src/`, outside the settings/components jsdom globs).
//
// Expected RED: useTheme does not report to the desktop yet → the
// `theme_set` invoke assertions fail (feature absent).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useTheme } from "./useTheme";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | undefined;
let host: HTMLElement;

afterEach(() => {
  if (root) {
    act(() => root!.unmount());
    host.remove();
    root = undefined;
  }
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (globalThis as { __TAURI__?: unknown }).__TAURI__;
  localStorage.clear();
});

// ── plumbing ────────────────────────────────────────────────────────────

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(`[data-testid='${id}']`);
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Flush the microtasks the effect's `setDesktopTheme(…).catch()` chain needs. */
async function settle() {
  for (let i = 0; i < 4; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/** jsdom has no matchMedia: stub the ONE query useTheme makes. */
function stubMatchMedia(light: boolean) {
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: light })));
}

/** Install `globalThis.__TAURI__.core.invoke` and return the mock. */
function stubInvoke(
  impl: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> = async () => undefined,
): ReturnType<typeof vi.fn> {
  const invoke = vi.fn(impl);
  (globalThis as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
  return invoke;
}

/** Renders the hook's state + a toggle button. */
function Probe() {
  const { theme, toggle } = useTheme();
  return (
    <div>
      <span data-testid="probe-theme">{theme}</span>
      <button data-testid="probe-toggle" onClick={toggle}>
        toggle
      </button>
    </div>
  );
}

describe("useTheme desktop reporting", () => {
  it("reports the initial theme on mount and the new theme after toggle", async () => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    const invoke = stubInvoke();
    stubMatchMedia(false); // no stored theme, OS dark ⇒ initial "dark"

    mount(<Probe />);
    await settle();

    expect(q("probe-theme").textContent).toBe("dark");
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenLastCalledWith("theme_set", { theme: "dark" });

    click(q("probe-toggle"));
    await settle();

    expect(q("probe-theme").textContent).toBe("light");
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(invoke).toHaveBeenLastCalledWith("theme_set", { theme: "light" });
  });

  it("respects a stored theme before falling back to the OS preference", async () => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    const invoke = stubInvoke();
    stubMatchMedia(false); // OS dark — must LOSE to the stored light theme
    localStorage.setItem("peakbot-theme", "light");

    mount(<Probe />);
    await settle();

    expect(q("probe-theme").textContent).toBe("light");
    expect(invoke).toHaveBeenLastCalledWith("theme_set", { theme: "light" });
  });

  it("never calls theme_set in a browser", async () => {
    const invoke = stubInvoke();
    stubMatchMedia(true);

    mount(<Probe />);
    await settle();
    expect(q("probe-theme").textContent).toBe("light");
    expect(invoke).not.toHaveBeenCalled();

    click(q("probe-toggle"));
    await settle();
    expect(q("probe-theme").textContent).toBe("dark");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("a rejecting theme_set stub raises no unhandled error", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    (globalThis as { isTauri?: unknown }).isTauri = true;
    const invoke = vi.fn(async () => {
      throw new Error("ipc down");
    });
    (globalThis as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
    stubMatchMedia(false);

    mount(<Probe />);
    await settle();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(q("probe-theme").textContent).toBe("dark"); // state unaffected

    click(q("probe-toggle"));
    await settle();
    expect(q("probe-theme").textContent).toBe("light");
    expect(invoke).toHaveBeenCalledTimes(2);
  });
});