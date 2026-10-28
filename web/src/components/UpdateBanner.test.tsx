// Tests for the auto-update banner (autoupdate plan, "Web" section).
//
// EXPECTED EXPORTS — the write-first contract this file is written against
// (web/src/components/UpdateBanner.tsx does not exist yet):
//
//   export function UpdateBanner()   // named export
//   export const POLL_MS = 30_000    // export the poll interval constant
//
// Behaviour under test (all IPC goes through the desktop.ts seam; this file
// stubs `globalThis.__TAURI__.core.invoke` and `globalThis.isTauri`):
//   - not desktop (`isTauri !== true`) → renders nothing, ZERO invoke calls,
//     even after 60s of fake time.
//   - update_status → null → renders nothing (polling keeps running).
//   - available → text "Update available — v{version}" (the dash is U+2014)
//     and a "Restart to update" button.
//   - click "Restart to update" → invoke("update_install") exactly once,
//     text "Restarting…" (U+2026 ellipsis), the button gone or disabled.
//   - installing → "Installing update…", no buttons at all.
//   - failed → "Update to v{version} failed" and a "Retry" button that
//     invokes update_install.
//   - first update_status REJECTION → renders nothing and polling stops
//     forever (no further invoke calls after 30s+).
//   - polls every 30s: a second update_status fires at 30s and a changed
//     status is reflected in the UI.
//   - dismiss: a ✕ button with accessible name "Dismiss" and a "Later"
//     button both hide the banner; the SAME version stays hidden on the
//     next poll; a NEW version shows the banner again.
//   - installUpdate rejection → the failed state is shown locally
//     ("Update to v{version} failed" + Retry) without waiting for a poll.
//   - the banner element has role="status".
//
// Harness modeled on TopBar.test.tsx: createRoot/act from react(-dom)/client,
// raw DOM queries (this repo has no testing-library), jsdom auto-enabled for
// src/components/** by vitest.config.ts. Fake timers throughout — the banner
// polls on a 30_000ms interval.
//
// EXPECTED RED before implementation: the ./UpdateBanner import fails
// (module absent), so every test here fails for the right reason — the
// feature does not exist yet.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { POLL_MS, UpdateBanner } from "./UpdateBanner";

type TauriGlobal = {
  isTauri?: unknown;
  __TAURI__?: { core?: { invoke?: unknown } };
};
const g = globalThis as TauriGlobal;

// React 19's flushSync checks IS_REACT_ACT_ENVIRONMENT; set once so the
// console stays clean (same as TopBar.test.tsx).
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── mount helpers ──────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;
let invoke: ReturnType<typeof vi.fn>;

interface MountOpts {
  /** false → `isTauri` stays unset (browser); the IPC stub is still
   *  installed so we can assert ZERO calls. Default: true. */
  desktop?: boolean;
  /** Queue of `update_status` results; the last entry repeats.
   *  Default: [null]. */
  statuses?: unknown[];
  /** If set, `update_status` throws (the first-call rejection path). */
  failStatus?: boolean;
  /** `update_install` behaviour. Default: a promise that never settles
   *  (the app is "restarting" — no further UI is expected). */
  install?: () => Promise<unknown>;
}

/** Install the desktop globals and mount <UpdateBanner/>. */
async function mountDesktop(opts: MountOpts = {}): Promise<HTMLDivElement> {
  const statuses = opts.statuses ?? [null];
  let i = 0;
  invoke = vi.fn(async (cmd: string) => {
    if (cmd === "update_status") {
      if (opts.failStatus) throw new Error("boom");
      return statuses[Math.min(i++, statuses.length - 1)];
    }
    if (cmd === "update_install") {
      return opts.install ? opts.install() : new Promise(() => {});
    }
    return undefined;
  });
  if (opts.desktop !== false) g.isTauri = true;
  g.__TAURI__ = { core: { invoke } };

  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<UpdateBanner />);
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
  delete g.__TAURI__;
  delete g.isTauri;
  vi.useRealTimers();
});

beforeEach(() => {
  vi.useFakeTimers();
});

/** Advance fake time inside act so timer-driven renders flush. */
async function advance(ms: number): Promise<void> {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
}

function buttonByText(el: Element, text: string): HTMLButtonElement | null {
  return [...el.querySelectorAll("button")].find(
    (b) => (b.textContent ?? "").trim() === text,
  ) as HTMLButtonElement | null;
}

async function click(btn: HTMLButtonElement): Promise<void> {
  await act(async () => {
    btn.click();
  });
}

function installCalls(): number {
  return invoke.mock.calls.filter(([cmd]) => cmd === "update_install").length;
}

const AVAILABLE_025 = { state: "available", version: "0.25.0" } as const;

// ─── tests ──────────────────────────────────────────────────────────────

