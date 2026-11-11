// App-level wiring for the Telegram-style mobile screen slide (design §7
// Step 1: M1–M7, D1, R1, W1). The Chats list and Settings are held mounted
// (and `inert`) while their WAAPI exit plays, then dropped on `finished`.
// Off the mobile shell, without WAAPI, or under prefers-reduced-motion they
// swap instantly, exactly as on master (D1/R1/W1 are regression locks).
//
// Harness: ./App.mobileChats.test.tsx plumbing, copied (not imported) so the
// two files stay independent, plus a controllable Element.prototype.animate.
//
// RED on origin/master: the list/Settings unmount immediately, `animate` is
// never called and `data-testid="settings-view"` does not exist — the
// feature is absent. D1/R1/W1 pass on master by design.

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState, ConversationSummary } from "./state";

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

function makeAgent(state: AppState | null, over: Partial<FakeAgent> = {}): FakeAgent {
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
    error: null,
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

// ─── controllable WAAPI mock ─────────────────────────────────────────────

interface FakeAnim {
  el: Element;
  keyframes: Keyframe[];
  options: KeyframeAnimationOptions;
  finished: Promise<void>;
  cancelled: boolean;
  cancel(): void;
  finish(): void;
}

let anims: FakeAnim[] = [];

function installWAAPI(): void {
  // A `function`, not an arrow: it records `this`, the animated element.
  (
    Element.prototype as unknown as {
      animate: (kf: Keyframe[], opts: KeyframeAnimationOptions) => FakeAnim;
    }
  ).animate = function (
    this: Element,
    keyframes: Keyframe[],
    options: KeyframeAnimationOptions,
  ) {
    let resolve: () => void;
    let reject: (e: unknown) => void;
    const finished = new Promise<void>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    // Pre-mark handled so a cancel() never surfaces as an unhandled rejection.
    void finished.catch(() => {});
    const anim: FakeAnim = {
      el: this,
      keyframes,
      options,
      finished,
      cancelled: false,
      cancel() {
        if (anim.cancelled) return;
        anim.cancelled = true;
        reject(new Error("AbortError"));
      },
      finish() {
        resolve();
      },
    };
    anims.push(anim);
    return anim;
  };
}

function removeWAAPI(): void {
  delete (Element.prototype as { animate?: unknown }).animate;
}

const EASING = "cubic-bezier(0.215, 0.61, 0.355, 1)";
const HOME = "translateX(0px)";
const EXIT_OPTS = { duration: 180, easing: EASING, fill: "forwards" };
const ENTER_OPTS = { duration: 180, easing: EASING };
const exitTo = (px: number) => [
  { opacity: 1, transform: HOME },
  { opacity: 0, transform: `translateX(${px}px)` },
];
const enterFrom = (px: number) => [
  { opacity: 0, transform: `translateX(${px}px)` },
  { opacity: 1, transform: HOME },
];

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

function click(el: Element): void {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
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
 *  starts from a quiescent arbiter. Bounded so a stuck traversal can't hang. */
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

/** A real system Back (popstate), as the Android Back button produces. */
async function systemBack(): Promise<void> {
  const popped = waitForPopstate();
  window.history.back();
  await act(async () => {
    await popped;
  });
  await flush();
}

// ─── helpers (design §7) ─────────────────────────────────────────────────

let reduce = false;

const list = (): HTMLElement | null => host.querySelector('[data-testid="chat-list"]');
const settings = (): HTMLElement | null => host.querySelector('[data-testid="settings-view"]');
const animsOn = (el: Element | null): FakeAnim[] => anims.filter((a) => a.el === el);
/** D1/R1/W1 locate Settings by this button, so they hold on master too. */
const settingsBack = (): HTMLButtonElement | null =>
  [...host.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes("← Back to chat"),
  ) ?? null;

beforeEach(() => {
  anims = [];
  reduce = false;
  setShell("mobile");
  vi.stubGlobal(
    "matchMedia",
    (media: string) =>
      ({
        get matches() {
          return media === "(prefers-reduced-motion: reduce)" && reduce;
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
  // jsdom has no ResizeObserver; the Transcript observes its scroll section.
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
  removeWAAPI();
  await settlePopstates(inFlight);
  setShell("browser");
  window.history.replaceState(null, "", "/");
});

// ─── shared flows ────────────────────────────────────────────────────────

/** Mount on the conversation with a second chat in the list, then show the list. */
async function listWithC2(): Promise<FakeAgent> {
  const fake = makeAgent(stateFor("c1"));
  hoisted.agent = fake;
  mount();
  await flush();
  // `loaded` flips on the first NEW list after mount, so deliver one.
  fake.conversations = [convoRow("c2", "home")];
  await toList();
  return fake;
}

const openC2 = (): Element | null =>
  host.querySelector('[data-testid="chat-row"][data-convo="c2"] button[data-testid="chat-open"]');

/** From the list, tap the gear: Settings opens over the list. */
async function openSettingsFromList(): Promise<void> {
  const gear = host.querySelector('button[aria-label="Settings"]');
  expect(gear, "the gear renders on the list").not.toBeNull();
  click(gear!);
  await flush();
}

// ─── M1–M7: motion allowed (mobile + WAAPI) ──────────────────────────────

describe("App mobile screen slide: motion (M1–M7)", () => {
  it("M1: opening a chat keeps the list mounted and inert until the exit finishes", async () => {
    installWAAPI();
    const fake = await listWithC2();
    const pushSpy = vi.spyOn(window.history, "pushState");
    const listEl = list();
    expect(listEl, "the list shows").not.toBeNull();

    click(openC2()!);
    await flush();

    expect(fake.switchConvo).toHaveBeenCalledWith("c2", "home");
    expect(list(), "the list is HELD while exiting").toBe(listEl);
    expect(list()!.hasAttribute("inert"), "the held list is inert").toBe(true);
    const exit = animsOn(list()).at(-1)!;
    expect(exit, "an exit animation ran on the list").toBeDefined();
    expect(exit.keyframes).toEqual(exitTo(-48));
    expect(exit.options).toEqual(EXIT_OPTS);
    expect(
      host.querySelector("textarea")!.closest("[inert]"),
      "the conversation is live during the exit",
    ).toBeNull();
    expect(pushSpy, "backStack parity: one pushState").toHaveBeenCalledTimes(1);

    await act(async () => {
      exit.finish();
    });
    expect(list(), "the list unmounts when the exit finishes").toBeNull();
  });

  it("M2: the FAB also plays the list exit", async () => {
    installWAAPI();
    const fake = await listWithC2();

    click(host.querySelector('button[aria-label="New chat"]')!);
    await flush();

    expect(fake.switchConvo).toHaveBeenCalledWith(null, null);
    expect(list(), "the list is held").not.toBeNull();
    expect(list()!.hasAttribute("inert")).toBe(true);
    const exit = animsOn(list()).at(-1)!;
    expect(exit.keyframes).toEqual(exitTo(-48));
    await act(async () => {
      exit.finish();
    });
    expect(list(), "the list unmounts when the exit finishes").toBeNull();
  });

  it('M3: header "‹ Chats" plays the list enter', async () => {
    installWAAPI();
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();

    await toList();

    expect(list(), "the list shows").not.toBeNull();
    expect(list()!.hasAttribute("inert"), "the live list is not inert").toBe(false);
    const on = animsOn(list());
    expect(on, "exactly one animation on the list").toHaveLength(1);
    expect(on[0].keyframes).toEqual(enterFrom(-48));
    expect(on[0].options, "enter has no fill").toEqual(ENTER_OPTS);
  });

  it("M4: Android Back (popstate) plays the list enter", async () => {
    installWAAPI();
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    const backSpy = vi.spyOn(window.history, "back");

    await systemBack();

    expect(list(), "the list shows").not.toBeNull();
    expect(list()!.hasAttribute("inert")).toBe(false);
    const on = animsOn(list());
    expect(on).toHaveLength(1);
    expect(on[0].keyframes).toEqual(enterFrom(-48));
    expect(on[0].options).toEqual(ENTER_OPTS);
    expect(backSpy, "only the test's own Back traversed").toHaveBeenCalledTimes(1);
  });

  it("M5: Settings opens with an enter from the right", async () => {
    installWAAPI();
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toList();

    await openSettingsFromList();

    expect(settings(), "Settings mounts with data-testid=settings-view").not.toBeNull();
    expect(settings()!.hasAttribute("inert")).toBe(false);
    const enter = animsOn(settings()).at(-1);
    expect(enter, "an enter animation ran on Settings").toBeDefined();
    expect(enter!.keyframes).toEqual(enterFrom(48));
    expect(enter!.options).toEqual(ENTER_OPTS);
  });

  it('M6: Settings "← Back to chat" holds, exits to the right, then unmounts', async () => {
    installWAAPI();
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toList();
    await openSettingsFromList();
    const settingsEl = settings();
    expect(settingsEl, "Settings is open").not.toBeNull();

    click(settingsBack()!);
    await flush();

    expect(settings(), "Settings is HELD while exiting").toBe(settingsEl);
    expect(settings()!.hasAttribute("inert"), "the held Settings is inert").toBe(true);
    const exit = animsOn(settings()).at(-1)!;
    expect(exit.keyframes).toEqual(exitTo(48));
    expect(exit.options).toEqual(EXIT_OPTS);

    await act(async () => {
      exit.finish();
    });
    expect(settings(), "Settings unmounts when the exit finishes").toBeNull();
    expect(list(), "the list is still there").not.toBeNull();
  });

  it("M7: Settings system Back holds, exits to the right, then unmounts", async () => {
    installWAAPI();
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toList();
    await openSettingsFromList();
    const settingsEl = settings();
    expect(settingsEl, "Settings is open").not.toBeNull();

    await systemBack();

    expect(settings(), "Settings is HELD while exiting").toBe(settingsEl);
    expect(settings()!.hasAttribute("inert")).toBe(true);
    const exit = animsOn(settings()).at(-1)!;
    expect(exit.keyframes).toEqual(exitTo(48));
    expect(exit.options).toEqual(EXIT_OPTS);

    await act(async () => {
      exit.finish();
    });
    expect(settings(), "Settings unmounts when the exit finishes").toBeNull();
    expect(list(), "the list stays").not.toBeNull();
  });
});

// ─── D1 / R1 / W1: instant swaps (regression locks, green on master) ─────

describe("App screen slide: instant swaps (D1, R1, W1)", () => {
  it.each(["desktop", "browser"] as const)(
    "D1: %s Settings: no animation, instant unmount",
    async (marker) => {
      installWAAPI();
      setShell(marker);
      hoisted.agent = makeAgent(stateFor("c1"));
      mount();
      await flush();

      act(() => {
        window.dispatchEvent(
          new KeyboardEvent("keydown", { key: ",", ctrlKey: true, cancelable: true }),
        );
      });
      await flush();
      expect(settingsBack(), "Settings opens on Ctrl+,").not.toBeNull();

      click(settingsBack()!);
      expect(settingsBack(), "Settings is gone in the same act, no finish()").toBeNull();
      expect(anims, "animate was never called").toEqual([]);
    },
  );

  /** The list → chat → list → Settings → close flow, asserting every swap is immediate. */
  async function instantFlow(): Promise<void> {
    const fake = await listWithC2();
    expect(list(), "the list shows").not.toBeNull();

    click(openC2()!);
    await flush();
    expect(fake.switchConvo).toHaveBeenCalledWith("c2", "home");
    expect(list(), "the list is gone immediately").toBeNull();

    await toList();
    expect(list(), "the list is back").not.toBeNull();
    expect(list()!.hasAttribute("inert"), "no inert on the live list").toBe(false);

    await openSettingsFromList();
    expect(settingsBack(), "Settings opens").not.toBeNull();

    click(settingsBack()!);
    expect(settingsBack(), "Settings is gone immediately").toBeNull();
    expect(settings(), "no held settings-view").toBeNull();
    expect(list(), "the list is still there").not.toBeNull();
  }

  it("R1: reduced motion: list and Settings swap instantly, animate never called", async () => {
    installWAAPI();
    reduce = true;

    await instantFlow();

    expect(anims, "animate was never called").toEqual([]);
  });

  it("W1: no WAAPI: list and Settings swap instantly", async () => {
    // No installWAAPI(): jsdom's default, `Element.prototype.animate` undefined.
    await instantFlow();

    expect(host.querySelector("[inert] [data-testid='chat-list']")).toBeNull();
    expect(anims).toEqual([]);
  });
});
