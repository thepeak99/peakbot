// T34b shell regressions (QA polish), mounted against the REAL Settings
// shell (Settings.tsx) with a stubbed fetch:
//  - fix 2: repo scope's global-only sections (Providers/MCP/General) render
//    the GLOBAL draft's values read-only, and the lock banner's
//    [Switch to Global] button switches the scope back;
//  - fix 3: the error panel title is neutral "N problem(s)" for LIVE
//    validation (before any Save attempt) and only says "Save failed —
//    N problem(s)" after a failed Save (422 PUT);
//  - fix 5: clicking Fix scrolls the flashed/red element into view
//    (scrollIntoView — stubbed here, jsdom has none).
//
// Harness: plain createRoot/act + stubbed globalThis.fetch (project
// convention, modeled on firstRun.test.tsx). Real timers: the SaveBar's
// 400 ms validate debounce is waited out with `wait()`.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Settings } from "./Settings";
import type { ConfigText, PutOk, SettingsMeta, ValidateRes } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixtures ───────────────────────────────────────────────────────────

const GLOBAL_PATH = "/home/u/.config/peakbot/config.yaml";
const REPO_PATH = "/work/repo/.peakbot/config.yaml";

const GLOBAL_TEXT = `providers:
  - name: globalprov
    type: ollama
    base_url: http://127.0.0.1:11434
    models:
      - name: qwen2.5-coder:14b
        alias: coder
        max_tokens: 4096
mcps:
  - name: globalmcp
    type: stdio
    command: npx
general:
  web:
    bind: 127.0.0.1:7823
profiles:
  global:
    agents:
      main:
        model: coder
`;

function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: GLOBAL_PATH,
    repo_path: REPO_PATH,
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "You are Code Crusader.",
    mcp_status: [{ name: "globalmcp", state: "connected" }],
    boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
    facts: {
      os: "linux",
      arch: "amd64",
      exe_path: "/opt/peakbot/bin/peakbot",
      data_dir: "/home/u/.local/share/peakbot",
      version: "0.24.0",
      install: null,
    },
  };
}

const GLOBAL_CFG: ConfigText = {
  scope: "global",
  path: GLOBAL_PATH,
  exists: true,
  text: GLOBAL_TEXT,
  sha256: "gsha",
};
const REPO_CFG: ConfigText = {
  scope: "repo",
  path: REPO_PATH,
  exists: false,
  text: "",
  sha256: null,
};

const VALIDATE_OK: ValidateRes = { ok: true, restart_required: [], warnings: [] };
const VALIDATE_422 = {
  error: "config is not valid",
  errors: [
    { file: "global", path: ["general", "agent_max_turns"], message: "must be a number" },
    { file: "global", path: ["general", "cost_tracking"], message: "must be a bool" },
  ],
};
const PUT_OK: PutOk = {
  path: GLOBAL_PATH,
  backup: null,
  sha256: "newsha",
  restart_required: [],
  applies_on_next_new: true,
};

// ─── mount / DOM plumbing ───────────────────────────────────────────────

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

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

/** Real-time wait inside act (outlasts the SaveBar's 400 ms debounce). */
async function wait(ms: number) {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

const tid = (id: string) => `[data-testid="${id}"]`;
const has = (id: string) => host.querySelector(tid(id)) !== null;
function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

function toggle(checkbox: HTMLInputElement) {
  act(() => {
    checkbox.click();
  });
}

// The header's scope switch buttons ("Global" / "This repo").
function scopeButton(label: string): HTMLButtonElement {
  const btns = Array.from(host.querySelectorAll("header button"));
  const b = btns.find((x) => (x.textContent ?? "").includes(label));
  if (!b) throw new Error(`missing scope button: ${label}`);
  return b as HTMLButtonElement;
}

// ─── fetch stub ─────────────────────────────────────────────────────────

interface Call {
  url: string;
  method: string;
  body: unknown;
}
interface ApiStub {
  validate?: { status: number; body: unknown };
  put?: { status: number; body: unknown };
}

function stubApi(s: ApiStub = {}): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      let body: unknown;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body);
        } catch {
          body = init.body;
        }
      }
      calls.push({ url, method, body });
      const reply = (status: number, json: unknown) => ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => json,
        text: async () => JSON.stringify(json),
      });
      if (url.startsWith("/api/settings")) return reply(200, meta());
      if (url.startsWith("/api/config/validate") && method === "POST") {
        const v = s.validate ?? { status: 200, body: VALIDATE_OK };
        return reply(v.status, v.body);
      }
      if (url.startsWith("/api/config") && method === "PUT") {
        const p = s.put ?? { status: 200, body: PUT_OK };
        return reply(p.status, p.body);
      }
      if (url.startsWith("/api/config")) {
        return reply(200, url.includes("scope=repo") ? REPO_CFG : GLOBAL_CFG);
      }
      return reply(404, { error: "unrouted " + url });
    }),
  );
  return calls;
}

