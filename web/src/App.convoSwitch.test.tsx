// @vitest-environment jsdom
//
// End-to-end (real App + real useAgent + FakeWebSocket, rows actually rendered
// in jsdom): switching conversations never shows the PREVIOUS conversation's
// messages (convo-state-cache design §8.3, S1–S7).
//
// Written FIRST (TDD), against the locked design. Expected on master:
//   RED   S1–S6  — useAgent keeps the previous chat's `state` after a switch,
//                  so its transcript stays on screen until the target's first
//                  frame (the bug), and nothing is recalled on a switch back.
//   GREEN S7     — a same-convo reconnect leaves transcript and controls alone
//                  (lock: it must stay green once the cache lands).
//
// Harness copied (repo convention, see App.screenSlide.test.tsx) from
// App.mobileChats.test.tsx (fixtures, toList/openRow/settlePopstates, setShell)
// and App.focus.test.tsx (the jsdom virtualizer setup: offsetHeight → 400, a
// no-op ResizeObserver) and App.tabs.test.tsx (mountApp with initialTabs).
// Unlike those files, `./useAgent` is NOT mocked: the point is the real hook.
//
// "Before any frame on the new socket": a switch's only async work is the fake
// socket's onopen microtask, which sends `attach` and never sets `state`, so
// awaiting a flush after the tap is still "before any frame".
//
// Focus mode is forced OFF so every message is a transcript row even while a
// turn runs (focus mode would hide a running turn's answer).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { TabsState } from "./convoTabs";
import type { AppState, ConversationSummary, WireChatMessage } from "./state";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fake WebSocket (useAgent.firstFrame.test.tsx conventions) ───────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 1; // OPEN, like the real socket once `onopen` has fired
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
    queueMicrotask(() => this.onopen?.());
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }
}

const sockets = (): FakeWebSocket[] => FakeWebSocket.instances;

// ─── fixtures (App.mobileChats.test.tsx) ─────────────────────────────────

const msg = (content: string): WireChatMessage => ({
  role: "agent",
  content,
  timestamp: "2026-01-01T00:00:00Z",
});

/** A minimal, valid-enough `AppState` frame for conversation `id`. */
function stateFor(id: string, name: string, texts: string[], over: Partial<AppState> = {}): AppState {
  return {
    chat: { messages: texts.map(msg), auto_scroll: true, scroll_offset: 0 },
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
    conversation: { id, name },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

/** A list row. Names deliberately never contain the transcript marker texts. */
const convoRow = (id: string, name: string): ConversationSummary =>
  ({
    id,
    name,
    updated_at: "2026-11-10T12:00:00.000Z",
    message_count: 3,
    model: "sonnet",
    active: false,
    profile: null,
  }) as ConversationSummary;

// ─── mount plumbing ──────────────────────────────────────────────────────

let root: Root | null = null;
let host: HTMLElement;
let offsetHeightDesc: PropertyDescriptor | undefined;

async function mount(initialTabs: TabsState | null = null): Promise<void> {
  host = document.createElement("div");
  document.body.appendChild(host);
  await act(async () => {
    root = createRoot(host);
    root.render(<App initialTabs={initialTabs} />);
  });
  await flush(); // the fake socket's onopen: sends `attach`
}

function unmount(): void {
  act(() => {
    root?.unmount();
  });
  root = null;
  host?.remove();
}

async function flush(): Promise<void> {
  for (let i = 0; i < 4; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

function setShell(marker: "desktop" | "mobile" | "browser"): void {
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  if (marker !== "browser") (globalThis as { peakbotShell?: unknown }).peakbotShell = marker;
}

function click(el: Element): void {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Deliver a server frame on the NEWEST socket. */
async function feed(frame: object): Promise<void> {
  const ws = sockets().at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
  });
}

/** The live handshake of the newest socket: attached + one state frame. */
async function live(
  id: string,
  name: string,
  texts: string[],
  over: Partial<AppState> = {},
): Promise<void> {
  await feed({ type: "attached", convo: id });
  await feed({ type: "state", state: stateFor(id, name, texts, over) });
}

const text = (): string => host.textContent ?? "";
const title = (): string | null =>
  host.querySelector('[data-testid="chat-title"]')?.textContent ?? null;
const stopButton = (): Element | null => host.querySelector('button[aria-label^="Stop"]');

/** The transcript's scrolling <section> (owner of the `[data-index]` rows). */
function transcriptSection(): HTMLElement | null {
  for (const row of host.querySelectorAll("[data-index]")) {
    let p: Element | null = row.parentElement;
    while (p && p.tagName.toLowerCase() !== "section") p = p.parentElement;
    if (p) return p as HTMLElement;
  }
  return null;
}

// ─── mobile navigation (App.mobileChats.test.tsx) ────────────────────────

async function openRow(id: string): Promise<void> {
  const open = host.querySelector(
    `[data-testid="chat-row"][data-convo="${id}"] button[data-testid="chat-open"]`,
  );
  expect(open, `row ${id} has an open button`).not.toBeNull();
  click(open!);
  await flush();
}

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

/** "‹ Chats": the conversation screen → the Chats list. */
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
  FakeWebSocket.instances = [];
  setShell("browser");
  vi.stubGlobal("WebSocket", FakeWebSocket);
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
  // jsdom has no ResizeObserver (the Transcript observes its scroll section).
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  // jsdom reports offsetHeight 0, so the virtualizer would render zero rows.
  offsetHeightDesc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get() {
      return 400;
    },
  });
  localStorage.clear();
  localStorage.setItem("peakbot-focus", "off");
  window.history.replaceState(null, "", "/?convo=c1");
});

