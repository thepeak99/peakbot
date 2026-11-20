// App-level wiring for the phone's Panels sheet, the inline status line, the
// header selection chip and the machine selector's back layers (mobile
// redesign design §2.5–§2.6, §4, §6: A1–A4, BK1–BK6 plus the inline-status
// and viewport wiring they imply).
//
// One open-panel state (`drawerTab`) drives either the desktop drawer
// (≥1024px) or the phone Panels sheet (<1024px); `useMediaQuery` picks.
// matchMedia here is CONTROLLABLE: `setDesktop(true)` flips every
// `(min-width: 1024px)` query and fires its change listeners, so the
// breakpoint can be crossed mid-test.
//
// Harness: ./App.mobileChats.test.tsx conventions (mocked ./useAgent, raw
// DOM, history spies, waitForPopstate). The fake agent is mutable: a test
// edits it and re-renders, like the real hook would on a state frame.
//
// RED before implementation: App has no Panels sheet / panels button /
// inline status line / header chip / machine pill yet, so these fail on the
// missing elements (feature absent).

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState, ConversationSummary, DirListing, WireChatMessage, WireMachineStatus, WireWelcome } from "./state";
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

/** Flip every `(min-width: 1024px)` query and notify its listeners. */
async function setDesktop(on: boolean): Promise<void> {
  desktopWidth = on;
  await act(async () => {
    for (const m of mqls) {
      if (!isDesktopQuery(m.query)) continue;
      m.matches = on;
      for (const l of m.listeners) l({ matches: on, media: m.query } as MediaQueryListEvent);
    }
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
  dirListing: null | DirListing;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn>;
}

const T = "2026-01-01T00:00:00Z";
const todoCall = (args: Record<string, unknown>): WireChatMessage => ({
  role: "toolcall",
  content: "",
  timestamp: T,
  tool_name: "todo",
  tool_args: JSON.stringify(args),
});

/** 3 todos, one completed ⇒ open 2, total 3. */
const TODO_MSGS: WireChatMessage[] = [
  todoCall({ action: "add", tasks: ["a", "b", "c"] }),
  todoCall({ action: "update", task_id: 3, status: "completed" }),
];

/** 2 running + 1 exited background processes ⇒ 2 running tasks. */
const BG = {
  running_count: 2,
  recent_summaries: [
    { id: 1, command: "sleep 100", status: "running" },
    { id: 2, command: "tail -f log", status: "running" },
    { id: 3, command: "ls", status: "exited", exit_code: 0 },
  ],
};

const WIRE_CATALOG_LOCAL = [
  {
    name: "dev",
    origin: "global",
    overrides_global: false,
    agents: [{ name: "team", model: "sonnet", subagents: [] }],
    color: "teal",
    machine: null,
  },
];
const HS = "https://home-server.local:7823";
const WIRE_CATALOG_REMOTE = [
  ...WIRE_CATALOG_LOCAL,
  {
    name: "home",
    origin: "remote",
    overrides_global: false,
    agents: [],
    color: "purple",
    machine: HS,
  },
];

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

const agentNow = (): FakeAgent => hoisted.agent as FakeAgent;

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

async function rerender(): Promise<void> {
  await act(async () => {
    root!.render(<App />);
  });
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

function click(el: Element): void {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

const tid = (id: string): HTMLElement | null =>
  document.body.querySelector<HTMLElement>(`[data-testid="${id}"]`);
const must = (id: string): HTMLElement => {
  const el = tid(id);
  expect(el, `${id} must render`).not.toBeNull();
  return el!;
};
const panelsDialog = (): HTMLElement | null =>
  document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="Panels"]');
const dialogNamed = (name: string): HTMLElement | null =>
  document.body.querySelector<HTMLElement>(`[role="dialog"][aria-label="${name}"]`);
const pillTabs = (): (string | null)[] =>
  [...document.body.querySelectorAll('[data-testid="panels-pill"]')].map((p) => p.getAttribute("data-tab"));

function waitForPopstate(): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(resolve, 250);
    window.addEventListener(
      "popstate",
      () => {
        clearTimeout(t);
        resolve();
      },
      { once: true },
    );
  });
}

/** A genuine OS Back: traverse history and let the popstate land. */
async function osBack(): Promise<void> {
  const popped = waitForPopstate();
  window.history.back();
  await act(async () => {
    await popped;
  });
  await flush();
}