describe("UpdateBanner", () => {
  it("exports POLL_MS = 30_000", () => {
    expect(POLL_MS).toBe(30_000);
  });

  it("renders nothing in a browser (not desktop) and makes zero invoke calls, even after 60s", async () => {
    const el = await mountDesktop({ desktop: false });

    expect(el.textContent).toBe("");
    expect(el.querySelector('[role="status"]')).toBeNull();

    await advance(60_000);

    expect(el.textContent).toBe("");
    expect(el.querySelector('[role="status"]')).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("renders nothing while update_status is null (but does the initial fetch)", async () => {
    const el = await mountDesktop({ statuses: [null] });

    expect(el.textContent).toBe("");
    expect(el.querySelector('[role="status"]')).toBeNull();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("update_status");
  });

  it("available: shows 'Update available — v0.25.0' and a 'Restart to update' button", async () => {
    const el = await mountDesktop({ statuses: [AVAILABLE_025] });

    // U+2014 em dash in the sentence — exact string, not "–" or "-".
    expect(el.textContent).toContain("Update available — v0.25.0");
    expect(buttonByText(el, "Restart to update")).not.toBeNull();
  });

  it("clicking 'Restart to update' invokes update_install exactly once and shows 'Restarting…'", async () => {
    const el = await mountDesktop({ statuses: [AVAILABLE_025] });

    const btn = buttonByText(el, "Restart to update");
    expect(btn).not.toBeNull();
    await click(btn!);

    expect(installCalls()).toBe(1);
    // U+2026 horizontal ellipsis — exact character, not "...".
    expect(el.textContent).toContain("Restarting…");
    const after = buttonByText(el, "Restart to update");
    expect(after === null || after.disabled).toBe(true);
  });

  it("installing: shows 'Installing update…' and renders no buttons", async () => {
    const el = await mountDesktop({ statuses: [{ state: "installing" }] });

    expect(el.textContent).toContain("Installing update…");
    expect(el.querySelectorAll("button")).toHaveLength(0);
  });

  it("failed: shows 'Update to v0.25.0 failed' and 'Retry' invokes update_install", async () => {
    const el = await mountDesktop({
      statuses: [{ state: "failed", version: "0.25.0" }],
    });

    expect(el.textContent).toContain("Update to v0.25.0 failed");
    const retry = buttonByText(el, "Retry");
    expect(retry).not.toBeNull();
    await click(retry!);

    expect(installCalls()).toBe(1);
    expect(el.textContent).toContain("Restarting…");
  });

  it("first update_status rejection: renders nothing and never polls again (30s+, 120s)", async () => {
    const el = await mountDesktop({ failStatus: true });

    expect(el.textContent).toBe("");
    expect(invoke).toHaveBeenCalledTimes(1);

    await advance(30_000);
    await advance(30_000);
    await advance(60_000);

    expect(el.textContent).toBe("");
    // Only the initial (rejected) call — polling is dead, not backoff.
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("polls every 30s and reflects a changed status", async () => {
    const el = await mountDesktop({ statuses: [null, AVAILABLE_025] });

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(el.textContent).toBe("");

    await advance(30_000);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(el.textContent).toContain("Update available — v0.25.0");

    await advance(30_000);
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it("dismiss: the ✕ button carries accessible name 'Dismiss' and hides the banner", async () => {
    const el = await mountDesktop({ statuses: [AVAILABLE_025] });

    const dismiss = el.querySelector('button[aria-label="Dismiss"]');
    expect(dismiss, "✕ button must have accessible name 'Dismiss'").not.toBeNull();
    expect(dismiss!.textContent).toContain("✕");
    await click(dismiss as HTMLButtonElement);

    expect(el.querySelector('[role="status"]')).toBeNull();
    expect(el.textContent).toBe("");
  });

  it("dismiss: the 'Later' button hides the banner", async () => {
    const el = await mountDesktop({ statuses: [AVAILABLE_025] });

    const later = buttonByText(el, "Later");
    expect(later).not.toBeNull();
    await click(later!);

    expect(el.querySelector('[role="status"]')).toBeNull();
    expect(el.textContent).toBe("");
  });

  it("dismissed version stays hidden on the next poll; a new version shows again", async () => {
    const el = await mountDesktop({
      statuses: [
        AVAILABLE_025,
        AVAILABLE_025,
        { state: "available", version: "0.26.0" },
      ],
    });

    const dismiss = el.querySelector('button[aria-label="Dismiss"]');
    expect(dismiss).not.toBeNull();
    await click(dismiss as HTMLButtonElement);
    expect(el.querySelector('[role="status"]')).toBeNull();

    // First poll after dismiss: same version → stays hidden.
    await advance(30_000);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(el.querySelector('[role="status"]')).toBeNull();
    expect(el.textContent).not.toContain("v0.25.0");

    // Second poll: new version → the banner is back.
    await advance(30_000);
    expect(invoke).toHaveBeenCalledTimes(3);
    expect(el.textContent).toContain("Update available — v0.26.0");
  });

  it("install rejection: shows the failed state locally with a working 'Retry'", async () => {
    const el = await mountDesktop({
      statuses: [AVAILABLE_025],
      // IPC contract: update_install rejects with a string.
      install: () => Promise.reject("download failed"),
    });

    const btn = buttonByText(el, "Restart to update");
    expect(btn).not.toBeNull();
    await click(btn!);

    expect(el.textContent).toContain("Update to v0.25.0 failed");
    const retry = buttonByText(el, "Retry");
    expect(retry).not.toBeNull();

    await click(retry!);
    expect(installCalls()).toBe(2);
  });

  it("banner element has role='status'", async () => {
    const el = await mountDesktop({ statuses: [AVAILABLE_025] });

    const status = el.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toContain("Update available — v0.25.0");
  });
});