// ═══ fix 2: repo scope global-only sections ═════════════════════════════

describe("repo scope: global-only sections (T34b fix 2)", () => {
  it("providers_in_repo_scope_show_the_global_values_read_only", async () => {
    stubApi();
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    // Switch to repo scope, open Providers.
    click(scopeButton("This repo"));
    await flush();
    click(q(tid("nav-providers")));
    await flush();

    // The GLOBAL draft's provider is shown (the repo file is empty —
    // before the fix the pane rendered the repo file's empty values).
    expect(q(tid("provider-name-0")).textContent).toContain("globalprov");
    expect(q(tid("model-row-0-0")).textContent).toContain("qwen2.5-coder:14b");

    // Every control is disabled (read-only).
    expect(q<HTMLSelectElement>(tid("provider-type-0")).disabled).toBe(true);
    expect(q<HTMLButtonElement>(tid("provider-add")).disabled).toBe(true);

    // The lock banner is the spec §9.5 copy with a [Switch to Global] button.
    const banner = q(tid("provider-readonly"));
    expect(banner.textContent).toContain(
      "Machine-wide — edit in Global scope. This section is read-only here.",
    );
    expect(q(tid("switch-to-global")).textContent).toBe("Switch to Global");
  });

  it("general_in_repo_scope_shows_the_global_bind_and_the_button_switches_back", async () => {
    stubApi();
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    click(scopeButton("This repo"));
    await flush();
    click(q(tid("nav-general")));
    await flush();

    // The GLOBAL general.web.bind is rendered (repo file has no general:).
    expect(q<HTMLInputElement>(tid("general-bind") + " input").value).toBe("127.0.0.1:7823");
    expect(q(tid("general-readonly")).textContent).toContain("read-only here");
    expect(q<HTMLInputElement>(tid("general-bind") + " input").disabled).toBe(true);

    // [Switch to Global] returns to the global scope.
    click(q(tid("switch-to-global")));
    await flush();
    expect(scopeButton("Global").className).toContain("bg-zinc-100");
    expect(has("general-readonly")).toBe(false);
  });

  it("mcps_in_repo_scope_show_the_global_servers_read_only", async () => {
    stubApi();
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    click(scopeButton("This repo"));
    await flush();
    click(q(tid("nav-mcps")));
    await flush();

    expect(q(tid("mcp-card-0")).textContent).toContain("globalmcp");
    expect(q<HTMLSelectElement>(tid("mcp-type-0")).disabled).toBe(true);
    expect(q<HTMLButtonElement>(tid("mcp-add")).disabled).toBe(true);
    expect(has("switch-to-global")).toBe(true);
  });
});

// ═══ fix 3: error panel title before / after a save ═════════════════════

describe("error panel title (T34b fix 3)", () => {
  it("live_validation_is_neutral_and_a_failed_save_says_save_failed", async () => {
    stubApi({ validate: { status: 422, body: VALIDATE_422 }, put: { status: 422, body: VALIDATE_422 } });
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    // Make the global draft dirty (General pane, cost_tracking toggle).
    click(q(tid("nav-general")));
    await flush();
    toggle(q<HTMLInputElement>(tid("general-cost-tracking") + " input"));
    expect(has("savebar")).toBe(true);

    // The SaveBar's debounced validate lands a 422 BEFORE any Save attempt.
    await wait(600);
    expect(has("error-panel")).toBe(true);
    const title = q(tid("error-panel-title"));
    expect(title.textContent).toBe("⚠ 2 problems");
    expect(title.textContent).not.toContain("Save failed");

    // Now a failed Save (422 PUT) — the title keeps "Save failed".
    click(q(tid("savebar-save")));
    await flush();
    expect(q(tid("error-panel-title")).textContent).toBe("✕ Save failed — 2 problems");
  });
});

// ═══ fix 5: Fix scrolls the target into view ════════════════════════════

describe("Fix scrolls the target field into view (T34b fix 5)", () => {
  it("fix_click_calls_scrollIntoView_on_the_flashed_element", async () => {
    const scrollIntoView = vi.fn();
    Element.prototype.scrollIntoView = scrollIntoView as unknown as Element["scrollIntoView"];
    stubApi({ validate: { status: 422, body: VALIDATE_422 } });
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    click(q(tid("nav-general")));
    await flush();
    toggle(q<HTMLInputElement>(tid("general-cost-tracking") + " input"));
    await wait(600); // debounced validate → 422 → error panel

    click(q(tid("error-fix")));
    await flush(); // the scroll is scheduled after the re-render (setTimeout 0)

    expect(scrollIntoView).toHaveBeenCalled();
    // The scrolled element is the flashed listed error (data-flash) or a
    // red field — never null (the jsdom guard is the optional call).
    const flashed = document.querySelector('[data-flash="true"]');
    const red = document.querySelector(".border-red-600");
    expect(flashed !== null || red !== null).toBe(true);
  });
});
