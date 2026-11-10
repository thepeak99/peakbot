// @vitest-environment jsdom
//
// Tests for `useMobileChats.ts` (design §3.4/§4): the mobile Chats glue hook.
//
//   export function useMobileChats(agent, onNotice): MobileChatsApi | null;
//   // null iff !isMobile(); then screen state + the back layer, list polling,
//   // the activity fold, the last-chat save, the refused-resume fallback, and
//   // open / newChat / showChats.
//
// The hook is mounted through a harness component (useConvoTabs.test.tsx
// conventions: raw createRoot + act, a stub agent). A SECOND `useBackDismiss`
// in the harness stands in for the Settings overlay's own layer, so N10–N12
// exercise the real LIFO nesting the app gets when Settings opens over the
// Chats screens.
//
// RED before implementation: `./useMobileChats` (and `./mobileStorage`) do
// not exist yet, so this file fails at import — the right reason (feature
// absent). The stub agent is written against the POST-implementation
// `AgentConnection`, cast through `unknown`.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useMobileChats } from "./useMobileChats";
import { useBackDismiss } from "./useBackDismiss";
import { bootMobileChats, LAST_CHAT_KEY } from "./mobileStorage";
import type { AgentConnection } from "./useAgent";
import type { ConversationSummary } from "./state";

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
  send: ReturnType<typeof vi.fn>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn>;
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

const convoRow = (
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
  }) as ConversationSummary;

// ─── harness ───────────────────────────────────────────────────────────────

type Api = {
  screen: "chats" | "conversation";
  loaded: boolean;
  unread: ReadonlySet<string>;
  backBadge: boolean;
  open(convo: string, profile: string | null): void;
  newChat(): void;
  showChats(): void;
};

let api: unknown;
// The Settings overlay's close — a stable fn, as `useBackDismiss` requires.
const settingsClose = vi.fn();
let container: HTMLDivElement | null = null;
let root: Root | null = null;

function Harness({
  agent,
  onNotice,
  settingsOpen = false,
}: {
  agent: StubAgent;
  onNotice: (message: string) => void;
  settingsOpen?: boolean;
}) {
  api = useMobileChats(agent as unknown as AgentConnection, onNotice);
  // The Settings layer the app opens over the Chats screens (App: useBackDismiss).
  useBackDismiss(settingsOpen, settingsClose);
  return null;
}

async function renderHook(
  agent: StubAgent,
  onNotice: (message: string) => void,
  over: { settingsOpen?: boolean } = {},
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
    root.render(<Harness agent={agent} onNotice={onNotice} settingsOpen={over.settingsOpen} />);
  });
}

async function rerender(
  agent: StubAgent,
  onNotice: (message: string) => void,
  over: { settingsOpen?: boolean } = {},
): Promise<void> {
  await act(async () => {
    root!.render(<Harness agent={agent} onNotice={onNotice} settingsOpen={over.settingsOpen} />);
  });
}

const A = (): Api => api as Api;

/** The OS Back / edge-swipe: a popstate is what the app reacts to. */
const pop = () => act(() => window.dispatchEvent(new PopStateEvent("popstate")));

/** Await the popstate that a `history.back()` produces (jsdom: one task). */
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

/** UI "‹ Chats": `showChats()` releases the marker with one back-traversal. */
async function toChats(): Promise<void> {
  await act(async () => {
    A().showChats();
  });
  const popped = waitForPopstate();
  await act(async () => {
    await popped;
  });
}

function setShell(marker: "desktop" | "mobile" | "browser") {
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  if (marker !== "browser") (globalThis as { peakbotShell?: unknown }).peakbotShell = marker;
}

