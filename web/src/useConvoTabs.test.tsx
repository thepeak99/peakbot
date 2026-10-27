// @vitest-environment jsdom
//
// Tests for `useConvoTabs.ts` + `bootDesktopTabs()` (design §4/§5):
//
//   export function bootDesktopTabs(): TabsState; // impure: reads storage+URL, writes URL
//   export interface ConvoTabsApi {
//     views: TabView[]; count: number;
//     activate(i: number): void; close(i: number): void; newTab(): void; open(convo: string): void;
//   }
//   export function useConvoTabs(agent: AgentConnection, initial: TabsState | null,
//                                activeTitle: string | null): ConvoTabsApi | null;
//
// Behaviour under test:
//   - `initial` null (browser) → null, no keydown listener, nothing happens
//   - the user picks the active tab; the SERVER tells the store which convo
//     it is really on: `agent.convo` (attached / state.conversation.id) makes
//     the active tab follow WITHOUT calling `switchConvo`
//   - user ops that move the active tab call `switchConvo` exactly once
//   - unread is derived from the running→idle edge of the polled list;
//     activating a tab clears its dot
//   - status polling (`request_conversations`) only when connected AND >1 tab
//   - window keydown (capture) maps shortcuts (tabShortcuts) to dispatch and
//     preventDefault on a match
//   - the store persists to localStorage on every state change
//
// RED before implementation: `./useConvoTabs` does not exist yet, so this
// file fails at import — the right reason (feature absent). The stub agent
// is written against the POST-implementation `AgentConnection` (adds
// `convo: string | null`; `switchConvo` accepts `null`), cast through
// `unknown` so the file still runs under vitest's transform.
//
// Harness modeled on useAttachments.test.tsx (createRoot/act off
// react-dom/client; jsdom via the per-file pragma).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { bootDesktopTabs, useConvoTabs } from "./useConvoTabs";
import type { ConvoTab, TabsState } from "./convoTabs";
import type { ConversationSummary, InboundMessage } from "./state";
import type { AgentConnection } from "./useAgent";

// ─── stub agent (post-implementation AgentConnection shape) ──────────────

interface StubAgent {
  connected: boolean;
  state: { is_running: boolean } | null;
  models: unknown[];
  activeAlias: string;
  conversations: ConversationSummary[];
  conversationsError: string | null;
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn<(msg: InboundMessage) => void>>;
  /** The conversation the socket is bound to (added by this feature). */
  convo: string | null;
  /** Re-attach (added `null` support by this feature). */
  switchConvo: (id: string | null) => void;
}

function makeAgent(over: Partial<StubAgent> = {}): StubAgent {
  return {
    connected: true,
    state: { is_running: false },
    models: [],
    activeAlias: "gpt",
    conversations: [],
    conversationsError: null,
    commands: [],
    dirListing: null,
    recentDirs: [],
    error: null,
    send: vi.fn(),
    convo: null,
    switchConvo: vi.fn(),
    ...over,
  };
}

const convo = (
  id: string,
  over: Partial<ConversationSummary> & { running?: boolean } = {},
): ConversationSummary =>
  ({
    id,
    name: `convo ${id}`,
    updated_at: "2026-10-16T00:00:00Z",
    message_count: 1,
    model: "m",
    active: false,
    ...over,
  } as ConversationSummary);

// ─── fixtures ────────────────────────────────────────────────────────────

// Literal ids: `id-<convo>` (or `id-new` for a null-convo tab), so the tests can
// assert exactly which tab a view / state entry is without depending on minting.
const tab = (convo: string | null, title = ""): ConvoTab => ({
  id: `id-${convo ?? "new"}`,
  convo,
  title,
});
const state = (tabs: ConvoTab[], active = 0): TabsState => ({ tabs, active }) as TabsState;

// ─── harness ─────────────────────────────────────────────────────────────

let api: unknown;
let container: HTMLDivElement | null = null;
let root: Root | null = null;

