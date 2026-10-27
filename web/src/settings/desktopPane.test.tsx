// D10 — Desktop pane (replaces the D8 "Desktop app settings ↗" link tests).
// Written BEFORE the code (TDD).
//
// Contract: ./contract.ts ("D10"), design "Desktop pane in web Settings —
// scoped remote IPC" §5 item 6. Pinned behaviours:
//   - desktop_nav_only_in_desktop   — `nav-desktop` in the Settings nav
//     (MACHINE group, last, after System) iff isDesktop() (strict
//     globalThis.isTauri === true); the old `nav-desktop-link` and the
//     text "Desktop app settings" no longer exist.
//   - desktop_pane_ready            — loading → ready; h2 "Desktop"; no
//     rewrite banner; checkbox checked/disabled per the view.
//   - desktop_toggle_invokes_command — the checkbox calls
//     invoke("toggle_background") and stays on the SERVER value (no
//     optimistic flip) until the next 1000 ms poll.
//   - desktop_not_offered           — !background_visible → the
//     unsupported note, no checkbox.
//   - desktop_unavailable_fallback  — any rejection (or missing __TAURI__)
//     → the final unavailable state + the "Open desktop settings" button
//     (location.href = "peakbot-desktop:settings"); polling stops.
//   - desktop_poll_stops_on_unmount — the 1000 ms interval is cleared.
//
// Harness: plain createRoot/act (project convention, no
// testing-library). jsdom is enabled for `src/settings/**/*.test.tsx`.
// fetch is stubbed for the shell tests; the desktop IPC is stubbed by
// setting `globalThis.__TAURI__ = { core: { invoke } }` (the seam in
// ../desktop.ts). Fake timers drive the 1000 ms poll.
//
// Expected RED: `./desktopPane` does not exist yet → import error; the
// shell nav test fails on the missing `nav-desktop` testid.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { DesktopPane } from "./desktopPane";
import { Settings } from "./Settings";
import type { SettingsMeta } from "./types";
import type { DesktopView } from "../desktop";

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
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (globalThis as { __TAURI__?: unknown }).__TAURI__;
});

// ── fixtures ────────────────────────────────────────────────────────────

function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/home/u/.config/peakbot/config.yaml",
    repo_path: "/work/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "",
    mcp_status: [],
    boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
    facts: {
      os: "linux",
      arch: "amd64",
      exe_path: "/opt/peakbot/bin/peakbot",
      data_dir: "/home/u/.local/share/peakbot",
      version: "0.24.7",
      install: null,
    },
  };
}

function view(over: Partial<DesktopView> = {}): DesktopView {
  return {
    cert_warning: false,
    background_visible: true,
    background: false,
    background_enabled: true,
    busy: false,
    ...over,
  };
}

type Reply = { status?: number; json: unknown };

/** Stub fetch; `routes` maps "METHOD /path" → reply. */
function stubFetch(routes: Record<string, Reply>): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      calls.push({ url, method });
      const path = url.split("?")[0];
      const hit = routes[`${method} ${path}`];
      if (!hit)
        return new Response(JSON.stringify({ error: `no route ${method} ${path}` }), { status: 404 });
      return new Response(JSON.stringify(hit.json), {
        status: hit.status ?? 200,
        headers: { "content-type": "application/json" },
      });
    }),
  );
  return calls;
}

interface Call {
  url: string;
  method: string;
}

/** Install `globalThis.__TAURI__.core.invoke` and return the mock. */
function stubInvoke(
  impl: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> = async () => undefined,
): ReturnType<typeof vi.fn> {
  const invoke = vi.fn(impl);
  (globalThis as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
  return invoke;
}

// ── mount / DOM plumbing ────────────────────────────────────────────────

/** Mount the pane ALONE (no shell context). */
function mountPane() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<DesktopPane />));
}

/** Mount the real Settings shell. */
async function mountShell() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<Settings convo={null} onClose={() => {}} />));
  await flush();
}

function unmount() {
  act(() => root!.unmount());
  host.remove();
  root = undefined;
}