function settlePopstates(count: number): Promise<void> {
  return new Promise<void>((resolve) => {
    if (count === 0) {
      resolve();
      return;
    }
    let landed = 0;
    const onPop = () => {
      landed += 1;
      if (landed < count) return;
      window.removeEventListener("popstate", onPop);
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(onPop, 1000);
    window.addEventListener("popstate", onPop);
  });
}

/** Header ‹ Chats → the list (a real Back under the hood). */
async function toList(): Promise<void> {
  const back = must("chat-back");
  const popped = waitForPopstate();
  click(back);
  await act(async () => {
    await popped;
  });
  await flush();
}

const chatShown = (): boolean => tid("chat-back") !== null && tid("chat-list") === null;

beforeEach(() => {
  desktopWidth = false;
  setBootEdition(undefined); // module-level edition signal: never leak "local" across tests
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
  window.localStorage.setItem("peakbot-focus", "off"); // phone line reads "working…"
  window.history.replaceState(null, "", "/");
});

afterEach(async () => {
  // jsdom folds two same-tick history.back() calls into ONE popstate, so
  // unmounting with the chat layer AND a sheet (Panels, Selection or
  // Machine) open would leave backStack owing a popstate that never lands
  // (and poison the next test). Close the sheet first, as a user would.
  if (tid("sheet-done")) {
    const closed = waitForPopstate();
    click(must("sheet-done"));
    await act(async () => {
      await closed;
    });
  }
  const backSpy = vi.spyOn(window.history, "back");
  unmount();
  const inFlight = backSpy.mock.calls.length;
  backSpy.mockRestore();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  await settlePopstates(inFlight);
  setShell("browser");
  window.localStorage.clear();
  window.history.replaceState(null, "", "/");
});

const withTabs = (over: Partial<AppState> = {}): AppState =>
  stateFor("c1", {
    chat: { messages: TODO_MSGS, auto_scroll: true, scroll_offset: 0 },
    bg: BG,
    ...over,
  } as Partial<AppState>);

// ═══ A1 — full width, no rail below 1024px (C1) ═════════════════════════

describe("App mobile sheet — A1: full width on a phone", () => {
  it("no drawer rail, and no bare mr-12 / pr-12 anywhere on the chat screen", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    expect(host.querySelector('aside[aria-label="Side panels"]'), "no drawer at <1024").toBeNull();
    expect(host.querySelector('[role="tablist"]'), "no rail tablist at <1024").toBeNull();
    expect(
      [...host.querySelectorAll(".mr-12, .pr-12")].map((e) => e.className),
      "the 48px rail reservation is lg-only",
    ).toEqual([]);
  });

  it("the empty transcript and the composer carry the lg-only reservation", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    const composer = host.querySelector("textarea")!.closest(".border-t") as HTMLElement;
    expect(composer.classList.contains("lg:pr-12")).toBe(true);
    const empty = host.querySelector("section.overflow-y-auto") as HTMLElement;
    expect(empty.classList.contains("lg:mr-12")).toBe(true);
  });

  it("the transcript pane and TranscriptNav use the full width (nav: right-4 lg:right-14)", async () => {
    // Give the virtualizer a viewport so a populated Transcript (and its nav) render.
    const desc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 400 });
    try {
      hoisted.agent = makeAgent(
        stateFor("c1", {
          chat: {
            messages: [{ role: "user", content: "hi", timestamp: T }, { role: "agent", content: "yo", timestamp: T }],
            auto_scroll: true,
            scroll_offset: 0,
          },
        } as Partial<AppState>),
      );
      mount();
      await flush();
      const pane = host.querySelector("[data-index]")!.closest("section") as HTMLElement;
      expect(pane.classList.contains("lg:mr-12")).toBe(true);
      expect(pane.classList.contains("mr-12")).toBe(false);
      const nav = [...host.querySelectorAll("div")].find((d) =>
        d.className.includes("pointer-events-none absolute bottom-4"),
      )!;
      expect(nav, "TranscriptNav renders").toBeDefined();
      expect(nav.classList.contains("right-4")).toBe(true);
      expect(nav.classList.contains("lg:right-14")).toBe(true);
      expect(nav.classList.contains("right-14"), "no bare right-14").toBe(false);
      expect([...host.querySelectorAll(".mr-12, .pr-12")]).toEqual([]);
    } finally {
      if (desc) Object.defineProperty(HTMLElement.prototype, "offsetHeight", desc);
    }
  });
});

// ═══ A2 — the panels button opens the sheet at medium (C2) ══════════════

describe("App mobile sheet — A2: the panels button", () => {
  it("opens a Panels dialog at the medium detent with a backdrop", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(panelsDialog(), "closed at first").toBeNull();

    click(must("panels-button"));
    await flush();

    const dlg = panelsDialog();
    expect(dlg, "the Panels dialog opens").not.toBeNull();
    expect(dlg!.getAttribute("data-detent")).toBe("medium");
    const h = parseFloat(dlg!.style.height);
    expect(Math.abs(h - window.innerHeight * 0.55), "≈55% of the viewport (±25px)").toBeLessThanOrEqual(25);
    expect(tid("sheet-backdrop"), "a visible backdrop").not.toBeNull();
    expect(chatShown(), "the chat stays underneath").toBe(true);
  });

  it("the button opens the FIRST visible tab; its pill is selected", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    const first = document.body.querySelector('[data-testid="panels-pill"]')!;
    expect(first.getAttribute("data-tab")).toBe("session");
    expect(first.getAttribute("aria-selected")).toBe("true");
  });

  it("a pill tap shows that tab's existing panel content (Tasks → the background list)", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    click(document.body.querySelector('[data-testid="panels-pill"][data-tab="tasks"]')!);
    await flush();
    const panel = panelsDialog()!.querySelector('[role="tabpanel"]')!;
    expect(panel.textContent, "BgPanel content").toContain("sleep 100");
  });
});

// ═══ A3 — the combined badge, hidden at 0, no button with 0 tabs ════════

describe("App mobile sheet — A3: the header badge", () => {
  it("is running background tasks + open todos (2 + 2 = 4)", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(must("panels-badge").textContent).toBe("4");
  });

  it("is absent at 0 (the button stays)", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    expect(tid("panels-button")).not.toBeNull();
    expect(tid("panels-badge")).toBeNull();
  });

  it("counts only the visible tabs (todo visible, tasks hidden ⇒ 2)", async () => {
    hoisted.agent = makeAgent(withTabs({ visible_tabs: ["session", "todo"] } as Partial<AppState>));
    mount();
    await flush();
    expect(must("panels-badge").textContent).toBe("2");
    click(must("panels-button"));
    await flush();
    expect(pillTabs(), "the pills are exactly the visible tabs, in canonical order").toEqual(["session", "todo"]);
  });

  it("with 0 visible tabs there is no panels button and no sheet", async () => {
    hoisted.agent = makeAgent(withTabs({ visible_tabs: [] } as Partial<AppState>));
    mount();
    await flush();
    expect(tid("panels-button"), "no button with nothing to open").toBeNull();
    expect(panelsDialog()).toBeNull();
  });

  it("the Todo pill badge is the OPEN count on a phone (2), not the row total", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    const todoPill = document.body.querySelector('[data-testid="panels-pill"][data-tab="todo"]')!;
    expect(todoPill.querySelector('[data-testid="panels-pill-badge"]')!.textContent).toBe("2");
  });
});

