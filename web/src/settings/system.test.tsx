// T30 — System pane (D7) + Desktop link (D8). Written BEFORE the code (TDD).
//
// Spec: make-config-great-again.md §9.10; contract: ./contract.ts
// ("./system (T30)"). Pinned behaviours:
//   - desktop_link_only_in_desktop   — `nav-desktop-link` in the Settings nav
//     iff isDesktop() (globalThis.isTauri === true).
//   - desktop_link_navigates_sentinel — click assigns
//     window.location.href = "peakbot-desktop:settings" and does not switch
//     the active pane.
//   - system_shows_version_and_paths  — facts.version + the Locations rows.
//   - service_install_calls_api       — "Enable at login" → POST
//     /api/system/service; plus Disable (DELETE), Refresh (GET), binary
//     install (POST /api/system/install) and error display.
//
// Harness: plain createRoot/act (project convention, no testing-library).
// jsdom is enabled for `src/settings/**/*.test.tsx`; fetch is stubbed.
//
// Expected RED: `./system` does not exist yet → import error; the desktop
// link tests fail on the missing `nav-desktop-link` testid.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SystemPane } from "./system";
import { Settings } from "./Settings";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { SettingsMeta } from "./types";

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
  delete (globalThis as { isTauri?: unknown }).isTauri;
});

// ── fixtures ────────────────────────────────────────────────────────────
type Install = { target: string; state: string; path: Record<string, unknown> } | null;

function meta(install: Install = {
  target: "/home/u/.local/bin/peakbot",
  state: "other",
  path: { status: "on_path" },
}): SettingsMeta {
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
      install,
    },
  };
}

function report(over: Record<string, unknown> = {}) {
  return {
    manager: "systemd-user",
    name: "peakbot.service",
    artifact: null,
    installed: false,
    exe: null,
    run_state: "stopped",
    tls: false,
    survives_logout: true,
    commands: [],
    notes: [],
    ...over,
  };
}

interface Call {
  url: string;
  method: string;
  body: unknown;
  contentType: string | null;
}

type Reply = { status?: number; json: unknown };

/** Stub fetch; `routes` maps "METHOD /path" → reply (or a function). */
function stubFetch(routes: Record<string, Reply | (() => Reply)>): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      const ct = init?.headers ? new Headers(init.headers).get("content-type") : null;
      let body: unknown = undefined;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body);
        } catch {
          body = init.body;
        }
      }
      calls.push({ url, method, body, contentType: ct });
      const path = url.split("?")[0];
      const hit = routes[`${method} ${path}`];
      if (!hit) return new Response(JSON.stringify({ error: `no route ${method} ${path}` }), { status: 404 });
      const r = typeof hit === "function" ? hit() : hit;
      return new Response(JSON.stringify(r.json), {
        status: r.status ?? 200,
        headers: { "content-type": "application/json" },
      });
    }),
  );
  return calls;
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(`[data-testid='${id}']`);
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}
const has = (id: string) => host.querySelector(`[data-testid='${id}']`) !== null;
const text = (id: string) => q(id).textContent ?? "";

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

function mountPane(m: SettingsMeta) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: m,
    doc: parse(""),
    globalDoc: parse(""),
    edit: () => {},
    errors: [],
    switchToGlobal: () => {},
  };
  act(() =>
    root!.render(
      <ToastProvider>
        <SettingsCtx.Provider value={ctx}>
          <SystemPane />
        </SettingsCtx.Provider>
      </ToastProvider>,
    ),
  );
}

async function mountShell() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<Settings convo={null} onClose={() => {}} />));
  await flush();
}

const SHELL_ROUTES = {
  "GET /api/settings": { json: meta() },
  "GET /api/config": {
    json: { scope: "global", path: "/home/u/.config/peakbot/config.yaml", exists: true, text: "", sha256: "abc" },
  },
  "GET /api/system/service": { json: report() },
};

