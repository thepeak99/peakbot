// @vitest-environment jsdom
//
// App-level tests for CONTRACT 5: the App computes
//   busy = isRunning || pendingInput > 0
// (so busy is true when pending_input_count > 0 EVEN WHILE is_running is
// false), passes it to useSessionVerbs, and delegates the composer's onSend
// to the hook's `submit`.
//
// Written FIRST, expected to be RED until App wires `useSessionVerbs`: today
// App's `onSend` calls `agent.send` directly and never calls `submit`, so the
// delegation assertion fails for the RIGHT reason (feature absent). The busy
// derivation assertions fail because `useSessionVerbs` is not called at all.
//
// Harness: the REAL App tree (web build, initialTabs = null) with
//   - `./useAgent` mocked by a controllable fake agent (a `state` frame is
//     what drives is_running / pending_input_count),
//   - `./uploads` mocked (ComposerSlot → useAttachments),
//   - `./useSessionVerbs` mocked by a spy that records the args it is called
//     with (so we can assert the derived `busy`) and returns {submit,
//     switchCwd} spies (so we can assert onSend → submit delegation).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { App } from "./App";
import type { AppState } from "./state";

// ─── mocks ───────────────────────────────────────────────────────────────

const hoisted = vi.hoisted(() => ({
  agent: null as unknown,
  submit: vi.fn(),
  switchCwd: vi.fn(),
}));

vi.mock("./useAgent", () => ({
  useAgent: () => hoisted.agent,
  setConvoInUrl: vi.fn(),
}));
vi.mock("./uploads", () => ({ uploadFile: vi.fn() }));
vi.mock("./useSessionVerbs", () => ({
  useSessionVerbs: (args: unknown) => {
    // Record the args App passes, so the busy derivation is observable.
    (hoisted as unknown as { lastArgs?: unknown }).lastArgs = args;
    return { submit: hoisted.submit, switchCwd: hoisted.switchCwd };
  },
}));

// ─── fake agent ──────────────────────────────────────────────────────────

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
    conversation: { id: "w1" },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

function makeAgent(state: AppState | null) {
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

// ─── render harness ──────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;
let originalMatchMedia: typeof window.matchMedia | undefined;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

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

  localStorage.clear();
  hoisted.submit = vi.fn();
  hoisted.switchCwd = vi.fn();
  (hoisted as unknown as { lastArgs?: unknown }).lastArgs = undefined;
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
});

async function mountApp(state: AppState | null): Promise<void> {
  const agent = makeAgent(state);
  hoisted.agent = agent;
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<App />);
  });
}

/** The `busy`/`cwd` App last passed to useSessionVerbs. */
const lastArgs = (): { busy: boolean; cwd: string } | undefined =>
  (hoisted as unknown as { lastArgs?: { busy: boolean; cwd: string } }).lastArgs;

const theTextbox = (): HTMLTextAreaElement => {
  const tas = Array.from(document.querySelectorAll("textarea"));
  expect(tas, "a composer textbox must render").toHaveLength(1);
  return tas[0];
};

const textareaSetter = Object.getOwnPropertyDescriptor(
  HTMLTextAreaElement.prototype,
  "value",
)!.set!;

async function typeInto(ta: HTMLTextAreaElement, value: string): Promise<void> {
  await act(async () => {
    textareaSetter.call(ta, value);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function pressEnter(ta: HTMLTextAreaElement): Promise<void> {
  await act(async () => {
    ta.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
  });
}

// ─── busy = isRunning || pendingInput > 0 ────────────────────────────────

describe("App — busy = isRunning || pendingInput > 0 (CONTRACT 5)", () => {
  it("busy is TRUE when is_running is true", async () => {
    await mountApp(stateFor({ is_running: true, pending_input_count: 0 }));
    expect(lastArgs()?.busy, "is_running ⇒ busy").toBe(true);
  });

  it("busy is TRUE when pending_input_count > 0 even while is_running is false", async () => {
    await mountApp(stateFor({ is_running: false, pending_input_count: 3 }));
    expect(lastArgs()?.busy, "queued input ⇒ busy (the headline case)").toBe(true);
  });

  it("busy is FALSE when neither is_running nor pending_input_count", async () => {
    await mountApp(stateFor({ is_running: false, pending_input_count: 0 }));
    expect(lastArgs()?.busy, "idle ⇒ not busy").toBe(false);
  });

  it("busy is FALSE when pending_input_count is 0 and is_running is false (explicit zero)", async () => {
    await mountApp(stateFor({ is_running: false, pending_input_count: 0 }));
    expect(lastArgs()?.busy).toBe(false);
  });

  it("cwd passed to useSessionVerbs comes from welcome.cwd", async () => {
    await mountApp(
      stateFor({
        welcome: {
          provider_name: "p",
          model: "m",
          max_tokens: 1,
          builtin_tools_count: 0,
          mcp_tools_count: 0,
          skills_count: 0,
          searxng_enabled: false,
          cost_tracking_enabled: false,
          compaction_enabled: true,
          compaction_threshold: 0.8,
          compaction_keep_recent: 0,
          conversation_persistence_enabled: true,
          cwd: "/work/dir",
        },
      }),
    );
    expect(lastArgs()?.cwd, "cwd from welcome.cwd").toBe("/work/dir");
  });
});

// ─── onSend delegates to submit ──────────────────────────────────────────

describe("App — onSend delegates to useSessionVerbs().submit (CONTRACT 5)", () => {
  it("pressing Enter in the composer calls submit(text, ids)", async () => {
    await mountApp(stateFor({ is_running: false, pending_input_count: 0 }));
    const ta = theTextbox();
    await typeInto(ta, "hello there");
    await pressEnter(ta);

    expect(hoisted.submit, "onSend must delegate to submit").toHaveBeenCalledTimes(1);
    expect(hoisted.submit).toHaveBeenCalledWith("hello there", []);
  });
});
