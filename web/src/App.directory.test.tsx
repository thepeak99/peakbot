// App-level tests for the Desktop directory tab (design §4.3).
//
// Hiding `directory` in ui.tabs also hides the desktop cwd picker.
// Desktop keeps its existing cwd picker in the TopBar; it doesn't get a
// Directory tab in the drawer.
//
// RED before implementation: App has no browsableCwd derivation and the
// TopBar/BottomBar still use their own edition/isRemote predicates.

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState, ConversationSummary, WireMachineStatus, WireWelcome } from "./state";

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
let desktopWidth = false;

const isDesktopQuery = (q: string): boolean => q.includes("min-width: 1024px");

function installMatchMedia(): void {
  mqls = [];
  vi.stubGlobal("matchMedia", (query: string) => {
    const mql: FakeMql = {
      query,
      matches: isDesktopQuery(query) ? desktopWidth : false,
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

// ─── fake agent + state fixtures ─────────────────────────────────────────

interface FakeAgent {
  connected: boolean;
  state: AppState | null;
  models: unknown[];
  activeAlias: string;
  conversations: ConversationSummary[];
  conversationsError: string | null;
  machines: WireMachineStatus[];
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn>;
}

const WELCOME: WireWelcome = {
  provider_name: "anthropic",
  model: "claude",
  max_tokens: 100000,
  builtin_tools_count: 10,
  mcp_tools_count: 1,
  skills_count: 2,
  searxng_enabled: false,
  cost_tracking_enabled: true,
  compaction_enabled: true,
  compaction_threshold: 0.8,
  compaction_keep_recent: 3,
  conversation_persistence_enabled: true,
  cwd: "/home/exe",
};

function stateFor(id: string | null, over: Partial<AppState> = {}): AppState {
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
    ...over,
  } as unknown as AppState;
}

function makeAgent(state: AppState | null, over: Partial<FakeAgent> = {}): FakeAgent {
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
    ...over,
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
  desktopWidth = true;
  setShell("browser");
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

// ═══ Desktop directory (design §4.3) ════════════════════════════════════

describe("App — desktop directory (design §4.3)", () => {
  it("≥1024 browser shell: TopBar cwd picker present by default", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("cwd-picker"), "cwd picker renders by default").not.toBeNull();
  });

  it("visible_tabs lacking directory → TopBar cwd picker absent", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
        visible_tabs: ["session", "todo", "files", "tasks", "bash", "agents"],
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("cwd-picker"), "cwd picker hidden when directory tab is hidden").toBeNull();
  });

  it("no directory drawer handle in the TabbedDrawer", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    const handles = host.querySelectorAll('aside [role="tab"]');
    const directoryHandle = Array.from(handles).find((t) =>
      (t.textContent ?? "").includes("Directory"),
    );
    expect(directoryHandle, "no Directory handle in the desktop drawer").toBeUndefined();
  });
});