// ═══ A4 — ≥1024px keeps the drawer, no sheet (C8, C26) ══════════════════

describe("App mobile sheet — A4: desktop width", () => {
  it("the rail is present, no panels button, no sheet", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    const rail = host.querySelector('aside[aria-label="Side panels"] [role="tablist"]');
    expect(rail, "the drawer rail renders at ≥1024px").not.toBeNull();
    expect(tid("panels-button"), "no panels button at ≥1024px").toBeNull();
    expect(panelsDialog()).toBeNull();
  });

  it("the drawer handle opens the drawer body (the existing desktop path)", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    const handle = [...host.querySelectorAll<HTMLElement>('aside [role="tab"]')].find((t) =>
      (t.textContent ?? "").includes("Tasks"),
    )!;
    click(handle);
    await flush();
    expect(panelsDialog(), "still no sheet").toBeNull();
    expect(host.querySelector('aside [role="tabpanel"]')!.textContent).toContain("sleep 100");
  });

  it("the mobile shell at ≥1024px also gets the rail and no panels button", async () => {
    desktopWidth = true;
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(host.querySelector('aside[aria-label="Side panels"]')).not.toBeNull();
    expect(tid("panels-button")).toBeNull();
  });

  it("a phone browser (browser shell, <1024px) gets the TopBar panels button and the sheet", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    const btn = must("panels-button");
    expect(host.querySelector("header")!.contains(btn), "the button lives in the TopBar").toBe(true);
    click(btn);
    await flush();
    expect(panelsDialog(), "the browser path opens the same sheet").not.toBeNull();
  });
});

// ═══ A5 — crossing the breakpoint while open (§4) ═══════════════════════

describe("App mobile sheet — A5: the viewport crosses 1024px with a panel open", () => {
  it("sheet → drawer on the SAME tab, releasing the back marker", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    click(document.body.querySelector('[data-testid="panels-pill"][data-tab="tasks"]')!);
    await flush();
    const backSpy = vi.spyOn(window.history, "back");

    await setDesktop(true);
    await flush();

    expect(panelsDialog(), "the sheet unmounts").toBeNull();
    expect(host.querySelector('aside [role="tabpanel"]')!.textContent, "the drawer shows that tab").toContain(
      "sleep 100",
    );
    expect(backSpy, "the sheet's marker is consumed once").toHaveBeenCalledTimes(1);
  });

  it("drawer → sheet on the same tab", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    const handle = [...host.querySelectorAll<HTMLElement>('aside [role="tab"]')].find((t) =>
      (t.textContent ?? "").includes("Tasks"),
    )!;
    click(handle);
    await flush();

    await setDesktop(false);
    await flush();

    expect(host.querySelector('aside[aria-label="Side panels"]'), "the drawer is gone").toBeNull();
    const active = document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!;
    expect(active.getAttribute("data-tab")).toBe("tasks");
  });
});

// ═══ A6 — the working pill and Stop (C6) ════════════════════════════════

describe("App mobile sheet — A6: the floating working pill", () => {
  it("shows while a turn runs; Stop sends {type:'stop'} once", async () => {
    hoisted.agent = makeAgent(withTabs({ is_running: true } as Partial<AppState>));
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(must("panels-working").textContent).toContain("working…");

    agentNow().send.mockClear();
    click(must("panels-stop"));
    const stops = agentNow().send.mock.calls.filter(([m]) => (m as { type: string }).type === "stop");
    expect(stops).toHaveLength(1);
  });

  it("disappears when the turn ends (the sheet stays)", async () => {
    hoisted.agent = makeAgent(withTabs({ is_running: true } as Partial<AppState>));
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(tid("panels-working")).not.toBeNull();

    agentNow().state = withTabs({ is_running: false } as Partial<AppState>);
    await rerender();

    expect(tid("panels-working"), "the pill follows is_running").toBeNull();
    expect(panelsDialog(), "the sheet stays open").not.toBeNull();
  });

  it("is absent when no turn runs", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(tid("panels-working")).toBeNull();
  });
});

// ═══ AS — the inline status line (C9–C12) ═══════════════════════════════

describe("App mobile sheet — AS: the inline status line", () => {
  it("running: one line with 'working…', ⏳ tasks and ☑ todos (C9)", async () => {
    hoisted.agent = makeAgent(withTabs({ is_running: true } as Partial<AppState>));
    mount();
    await flush();
    const row = must("activity-row");
    expect(row.getAttribute("data-variant")).toBe("line");
    expect(must("activity-label").textContent).toContain("working…");
    expect(must("activity-tasks").textContent).toBe("⏳ 2");
    expect(must("activity-todos").textContent).toBe("☑ 1/3");
  });

  it("idle with tasks or todos: counts only, no 'working…' (C10)", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(tid("activity-row")).not.toBeNull();
    expect(tid("activity-label"), "no working label while idle").toBeNull();
    expect(tid("activity-tasks")).not.toBeNull();
  });

  it("idle with nothing: the line is absent (C10)", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    expect(tid("activity-row")).toBeNull();
  });

  it("running with nothing else: just 'working…'", async () => {
    hoisted.agent = makeAgent(stateFor("c1", { is_running: true } as Partial<AppState>));
    mount();
    await flush();
    expect(must("activity-label").textContent).toContain("working…");
    expect(tid("activity-tasks")).toBeNull();
    expect(tid("activity-todos")).toBeNull();
  });

  it("tapping tasks opens the sheet on Tasks; todos on Todo (C11)", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("activity-tasks"));
    await flush();
    expect(panelsDialog()).not.toBeNull();
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
    ).toBe("tasks");
  });

  it("tapping todos opens the sheet on Todo", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("activity-todos"));
    await flush();
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
    ).toBe("todo");
  });

  it("a hidden tab's count is not shown (tasks hidden ⇒ no ⏳ segment)", async () => {
    hoisted.agent = makeAgent(withTabs({ visible_tabs: ["session", "todo"] } as Partial<AppState>));
    mount();
    await flush();
    expect(tid("activity-tasks"), "no button that opens a hidden tab").toBeNull();
    expect(tid("activity-todos")).not.toBeNull();
  });

  it("focus mode on, turn in flight: the step label stays and the counts are appended", async () => {
    window.localStorage.setItem("peakbot-focus", "on");
    hoisted.agent = makeAgent(withTabs({ is_running: true } as Partial<AppState>));
    mount();
    await flush();
    expect(must("activity-label").textContent).toContain("Thinking");
    expect(tid("activity-tasks")).not.toBeNull();
  });

  it("C12: at ≥1024px the focus-mode row is the boxed desktop row with no counts", async () => {
    desktopWidth = true;
    setShell("browser");
    window.localStorage.setItem("peakbot-focus", "on");
    hoisted.agent = makeAgent(withTabs({ is_running: true } as Partial<AppState>));
    mount();
    await flush();
    const row = must("activity-row");
    expect(row.getAttribute("data-variant")).toBe("box");
    expect(tid("activity-tasks"), "no counts on desktop").toBeNull();
    expect(tid("activity-todos")).toBeNull();
  });

  it("C12: at ≥1024px, idle or non-focus, there is no activity row", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(tid("activity-row")).toBeNull();
  });
});