/** Fake only the timer APIs the polling interval uses. */
function useIntervalFakes(): void {
  vi.useFakeTimers({
    toFake: ["setInterval", "clearInterval", "setTimeout", "clearTimeout"],
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  setShell("mobile");
  localStorage.clear();
  window.history.replaceState(null, "", "/");
  settingsClose.mockClear();
});

afterEach(async () => {
  const backSpy = vi.spyOn(window.history, "back");
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  let inFlight = backSpy.mock.calls.length;
  if (inFlight > 0 && vi.isFakeTimers()) {
    // The traversal is queued on the fake clock and the restore below would
    // discard it: land it now.
    let landed = 0;
    const onPop = () => {
      landed += 1;
    };
    window.addEventListener("popstate", onPop);
    vi.advanceTimersByTime(16);
    window.removeEventListener("popstate", onPop);
    inFlight = Math.max(0, inFlight - landed);
  }
  backSpy.mockRestore();
  vi.useRealTimers();
  vi.restoreAllMocks();
  await settlePopstates(inFlight);
  delete (document as unknown as { hidden?: boolean }).hidden;
  setShell("browser");
  window.history.replaceState(null, "", "/");
});

// ─── N: the navigation state machine ──────────────────────────────────────

describe("useMobileChats — navigation (N1–N13)", () => {
  it("N1: launch on / with nothing stored mounts on the conversation, no agent calls", async () => {
    const agent = makeAgent({ convo: null });
    const pushSpy = vi.spyOn(window.history, "pushState");

    await renderHook(agent, vi.fn());

    expect(api).not.toBeNull();
    expect(A().screen).toBe("conversation");
    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(pushSpy, "the launch marker is pushed once").toHaveBeenCalledTimes(1);
  });

  it("N2: a stored last chat seeds the URL, then the marker is pushed", async () => {
    window.localStorage.setItem(
      LAST_CHAT_KEY,
      JSON.stringify({ v: 1, convo: "c9", profile: "home" }),
    );
    window.history.replaceState(null, "", "/");
    bootMobileChats();
    expect(window.location.search).toBe("?convo=c9&profile=home");

    const pushSpy = vi.spyOn(window.history, "pushState");
    const agent = makeAgent({ convo: "c9" });
    await renderHook(agent, vi.fn());

    expect(A().screen).toBe("conversation");
    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("N3: a deep link keeps the URL; only the marker is pushed", async () => {
    window.localStorage.setItem(
      LAST_CHAT_KEY,
      JSON.stringify({ v: 1, convo: "c9", profile: null }),
    );
    window.history.replaceState(null, "", "/?convo=c2");
    bootMobileChats();
    expect(window.location.search).toBe("?convo=c2");

    const pushSpy = vi.spyOn(window.history, "pushState");
    const agent = makeAgent({ convo: "c2" });
    await renderHook(agent, vi.fn());

    expect(A().screen).toBe("conversation");
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("N4: '‹ Chats' (showChats) releases the marker with one back()", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());

    const backSpy = vi.spyOn(window.history, "back");
    const pushSpy = vi.spyOn(window.history, "pushState");
    agent.send.mockClear();
    await toChats();

    expect(A().screen).toBe("chats");
    expect(agent.send).toHaveBeenCalledTimes(1);
    expect(agent.send).toHaveBeenCalledWith({ type: "request_conversations" });
    expect(backSpy, "the UI close issues exactly one back-traversal").toHaveBeenCalledTimes(1);
    expect(pushSpy).not.toHaveBeenCalled();
  });

  it("N5: an OS Back (popstate) goes to the list without a back() of its own", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());

    const backSpy = vi.spyOn(window.history, "back");
    const pushSpy = vi.spyOn(window.history, "pushState");
    agent.send.mockClear();
    pop();

    expect(A().screen).toBe("chats");
    expect(agent.send).toHaveBeenCalledTimes(1);
    expect(agent.send).toHaveBeenCalledWith({ type: "request_conversations" });
    expect(backSpy).not.toHaveBeenCalled();
    expect(pushSpy).not.toHaveBeenCalled();
  });

  it("N6: open() of another chat re-attaches and pushes a marker", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());
    await toChats();

    const pushSpy = vi.spyOn(window.history, "pushState");
    await act(async () => {
      A().open("c2", "home");
    });

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("c2", "home");
    expect(A().screen).toBe("conversation");
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("N7: open() of the already-bound chat does not re-attach", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());
    await toChats();

    const pushSpy = vi.spyOn(window.history, "pushState");
    await act(async () => {
      A().open("c1", "dev");
    });

    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(A().screen).toBe("conversation");
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("N8: newChat() re-attaches to null and pushes a marker", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());
    await toChats();

    const pushSpy = vi.spyOn(window.history, "pushState");
    await act(async () => {
      A().newChat();
    });

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith(null, null);
    expect(A().screen).toBe("conversation");
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("N9: an OS Back on the list issues no history call; the popstate is ignored", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn());
    await toChats();

    const backSpy = vi.spyOn(window.history, "back");
    const goSpy = vi.spyOn(window.history, "go");
    const pushSpy = vi.spyOn(window.history, "pushState");
    pop();

    expect(backSpy).not.toHaveBeenCalled();
    expect(goSpy).not.toHaveBeenCalled();
    expect(pushSpy).not.toHaveBeenCalled();
    expect(A().screen, "a stray popstate leaves the list").toBe("chats");
  });

  it("N10: an OS Back with Settings over the list closes only Settings", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn(), { settingsOpen: false });
    await toChats();
    await rerender(agent, vi.fn(), { settingsOpen: true });

    const backSpy = vi.spyOn(window.history, "back");
    pop();

    expect(settingsClose).toHaveBeenCalledTimes(1);
    expect(A().screen).toBe("chats");
    expect(backSpy).not.toHaveBeenCalled();
  });

  it("N11: an OS Back with Settings over the conversation closes Settings; a second Back lists", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn(), { settingsOpen: true });

    const backSpy = vi.spyOn(window.history, "back");
    pop();
    expect(settingsClose).toHaveBeenCalledTimes(1);
    expect(A().screen).toBe("conversation");

    pop();
    expect(A().screen).toBe("chats");
    expect(backSpy).not.toHaveBeenCalled();
  });

  it("N12: the Settings UI close consumes its entry; the popstate changes nothing", async () => {
    const agent = makeAgent({ convo: "c1" });
    await renderHook(agent, vi.fn(), { settingsOpen: true });

    const backSpy = vi.spyOn(window.history, "back");
    await rerender(agent, vi.fn(), { settingsOpen: false });
    expect(backSpy, "the UI close issues exactly one back-traversal").toHaveBeenCalledTimes(1);

    const popped = waitForPopstate();
    await act(async () => {
      await popped;
    });
    expect(A().screen, "the consumed marker's popstate is a no-op").toBe("conversation");
  });

  it.each(["browser", "desktop"] as const)(
    "N13: the %s shell gets a null hook and no history traffic",
    async (marker) => {
      setShell(marker);
      const agent = makeAgent({ convo: null });
      const pushSpy = vi.spyOn(window.history, "pushState");

      await renderHook(agent, vi.fn());

      expect(api).toBeNull();
      expect(pushSpy).not.toHaveBeenCalled();
      expect(agent.send).not.toHaveBeenCalled();
    },
  );
});

