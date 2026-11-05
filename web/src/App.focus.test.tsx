// @vitest-environment jsdom
// Focus mode + command_running (meter-and-focus ticket, Bug 2).
//
// While a slash command runs (e.g. /compact) the backend sets is_running
// WITHOUT adding a user row — the trailing turn is finished. The App must
// compute `turnInFlight = isRunning && !commandRunning` and pass THAT to
// focusView (and to the ActivityRow gate) — today it passes is_running
// straight through, so the last answer vanishes from the transcript (shown
// only as the activity row) until the command finishes.
//
// Focus mode is ON by default (readFocusPref(null) === true) and
// localStorage is cleared in beforeEach — no preference wiring needed.
//
// Harness mirrors App.tabs.test.tsx (same useAgent/uploads mocks).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState, InboundMessage, WireChatMessage } from "./state";

// ─── mocks (same shape as App.tabs.test.tsx) ──────────────────────────────

const hoisted = vi.hoisted(() => ({ agent: null as unknown }));

// `useConvoTabs` also imports `setConvoInUrl` from "./useAgent".
vi.mock("./useAgent", () => ({
  useAgent: () => hoisted.agent,
  setConvoInUrl: vi.fn(),
}));
vi.mock("./uploads", () => ({ uploadFile: vi.fn() }));

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fake agent ────────────────────────────────────────────────────────────

interface FakeAgent {
  connected: boolean;
  state: AppState | null;
  models: unknown[];
  activeAlias: string;
  conversations: unknown[];
  conversationsError: string | null;
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn<(msg: InboundMessage) => void>>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn<(id: string | null) => void>>;
}

const T = "2026-01-01T00:00:00Z";
const userMsg = (content: string): WireChatMessage => ({
  role: "user",
  content,
  timestamp: T,
});
const agentMsg = (content: string): WireChatMessage => ({
  role: "agent",
  content,
  timestamp: T,
});

/** A minimal, valid-enough `AppState` frame for conversation "c1". */
function stateFor(over: Partial<AppState> = {}): AppState {
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
    conversation: { id: "c1" },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

function makeAgent(over: Partial<AppState> = {}): FakeAgent {
  return {
    connected: true,
    state: stateFor(over),
    models: [],
    activeAlias: "",
    conversations: [],
    conversationsError: null,
    commands: [],
    dirListing: null,
    recentDirs: [],
    error: null,
    send: vi.fn(),
    convo: "c1",
    switchConvo: vi.fn(),
  };
}

// ─── render harness ────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;
let originalMatchMedia: typeof window.matchMedia | undefined;
let offsetHeightDesc: PropertyDescriptor | undefined;

beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  window.matchMedia = ((media: string) =>
    ({
      matches: false,
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList) as typeof window.matchMedia;

  // jsdom has no ResizeObserver (Transcript observes its scroll section).
  (window as unknown as { ResizeObserver?: unknown }).ResizeObserver ??= class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };

  // Give the virtualizer a non-zero viewport: jsdom reports offsetHeight 0,
  // so @tanstack/virtual would render zero rows and no transcript content
  // would reach the DOM. 400px fits both fixture rows (est. 34px each).
  offsetHeightDesc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get() {
      return 400;
    },
  });

  // Focus mode defaults ON: "peakbot-focus" absent → on.
  localStorage.clear();
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  hoisted.agent = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
  if (offsetHeightDesc) {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", offsetHeightDesc);
  }
});

async function mountApp(over: Partial<AppState> = {}): Promise<void> {
  const agent = makeAgent(over);
  hoisted.agent = agent;
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<App />);
  });
}

/** The transcript's scrolling <section> — the one that owns the
 * `[data-index]` rows (the activity row sits OUTSIDE it). */
function transcriptSection(container: HTMLElement): HTMLElement | null {
  for (const row of container.querySelectorAll("[data-index]")) {
    let p: Element | null = row.parentElement;
    while (p && p.tagName.toLowerCase() !== "section") p = p.parentElement;
    if (p) return p as HTMLElement;
  }
  return null;
}

// ─── tests ─────────────────────────────────────────────────────────────────

describe("App focus mode with a running command (command_running)", () => {
  it("shows the last answer in the transcript while a command runs (RED today)", async () => {
    // A finished turn: user "q" → agent answer. /compact is running:
    // is_running true, command_running true, NO new user row.
    await mountApp({
      chat: {
        messages: [userMsg("q"), agentMsg("THE LAST ANSWER")],
        auto_scroll: true,
        scroll_offset: 0,
      },
      is_running: true,
      command_running: true,
    });

    const section = transcriptSection(container!);
    expect(section, "transcript section with rows must be rendered").not.toBeNull();
    expect(
      section!.textContent,
      "the finished answer must stay visible in the transcript while /compact runs — " +
        "focusView must receive turnInFlight = is_running && !command_running (false here)",
    ).toContain("THE LAST ANSWER");
  });

  it("hides the finished answer from the transcript when is_running WITHOUT command_running (guard, GREEN today)", async () => {
    // A real in-flight turn: focus mode keeps only the user row; the answer
    // exists today only as the activity row. A command_running fix must not
    // change this.
    await mountApp({
      chat: {
        messages: [userMsg("q"), agentMsg("THE LAST ANSWER")],
        auto_scroll: true,
        scroll_offset: 0,
      },
      is_running: true,
    });

    const section = transcriptSection(container!);
    expect(section, "transcript section with rows must be rendered").not.toBeNull();
    expect(
      section!.textContent,
      "an in-flight turn must keep the answer out of the transcript",
    ).not.toContain("THE LAST ANSWER");
    // Characterization: today the answer surfaces only as the activity row.
    const activity = container!.querySelector("[role='status']");
    expect(activity, "the activity row must be rendered for a running turn").not.toBeNull();
    expect(activity!.textContent).toContain("THE LAST ANSWER");
  });
});