// ═══ H — the header subtitle is the selection chip (C13–C15, C23) ═══════

describe("App mobile sheet — H: the header selection chip", () => {
  const sel = (profile: string) => ({ profile, agent: "team" });

  it("the chip lives in the header subtitle, and the bottom bar is gone (C14)", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_LOCAL, selection: sel("dev") } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    const chip = must("selection-chip");
    expect(must("chat-subtitle").contains(chip)).toBe(true);
    expect(chip.textContent).toContain("dev");
    expect(tid("bottom-bar"), "no bottom bar in the shell for a local chat").toBeNull();
  });

  it("tapping the chip opens the Selection sheet (C13)", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_LOCAL, selection: sel("dev") } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("selection-chip"));
    await flush();
    expect(dialogNamed("Selection")).not.toBeNull();
  });

  // Reverses C15/C23 (user decision after device testing): the subtitle was
  // "10.2.0.48 · [pill: default on 10.2.0.4…]" — the host twice. Only the pill
  // stays, as in the mock.
  it("a remote chat shows NO host prefix outside the pill — the subtitle is just the pill (C15, C23 reversed)", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_REMOTE, selection: sel("home") } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("chat-machine"), "no separate machine prefix element").toBeNull();
    const subtitle = must("chat-subtitle");
    const pill = must("selection-chip");
    expect(subtitle.contains(pill), "the pill lives in the subtitle").toBe(true);
    expect(subtitle.textContent ?? "", "no host text outside the pill").not.toContain("home-server.local");
    expect(
      subtitle.textContent,
      "every character of the subtitle text belongs to the pill",
    ).toBe(pill.textContent);
    expect(pill.textContent, "the pill still shows the remote profile name").toContain("home");
  });

  it("a remote chat whose profile name carries the host shows that host exactly once (in the pill)", async () => {
    const catalog = [
      ...WIRE_CATALOG_LOCAL,
      { ...WIRE_CATALOG_REMOTE[1], name: "default on home-server.local" },
    ];
    hoisted.agent = makeAgent(
      stateFor("c1", {
        catalog,
        selection: sel("default on home-server.local"),
      } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    const subtitle = must("chat-subtitle");
    const pill = must("selection-chip");
    expect(pill.textContent).toContain("default on home-server.local");
    expect(
      (subtitle.textContent ?? "").split("home-server.local").length - 1,
      "the host appears once in the subtitle, not twice",
    ).toBe(1);
    expect(tid("chat-machine")).toBeNull();
  });

  it("a local chat has no machine prefix and its subtitle is just the pill", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_REMOTE, selection: sel("dev") } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("chat-machine")).toBeNull();
    const subtitle = must("chat-subtitle");
    const pill = must("selection-chip");
    expect(subtitle.textContent, "no text outside the pill").toBe(pill.textContent);
    expect(pill.textContent).toContain("dev");
  });

  // Reverses the kept back-dot (batch 1 keep-R, B3): the user wants it gone.
  it("the ‹ Chats back button shows NO dot even when another chat is running or unread", async () => {
    const other: ConversationSummary = {
      id: "c2",
      name: "Other chat",
      updated_at: T,
      message_count: 3,
      model: "m",
      active: false,
      running: true,
    };
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_LOCAL, selection: sel("dev") } as unknown as Partial<AppState>),
      { conversations: [{ ...other, id: "c1", name: "This chat", running: false, active: true }, other] },
    );
    mount();
    await flush();
    const back = must("chat-back");
    expect(tid("chat-back-badge"), "no back-badge dot while another chat is running").toBeNull();
    expect(back.querySelector(".rounded-full"), "no dot of any kind on the back button").toBeNull();
  });

  it("the browser shell keeps the chip in the bottom bar (C14, C26)", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(
      stateFor("c1", { catalog: WIRE_CATALOG_LOCAL, selection: sel("dev") } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    const bar = must("bottom-bar");
    expect(bar.querySelector('[data-testid="selection-chip"]'), "where it is today").not.toBeNull();
  });
});

// ═══ BK — back-stack layers (C5, C22) ═══════════════════════════════════

describe("App mobile sheet — BK1: Back closes the panels, then leaves the chat", () => {
  it("chat → panels → Back closes only the panels → Back → the list", async () => {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(panelsDialog()).not.toBeNull();

    await osBack();
    expect(panelsDialog(), "the sheet closes").toBeNull();
    expect(chatShown(), "Back from an open sheet never leaves the chat").toBe(true);

    await osBack();
    expect(tid("chat-list"), "the second Back reaches the Chats list").not.toBeNull();
  });
});

