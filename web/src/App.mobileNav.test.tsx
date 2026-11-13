// App-level mobile behaviour: the refused-first-attach UX and the
// full-screen Settings overlay's Back handling.
//
// ── The refused first attach (GREEN lock) ────────────────────────────────
// With a relay-only config the first attach may be refused (remote down,
// E-VERSION, E-TLS-PIN): the server sends ONE `error` frame and no `state`
// frame. The SPA must show that error (the red bar) and keep the Settings
// gear reachable — the user's only way to fix the broken remote profile.
// This already holds today: `error` frames render in the red bar, and the
// gear gate (`settingsGearEnabled`) treats the absent state fields as
// available. These tests lock it.
//
// ── The Settings overlay and Back (RED) ──────────────────────────────────
// Device testing found Settings is NOT a history entry (the SPA never
// pushStates), so the OS Back in Settings exits the app at the SPA root.
// Contract (mobile shell; applying it in every shell is acceptable):
//   * opening Settings pushes a history entry FROM THE CLICK HANDLER
//     (the gear);
//   * a `popstate` while the overlay is open CLOSES it — the app stays
//     (the chat and its socket remain mounted underneath);
//   * closing via the UI ("← Back to chat") CONSUMES the pushed entry
//     (history returns to the baseline) — no dangling entry, no double
//     Back.
//
// Harness: ./App.tabs.test.tsx conventions (mocked ./useAgent, raw DOM).

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState } from "./state";

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
  conversations: unknown[];
  conversationsError: string | null;
  machines: { id: string; online: boolean }[];
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
    machines: [],
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

// ─── mount plumbing ──────────────────────────────────────────────────────

let root: Root | null = null;
let host: HTMLElement;

function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<App />));
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

const GEAR = 'button[aria-label="Settings"]';
/** The Settings overlay's "← Back to chat" control (its root marker). */
const SETTINGS_MARKER = "← Back to chat";

function settingsOpen(): boolean {
  return (host.textContent ?? "").includes(SETTINGS_MARKER);
}