function Harness({
  agent,
  initial,
  title,
}: {
  agent: StubAgent;
  initial: TabsState | null;
  title: string | null;
}) {
  api = useConvoTabs(
    agent as unknown as AgentConnection,
    initial,
    title,
  );
  return null;
}

async function renderHook(
  agent: StubAgent,
  initial: TabsState | null,
  title: string | null = null,
): Promise<void> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Harness agent={agent} initial={initial} title={title} />);
  });
}

async function rerender(
  agent: StubAgent,
  initial: TabsState | null,
  title: string | null = null,
): Promise<void> {
  await act(async () => {
    root!.render(<Harness agent={agent} initial={initial} title={title} />);
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

/** Fake only the timer APIs the polling interval uses; leaving microtasks /
 * nextTick real keeps React's `act` from stalling. */
function useIntervalFakes(): void {
  vi.useFakeTimers({
    toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout"],
  });
}

/** Dispatch a window keydown the way the page would. */
function key(
  over: Partial<KeyboardEvent> & { key: string },
): KeyboardEvent {
  const e = new KeyboardEvent("keydown", {
    cancelable: true,
    bubbles: true,
    ...over,
  });
  act(() => {
    window.dispatchEvent(e);
  });
  return e;
}

type Views = {
  key: string;
  label: string;
  tooltip: string;
  untitled: boolean;
  active: boolean;
  running: boolean;
  unread: boolean;
}[];

const views = (): Views => (api as { views: Views }).views;

// ─── browser (initial null) ──────────────────────────────────────────────

describe("useConvoTabs — browser (initial null)", () => {
  it("returns null and adds no keydown listener", async () => {
    const addSpy = vi.spyOn(window, "addEventListener");
    const agent = makeAgent();

    await renderHook(agent, null);

    expect(api).toBeNull();
    expect(
      addSpy.mock.calls.filter((c) => c[0] === "keydown"),
      "no keydown listener in the browser",
    ).toHaveLength(0);
    // A browser Ctrl+T must do nothing here (the page must not fight the
    // browser's own shortcuts).
    expect(() => key({ key: "t", ctrlKey: true })).not.toThrow();
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });
});

// ─── mount ───────────────────────────────────────────────────────────────

describe("useConvoTabs — mount", () => {
  it("exposes the initial tabs as views and count, without switching", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    expect(api).not.toBeNull();
    const v = views();
    expect(v).toHaveLength(2);
    expect(v[0]).toMatchObject({ key: "id-a", label: "A", active: true });
    expect(v[1]).toMatchObject({ key: "id-b", label: "B", active: false });
    expect((api as { count: number }).count).toBe(2);
    // Mount must not re-attach: the URL already carries the active tab.
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("persists the store to localStorage on mount", async () => {
    // The socket is bound to the ACTIVE tab ("b"): a mismatch would be
    // (correctly) healed by the follow-the-server rule and change the store.
    const agent = makeAgent({ convo: "b" });
    const initial = state([tab("a", "A"), tab("b", "B")], 1);

    await renderHook(agent, initial);

    const raw = localStorage.getItem("peakbot.desktop.tabs");
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw!)).toEqual({
      v: 1,
      tabs: [
        { convo: "a", title: "A" },
        { convo: "b", title: "B" },
      ],
      active: 1,
    });
  });
});

// ─── user ops ────────────────────────────────────────────────────────────

describe("useConvoTabs — user ops", () => {
  it("activate() calls switchConvo once with the target tab's convo", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { activate(i: number): void }).activate(1);
    });

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b");
    expect(views()[1].active).toBe(true);
    // The persisted store follows.
    expect(JSON.parse(localStorage.getItem("peakbot.desktop.tabs")!)["active"]).toBe(1);
  });

  it("activate() of the current tab does not switch", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { activate(i: number): void }).activate(0);
    });

    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("close() of the active tab switches to the right neighbour", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B"), tab("c", "C")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { close(i: number): void }).close(0);
    });

    expect(agent.switchConvo).toHaveBeenCalledWith("b");
    expect(views()).toHaveLength(2);
    expect(views()[0].key).toBe("id-b");
  });

  it("close() of an inactive tab does not switch", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B"), tab("c", "C")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { close(i: number): void }).close(2);
    });

    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(views()).toHaveLength(2);
  });

  it("close() of the last tab gives a fresh tab and switches to null", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { close(i: number): void }).close(0);
    });

    expect(agent.switchConvo).toHaveBeenCalledWith(null);
    const v = views();
    expect(v).toHaveLength(1);
    // The closed tab's id must not survive: the replacement is a NEW tab (I5).
    expect(v[0]).toMatchObject({ label: "New conversation", untitled: true });
    expect(v[0].key).not.toBe("id-a");
  });

  it("newTab() appends a fresh tab and switches to null", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { newTab(): void }).newTab();
    });

    expect(agent.switchConvo).toHaveBeenCalledWith(null);
    const v = views();
    expect(v).toHaveLength(2);
    expect(v[1]).toMatchObject({ key: expect.any(String), active: true, untitled: true });
    expect(v[1].key).not.toBe(v[0].key);
  });

  it("open() appends and activates an unopened convo, switching to it", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { open(convo: string): void }).open("d");
    });

    expect(agent.switchConvo).toHaveBeenCalledWith("d");
    const v = views();
    expect(v).toHaveLength(2);
    // The key is the minted tab id (I5), not the convo.
    expect(v[1]).toMatchObject({ key: expect.any(String), active: true });
    expect(v[1].key).not.toBe("d");
    expect(v[1].key).not.toBe(v[0].key);
  });

  it("open() of an already-open convo just activates it", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    await act(async () => {
      (api as { open(convo: string): void }).open("b");
    });

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b");
    expect(views()).toHaveLength(2); // no append
  });
});