describe("App mobile sheet — BK2: every UI close consumes exactly one marker", () => {
  async function openPanels(): Promise<void> {
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
  }

  it.each([
    ["Done", () => click(must("sheet-done"))],
    ["the backdrop", () => click(must("sheet-backdrop"))],
    [
      "Esc",
      () =>
        act(() => {
          document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
        }),
    ],
  ] as const)("%s closes the sheet with one back()", async (_label, close) => {
    await openPanels();
    const backSpy = vi.spyOn(window.history, "back");
    const popped = waitForPopstate();
    close();
    await act(async () => {
      await popped;
    });
    await flush();
    expect(panelsDialog(), "closed").toBeNull();
    expect(backSpy, "one marker consumed").toHaveBeenCalledTimes(1);
    expect(chatShown(), "the chat stays").toBe(true);
  });

  it("after a UI close, a Back goes to the list (no dangling marker)", async () => {
    await openPanels();
    const popped = waitForPopstate();
    click(must("sheet-done"));
    await act(async () => {
      await popped;
    });
    await flush();
    await osBack();
    expect(tid("chat-list"), "the next Back is the chat's own").not.toBeNull();
  });
});

describe("App mobile sheet — BK3: Back closes the selection sheet", () => {
  it("a popstate closes it and the chat stays", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        catalog: WIRE_CATALOG_LOCAL,
        selection: { profile: "dev", agent: "team" },
      } as unknown as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("selection-chip"));
    await flush();
    expect(dialogNamed("Selection")).not.toBeNull();

    await osBack();
    expect(dialogNamed("Selection"), "the sheet closes").toBeNull();
    expect(chatShown(), "the chat stays").toBe(true);
  });
});

describe("App mobile sheet — BK4/BK5: the machine selector's layers on the list", () => {
  const withMachines = (): FakeAgent =>
    makeAgent(
      stateFor("c1", {
        catalog: WIRE_CATALOG_REMOTE,
        selection: { profile: "dev", agent: "team" },
      } as unknown as Partial<AppState>),
      { machines: [{ id: HS, online: true }] },
    );

  it("BK4: Back closes the Machine sheet and the list stays", async () => {
    hoisted.agent = withMachines();
    mount();
    await flush();
    await toList();
    click(must("machine-pill"));
    await flush();
    expect(dialogNamed("Machine")).not.toBeNull();

    await osBack();
    expect(dialogNamed("Machine"), "the sheet closes").toBeNull();
    expect(tid("chat-list"), "the list stays").not.toBeNull();
  });

  it("BK5: All → picker → pick → chat; ONE Back returns to the list (C22)", async () => {
    hoisted.agent = withMachines();
    mount();
    await flush();
    await toList();

    click(document.body.querySelector('button[aria-label="New chat"]')!);
    await flush();
    expect(dialogNamed("New chat on…"), "All opens the picker").not.toBeNull();

    // The pick releases the picker's marker (a back()) and the chat pushes
    // its own; wait for that traversal to land before judging history.
    const settled = waitForPopstate();
    click(document.body.querySelector(`[data-testid="newchat-option"][data-machine="${HS}"]`)!);
    await act(async () => {
      await settled;
    });
    await flush();

    expect(agentNow().switchConvo).toHaveBeenCalledWith(null, "home");
    expect(dialogNamed("New chat on…"), "the picker is gone").toBeNull();
    expect(chatShown(), "the new chat is on screen").toBe(true);

    await osBack();
    expect(tid("chat-list"), "one Back returns to the list").not.toBeNull();
  });

  it("a selected machine's FAB skips the picker: switchConvo(null, firstProfile)", async () => {
    hoisted.agent = withMachines();
    mount();
    await flush();
    await toList();
    click(must("machine-pill"));
    await flush();
    click(document.body.querySelector(`[data-testid="machine-option"][data-machine="${HS}"]`)!);
    await flush();
    // Close the Machine sheet's marker before the FAB (Done = one back()).
    const popped = waitForPopstate();
    if (dialogNamed("Machine")) click(must("sheet-done"));
    await act(async () => {
      await popped;
    });
    await flush();

    const settled = waitForPopstate();
    click(document.body.querySelector('button[aria-label="New chat"]')!);
    await act(async () => {
      await settled;
    });
    await flush();
    expect(agentNow().switchConvo).toHaveBeenCalledWith(null, "home");
    expect(dialogNamed("New chat on…"), "no picker for a selected machine").toBeNull();
  });
});

describe("App mobile sheet — BK6: the active tab disappears under an open sheet", () => {
  it("relocates to the first still-visible tab, then closes when none is left", async () => {
    hoisted.agent = makeAgent(withTabs({ visible_tabs: ["todo", "bash"] } as Partial<AppState>));
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
      "opens on the first visible tab",
    ).toBe("todo");

    // The bound profile's tabs drop Todo out from under the open sheet.
    agentNow().state = withTabs({ visible_tabs: ["bash"] } as Partial<AppState>);
    await rerender();
    await flush();
    expect(pillTabs()).toEqual(["bash"]);
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
      "relocated to the first visible tab",
    ).toBe("bash");

    // Now nothing is visible: the sheet closes and consumes its marker.
    const backSpy = vi.spyOn(window.history, "back");
    agentNow().state = withTabs({ visible_tabs: [] } as Partial<AppState>);
    await rerender();
    await flush();
    expect(panelsDialog(), "the sheet closes").toBeNull();
    expect(tid("panels-button"), "no button either").toBeNull();
    expect(backSpy, "its marker is consumed once").toHaveBeenCalledTimes(1);
  });
});

// ═══ B31 — the watch banner (batch 3, fidelity plan 3.9) ═════════════════

