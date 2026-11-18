// App integration: the single layout decision (design §2/§5).
//
// After this change, `useLayout()` is the ONLY layout decision; it combines
// `shell()` and the PHONE_BROWSER_QUERY media query and mirrors the result
// onto `document.documentElement.dataset.layout`. App derives its desktop/
// phone flag from that. The `(min-width: 1024px)` media query is retired.
//
// This file tests that:
// - Browser with phone viewport false → desktop layout (rail, no panels button)
// - Browser with phone viewport true → phone layout (panels button, no rail)
// - Desktop shell always yields desktop layout
// - Mobile shell always yields phone layout
// - Layout class contracts are met (Transcript section desktop:mr-12, composer
//   root desktop:pr-12, hint row desktop:flex, TopBar right cluster shrink-0)
//
// RED before implementation: App does not call useLayout() yet and does not
// apply the data-layout attribute, so these fail on missing behavior.

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState } from "./state";
import { setBootEdition } from "./desktop";

// ─── mocks ───────────────────────────────────────────────────────────────

const hoisted = vi.hoisted(() => ({ agent: null as unknown }));

vi.mock("./useAgent", () => ({
  useAgent: () => hoisted.agent,
  setConvoInUrl: vi.fn(),
}));
vi.mock("./uploads", () => ({ uploadFile: vi.fn() }));

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── controllable matchMedia ─────────────────────────────────────────────

interface FakeMql {
  query: string;
  matches: boolean;
  listeners: Set<(e: MediaQueryListEvent) => void>;
}
let mqls: FakeMql[] = [];
let phoneViewport = false;

function installMatchMedia(): void {
  mqls = [];
  vi.stubGlobal("matchMedia", (query: string) => {
    const mql: FakeMql = {
      query,
      matches: query.includes("max-width: 1023px") ? phoneViewport : false,
      listeners: new Set(),
    };
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
  phoneViewport = on;
  await act(async () => {
    for (const m of mqls) {
      if (!m.query.includes("max-width: 1023px")) continue;
      m.matches = on;
      for (const l of m.listeners) l({ matches: on, media: m.query } as MediaQueryListEvent);
    }
  });
}

// ─── fake agent + state fixtures ─────────────────────────────────────────

const T = "2026-01-01T00:00:00Z";

function stateFor(id: string | null): AppState {
  return {
    chat: { messages: [], auto_scroll: true, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: {
      total_input_tokens: 0,
      total_output_tokens: 0,
      total_api_calls: 0,
      total_cost: 0,
      model: "m",
      provider_name: "p",
      model_alias: "a",
    },
    context: {
      current_usage: 0,
      window_size: 1000,
      compaction_enabled: true,
      compaction_threshold: 0.8,
    },
    conversation: id === null ? null : { id },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
  } as unknown as AppState;
}

function makeAgent(state: AppState | null): Record<string, unknown> {
  return {
    connected: true,
    state,
    models: [],
    activeAlias: "",
    conversations: [],
    conversationsError: null,
    machines: [],
    commands: [],
    dirListing: null,
    recentDirs: [],
    error: null,
    send: vi.fn(),
    convo: state?.conversation?.id ?? null,
    switchConvo: vi.fn(),
  };
}

// ─── mount plumbing ──────────────────────────────────────────────────────

let root: Root | null = null;
let host: HTMLElement;

function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<App />));
}

function unmount() {
  act(() => {
    root?.unmount();
  });
  root = null;
  host?.remove();
}