afterEach(async () => {
  const backSpy = vi.spyOn(window.history, "back");
  unmount();
  const inFlight = backSpy.mock.calls.length;
  backSpy.mockRestore();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
  if (offsetHeightDesc) Object.defineProperty(HTMLElement.prototype, "offsetHeight", offsetHeightDesc);
  await settlePopstates(inFlight);
  setShell("browser");
  window.history.replaceState(null, "", "/");
});

// ═════════════════════════════════════════════════════════════════════════
// Mobile: row taps through the Chats list (S1–S3)
// ═════════════════════════════════════════════════════════════════════════

/** S1's steps: mobile on c1 (running, "ALPHA-ONE"), then tap the unseen c2
 * and land its first frame ("BRAVO-ONE"). Leaves the app on c2, live. */
async function mobileVisitBravo(): Promise<void> {
  setShell("mobile");
  await mount();
  await live("c1", "Alpha", ["ALPHA-ONE"], { is_running: true });
  expect(text(), "precondition: c1 is on screen").toContain("ALPHA-ONE");
  expect(stopButton(), "precondition: c1's turn is running").not.toBeNull();

  await feed({
    type: "conversations_list",
    items: [convoRow("c1", "First chat"), convoRow("c2", "Second chat")],
  });
  await toList();
  await openRow("c2");
}

/** S2's steps: back to the list and tap c1 — a recent chat, so a cache hit. */
async function mobileReopenAlpha(): Promise<void> {
  await live("c2", "Bravo", ["BRAVO-ONE"]);
  expect(text(), "precondition: c2 is live").toContain("BRAVO-ONE");
  await toList();
  await openRow("c1");
}

describe("App convo switch — mobile row taps", () => {
  it("S1: tapping an unseen chat never shows the previous chat", async () => {
    await mobileVisitBravo();

    // Before ANY frame on the new socket:
    expect(sockets(), "the tap dialed a new socket").toHaveLength(2);
    expect(text(), "c1's transcript must not linger under c2").not.toContain("ALPHA-ONE");
    expect(title(), "no transcript yet → the header says New chat").toBe("New chat");
    expect(stopButton(), "c1's running turn must not offer Stop on c2").toBeNull();
  });

  it("S2: reopening a recent chat shows it instantly, without live controls", async () => {
    await mobileVisitBravo();
    await mobileReopenAlpha();

    // Before ANY frame on c1's new socket:
    expect(sockets(), "boot + c2 + c1").toHaveLength(3);
    expect(text(), "c1's last-seen transcript").toContain("ALPHA-ONE");
    expect(text(), "c2's transcript must be gone").not.toContain("BRAVO-ONE");
    expect(title()).toBe("Alpha");
    expect(stopButton(), "c1 was running when left, but the snapshot is quiesced").toBeNull();
  });

  it("S3: the live frame refreshes in place without remounting the transcript", async () => {
    await mobileVisitBravo();
    await mobileReopenAlpha();
    expect(text(), "precondition (S2): c1 is recalled").toContain("ALPHA-ONE");
    const before = transcriptSection();
    expect(before, "the recalled transcript is rendered").not.toBeNull();

    await live("c1", "Alpha", ["ALPHA-ONE", "ALPHA-TWO"], { is_running: true });

    expect(text()).toContain("ALPHA-TWO");
    expect(stopButton(), "live controls arrive with the live frame").not.toBeNull();
    expect(transcriptSection(), "same scroll element: no remount").toBe(before);
  });
});

// ═════════════════════════════════════════════════════════════════════════
// Desktop: tab bar (S4), picker (S5); browser: picker (S6)
// ═════════════════════════════════════════════════════════════════════════

const tab = (id: string, convo: string | null, title: string) => ({ id, convo, title, profile: null });
const tabsState = (tabs: ReturnType<typeof tab>[], active = 0): TabsState =>
  ({ tabs, active }) as unknown as TabsState;

const tabEls = (): HTMLElement[] =>
  Array.from(host.querySelectorAll<HTMLElement>('nav[aria-label="Conversations"] [role="tab"]'));