describe("App mobile sheet — B31: the watch banner", () => {
  const subMsg: WireChatMessage = {
    role: "agent",
    content: "found the bug",
    timestamp: T,
    source: { kind: "sub_agent", role: "researcher" },
  };

  /** The banner's own "Clear" — the composer carries a second watch note
   *  with its own Clear, which must not satisfy these assertions. */
  const bannerClear = (): HTMLButtonElement | undefined =>
    [...host.querySelectorAll("button")].find(
      (b) =>
        b.textContent?.trim() === "Clear" &&
        !b.closest('[data-testid="composer"]') &&
        !b.closest("footer, .border-t") &&
        (b.closest("div")?.textContent ?? "").includes("Watching"),
    );

  /** Mount a chat with one researcher turn; assert the global view has no banner. */
  async function setup(): Promise<void> {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        chat: {
          messages: [
            { role: "user", content: "dig in", timestamp: T },
            subMsg,
          ],
          auto_scroll: true,
          scroll_offset: 0,
        },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(bannerClear(), "no banner in the global view").toBeUndefined();
  }

  /** Panels → Agents → watch the researcher's lane. */
  async function watchResearcher(): Promise<void> {
    click(must("panels-button"));
    await flush();
    click(document.body.querySelector('[data-testid="panels-pill"][data-tab="agents"]')!);
    await flush();
    click(document.body.querySelector('button[title^="Watch every turn researcher"]')!);
    await flush();
  }

  it("shows 'Watching ‹who›' and Clear returns to the global view", async () => {
    await setup();
    await watchResearcher();

    const clear = bannerClear();
    expect(clear, "the banner (outside the composer) renders with its Clear").toBeDefined();
    expect(clear!.closest("div")!.textContent).toContain("Watching researcher");

    click(clear!);
    await flush();
    expect(bannerClear(), "back to the global view, banner gone").toBeUndefined();
  });

  it("the 'N messages' count is hidden on the phone (max-lg:hidden) but still rendered", async () => {
    await setup();
    await watchResearcher();
    const count = [...bannerClear()!.closest("div")!.querySelectorAll("span")].find((s) =>
      /^\d+ messages?$/.test(s.textContent?.trim() ?? ""),
    );
    expect(count, "the count span exists").toBeDefined();
    expect(count!.textContent!.trim()).toBe("1 message");
    expect(count!.classList.contains("max-lg:hidden"), "hidden below lg only").toBe(true);
  });

  it("desktop lock: at ≥1024px the banner still reads 'Watching ‹who›' with its message count, and Clear works", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(
      stateFor("c1", {
        chat: {
          messages: [{ role: "user", content: "dig in", timestamp: T }, subMsg],
          auto_scroll: true,
          scroll_offset: 0,
        },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    const handle = [...host.querySelectorAll<HTMLElement>('aside [role="tab"]')].find((t) =>
      (t.textContent ?? "").includes("Agents"),
    )!;
    click(handle);
    await flush();
    click(host.querySelector('button[title^="Watch every turn researcher"]')!);
    await flush();

    const clear = bannerClear();
    expect(clear, "the banner renders with its Clear").toBeDefined();
    const banner = clear!.closest("div")!;
    expect(banner.textContent).toContain("Watching researcher");
    expect(banner.textContent, "the count stays on desktop").toContain("1 message");
    click(clear!);
    await flush();
    expect(bannerClear()).toBeUndefined();
  });
});

// ═══ X2 — Transcript gets `phone={!isDesktop}` (batch 3, 3.1) ═════════════

describe("App mobile sheet — X2: the transcript rows follow the breakpoint", () => {
  const withRows = (): FakeAgent =>
    makeAgent(
      stateFor("c1", {
        chat: {
          messages: [
            { role: "user", content: "hi", timestamp: T },
            { role: "agent", content: "yo", timestamp: T },
          ],
          auto_scroll: true,
          scroll_offset: 0,
        },
      } as Partial<AppState>),
    );

  async function withViewport(run: () => Promise<void>): Promise<void> {
    const desc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 400 });
    try {
      await run();
    } finally {
      if (desc) Object.defineProperty(HTMLElement.prototype, "offsetHeight", desc);
    }
  }

  it("on a phone the rows carry no 'You' / 'Shifu' labels", async () => {
    await withViewport(async () => {
      hoisted.agent = withRows();
      mount();
      await flush();
      const rows = [...host.querySelectorAll("[data-index]")].map((r) => r.textContent ?? "");
      expect(rows.length, "rows render").toBe(2);
      expect(rows.join("|")).toContain("hi");
      expect(rows.join("|")).toContain("yo");
      expect(rows.join("|"), "no role labels on the phone").not.toMatch(/You|Shifu/);
    });
  });

  it("at ≥1024px the rows keep today's labelled cards", async () => {
    await withViewport(async () => {
      desktopWidth = true;
      setShell("browser");
      hoisted.agent = withRows();
      mount();
      await flush();
      const rows = [...host.querySelectorAll("[data-index]")].map((r) => r.textContent ?? "");
      expect(rows.join("|")).toContain("You");
      expect(rows.join("|")).toContain("Shifu");
    });
  });
});

// ═══ D1 — option D: the desktop shell is always wide ════════════════════
//
// `window.peakbotShell = "desktop"` (the desktop app's init script,
// desktop/src-tauri/src/window.rs) must force the DESKTOP layout at every
// viewport width — the desktop window is sized ≥1024×600 (see the
// MIN_WIDTH/MIN_HEIGHT tests in window.rs) and never shrinks into the phone
// face. The phone layout stays the browser's (and the mobile shell's) at
// <1024px.
//
// RED before implementation: App derives the layout solely from
// useMediaQuery("(min-width: 1024px)"), so a narrow desktop shell renders
// the phone layout and the first four tests fail. `peakbotShell` is reset
// to "browser" by the file-wide afterEach.

