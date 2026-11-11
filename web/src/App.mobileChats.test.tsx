// App-level wiring for the mobile Chats home screen (design §4.3, A1–A9).
//
// The mobile shell (shell() === "mobile") swaps the TopBar for the
// Conversation header, renders the Chats list as a layer, and drops the
// ConversationsPicker + lg:hidden from the bottom bar. Browser and desktop
// shells are untouched (A8/A9 are the regression locks).
//
// Harness: ./App.mobileNav.test.tsx conventions (mocked ./useAgent, raw DOM,
// matchMedia + fetch stubs, history spies, waitForPopstate).
//
// RED before implementation: App does not render chat-back / chat-list /
// ChatHeader yet, and the bottom bar still carries the picker + lg:hidden in
// the mobile shell — so these fail on the missing elements / wrong classes,
// the right reason (feature absent).

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState, ConversationSummary, WireChatMessage } from "./state";

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

// ─── fake agent ──────────────────────────────────────────────────────────

interface FakeAgent {
  connected: boolean;
  state: AppState | null;
  models: unknown[];
  activeAlias: string;
  conversations: ConversationSummary[];
  conversationsError: string | null;
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn>;
}

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

function makeAgent(
  state: AppState | null,
  error: string | null = null,
  over: Partial<FakeAgent> = {},
): FakeAgent {
  return {
    connected: true,
    state,
    models: [],
    activeAlias: "",
    conversations: [],
    conversationsError: null,
    commands: [],
    dirListing: null,
    recentDirs: [],
    error,
    send: vi.fn(),
    convo: state?.conversation?.id ?? null,
    switchConvo: vi.fn(),
    ...over,
  };
}

const convoRow = (id: string, profile: string | null = null): ConversationSummary =>
  ({
    id,
    name: `convo ${id}`,
    updated_at: "2026-11-10T12:00:00.000Z",
    message_count: 3,
    model: "sonnet",
    active: false,
    profile,
  }) as ConversationSummary;

const msg = (content: string): WireChatMessage => ({
  role: "agent",
  content,
  timestamp: "2026-01-01T00:00:00Z",
});

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

const SETTINGS_MARKER = "← Back to chat";
const settingsOpen = (): boolean => (host.textContent ?? "").includes(SETTINGS_MARKER);

/** The TopBar wordmark (logo + "Shifu") — absent in the mobile shell. */
function topBarWordmark(): Element | null {
  return (
    [...host.querySelectorAll("span.font-semibold")].find(
      (s) => (s.textContent ?? "").trim() === "Shifu",
    ) ?? null
  );
}