// ─── server-driven: the store follows the binding ────────────────────────

describe("useConvoTabs — the store follows the server binding", () => {
  it("agent.convo (attached) makes the active tab follow WITHOUT switchConvo", async () => {
    // A fresh (null) tab: the user hit +, the server minted a session and
    // answered `attached "d"`. The active tab must bind to "d".
    const agent = makeAgent({ convo: null });
    const initial = state([tab("a", "A"), tab(null, "")], 1);

    await renderHook(agent, initial);

    agent.convo = "d";
    await rerender(agent, initial);

    expect(agent.switchConvo, "bound must never call switchConvo").not.toHaveBeenCalled();
    const v = views();
    // Binding keeps the active tab's id (I5): same key, new convo.
    expect(v[1]).toMatchObject({ key: "id-new", active: true });
    expect(v[0]).toMatchObject({ key: "id-a" });
    expect((api as { state: { tabs: { convo: string | null }[] } }).state.tabs[1].convo).toBe("d");
  });

  it("retitle: activeTitle renames the active tab (hasTranscript path)", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "Old title"), tab("b", "B")], 0);

    await renderHook(agent, initial);
    expect(views()[0].label).toBe("Old title");

    await rerender(agent, initial, "New title");
    expect(views()[0].label).toBe("New title");

    // Without a transcript there is no title to retitle from.
    await rerender(agent, initial, null);
    expect(views()[0].label).toBe("New title"); // unchanged, not cleared
  });
});

// ─── status: running / unread ────────────────────────────────────────────