// ── D8: Desktop link ───────────────────────────────────────────────────
describe("Desktop app settings link (D8)", () => {
  it("desktop_link_only_in_desktop", async () => {
    stubFetch(SHELL_ROUTES);

    // Browser: no globalThis.isTauri → no link at all.
    await mountShell();
    expect(has("nav-desktop-link")).toBe(false);
    expect(host.textContent).not.toContain("Desktop app settings");

    // Anything but a strict `true` is not the desktop app.
    act(() => root!.unmount());
    host.remove();
    root = undefined;
    (globalThis as { isTauri?: unknown }).isTauri = "true";
    await mountShell();
    expect(has("nav-desktop-link")).toBe(false);

    // Desktop: the link is present, labelled per spec, in the MACHINE group
    // after System.
    act(() => root!.unmount());
    host.remove();
    root = undefined;
    (globalThis as { isTauri?: unknown }).isTauri = true;
    await mountShell();
    const link = q("nav-desktop-link");
    expect(link.textContent).toContain("Desktop app settings ↗");
    const system = q("nav-system");
    expect(
      system.compareDocumentPosition(link) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("desktop_link_navigates_sentinel", async () => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    stubFetch(SHELL_ROUTES);
    // jsdom cannot navigate to a custom scheme: replace location with a
    // recorder so the assignment is observable.
    const loc = { href: "http://localhost/" };
    vi.stubGlobal("location", loc);

    await mountShell();
    click(q("nav-system"));
    await flush();
    expect(has("system-pane")).toBe(true);

    const ev = new MouseEvent("click", { bubbles: true, cancelable: true });
    act(() => {
      q("nav-desktop-link").dispatchEvent(ev);
    });
    expect(loc.href).toBe("peakbot-desktop:settings");
    // A link, not a pane: the System pane stays the active one.
    expect(has("system-pane")).toBe(true);
  });
});

// ── D7: System pane ────────────────────────────────────────────────────
describe("SystemPane (D7)", () => {
  it("system_shows_version_and_paths", async () => {
    stubFetch({ "GET /api/system/service": { json: report() } });
    mountPane(meta());
    await flush();

    expect(has("system-pane")).toBe(true);
    expect(text("system-version")).toContain("0.24.7");
    expect(text("system-path-config")).toContain("/home/u/.config/peakbot/config.yaml");
    expect(text("system-path-repo")).toContain("/work/repo/.peakbot/config.yaml");
    expect(text("system-path-data")).toContain("/home/u/.local/share/peakbot");
    expect(text("system-path-binary")).toContain("/opt/peakbot/bin/peakbot");
    expect(text("system-path-install")).toContain("/home/u/.local/bin/peakbot");
    expect(text("system-path-pill")).toMatch(/on PATH/i);
    // Spec: the mockup's "Check for updates" is fake — omitted in v1.
    expect(host.textContent ?? "").not.toMatch(/check for updates/i);
  });

  it("path_pill_reports_not_on_path_and_missing_install_info_does_not_crash", async () => {
    stubFetch({ "GET /api/system/service": { json: report() } });
    mountPane(
      meta({
        target: "/home/u/.local/bin/peakbot",
        state: "absent",
        path: { status: "absent", hint: "add ~/.local/bin to PATH" },
      }),
    );
    await flush();
    expect(text("system-path-pill")).toMatch(/not on PATH/i);

    act(() => root!.unmount());
    host.remove();
    root = undefined;

    // facts.install === null (e.g. no install target on this platform).
    stubFetch({ "GET /api/system/service": { json: report() } });
    mountPane(meta(null));
    await flush();
    expect(has("system-pane")).toBe(true);
    expect(text("system-version")).toContain("0.24.7");
  });

  it("service_status_loaded_on_mount", async () => {
    const calls = stubFetch({
      "GET /api/system/service": {
        json: report({ installed: true, run_state: "running", manager: "launchd-agent" }),
      },
    });
    mountPane(meta());
    await flush();

    expect(calls.filter((c) => c.method === "GET" && c.url === "/api/system/service")).toHaveLength(1);
    expect(text("system-service-kind")).toContain("launchd-agent");
    expect(text("system-service-state")).toContain("running");
    const installed = text("system-service-installed");
    expect(installed).toMatch(/installed/i);
    expect(installed).not.toMatch(/not installed/i);
  });

  it("service_install_calls_api", async () => {
    const calls = stubFetch({
      "GET /api/system/service": { json: report({ installed: false, run_state: "stopped" }) },
      "POST /api/system/service": { json: report({ installed: true, run_state: "running" }) },
    });
    mountPane(meta());
    await flush();
    expect(text("system-service-installed")).toMatch(/not installed/i);
    expect(text("system-service-state")).toContain("stopped");

    click(q("system-service-enable"));
    await flush();

    const posts = calls.filter((c) => c.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0].url).toBe("/api/system/service");
    expect(posts[0].contentType ?? "").toContain("application/json");
    expect((posts[0].body as { op?: string }).op).toBe("install");
    // The pane shows the report the POST returned.
    expect(text("system-service-state")).toContain("running");
    const installed = text("system-service-installed");
    expect(installed).toMatch(/installed/i);
    expect(installed).not.toMatch(/not installed/i);
    expect(has("system-error")).toBe(false);
  });

  it("service_uninstall_calls_delete", async () => {
    const calls = stubFetch({
      "GET /api/system/service": { json: report({ installed: true, run_state: "running" }) },
      "DELETE /api/system/service": { json: report({ installed: false, run_state: "stopped" }) },
    });
    mountPane(meta());
    await flush();

    click(q("system-service-disable"));
    await flush();

    const dels = calls.filter((c) => c.method === "DELETE");
    expect(dels).toHaveLength(1);
    expect(dels[0].url).toBe("/api/system/service");
    expect(text("system-service-installed")).toMatch(/not installed/i);
    expect(text("system-service-state")).toContain("stopped");
  });

  it("service_refresh_refetches_status", async () => {
    let n = 0;
    const calls = stubFetch({
      "GET /api/system/service": () => ({
        json: report({ installed: true, run_state: n++ === 0 ? "stopped" : "running" }),
      }),
    });
    mountPane(meta());
    await flush();
    expect(text("system-service-state")).toContain("stopped");

    click(q("system-service-refresh"));
    await flush();

    expect(calls.filter((c) => c.method === "GET" && c.url === "/api/system/service")).toHaveLength(2);
    expect(text("system-service-state")).toContain("running");
  });

  it("service_error_is_shown_and_state_kept", async () => {
    stubFetch({
      "GET /api/system/service": { json: report({ installed: false }) },
      "POST /api/system/service": {
        status: 422,
        json: { error: "a LAN bind requires a token" },
      },
    });
    mountPane(meta());
    await flush();

    click(q("system-service-enable"));
    await flush();

    expect(text("system-error")).toContain("a LAN bind requires a token");
    // Failed install: still reported as not installed.
    expect(text("system-service-installed")).toMatch(/not installed/i);
  });

  it("binary_install_posts_to_install_endpoint", async () => {
    const calls = stubFetch({
      "GET /api/system/service": { json: report() },
      "POST /api/system/install": {
        json: {
          source: "/opt/peakbot/bin/peakbot",
          target: "/home/u/.local/bin/peakbot",
          action: "installed",
          path: { status: "on_path" },
          notes: [],
        },
      },
    });
    mountPane(meta());
    await flush();
    expect(has("system-install-result")).toBe(false);

    click(q("system-install"));
    await flush();

    const posts = calls.filter((c) => c.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0].url).toBe("/api/system/install");
    expect(posts[0].contentType ?? "").toContain("application/json");
    expect(typeof posts[0].body).toBe("object");
    const result = text("system-install-result");
    expect(result).toContain("installed");
    expect(result).toContain("/home/u/.local/bin/peakbot");
  });
});