describe("App desktop — option D: the desktop shell is always wide", () => {
  const withRows = (): FakeAgent =>
    makeAgent(
      stateFor("c1", {
        chat: {
          messages: [
            { role: "user", content: "hi", timestamp: T },
            { role: "agent", content: "yo", timestamp: T },
          ],
          auto_scroll: true,
          scroll_offset: 0,
        },
      } as Partial<AppState>),
    );

  async function withViewport(run: () => Promise<void>): Promise<void> {
    const desc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", { configurable: true, get: () => 400 });
    try {
      await run();
    } finally {
      if (desc) Object.defineProperty(HTMLElement.prototype, "offsetHeight", desc);
    }
  }

  it("a NARROW desktop-shell viewport renders the drawer rail — no panels button, no sheet", async () => {
    // desktopWidth stays false: matchMedia reports <1024.
    setShell("desktop");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    const rail = host.querySelector('aside[aria-label="Side panels"] [role="tablist"]');
    expect(rail, "the desktop drawer rail renders in the desktop shell at any width").not.toBeNull();
    expect(tid("panels-button"), "no phone panels button in the desktop shell").toBeNull();
    expect(panelsDialog(), "no phone sheet in the desktop shell").toBeNull();
    // The desktop shell has no Chats screen, so no phone ChatHeader (back button);
    // its <header> is the desktop TopBar.
    expect(tid("chat-back"), "no phone ChatHeader in the desktop shell").toBeNull();
  });

  it("a NARROW desktop-shell viewport keeps the desktop composer face ('Type a message…')", async () => {
    setShell("desktop");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    // data-testid="composer" is the phone face's marker, so the desktop face has none.
    expect(tid("composer"), "no phone composer face in the desktop shell").toBeNull();
    const ta = host.querySelector("textarea") as HTMLTextAreaElement;
    expect(ta, "the composer textarea renders").not.toBeNull();
    expect(ta!.placeholder, "the desktop placeholder, not the phone's 'Message'").toBe("Type a message…");
  });

  it("a NARROW desktop-shell viewport keeps the desktop message labels ('You' / 'Shifu')", async () => {
    await withViewport(async () => {
      setShell("desktop");
      hoisted.agent = withRows();
      mount();
      await flush();
      const rows = [...host.querySelectorAll("[data-index]")].map((r) => r.textContent ?? "");
      expect(rows.length, "rows render").toBe(2);
      expect(rows.join("|"), "the user row keeps its 'You' label").toContain("You");
      expect(rows.join("|"), "the agent row keeps its label").toContain("Shifu");
    });
  });

  it("crossing the breakpoint DOWN inside the desktop shell never shows the phone layout", async () => {
    desktopWidth = true;
    setShell("desktop");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(
      host.querySelector('aside[aria-label="Side panels"] [role="tablist"]'),
      "wide desktop shell: the rail",
    ).not.toBeNull();

    await setDesktop(false);
    await flush();

    expect(
      host.querySelector('aside[aria-label="Side panels"] [role="tablist"]'),
      "the rail survives the shrink",
    ).not.toBeNull();
    expect(tid("panels-button"), "no phone panels button after the shrink").toBeNull();
    expect(panelsDialog(), "no phone sheet after the shrink").toBeNull();
  });

  it("the BROWSER at the same narrow width still gets the phone layout (regression guard)", async () => {
    setShell("browser");
    hoisted.agent = makeAgent(withTabs());
    mount();
    await flush();
    expect(host.querySelector('aside[aria-label="Side panels"]'), "no drawer rail in a browser").toBeNull();
    expect(tid("panels-button"), "the phone panels button renders in a browser").not.toBeNull();
  });
});

// ═══ DIR — the Directory tab (design §4, §7.3) ═════════════════════════