/** The conversations picker in the TopBar header (the BottomBar carries a
 * second one in a browser; it is `lg:hidden`, but jsdom applies no CSS). */
const headerPicker = (): HTMLElement | null =>
  host.querySelector<HTMLElement>('header button[title="Load a saved conversation"]');

/** Open the TopBar picker, deliver a list holding c2, and click c2's row. */
async function pickRowC2(): Promise<void> {
  const picker = headerPicker();
  expect(picker, "the TopBar conversations picker renders").not.toBeNull();
  click(picker!);
  await flush();
  await feed({ type: "conversations_list", items: [convoRow("c2", "Second chat")] });
  const row = Array.from(host.querySelectorAll<HTMLElement>("header button")).find((b) =>
    (b.textContent ?? "").includes("Second chat"),
  );
  expect(row, "the picker lists c2").toBeDefined();
  click(row!);
  await flush();
}

describe("App convo switch — desktop and browser", () => {
  it("S4: desktop tab bar: switching tabs shows the target, cached or blank", async () => {
    setShell("desktop");
    window.history.replaceState(null, "", "/?convo=a");
    await mount(tabsState([tab("t-a", "a", "A"), tab("t-b", "b", "B")], 0));
    await live("a", "Alpha", ["ALPHA-ONE"]);
    expect(text(), "precondition: tab a is on screen").toContain("ALPHA-ONE");

    // Tab b was never seen: blank, not tab a's transcript.
    click(tabEls()[1]);
    await flush();
    expect(sockets(), "the tab click dialed a new socket").toHaveLength(2);
    expect(text(), "a's transcript must not linger under tab b").not.toContain("ALPHA-ONE");

    await live("b", "Bravo", ["BRAVO-ONE"]);
    expect(text(), "precondition: b is live").toContain("BRAVO-ONE");

    // Back to tab a: its last-seen transcript at once, before any frame.
    click(tabEls()[0]);
    await flush();
    expect(sockets(), "boot + b + a").toHaveLength(3);
    expect(text(), "a is recalled instantly").toContain("ALPHA-ONE");
    expect(text(), "b's transcript is gone").not.toContain("BRAVO-ONE");
  });

  it("S5: the desktop picker opens a chat the same way", async () => {
    setShell("desktop");
    window.history.replaceState(null, "", "/?convo=a");
    await mount(tabsState([tab("t-a", "a", "A")], 0));
    await live("a", "Alpha", ["ALPHA-ONE"]);
    expect(text(), "precondition: a is on screen").toContain("ALPHA-ONE");

    await pickRowC2();

    expect(sockets(), "the pick dialed a new socket").toHaveLength(2);
    expect(text(), "a's transcript must not linger before c2's frame").not.toContain("ALPHA-ONE");
  });

  it("S6: the browser picker (no tabs) opens a chat the same way", async () => {
    // A transcript on screen makes the picker ask "Load …? This replaces…".
    vi.spyOn(window, "confirm").mockReturnValue(true);
    window.history.replaceState(null, "", "/?convo=a");
    await mount(null);
    await live("a", "Alpha", ["ALPHA-ONE"]);
    expect(text(), "precondition: a is on screen").toContain("ALPHA-ONE");

    await pickRowC2();

    expect(sockets(), "the pick dialed a new socket").toHaveLength(2);
    expect(text(), "a's transcript must not linger before c2's frame").not.toContain("ALPHA-ONE");
  });
});

// ═════════════════════════════════════════════════════════════════════════
// S7: a same-convo reconnect leaves everything alone (GREEN lock)
// ═════════════════════════════════════════════════════════════════════════

describe("App convo switch — same-convo reconnect", () => {
  it("S7: leaves the transcript and controls alone", async () => {
    await mount(null); // browser shell, real timers: `live`/`mount` flush on setTimeout
    await live("c1", "Alpha", ["ALPHA-ONE"], { is_running: true });
    const section = transcriptSection();
    expect(section, "precondition: the transcript is rendered").not.toBeNull();
    expect(stopButton(), "precondition: Stop is offered").not.toBeNull();

    // An unasked close, then the 500 ms backoff reconnect (fake timers only
    // from here on, so the real-timer flushes above are unaffected).
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    await act(async () => {
      sockets().at(-1)!.close();
    });
    await act(async () => {
      vi.advanceTimersByTime(600);
      await Promise.resolve(); // the replacement's onopen
    });
    expect(sockets(), "a replacement socket was dialed").toHaveLength(2);
    await feed({ type: "attached", convo: "c1" });

    expect(text(), "the live transcript stays").toContain("ALPHA-ONE");
    expect(stopButton(), "the live Stop stays").not.toBeNull();
    expect(transcriptSection(), "same scroll element: no remount").toBe(section);
  });
});