/** Navigate from the conversation to the Chats list (the gear now lives there). */
async function toChatsScreen(): Promise<void> {
  const back = host.querySelector<HTMLElement>('[data-testid="chat-back"]');
  expect(back, "chat-back must render on the conversation").not.toBeNull();
  const popped = waitForPopstate();
  act(() => {
    back!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
  await act(async () => {
    await popped;
  });
  await flush();
}

/** Click the Settings gear (now on the Chats screen). */
function clickGear(): void {
  const gear = host.querySelector<HTMLElement>(GEAR);
  expect(gear, "the Settings gear must render on the list").not.toBeNull();
  act(() => {
    gear!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Open Settings: go to the Chats screen first, then click the gear. */
async function openSettings(): Promise<void> {
  await toChatsScreen();
  clickGear();
}

/** Close the overlay via its own UI ("← Back to chat"). */
function closeSettingsViaUi() {
  const back = [...host.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(SETTINGS_MARKER),
  );
  expect(back, "the overlay's Back-to-chat control must render").toBeDefined();
  act(() => {
    back!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Await the popstate that a history.back() produces (jsdom: one task). */
function waitForPopstate(): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(resolve, 250); // jsdom delivers it in a task
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

beforeEach(() => {
  setShell("mobile");
  // jsdom has no matchMedia; useMediaQuery (drawer, composer) needs it.
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
  // No server in tests: the Settings shell's fetches settle into .catch.
  vi.stubGlobal("fetch", vi.fn(async () => {
    throw new Error("no server in tests");
  }));
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  if (root) {
    act(() => root!.unmount());
    root = null;
  }
  host.remove();
  setShell("browser");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

// ═══ the refused first attach ═══════════════════════════════════════════

const REFUSALS = [
  // Remote down.
  "Remote profile 'dev on host': connection refused (10.2.0.48:7899)",
  // E-VERSION (the strict exact-match rule).
  "Remote profile 'dev on host': protocol version mismatch — this PeakBot is 0.27.0, the server is 0.26.0",
  // E-TLS-PIN (the exact relay string).
  "Remote profile 'dev on host': the server's CA is sha256:a4d3…, but the config pins sha256:4ad3…. Refusing to connect. If the server's CA was rotated, update `ca` in Settings → Profiles.",
] as const;

describe("refused first attach — the error shows and the gear stays reachable", () => {
  it.each(REFUSALS.map((m) => [m.slice(0, 40), m] as const))(
    "an_%s…_error_frame_shows_the_message_and_the_gear",
    async (_label, message) => {
      // The refused attach: the socket is up, the `attach` was refused with
      // an `error` frame, and NO `state` frame ever arrives.
      hoisted.agent = makeAgent(null, message);
      mount();
      await flush();

      // The error is visible (the red bar, verbatim).
      expect(host.textContent).toContain(message);

      // The gear stays reachable: with no state frame, the gate's absent
      // fields read as available, and the user's only fix (Settings →
      // Profiles) must be openable — now from the Chats screen.
      await openSettings();
      await flush();
      expect(settingsOpen(), "the Settings overlay must open from the gear").toBe(true);
    },
  );

  it("the_error_does_not_block_the_gear_even_with_a_state_frame_missing_settings", async () => {
    // Even a state frame that arrives without the settings fields keeps the
    // gear (absent settings_available = available).
    hoisted.agent = makeAgent(stateFor("c1", { settings_available: undefined }), "boom");
    mount();
    await flush();
    expect(host.textContent).toContain("boom");
    await openSettings();
    await flush();
    expect(settingsOpen()).toBe(true);
  });

  it("a refused resume (bound convo, no state) falls back to a fresh chat once", async () => {
    // A stored last chat the server refuses: `state` is null, an `error` is
    // set, and the socket is still bound to the (now unknown) convo. The hook
    // must re-attach to a fresh chat exactly once (P5) and keep the gear up.
    const message = "Remote profile 'dev on host': connection refused (10.2.0.48:7899)";
    hoisted.agent = makeAgent(null, message, { convo: "c9" });
    mount();
    await flush();

    const agent = hoisted.agent as FakeAgent;
    expect(host.textContent).toContain(message);
    expect(agent.switchConvo, "the fallback re-attaches once").toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith(null, null);

    await openSettings();
    await flush();
    expect(settingsOpen()).toBe(true);
  });
});

// ═══ the Settings overlay and Back (mobile shell) ═══════════════════════
// `history.length` never shrinks on back() (browsers and jsdom alike), so
// the entry accounting is done with spies: one pushState per open, and one
// back-traversal per UI close.

describe("Settings overlay — Back navigation (mobile shell)", () => {
  function spyHistory() {
    return {
      push: vi.spyOn(window.history, "pushState"),
      back: vi.spyOn(window.history, "back"),
      go: vi.spyOn(window.history, "go"),
    };
  }
  /** Back-traversals issued by the app itself (back() or go(-1)). */
  const traversals = (h: ReturnType<typeof spyHistory>) =>
    h.back.mock.calls.length + h.go.mock.calls.filter(([n]) => n === -1).length;

  it("opening_settings_pushes_a_history_entry_from_the_click_handler", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toChatsScreen();
    const h = spyHistory(); // after the list navigation: count only the gear
    expect(settingsOpen()).toBe(false);

    clickGear(); // the click handler must do the push
    expect(settingsOpen(), "the overlay opens").toBe(true);
    expect(h.push, "the gear's click handler must push exactly one history entry").toHaveBeenCalledTimes(1);
  });

  it("a_real_back_closes_the_overlay_and_the_app_stays", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await openSettings();
    expect(settingsOpen()).toBe(true);

    // The OS Back: a genuine traversal of the entry the gear pushed.
    const popped = waitForPopstate();
    window.history.back();
    await act(async () => {
      await popped;
    });
    await flush();

    expect(settingsOpen(), "the overlay must close on Back").toBe(false);
    // The app stayed: the gear (on the list) and the composer are still there.
    expect(host.querySelector(GEAR), "the gear is still on the list").not.toBeNull();
    expect(host.querySelector("textarea"), "the composer is still mounted").not.toBeNull();
  });

  it("a_back_close_issues_no_traversal_of_its_own", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await openSettings();
    const h = spyHistory(); // spy AFTER the open: count only the close

    const popped = waitForPopstate();
    window.history.back(); // the user's Back (counted by the spy: 1)
    await act(async () => {
      await popped;
    });
    await flush();

    // Back already consumed the overlay's entry: the app must NOT also
    // traverse history itself (that would be a double Back).
    expect(settingsOpen()).toBe(false);
    expect(traversals(h), "only the user's own Back traversed").toBe(1);
  });

  it("closing_via_the_ui_consumes_the_entry_no_double_back", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await openSettings();
    const h = spyHistory(); // count only the close

    const popped = waitForPopstate();
    closeSettingsViaUi();
    // The UI close must consume the pushed entry with exactly one
    // back-traversal — a dangling entry would let a second Back land on a
    // state the user never opened (or exit the app from a closed overlay).
    await act(async () => {
      await popped;
    });
    await flush();
    expect(settingsOpen(), "the overlay is closed").toBe(false);
    expect(traversals(h), "the UI close issues exactly one back-traversal").toBe(1);
  });

  it("reopening_after_a_ui_close_pushes_a_fresh_entry", async () => {
    hoisted.agent = makeAgent(stateFor("c1"));
    mount();
    await flush();
    await toChatsScreen();
    const h = spyHistory(); // after the list navigation: count only the gear

    clickGear();
    const popped = waitForPopstate();
    closeSettingsViaUi();
    await act(async () => {
      await popped;
    });
    await flush();

    clickGear(); // already on the list: the gear opens a fresh entry
    expect(settingsOpen()).toBe(true);
    expect(h.push, "each open pushes its own entry").toHaveBeenCalledTimes(2);
  });
});