describe("App mobile sheet — DIR: the Directory tab", () => {
  it("idle with cwd shows only 📁 basename in the activity row", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("activity-dir")).not.toBeNull();
    expect(tid("activity-dir")!.textContent).toContain("📁 exe");
    expect(tid("activity-tasks")).toBeNull();
    expect(tid("activity-todos")).toBeNull();
  });

  it("tapping 📁 opens the sheet on the Directory tab", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("activity-dir"));
    await flush();
    expect(panelsDialog()).not.toBeNull();
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
    ).toBe("directory");
  });

  it("pills order: directory first", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    const tabs = pillTabs();
    expect(tabs[0]).toBe("directory");
  });

  it("panels button opens directory (Q3 default)", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    expect(
      document.body.querySelector('[data-testid="panels-pill"][aria-selected="true"]')!.getAttribute("data-tab"),
    ).toBe("directory");
  });

  it("visible_tabs without directory → no activity-dir, no directory pill", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
        visible_tabs: ["session", "todo", "files", "tasks", "bash", "agents"],
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("activity-dir")).toBeNull();
    click(must("panels-button"));
    await flush();
    expect(pillTabs()).not.toContain("directory");
  });

  it("local edition local chat → no 📁", async () => {
    setBootEdition("local");
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
        catalog: WIRE_CATALOG_LOCAL,
        selection: { profile: "dev", agent: "team" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("activity-dir")).toBeNull();
  });

  it("local edition remote profile → 📁 present", async () => {
    setBootEdition("local");
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/srv/remote" },
        catalog: WIRE_CATALOG_REMOTE,
        selection: { profile: "home", agent: "team" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("activity-dir")).not.toBeNull();
  });

  it("choose a dir in the tab → switch_cwd sent, sheet closes", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/home/exe" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("activity-dir"));
    await flush();
    // Simulate a dir_listing response for a different path
    agentNow().dirListing = { path: "/other", parent: "/", entries: [], error: null };
    await rerender();
    await flush();
    const useBtn = must("use-directory");
    expect(useBtn.textContent).toContain("Use this directory");
    click(useBtn);
    await flush();
    expect(agentNow().send).toHaveBeenCalledWith({ type: "switch_cwd", path: "/other" });
    expect(panelsDialog(), "sheet closes after commit").toBeNull();
  });

  it("mobile shell phone width remote cwd → no bottom-bar", async () => {
    setShell("mobile");
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/srv/remote" },
        catalog: WIRE_CATALOG_REMOTE,
        selection: { profile: "home", agent: "team" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    // The footer is removed in the phone app: cwd is reached via the activity
    // row 📁 / Directory tab, so no BottomBar and no duplicate CwdPicker.
    expect(tid("activity-dir"), "cwd stays reachable via the activity row 📁").not.toBeNull();
    expect(tid("bottom-bar"), "no footer BottomBar in the phone app").toBeNull();
    expect(tid("cwd-picker"), "no footer CwdPicker duplicating the activity-row 📁").toBeNull();
  });

  it("mobile shell ≥lg remote cwd → bottom-bar with cwd picker", async () => {
    desktopWidth = true;
    setShell("mobile");
    hoisted.agent = makeAgent(
      stateFor("c1", {
        welcome: { ...WELCOME, cwd: "/srv/remote" },
        catalog: WIRE_CATALOG_REMOTE,
        selection: { profile: "home", agent: "team" },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    expect(tid("bottom-bar")).not.toBeNull();
    expect(tid("bottom-bar")!.querySelector('button[title="/srv/remote"]')).not.toBeNull();
  });
});

// ═══ B32 — the fresh-chat empty state (batch 3, fidelity plan 3.10) ══════

describe("App mobile sheet — B32: the fresh-chat empty state", () => {
  it("shows the welcome card and the 'Nothing here yet' hint", async () => {
    hoisted.agent = makeAgent(stateFor("c1", { welcome: WELCOME }));
    mount();
    await flush();
    expect(host.textContent, "the welcome card is kept").toContain("Welcome to Shifu");
    const hint = [...host.querySelectorAll("p")].find((p) =>
      p.textContent?.includes("Nothing here yet"),
    );
    expect(hint, "the 'Nothing here yet' hint").toBeDefined();
    // lg:hidden is the contract: the hint is phone-only, the desktop render
    // is unchanged.
    expect(hint!.classList.contains("lg:hidden"), "the hint is phone-only").toBe(true);
  });
});

// ═══ C17 — the Bash pill badge is desktop-only (batch 4, 4.23) ════════════

describe("App mobile sheet — C17: the Bash pill badge", () => {
  const withBash = (): AppState =>
    withTabs({
      bash_panel: {
        kind: "running",
        command: "cargo test",
        pid: 4242,
        started_at: T,
        tail: ["running 12 tests"],
      },
    } as Partial<AppState>);

  it("the Bash pill has no badge on a phone while bash runs", async () => {
    hoisted.agent = makeAgent(withBash());
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    const bashPill = document.body.querySelector('[data-testid="panels-pill"][data-tab="bash"]');
    expect(bashPill, "the Bash pill renders").not.toBeNull();
    expect(
      bashPill!.querySelector('[data-testid="panels-pill-badge"]'),
      "no badge on the phone (proto:470)",
    ).toBeNull();
  });

  it("the drawer keeps the Bash badge (desktop lock)", async () => {
    desktopWidth = true;
    setShell("browser");
    hoisted.agent = makeAgent(withBash());
    mount();
    await flush();
    const bashTab = [...host.querySelectorAll<HTMLElement>('aside [role="tab"]')].find((t) =>
      (t.textContent ?? "").includes("Bash"),
    );
    expect(bashTab, "the Bash rail handle renders").toBeDefined();
    const badge = [...bashTab!.querySelectorAll("span")].find(
      (s) => s.textContent?.trim() === "1",
    );
    expect(badge, "the desktop drawer keeps the Bash badge").toBeDefined();
  });
});

// ═══ X6 — Watch in the Agents sheet closes the sheet (batch 4, 4.22) ══════

describe("App mobile sheet — X6: Watch in the Agents sheet", () => {
  const subMsg: WireChatMessage = {
    role: "agent",
    content: "found the bug",
    timestamp: T,
    source: { kind: "sub_agent", role: "researcher" },
  };

  /** The banner's own "Clear" — the composer carries a second watch note
   * with its own Clear, which must not satisfy these assertions. */
  const bannerClear = (): HTMLButtonElement | undefined =>
    [...host.querySelectorAll("button")].find(
      (b) =>
        b.textContent?.trim() === "Clear" &&
        !b.closest('[data-testid="composer"]') &&
        !b.closest("footer, .border-t") &&
        (b.closest("div")?.textContent ?? "").includes("Watching"),
    );

  it("closes the sheet and shows the banner", async () => {
    hoisted.agent = makeAgent(
      stateFor("c1", {
        chat: {
          messages: [{ role: "user", content: "dig in", timestamp: T }, subMsg],
          auto_scroll: true,
          scroll_offset: 0,
        },
      } as Partial<AppState>),
    );
    mount();
    await flush();
    click(must("panels-button"));
    await flush();
    click(document.body.querySelector('[data-testid="panels-pill"][data-tab="agents"]')!);
    await flush();
    const panel = panelsDialog()!.querySelector('[role="tabpanel"]')!;
    const rows = [...panel.querySelectorAll<HTMLElement>("li")];
    const row = rows.find((r) => (r.textContent ?? "").includes("researcher"));
    const watch = row?.querySelector<HTMLButtonElement>("button");
    expect(watch, "the researcher row carries the Watch pill").not.toBeNull();
    expect(watch!.textContent, "the pill reads Watch").toContain("Watch");

    click(watch!);
    await flush();
    expect(panelsDialog(), "the sheet closes on Watch").toBeNull();
    const clear = bannerClear();
    expect(clear, "the watch banner renders with its Clear").toBeDefined();
    expect(clear!.closest("div")!.textContent).toContain("Watching researcher");
  });
});