describe("useConvoTabs — background status (running → unread)", () => {
  it("shows the spinner on a running background tab and the dot when its turn finishes", async () => {
    const agent = makeAgent({
      convo: "a",
      conversations: [convo("a"), convo("b", { running: true })],
    });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    // B is running in the background (from the polled list).
    expect(views()[1]).toMatchObject({ running: true, unread: false });

    // Switch to B (its turn is in progress).
    await act(async () => {
      (api as { activate(i: number): void }).activate(1);
    });
    agent.convo = "b";
    agent.state = { is_running: true };
    await rerender(agent, initial);

    // The ACTIVE tab's running state comes live from state.is_running.
    expect(views()[1]).toMatchObject({ active: true, running: true });

    // Switch back to A; B was running when we left.
    await act(async () => {
      (api as { activate(i: number): void }).activate(0);
    });
    agent.convo = "a";
    agent.state = { is_running: false };
    await rerender(agent, initial);

    // B's turn finishes in the background: the polled list flips it to idle.
    agent.conversations = [convo("a"), convo("b", { running: false })];
    await rerender(agent, initial);

    expect(views()[1]).toMatchObject({ running: false, unread: true });
    // The active tab stays dot-free.
    expect(views()[0].unread).toBe(false);

    // Activating B clears the dot (enter).
    await act(async () => {
      (api as { activate(i: number): void }).activate(1);
    });
    expect(views()[1].unread).toBe(false);
  });

  it("does not set unread for a tab that was idle all along", async () => {
    const agent = makeAgent({
      convo: "a",
      conversations: [convo("a"), convo("b")],
    });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);
    expect(views()[1]).toMatchObject({ running: false, unread: false });

    // The list refreshes; B was never running → no dot.
    agent.conversations = [convo("a"), convo("b", { running: false })];
    await rerender(agent, initial);
    expect(views()[1]).toMatchObject({ running: false, unread: false });
  });
});

// ─── polling ─────────────────────────────────────────────────────────────

describe("useConvoTabs — status polling", () => {
  it("polls request_conversations every 4s when connected with more than one tab", async () => {
    useIntervalFakes();
    const agent = makeAgent({
      connected: true,
      conversations: [convo("a"), convo("b")],
    });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);
    agent.send.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(4000);
    });
    expect(agent.send).toHaveBeenCalledWith({ type: "request_conversations" });

    await act(async () => {
      vi.advanceTimersByTime(4000);
    });
    expect(agent.send).toHaveBeenCalledTimes(2);
  });

  it("does not poll with a single tab", async () => {
    useIntervalFakes();
    const agent = makeAgent({ connected: true, conversations: [convo("a")] });
    const initial = state([tab("a", "A")], 0);

    await renderHook(agent, initial);
    agent.send.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(12_000); // three intervals
    });
    expect(agent.send).not.toHaveBeenCalledWith({ type: "request_conversations" });
  });

  it("does not poll while disconnected", async () => {
    useIntervalFakes();
    const agent = makeAgent({
      connected: false,
      conversations: [convo("a"), convo("b")],
    });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);

    await renderHook(agent, initial);
    agent.send.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(12_000);
    });
    expect(agent.send).not.toHaveBeenCalledWith({ type: "request_conversations" });
  });
});

// ─── keyboard shortcuts ──────────────────────────────────────────────────

describe("useConvoTabs — keyboard shortcuts", () => {
  const initial = state([tab("a", "A"), tab("b", "B"), tab("c", "C")], 0);

  it("Ctrl+T opens a new tab and preventDefaults", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    const e = key({ key: "t", code: "KeyT", ctrlKey: true });

    expect(e.defaultPrevented).toBe(true);
    expect(agent.switchConvo).toHaveBeenCalledWith(null);
    expect(views()).toHaveLength(4);
  });

  it("Cmd+T opens a new tab (macOS)", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    key({ key: "t", code: "KeyT", metaKey: true });

    expect(agent.switchConvo).toHaveBeenCalledWith(null);
  });

  it("Ctrl+W closes the active tab", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    const e = key({ key: "w", code: "KeyW", ctrlKey: true });

    expect(e.defaultPrevented).toBe(true);
    expect(views()).toHaveLength(2);
    expect(views()[0].key).toBe("id-b"); // right neighbour
    expect(agent.switchConvo).toHaveBeenCalledWith("b");
  });

  it("Ctrl+Tab cycles forward and Ctrl+Shift+Tab backward", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    key({ key: "Tab", code: "Tab", ctrlKey: true });
    expect(views()[1].active).toBe(true);

    key({ key: "Tab", code: "Tab", ctrlKey: true, shiftKey: true });
    expect(views()[0].active).toBe(true);
  });

  it("Ctrl+1..9 jumps to a tab (9 = last)", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    key({ key: "3", code: "Digit3", ctrlKey: true });
    expect(views()[2].active).toBe(true);

    key({ key: "9", code: "Digit9", ctrlKey: true });
    expect(views()[2].active).toBe(true); // 3 tabs: 9 → last
  });

  it("Alt combos are ignored (not preventDefaulted, no action)", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, initial);

    const e = key({ key: "t", code: "KeyT", ctrlKey: true, altKey: true });

    expect(e.defaultPrevented).toBe(false);
    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(views()).toHaveLength(3);
  });
});