async function flush() {
  for (let i = 0; i < 4; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

function setShell(marker: "desktop" | "mobile" | "browser") {
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  if (marker !== "browser") (globalThis as { peakbotShell?: unknown }).peakbotShell = marker;
}

const tid = (id: string): HTMLElement | null =>
  document.body.querySelector<HTMLElement>(`[data-testid="${id}"]`);

beforeEach(() => {
  phoneViewport = false;
  setBootEdition(undefined);
  setShell("mobile");
  installMatchMedia();
  vi.stubGlobal("fetch", vi.fn(async () => {
    throw new Error("no server in tests");
  }));
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  window.localStorage.clear();
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  unmount();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  setShell("browser");
  window.localStorage.clear();
  window.history.replaceState(null, "", "/");
});

// ═══ N3a: browser, phone query false → desktop layout ════════════════════

describe("App layout — N3a: browser, phone viewport false → desktop", () => {
  it("sets data-layout=desktop, shows rail, no panels button", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    expect(document.documentElement.dataset.layout).toBe("desktop");

    // Drawer rail present
    const rail = host.querySelector('aside[aria-label="Side panels"] [role="tablist"]');
    expect(rail, "desktop layout shows the drawer rail").not.toBeNull();

    // No panels button (phone-only)
    expect(tid("panels-button"), "desktop layout has no panels button").toBeNull();

    // Bottom bar footer renders
    expect(tid("bottom-bar")).not.toBeNull();
  });
});

// ═══ N3b: browser, phone query true → phone layout ═══════════════════════

describe("App layout — N3b: browser, phone viewport true → phone", () => {
  it("sets data-layout=phone, shows panels button, no rail", async () => {
    setShell("browser");
    phoneViewport = true;
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    expect(document.documentElement.dataset.layout).toBe("phone");

    // No drawer rail
    expect(host.querySelector('aside[aria-label="Side panels"]'), "phone layout has no drawer").toBeNull();

    // Panels button present
    expect(tid("panels-button"), "phone layout shows panels button").not.toBeNull();
  });
});

// ═══ N3c: desktop shell always desktop ═══════════════════════════════════

describe("App layout — N3c: desktop shell always desktop", () => {
  it("phone viewport true still yields desktop layout", async () => {
    setShell("desktop");
    phoneViewport = true;
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    expect(document.documentElement.dataset.layout).toBe("desktop");

    const rail = host.querySelector('aside[aria-label="Side panels"] [role="tablist"]');
    expect(rail, "desktop shell shows rail even at narrow width").not.toBeNull();
    expect(tid("panels-button")).toBeNull();
  });
});

// ═══ N3d: mobile shell always phone ══════════════════════════════════════

describe("App layout — N3d: mobile shell always phone", () => {
  it("phone viewport false still yields phone layout, no rail", async () => {
    setShell("mobile");
    phoneViewport = false;
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    expect(document.documentElement.dataset.layout).toBe("phone");

    expect(host.querySelector('aside[aria-label="Side panels"]'), "mobile shell has no drawer").toBeNull();
    expect(tid("panels-button")).not.toBeNull();
  });
});

// ═══ N3e: layout class contracts ═════════════════════════════════════════

describe("App layout — N3e: desktop layout class contracts", () => {
  it("Transcript section carries desktop:mr-12", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    const section = host.querySelector("section.overflow-y-auto");
    expect(section, "transcript section renders").not.toBeNull();
    expect(section!.classList.contains("desktop:mr-12")).toBe(true);
  });

  it("composer root carries desktop:pr-12", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    const composer = host.querySelector("textarea")!.closest(".border-t");
    expect(composer, "composer root renders").not.toBeNull();
    expect(composer!.classList.contains("desktop:pr-12")).toBe(true);
  });

  it("composer hint row uses desktop:flex", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    // The hint row is the row under the textarea containing command hints
    const hintRow = [...host.querySelectorAll("div")].find(
      (d) => d.className.includes("hidden") && d.className.includes("desktop:flex"),
    );
    expect(hintRow, "hint row with desktop:flex renders").not.toBeNull();
  });

  it("TopBar right cluster has shrink-0", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    const header = host.querySelector("header");
    expect(header, "header renders").not.toBeNull();

    // Right cluster: the ml-auto flex group in TopBar
    const rightCluster = [...header!.querySelectorAll("div")].find(
      (d) => d.className.includes("ml-auto") && d.className.includes("flex"),
    );
    expect(rightCluster, "TopBar right cluster renders").not.toBeNull();
    expect(rightCluster!.classList.contains("shrink-0")).toBe(true);
  });

  it("no bare lg:pr-12 or lg:mr-12 on chat screen elements (replaced by desktop:)", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    // The 48px rail reservation must be desktop:-only, not lg:-only
    const bareLg = host.querySelectorAll(".lg:pr-12, .lg:mr-12");
    expect(
      [...bareLg].map((e) => e.className),
      "no bare lg:pr-12 or lg:mr-12 (replaced by desktop: variants)",
    ).toEqual([]);
  });
});
