// Regression tests for the Composer's action row: the mobile queue button
// and the sub-agent Pause/Resume button.
//
// Mobile queue (issue #123): on a touch device Enter inserts a newline by
// design, so the only path to send is the button. The dispatch button is
// ALWAYS mounted and ALWAYS the last button in DOM order; while `isRunning`
// it reads "Queue" (mid-turn sends are queued server-side) and Stop sits
// before it. Stop's accessible name warns about discarding queued sends
// when `pendingInput > 0`, and an inline hint line shows the queue depth.
//
// Pause/Resume (pause-subagents): while a pausable sub-agent runs, an amber
// Pause/Resume button sits before Stop. It sends the explicit
// `{"type":"pause"}` / `{"type":"resume"}` frames (never a toggle) and
// relabels by the wire pause state — "⏸ Pause" while running, "▶ Resume"
// while pausing or paused. Stop stays as is and aborts everything, paused
// sub-agent included.
//
// Harness modeled on `TopBar.test.tsx` / `Transcript.test.tsx`: plain
// `createRoot` + `act` (jsdom is auto-enabled for `src/components/**` by
// `vitest.config.ts`).
//
// jsdom has no `window.matchMedia` and `useMediaQuery` (useMediaQuery.ts:13)
// calls it on mount, so a controllable stub is installed in `beforeEach`
// and restored in `afterEach`. `matches` is driven by the `touchMatches`
// flag set per test; only `(pointer: coarse)` is targeted, any other query
// is tolerated and reports `false`.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Composer } from "./Composer";
import type { InboundMessage, SlashCommand } from "../state";
import type { SubAgentRun } from "../types";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching the other component tests' setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── matchMedia stub ──────────────────────────────────────────────────────

// jsdom ships no `window.matchMedia`; `useMediaQuery` needs it on mount.
// `touchMatches` drives the `(pointer: coarse)` query per test (true =
// phone/tablet). Other queries are tolerated and report `false`.
let touchMatches = false;
let originalMatchMedia: typeof window.matchMedia | undefined;

beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  touchMatches = false;
  window.matchMedia = (media: string): MediaQueryList => {
    const mql = {
      // Getter (not a captured value) so the stub stays live if a component
      // re-reads `matches` after the flag flips.
      get matches(): boolean {
        return media === "(pointer: coarse)" ? touchMatches : false;
      },
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    };
    return mql as unknown as MediaQueryList;
  };
});

// ─── fixture ──────────────────────────────────────────────────────────────

interface ComposerProps {
  isRunning: boolean;
  connected: boolean;
  commands: SlashCommand[];
  onSend: (text: string) => void;
  onStop: () => void;
  subAgent: SubAgentRun | null;
  onPause: () => void;
  onResume: () => void;
  watchingRole?: string | null;
  onClearWatch?: () => void;
  pendingInput: number;
}

const makeProps = (overrides: Partial<ComposerProps> = {}): ComposerProps => ({
  isRunning: false,
  connected: true,
  commands: [],
  onSend: vi.fn(),
  onStop: vi.fn(),
  subAgent: null,
  onPause: vi.fn(),
  onResume: vi.fn(),
  watchingRole: null,
  onClearWatch: () => {},
  pendingInput: 0,
  ...overrides,
});

// A pausable sub-agent in the given pause state.
function subAgent(pause: SubAgentRun["pause"]): SubAgentRun {
  return { role: "researcher", pausable: true, pause };
}

// ─── mount helper ─────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: ComposerProps): Promise<HTMLDivElement> {
  // Replace any previous mount (test 7 renders two variants in one test).
  await act(async () => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }

  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Composer {...props} />);
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
  // Restore the matchMedia stub (jsdom has none, so usually `delete`).
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
});

// ─── DOM query helpers ────────────────────────────────────────────────────

const allButtons = (el: HTMLElement): HTMLButtonElement[] =>
  Array.from(el.querySelectorAll("button"));

/** A button whose visible text is exactly `text` (trimmed). */
const buttonWithText = (el: HTMLElement, text: string): HTMLButtonElement | null =>
  allButtons(el).find((b) => (b.textContent ?? "").trim() === text) ?? null;

/** The Stop control: a button whose accessible name (title, aria-label, or
 *  text content) starts with "Stop". */
const stopControl = (el: HTMLElement): HTMLButtonElement | null =>
  allButtons(el).find((b) => {
    const title = b.getAttribute("title") ?? "";
    const aria = b.getAttribute("aria-label") ?? "";
    const text = (b.textContent ?? "").trim();
    return title.startsWith("Stop") || aria.startsWith("Stop") || text.startsWith("Stop");
  }) ?? null;