// ─── bootDesktopTabs ─────────────────────────────────────────────────────

describe("bootDesktopTabs", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", "/");
  });

  it("starts one tab for the URL convo when nothing is stored", () => {
    window.history.replaceState(null, "", "/?convo=abc");
    expect(bootDesktopTabs()).toEqual({
      tabs: [{ id: expect.any(String), convo: "abc", title: "" }],
      active: 0,
    });
    expect(new URLSearchParams(window.location.search).get("convo")).toBe("abc");
  });

  it("activates the stored tab for the URL convo (URL wins for activation)", () => {
    localStorage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({
        v: 1,
        tabs: [
          { convo: "a", title: "A" },
          { convo: "b", title: "B" },
        ],
        active: 0,
      }),
    );
    window.history.replaceState(null, "", "/?convo=b");

    const s = bootDesktopTabs();
    expect(s.active).toBe(1);
    expect(s.tabs).toHaveLength(2);
    // The URL is kept in sync with the active tab.
    expect(new URLSearchParams(window.location.search).get("convo")).toBe("b");
  });

  it("keeps stored state when there is no URL convo and writes the active convo into the URL", () => {
    localStorage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({
        v: 1,
        tabs: [
          { convo: "a", title: "A" },
          { convo: "b", title: "B" },
        ],
        active: 1,
      }),
    );

    const s = bootDesktopTabs();
    expect(s.active).toBe(1);
    expect(new URLSearchParams(window.location.search).get("convo")).toBe("b");
  });

  it("appends and activates the URL convo when it is not in the stored tabs", () => {
    localStorage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({
        v: 1,
        tabs: [{ convo: "a", title: "A" }],
        active: 0,
      }),
    );
    window.history.replaceState(null, "", "/?convo=c");

    const s = bootDesktopTabs();
    expect(s.tabs).toHaveLength(2);
    expect(s.active).toBe(1);
    expect(s.tabs[1]).toEqual({ id: expect.any(String), convo: "c", title: "" });
  });

  it("leaves no ?convo param when the active stored tab has no convo", () => {
    localStorage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({
        v: 1,
        tabs: [
          { convo: "a", title: "A" },
          { convo: null, title: "" },
        ],
        active: 1,
      }),
    );
    // No URL convo (the desktop bridge's re-navigation carries none).

    const s = bootDesktopTabs();
    expect(s.active).toBe(1);
    expect(new URLSearchParams(window.location.search).get("convo")).toBeNull();
  });

  it("a stale URL convo wins over the stored active tab (URL is the binding)", () => {
    localStorage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({
        v: 1,
        tabs: [
          { convo: "a", title: "A" },
          { convo: null, title: "" },
        ],
        active: 1,
      }),
    );
    window.history.replaceState(null, "", "/?convo=fromurl");

    const s = bootDesktopTabs();
    // Appended and activated (design §4: URL wins), URL kept as-is.
    expect(s.tabs).toHaveLength(3);
    expect(s.active).toBe(2);
    expect(s.tabs[2]).toEqual({ id: expect.any(String), convo: "fromurl", title: "" });
    expect(new URLSearchParams(window.location.search).get("convo")).toBe("fromurl");
  });
});