// ─── P: polling, fold, save, refused-resume ───────────────────────────────

describe("useMobileChats — polling and activity (P1–P6)", () => {
  it("P1: the connect edge sends once, then every 4 s while connected", async () => {
    useIntervalFakes();
    const agent = makeAgent({ connected: false, convo: null });
    await renderHook(agent, vi.fn());

    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    expect(agent.send, "no polling while disconnected").not.toHaveBeenCalledWith({
      type: "request_conversations",
    });

    agent.send.mockClear();
    agent.connected = true;
    await rerender(agent, vi.fn());
    expect(agent.send, "the connect edge sends once").toHaveBeenCalledTimes(1);
    expect(agent.send).toHaveBeenCalledWith({ type: "request_conversations" });
    agent.send.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(4000);
    });
    expect(agent.send).toHaveBeenCalledTimes(1);
    await act(async () => {
      vi.advanceTimersByTime(4000);
    });
    expect(agent.send).toHaveBeenCalledTimes(2);
  });

  it("P2: a hidden page sends nothing on a tick", async () => {
    useIntervalFakes();
    const agent = makeAgent({ connected: true, convo: null });
    await renderHook(agent, vi.fn());
    agent.send.mockClear();

    Object.defineProperty(document, "hidden", { value: true, configurable: true });
    await act(async () => {
      vi.advanceTimersByTime(8000);
    });
    expect(agent.send).not.toHaveBeenCalledWith({ type: "request_conversations" });
  });

  it("P3: each new list is folded against the viewed chat; loaded flips true", async () => {
    const agent = makeAgent({ connected: true, convo: "c1" });
    await renderHook(agent, vi.fn());
    expect(A().loaded).toBe(false);

    agent.conversations = [convoRow("c1"), convoRow("c2", { running: true })];
    await rerender(agent, vi.fn());
    expect(A().loaded).toBe(true);
    // c2 is running but was not running before → no unread edge yet.
    expect([...A().unread]).toEqual([]);
    // c2 is running and unbound → the back button shows a badge.
    expect(A().backBadge).toBe(true);

    // c2's turn finishes while we view c1 → unread.
    agent.conversations = [convoRow("c1"), convoRow("c2", { running: false })];
    await rerender(agent, vi.fn());
    expect([...A().unread]).toEqual(["c2"]);
  });

  it("P4: a bound convo is saved with the URL profile; null saves nothing", async () => {
    window.history.replaceState(null, "", "/?profile=home");
    const agent = makeAgent({ connected: true, convo: null });
    await renderHook(agent, vi.fn());
    expect(window.localStorage.getItem(LAST_CHAT_KEY), "null saves nothing").toBeNull();

    agent.convo = "c9";
    await rerender(agent, vi.fn());
    expect(JSON.parse(window.localStorage.getItem(LAST_CHAT_KEY)!)).toEqual({
      v: 1,
      convo: "c9",
      profile: "home",
    });
  });

  it("P5: a refused resume (no state, an error, a bound convo) falls back once", async () => {
    const onNotice = vi.fn();
    const agent = makeAgent({
      connected: true,
      state: null,
      error: "remote down",
      convo: "c9",
    });
    await renderHook(agent, onNotice);

    expect(onNotice).toHaveBeenCalledTimes(1);
    expect(onNotice).toHaveBeenCalledWith("Couldn't reopen your last chat — remote down");
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith(null, null);

    // The re-attach clears the binding: the fallback must not fire again.
    agent.convo = null;
    await rerender(agent, onNotice);
    expect(onNotice).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
  });

  it("P6: the fallback is skipped when the convo is null or a state exists", async () => {
    const onNoticeA = vi.fn();
    const agentA = makeAgent({ connected: true, state: null, error: "remote down", convo: null });
    await renderHook(agentA, onNoticeA);
    expect(onNoticeA).not.toHaveBeenCalled();
    expect(agentA.switchConvo).not.toHaveBeenCalled();

    const onNoticeB = vi.fn();
    const agentB = makeAgent({
      connected: true,
      state: { is_running: false },
      error: "remote down",
      convo: "c9",
    });
    await renderHook(agentB, onNoticeB);
    expect(onNoticeB).not.toHaveBeenCalled();
    expect(agentB.switchConvo).not.toHaveBeenCalled();
  });
});