/** True when any element in the subtree has text containing both `count`
 *  and the word "discard" (case-insensitive on the word). */
const hasDiscardCount = (el: HTMLElement, count: number): boolean =>
  Array.from(el.querySelectorAll("*")).some((node) => {
    const text = node.textContent ?? "";
    return text.includes(String(count)) && text.toLowerCase().includes("discard");
  });

/** Find a rendered button by its text content. */
function findButton(el: HTMLDivElement, text: string): HTMLButtonElement {
  const btn = Array.from(el.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!btn) {
    throw new Error(`no button containing "${text}" in:\n${el.innerHTML}`);
  }
  return btn;
}

/** True when any rendered button contains the text. */
function hasButton(el: HTMLDivElement, text: string): boolean {
  return Array.from(el.querySelectorAll("button")).some((b) =>
    b.textContent?.includes(text),
  );
}

// Set a textarea's value the way a user typing would, so React's onChange
// (backed by the native `input` event) fires. Direct `el.value = ...` would
// bypass React's controlled state.
const textareaValueSetter = Object.getOwnPropertyDescriptor(
  HTMLTextAreaElement.prototype,
  "value",
)!.set!;

const typeInto = async (el: HTMLTextAreaElement, value: string): Promise<void> => {
  await act(async () => {
    textareaValueSetter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
};

const click = async (el: HTMLElement): Promise<void> => {
  await act(async () => {
    el.click();
  });
};

const pressEnter = async (el: HTMLTextAreaElement): Promise<void> => {
  await act(async () => {
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
  });
};

// ─── tests ────────────────────────────────────────────────────────────────

describe("Composer — mobile queue button while running", () => {
  // T1 — THE bug: on touch while running, both controls must be present.
  it("touch + running: renders BOTH a 'Queue' dispatch button and a Stop control", async () => {
    touchMatches = true;
    const el = await mount(makeProps({ isRunning: true }));

    const queue = buttonWithText(el, "Queue");
    expect(
      queue,
      "expected a button with text 'Queue' while running",
    ).not.toBeNull();
    const stop = stopControl(el);
    expect(
      stop,
      "expected a Stop control (title/aria-label/text starting with 'Stop') while running",
    ).not.toBeNull();
  });

  // T2 — dispatch rides the existing onSend path, never onStop.
  it("touch + running: clicking 'Queue' calls onSend once with the text, does not call onStop, and clears the textarea", async () => {
    touchMatches = true;
    const props = makeProps({ isRunning: true });
    const el = await mount(props);

    const queue = buttonWithText(el, "Queue");
    expect(queue, "expected a 'Queue' button while running").not.toBeNull();

    const textarea = el.querySelector("textarea");
    expect(textarea).not.toBeNull();
    await typeInto(textarea!, "queue me up");

    await click(queue!);

    expect(props.onSend).toHaveBeenCalledTimes(1);
    expect(props.onSend).toHaveBeenCalledWith("queue me up");
    expect(props.onStop).not.toHaveBeenCalled();
    expect(textarea!.value).toBe("");
  });

  // T3 — DOM order: Stop before dispatch; dispatch last.
  it("running: the Stop control appears before the dispatch button, which is the last button in DOM order", async () => {
    const el = await mount(makeProps({ isRunning: true }));

    const queue = buttonWithText(el, "Queue");
    expect(queue, "expected a 'Queue' dispatch button while running").not.toBeNull();
    const stop = stopControl(el);
    expect(stop, "expected a Stop control while running").not.toBeNull();

    const btns = allButtons(el);
    expect(
      btns.indexOf(stop!),
      "expected the Stop control to appear before the dispatch button",
    ).toBeLessThan(btns.indexOf(queue!));
    expect(
      btns[btns.length - 1],
      "expected the dispatch button to be the last button in DOM order",
    ).toBe(queue);
  });

  // T4 — idle behaviour: dispatch reads "Send", no Stop.
  it("idle: the dispatch button reads 'Send' and no Stop control exists", async () => {
    const el = await mount(makeProps({ isRunning: false }));

    expect(
      buttonWithText(el, "Send"),
      "expected a 'Send' dispatch button while idle",
    ).not.toBeNull();
    expect(stopControl(el), "expected no Stop control while idle").toBeNull();
  });

  // T5 — nothing to send → disabled (`canSend`), click is a no-op.
  it("running + empty input: the 'Queue' button is disabled and clicking it does not call onSend", async () => {
    const props = makeProps({ isRunning: true });
    const el = await mount(props);

    const queue = buttonWithText(el, "Queue");
    expect(queue, "expected a 'Queue' button while running").not.toBeNull();
    expect(
      queue!.disabled,
      "expected the 'Queue' button to be disabled when there is nothing to send",
    ).toBe(true);

    await click(queue!);
    expect(props.onSend).not.toHaveBeenCalled();
  });

  // T6 — touch Enter never sends, even while running (locks commit 63f526e).
  it("touch + running: Enter without shift does not call onSend (newline behaviour preserved)", async () => {
    touchMatches = true;
    const props = makeProps({ isRunning: true });
    const el = await mount(props);

    const textarea = el.querySelector("textarea");
    expect(textarea).not.toBeNull();
    await typeInto(textarea!, "still typing");
    await pressEnter(textarea!);

    expect(props.onSend).not.toHaveBeenCalled();
    // The newline path leaves the text in place — it was not consumed as a send.
    expect(textarea!.value).toBe("still typing");
  });

  // T7 — the pendingInput hint line: present with the count + "discard"
  // while running and queued, absent when nothing is queued.
  it("running + pendingInput=2: an element contains both '2' and 'discard'; with pendingInput=0 no such element exists", async () => {
    const elWith = await mount(makeProps({ isRunning: true, pendingInput: 2 }));
    expect(
      hasDiscardCount(elWith, 2),
      "expected an element containing both '2' and 'discard' while running with 2 queued messages",
    ).toBe(true);

    const elWithout = await mount(makeProps({ isRunning: true, pendingInput: 0 }));
    expect(
      hasDiscardCount(elWithout, 0),
      "expected no element containing both '0' and 'discard' when nothing is queued",
    ).toBe(false);
  });
});

// ─── Pause/Resume visibility ─────────────────────────────────────────────

describe("Composer — Pause/Resume visibility", () => {
  it("hides the Pause button when idle", async () => {
    const el = await mount(makeProps({
      isRunning: false,
      subAgent: subAgent("running"),
    }));

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
  });

  it("hides the Pause button when only the orchestrator runs (subAgent null)", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: null,
    }));

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
    // Stop still shows while running.
    expect(hasButton(el, "Stop")).toBe(true);
  });

  it("hides the Pause button when the sub-agent is not pausable", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: { role: "researcher", pausable: false, pause: "running" },
    }));

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
    expect(hasButton(el, "Stop")).toBe(true);
  });
});