// ─── `state` + stable ids (desktop-tab-composers) ────────────────────────
// `ConvoTabsApi.state` mirrors the store read-only: App maps it to one
// keep-alive <ComposerSlot key={tab.id}> per tab, so the active tab's id must
// not change when the server binds its conversation.

type StateOf = { tabs: { id: string; convo: string | null; title: string }[]; active: number };
const storeState = (): StateOf => (api as { state: StateOf }).state;

describe("useConvoTabs — state mirror and id stability", () => {
  it("exposes the store as `state` (tabs with ids, and the active index)", async () => {
    const agent = makeAgent({ convo: "b" });
    await renderHook(agent, state([tab("a", "A"), tab("b", "B")], 1));

    expect(storeState().active).toBe(1);
    expect(storeState().tabs.map((t) => [t.id, t.convo, t.title])).toEqual([
      ["id-a", "a", "A"],
      ["id-b", "b", "B"],
    ]);
  });

  it("`state` follows user ops (activate / newTab / close)", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, state([tab("a", "A"), tab("b", "B")], 0));

    await act(async () => {
      (api as { activate(i: number): void }).activate(1);
    });
    expect(storeState().active).toBe(1);

    await act(async () => {
      (api as { newTab(): void }).newTab();
    });
    expect(storeState().tabs).toHaveLength(3);
    expect(storeState().active).toBe(2);
    expect(storeState().tabs[2].convo).toBeNull();

    await act(async () => {
      (api as { close(i: number): void }).close(0);
    });
    expect(storeState().tabs.map((t) => t.id)).toEqual(["id-b", storeState().tabs[1].id]);
  });

  it("`state` and `views` describe the same tabs, view.key === tab.id", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, state([tab("a", "A"), tab(null, ""), tab("c", "C")], 0));
    expect(views().map((v) => v.key)).toEqual(storeState().tabs.map((t) => t.id));
  });

  it("the active tab's id is stable when agent.convo goes null -> 'u'", async () => {
    const agent = makeAgent({ convo: null });
    const initial = state([tab("a", "A"), tab(null, "")], 1);
    await renderHook(agent, initial);
    const before = storeState().tabs[1].id;

    agent.convo = "u";
    await rerender(agent, initial);

    expect(storeState().tabs[1]).toMatchObject({ id: before, convo: "u" });
  });

  it("the active tab's id is stable when the server rebinds it X -> Y", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);
    await renderHook(agent, initial);
    const before = storeState().tabs[0].id;

    agent.convo = "zzz";
    await rerender(agent, initial);

    expect(storeState().tabs[0]).toMatchObject({ id: before, convo: "zzz" });
    expect(storeState().tabs[1].id).toBe("id-b");
  });

  it("a merge (server binds the active tab to another open tab's convo) keeps the OTHER tab's id", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B")], 0);
    await renderHook(agent, initial);

    agent.convo = "b";
    await rerender(agent, initial);

    expect(storeState().tabs).toHaveLength(1);
    expect(storeState().tabs[0]).toMatchObject({ id: "id-b", convo: "b" });
  });

  it("closing the last tab mints a NEW id (the old composer slot is not reused)", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, state([tab("a", "A")], 0));

    await act(async () => {
      (api as { close(i: number): void }).close(0);
    });

    expect(storeState().tabs).toHaveLength(1);
    expect(storeState().tabs[0].id).not.toBe("id-a");
  });

  it("`api` is null (no state) in a browser", async () => {
    await renderHook(makeAgent(), null);
    expect(api).toBeNull();
  });

  it("persists {convo,title} only: no `id` key in storage", async () => {
    const agent = makeAgent({ convo: "a" });
    await renderHook(agent, state([tab("a", "A"), tab("b", "B")], 0));
    const raw = JSON.parse(localStorage.getItem("peakbot.desktop.tabs")!);
    for (const t of raw.tabs) expect(Object.keys(t).sort()).toEqual(["convo", "title"]);
  });
});