function click(el: Element): void {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

// ─── composer typing (M-tests) ────────────────────────────────────────────

const textareaSetter = Object.getOwnPropertyDescriptor(
  HTMLTextAreaElement.prototype,
  "value",
)!.set!;

/** Type into the (single) composer's textarea: native setter + input event,
 * as in App.tabs.test.tsx. */
async function typeInto(ta: HTMLTextAreaElement, value: string): Promise<void> {
  await act(async () => {
    textareaSetter.call(ta, value);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** The composer's textarea (the mobile shell renders exactly one). */
function theTextbox(): HTMLTextAreaElement {
  const ta = host.querySelector("textarea");
  expect(ta, "the composer textarea must render").not.toBeNull();
  return ta!;
}

/** The composer's Send button. */
function theSendButton(): HTMLButtonElement {
  const b = Array.from(host.querySelectorAll("button")).find(
    (btn) => (btn.textContent ?? "").trim() === "Send",
  );
  expect(b, "the composer's Send button must render").toBeDefined();
  return b!;
}

/** Tap a chat row's open button, like a user would. */
async function openRow(id: string): Promise<void> {
  const open = host.querySelector(
    `[data-testid="chat-row"][data-convo="${id}"] button[data-testid="chat-open"]`,
  );
  expect(open, `row ${id} has an open button`).not.toBeNull();
  click(open!);
  await flush();
}

/** Await the popstate that a history.back() produces (jsdom: one task). */
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

/** Let the unmount's in-flight back() traversals land so the next test
 *  starts from a quiescent arbiter (no owed marker push). Bounded, because
 *  a stuck traversal must not hang the suite. */
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

/** Navigate from the conversation to the Chats list via the header back. */
async function toList(): Promise<void> {
  const back = host.querySelector<HTMLElement>('[data-testid="chat-back"]');
  expect(back, "chat-back must render on the conversation").not.toBeNull();
  const popped = waitForPopstate();
  click(back!);
  await act(async () => {
    await popped;
  });
  await flush();
}

beforeEach(() => {
  setShell("mobile");
  vi.stubGlobal(
    "matchMedia",
    (media: string) =>
      ({
        get matches() {
          return false;
        },
        media,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList,
  );
  vi.stubGlobal("fetch", vi.fn(async () => {
    throw new Error("no server in tests");
  }));
  // jsdom has no ResizeObserver; the Transcript (A7) observes its scroll section.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  window.history.replaceState(null, "", "/");
});

afterEach(async () => {
  const backSpy = vi.spyOn(window.history, "back");
  unmount();
  const inFlight = backSpy.mock.calls.length;
  backSpy.mockRestore();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  await settlePopstates(inFlight);
  setShell("browser");
  window.history.replaceState(null, "", "/");
});

// ─── A1–A9 ───────────────────────────────────────────────────────────────

describe("App mobile Chats wiring (A1–A9)", () => {
  it("A1: the mobile shell mounts on the conversation with one history marker", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    const pushSpy = vi.spyOn(window.history, "pushState");
    mount();
    await flush();

    expect(host.querySelector('[data-testid="chat-back"]'), "chat-back renders").not.toBeNull();
    expect(host.querySelector('[data-testid="chat-list"]'), "no list at first").toBeNull();
    expect(topBarWordmark(), "no TopBar wordmark").toBeNull();
    expect(
      host.querySelector('button[title="Load a saved conversation"]'),
      "no ConversationsPicker in the bottom bar",
    ).toBeNull();
    expect(pushSpy, "exactly one pushState at mount").toHaveBeenCalledTimes(1);
  });

  it("A2: '‹ Chats' shows the list and inert-s the conversation", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    await toList();

    expect(host.querySelector('[data-testid="chat-list"]'), "the list shows").not.toBeNull();
    const inert = host.querySelector("[inert]");
    expect(inert, "a wrapper is inert").not.toBeNull();
    expect(inert!.querySelector("textarea"), "the composer sits inside the inert wrapper").not.toBeNull();
    expect(hoisted.agent as FakeAgent).toHaveProperty("send");
    expect((hoisted.agent as FakeAgent).send).toHaveBeenCalledWith({ type: "request_conversations" });
  });

  it("A3: tapping a row re-attaches to it and hides the list", async () => {
    const fake = makeAgent(stateFor("c1"));
    hoisted.agent = fake;
    mount();
    await flush();
    // `loaded` flips on the first NEW list after mount, so deliver one.
    fake.conversations = [convoRow("c2", "home")];
    await toList();

    const pushSpy = vi.spyOn(window.history, "pushState");
    const open = host.querySelector(
      '[data-testid="chat-row"][data-convo="c2"] button[data-testid="chat-open"]',
    );
    expect(open, "the row's open button renders").not.toBeNull();
    click(open!);
    await flush();

    const agent = hoisted.agent as FakeAgent;
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("c2", "home");
    expect(host.querySelector('[data-testid="chat-list"]'), "the list is gone").toBeNull();
    expect(pushSpy, "one more pushState").toHaveBeenCalledTimes(1);
  });

  it("A4: the FAB starts a new chat in place", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toList();

    const fab = host.querySelector('button[aria-label="New chat"]');
    expect(fab, "the FAB renders on the list").not.toBeNull();
    click(fab!);
    await flush();

    const agent = hoisted.agent as FakeAgent;
    expect(agent.switchConvo).toHaveBeenCalledWith(null, null);
    expect(host.querySelector('[data-testid="chat-list"]'), "the list is gone").toBeNull();
  });

  it("A5: a real Back on the conversation shows the list; the app adds no traversal", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    const backSpy = vi.spyOn(window.history, "back");
    const goSpy = vi.spyOn(window.history, "go");
    const popped = waitForPopstate();
    window.history.back();
    await act(async () => {
      await popped;
    });
    await flush();

    expect(host.querySelector('[data-testid="chat-list"]'), "the list shows").not.toBeNull();
    const traversals = backSpy.mock.calls.length + goSpy.mock.calls.filter(([n]) => n === -1).length;
    expect(traversals, "only the user's own Back traversed").toBe(1);
  });

  it("A6: Settings from the list; a Back closes only Settings", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toList();

    const gear = host.querySelector('button[aria-label="Settings"]');
    expect(gear, "the gear renders on the list").not.toBeNull();
    click(gear!);
    await flush();
    expect(settingsOpen(), "Settings opens").toBe(true);

    const popped = waitForPopstate();
    window.history.back();
    await act(async () => {
      await popped;
    });
    await flush();

    expect(settingsOpen(), "Settings closes on Back").toBe(false);
    expect(host.querySelector('[data-testid="chat-list"]'), "the list stays").not.toBeNull();
  });

  it("A7: the title is the bound conversation's name, else 'New chat'", async () => {
    // Bound with a transcript.
    hoisted.agent = makeAgent(
      stateFor("c1", {
        conversation: { id: "c1", name: "Fix WS" },
        chat: { messages: [msg("hello")], auto_scroll: true, scroll_offset: 0 },
      }),
      null,
      { convo: "c1" },
    );
    mount();
    await flush();
    expect(host.querySelector('[data-testid="chat-title"]')!.textContent).toBe("Fix WS");
    unmount();

    // No transcript.
    hoisted.agent = makeAgent(
      stateFor("c1", { conversation: { id: "c1", name: "Fix WS" } }),
      null,
      { convo: "c1" },
    );
    mount();
    await flush();
    expect(host.querySelector('[data-testid="chat-title"]')!.textContent).toBe("New chat");
  });

  it("A8: the bottom bar drops lg:hidden in the mobile shell, keeps it in a browser", async () => {
    // Mobile shell.
    setShell("mobile");
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    const mobileBar = host.querySelector('[data-testid="bottom-bar"]');
    expect(mobileBar, "the bottom bar renders in the mobile shell").not.toBeNull();
    expect(mobileBar!.classList.contains("lg:hidden"), "no lg:hidden in the mobile shell").toBe(false);
    unmount();

    // Browser.
    setShell("browser");
    mount();
    await flush();
    const browserBar = host.querySelector('[data-testid="bottom-bar"]')!;
    expect(browserBar.classList.contains("lg:hidden"), "lg:hidden in a browser").toBe(true);
    expect(
      browserBar.querySelector('button[title="Load a saved conversation"]'),
      "the picker renders in a browser",
    ).not.toBeNull();
  });

  it.each(["browser", "desktop"] as const)(
    "A9: the %s shell renders the TopBar with no mobile chrome and zero pushState",
    async (marker) => {
      setShell(marker);
      hoisted.agent = makeAgent(stateFor("c1"));
      const pushSpy = vi.spyOn(window.history, "pushState");
      mount();
      await flush();

      expect(topBarWordmark(), "the TopBar wordmark renders").not.toBeNull();
      expect(host.querySelector('[data-testid="chat-back"]'), "no chat-back").toBeNull();
      expect(host.querySelector('[data-testid="chat-list"]'), "no chat-list").toBeNull();
      expect(pushSpy, "zero pushState at mount").toHaveBeenCalledTimes(0);
    },
  );
});

// ─── M1–M3: per-conversation drafts on the single (mobile) composer ──────
//
// The mobile shell renders ONE composer for every conversation (the
// single-slot branch of App), so today a half-typed message follows the user
// across the ChatList. Required: behave like the desktop tabs — B never
// shows A's text, and returning to A restores A's draft. The Composer will
// get a `draftKey` prop (the bound convo: `agent.convo`, null = fresh chat,
// null → id = the server naming the fresh chat, which is NOT a switch).
//
// These drive the REAL App tree: the fake `switchConvo` mimics useAgent and
// binds `convo` + `state` synchronously, so `open()`/`newChat()` re-render
// App onto the new conversation (the A-tests use a plain spy and only assert
// the call).

describe("App mobile — per-conversation composer drafts (M1–M3)", () => {
  /** An agent whose switchConvo, like the real useAgent, sets `convo`
   * (and that convo's state) synchronously, so the App re-renders bound to
   * the new conversation when a row is opened / the FAB is tapped. The real
   * useAgent keeps `convo`/`state` in React state (setConvo on switch), so
   * the switch re-renders App; the mock returns a plain object, so the fake
   * performs that re-render itself. */
  function bindingAgent(convo: string | null): FakeAgent {
    const fake = makeAgent(stateFor(convo));
    fake.switchConvo = vi.fn((id: string | null) => {
      fake.convo = id;
      fake.state = stateFor(id);
      act(() => {
        root!.render(<App />);
      });
    });
    return fake;
  }

  it("M1: a draft typed in c1 does not follow to c2 and comes back", async () => {
    const fake = bindingAgent("c1");
    hoisted.agent = fake;
    mount();
    await flush();
    // `loaded` flips on the first NEW list after mount, so deliver one
    // (same pattern as A3).
    fake.conversations = [convoRow("c1"), convoRow("c2")];

    await typeInto(theTextbox(), "draft in A");
    await toList();
    await openRow("c2");
    expect(theTextbox().value, "c2 must not show c1's half-typed text").toBe("");

    await toList();
    await openRow("c1");
    expect(theTextbox().value, "c1's draft is restored").toBe("draft in A");
  });

  it("M2: the FAB opens an empty composer and c1's draft comes back", async () => {
    const fake = bindingAgent("c1");
    hoisted.agent = fake;
    mount();
    await flush();
    fake.conversations = [convoRow("c1")];

    await typeInto(theTextbox(), "draft in A");
    await toList();
    const fab = host.querySelector('button[aria-label="New chat"]');
    expect(fab, "the FAB renders on the list").not.toBeNull();
    click(fab!);
    await flush();
    expect(theTextbox().value, "a new chat starts empty").toBe("");

    await toList();
    await openRow("c1");
    expect(theTextbox().value, "c1's draft is restored").toBe("draft in A");
  });

  it("M3: sent text is not restored", async () => {
    const fake = bindingAgent("c1");
    hoisted.agent = fake;
    mount();
    await flush();
    fake.conversations = [convoRow("c1"), convoRow("c2")];

    await typeInto(theTextbox(), "sent away");
    await act(async () => {
      theSendButton().click();
    });
    expect(fake.send).toHaveBeenCalledWith(
      expect.objectContaining({ type: "send_message", text: "sent away" }),
    );

    await toList();
    await openRow("c2");
    await toList();
    await openRow("c1");
    expect(theTextbox().value, "a sent message is never restored").toBe("");
  });
});