/** Real-timer microtask flush (shell tests, no fake timers). */
async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

/** Microtask flush while fake timers are running (no real setTimeout). */
async function settle() {
  await act(async () => {
    for (let i = 0; i < 4; i++) await Promise.resolve();
  });
}

/** Advance fake time by `ms` (firing the poll interval) and settle. */
async function poll(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
    for (let i = 0; i < 4; i++) await Promise.resolve();
  });
}

const q = <T extends Element = HTMLElement>(id: string): T => {
  const el = host.querySelector(`[data-testid='${id}']`);
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
};
const has = (id: string) => host.querySelector(`[data-testid='${id}']`) !== null;
const text = (id: string) => q(id).textContent ?? "";

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

const SHELL_ROUTES = {
  "GET /api/settings": { json: meta() },
  "GET /api/config": {
    json: { scope: "global", path: "/home/u/.config/peakbot/config.yaml", exists: true, text: "", sha256: "abc" },
  },
};

// ── D10: nav ────────────────────────────────────────────────────────────

describe("Desktop pane (D10)", () => {
  it("desktop_nav_only_in_desktop", async () => {
    stubFetch(SHELL_ROUTES);

    // Browser: no globalThis.isTauri → no nav item, no old link text.
    await mountShell();
    expect(has("nav-desktop")).toBe(false);
    expect(has("nav-desktop-link")).toBe(false);
    expect(host.textContent).not.toContain("Desktop app settings");
    expect(
      Array.from(host.querySelectorAll<HTMLOptionElement>("select option")).some((o) => o.value === "desktop"),
    ).toBe(false);

    // Anything but a strict `true` is not the desktop app.
    unmount();
    (globalThis as { isTauri?: unknown }).isTauri = "true";
    await mountShell();
    expect(has("nav-desktop")).toBe(false);
    expect(host.textContent).not.toContain("Desktop app settings");

    // Desktop: `nav-desktop` is present, labelled "Desktop", LAST in the
    // MACHINE group (after nav-system), and the mobile select offers it.
    unmount();
    (globalThis as { isTauri?: unknown }).isTauri = true;
    stubFetch(SHELL_ROUTES);
    await mountShell();
    const nav = q("nav-desktop");
    expect(nav.textContent).toContain("Desktop");
    expect(has("nav-desktop-link")).toBe(false);
    expect(host.textContent).not.toContain("Desktop app settings");
    const system = q("nav-system");
    expect(
      system.compareDocumentPosition(nav) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      Array.from(host.querySelectorAll<HTMLOptionElement>("select option")).some((o) => o.value === "desktop"),
    ).toBe(true);
  });

  it("desktop_pane_ready", async () => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    stubFetch(SHELL_ROUTES);
    stubInvoke(async (cmd: string) =>
      cmd === "settings_get" ? view({ background: true, background_enabled: false }) : undefined,
    );

    await mountShell();
    click(q("nav-desktop"));
    // The on-mount poll is in flight: the pane shows its loading state.
    expect(has("desktop-loading")).toBe(true);
    expect(has("desktop-background")).toBe(false);

    await flush();
    expect(has("desktop-loading")).toBe(false);

    // Section head per D10; the rewrite banner is a FORM-pane thing and the
    // Desktop pane is not a form pane.
    const heads = Array.from(host.querySelectorAll("h2")).map((h) => h.textContent ?? "");
    expect(heads).toContain("Desktop");
    expect(has("rewrite-banner")).toBe(false);

    // ready + background_visible: the checkbox is CONTROLLED by the view
    // (checked = view.background, disabled = !view.background_enabled).
    const box = q<HTMLInputElement>("desktop-background");
    expect(box.checked).toBe(true);
    expect(box.disabled).toBe(true);
    expect(q("desktop-pane").textContent).toContain("Keep PeakBot running in background");
    expect(text("desktop-background-note")).toContain(
      "Runs PeakBot as a background service that keeps going after you quit the app. Changing this restarts PeakBot.",
    );
    expect(has("desktop-background-unsupported")).toBe(false);
    expect(has("desktop-unavailable")).toBe(false);
  });

  it("desktop_toggle_invokes_command", async () => {
    vi.useFakeTimers();
    const invoke = stubInvoke(async (cmd: string) =>
      cmd === "settings_get" ? view({ background: false, background_enabled: true }) : undefined,
    );
    mountPane();
    await settle();
    expect(q<HTMLInputElement>("desktop-background").checked).toBe(false);

    click(q("desktop-background"));
    await settle();
    // The on-mount poll + exactly one toggle_background call.
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(invoke).toHaveBeenLastCalledWith("toggle_background", undefined);
    // Controlled, server value only: no optimistic flip.
    expect(q<HTMLInputElement>("desktop-background").checked).toBe(false);

    // The next 1000 ms poll reports the new state → the checkbox follows.
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "settings_get" ? view({ background: true, background_enabled: true }) : undefined,
    );
    await poll(1000);
    expect(q<HTMLInputElement>("desktop-background").checked).toBe(true);
  });

  it("desktop_not_offered", async () => {
    vi.useFakeTimers();
    stubInvoke(async (cmd: string) =>
      cmd === "settings_get" ? view({ background_visible: false, background: true }) : undefined,
    );
    mountPane();
    await settle();

    expect(has("desktop-background-unsupported")).toBe(true);
    expect(text("desktop-background-unsupported")).toBe(
      "Background mode is not available on this platform yet.",
    );
    // No checkbox at all — and the current state is NOT shown as a control.
    expect(has("desktop-background")).toBe(false);
    expect(has("desktop-background-note")).toBe(false);
    expect(has("desktop-unavailable")).toBe(false);
  });

  it("desktop_unavailable_fallback", async () => {
    vi.useFakeTimers();
    // The rejection path must not surface as an unhandled console error.
    vi.spyOn(console, "error").mockImplementation(() => {});
    // jsdom cannot navigate to a custom scheme: record the assignment.
    const loc = { href: "http://localhost/" };
    vi.stubGlobal("location", loc);

    // Variant 1: the IPC stub rejects.
    const invoke = vi.fn(async () => {
      throw new Error("no route");
    });
    (globalThis as { __TAURI__?: unknown }).__TAURI__ = { core: { invoke } };
    mountPane();
    await settle();
    expect(has("desktop-unavailable")).toBe(true);
    expect(text("desktop-unavailable")).toContain(
      "Desktop options can't be loaded here — PeakBot is not on the desktop app's default address, or the desktop app is older.",
    );
    expect(has("desktop-loading")).toBe(false);
    expect(has("desktop-background")).toBe(false);

    // The escape hatch opens the bundled settings page (custom scheme).
    click(q("desktop-open-settings-page"));
    expect(loc.href).toBe("peakbot-desktop:settings");

    // unavailable is FINAL per mount: the on-mount call is the ONLY call —
    // polling stopped, nothing after 3 s of fake time.
    expect(invoke).toHaveBeenCalledTimes(1);
    await poll(3000);
    expect(invoke).toHaveBeenCalledTimes(1);

    // Variant 2: no __TAURI__ at all (isTauri true, older desktop app).
    unmount();
    delete (globalThis as { __TAURI__?: unknown }).__TAURI__;
    (globalThis as { isTauri?: unknown }).isTauri = true;
    mountPane();
    await settle();
    expect(has("desktop-unavailable")).toBe(true);
    expect(has("desktop-open-settings-page")).toBe(true);
  });

  it("desktop_poll_stops_on_unmount", async () => {
    vi.useFakeTimers();
    const invoke = stubInvoke(async (cmd: string) =>
      cmd === "settings_get" ? view() : undefined,
    );
    mountPane();
    await settle();
    // On mount + one 1 s poll.
    await poll(1000);
    expect(invoke).toHaveBeenCalledTimes(2);

    unmount();
    await poll(5000);
    expect(invoke).toHaveBeenCalledTimes(2);
  });
});