// ─── Pause/Resume label + frames ─────────────────────────────────────────

describe("Composer — Pause/Resume label and wire frames", () => {
  it("shows '⏸ Pause' with the pause tooltip while pause === 'running'", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
    }));

    const btn = findButton(el, "Pause");
    expect(btn.textContent).toContain("⏸");
    expect(btn.title).toBe("Pause sub-agent after its current step");
    expect(hasButton(el, "Resume")).toBe(false);
  });

  it("clicking Pause sends the explicit {type:'pause'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
      onPause: () => sent.push({ type: "pause" }),
    }));

    const btn = findButton(el, "Pause");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "pause" }]);
  });

  it("shows '▶ Resume' with the resume tooltip while pause === 'pausing'", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("pausing"),
    }));

    const btn = findButton(el, "Resume");
    expect(btn.textContent).toContain("▶");
    expect(btn.title).toBe("Resume sub-agent");
    expect(hasButton(el, "Pause")).toBe(false);
  });

  it("clicking Resume (pausing) sends the explicit {type:'resume'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("pausing"),
      onResume: () => sent.push({ type: "resume" }),
    }));

    const btn = findButton(el, "Resume");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "resume" }]);
  });

  it("shows '▶ Resume' and sends {type:'resume'} while pause === 'paused'", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("paused"),
      onResume: () => sent.push({ type: "resume" }),
    }));

    const btn = findButton(el, "Resume");
    expect(btn.textContent).toContain("▶");
    expect(btn.title).toBe("Resume sub-agent");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "resume" }]);
  });
});

// ─── Stop unchanged ─────────────────────────────────────────────────────

describe("Composer — Stop stays as is", () => {
  it("renders Stop next to Pause while a pausable sub-agent runs, and Stop still sends {type:'stop'}", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
      onStop: () => sent.push({ type: "stop" }),
      onPause: () => sent.push({ type: "pause" }),
    }));

    expect(hasButton(el, "Pause")).toBe(true);
    const stop = findButton(el, "Stop");
    await act(async () => {
      stop.click();
    });

    expect(sent).toEqual([{ type: "stop" }]);
  });
});
