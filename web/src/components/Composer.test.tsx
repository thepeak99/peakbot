// RED tests for the mobile queue-button bug in `Composer.tsx`.
//
// Bug: on a touch device you cannot queue a message while the agent is
// running. Enter-to-send is deliberately disabled on coarse pointers
// (Composer.tsx:221-226 — that stays), and the action slot is a single
// `isRunning ? <Stop> : <Send>` (Composer.tsx:368-384), so while running
// the Send button is unmounted and there is no way to queue. The backend
// already accepts mid-turn sends and queues them — this is UI-only.
//
// Agreed design these tests lock in (written BEFORE the implementation;
// tests 1, 2, 3, 5, 7 are expected RED today; 4 and 6 describe existing
// behaviour and are expected GREEN):
//
//   - While `isRunning`, the composer renders BOTH a Stop control and a
//     dispatch button. The dispatch button is ALWAYS mounted and ALWAYS
//     the last button in DOM order; Stop, when present, comes immediately
//     before it.
//   - Dispatch button text: `Send` when idle, `Queue` when `isRunning`.
//     Same emerald styling in both states.
//   - Clicking dispatch goes through the existing `onSend(text)` path
//     (same as today's `submit()`), clears the textarea, and does NOT
//     call `onStop`.
//   - Dispatch is disabled when there is nothing to send (the existing
//     `canSend` logic: non-empty trimmed text or ≥1 attached image, and
//     `connected`).
//   - Stop's accessible name/title: plain `Stop` when `pendingInput ===
//     0`; when `pendingInput > 0` it warns about discarding, containing
//     both the count and the word `discard`.
//   - NEW PROP `pendingInput: number` (sourced in App.tsx from
//     `state.pending_input_count`). While `isRunning && pendingInput > 0`
//     an inline hint line — NOT breakpoint-gated — contains the count and
//     the word `discard`; absent when `pendingInput === 0`.
//   - On touch, Enter (no shift) still does NOT send — it inserts a
//     newline. Must stay true even while running (locks commit 63f526e).
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
//
// `pendingInput` is not on Composer's declared props today, so the fixture
// is typed as the real interface plus the new field and passed through an
// `as unknown as Parameters<typeof Composer>[0]` cast (the established
// pattern in this repo, see TopBar.test.tsx) so the file still
// type-checks and vitest runs it against today's component. Post-
// implementation the cast becomes a no-op identity.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Composer } from "./Composer";
import type { SlashCommand } from "../state";

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

// Composer's declared props today (Composer.tsx:55-75) have no `pendingInput`
// field. Type the fixture as the real interface PLUS the not-yet-existing
// field, so the fixture reads the way the post-implementation call site will,
// while the cast in `mount` keeps the file compiling against today's
// component.
interface ComposerProps {
  isRunning: boolean;
  connected: boolean;
  commands: SlashCommand[];
  onSend: (text: string) => void;
  onStop: () => void;
  watchingRole?: string | null;
  onClearWatch?: () => void;
  /** NEW — not on Composer's props type yet. Sourced in App.tsx from
   *  `state.pending_input_count`. */
  pendingInput: number;
}

const makeProps = (overrides: Partial<ComposerProps> = {}): ComposerProps => ({
  isRunning: false,
  connected: true,
  commands: [],
  onSend: vi.fn(),
  onStop: vi.fn(),
  watchingRole: null,
  onClearWatch: () => {},
  pendingInput: 0,
  ...overrides,
});

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
    // `pendingInput` is not part of Composer's declared props today, so we
    // cast through `unknown` to pass it anyway — the point of these tests
    // is to prove the prop is currently ignored/absent, not to fight the
    // type system. Post-implementation this cast becomes a no-op identity.
    root.render(
      <Composer {...(props as unknown as Parameters<typeof Composer>[0])} />,
    );
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
  // RED today: the action slot is `isRunning ? <Stop> : <Send>`, so no
  // "Queue" button exists while running.
  it("touch + running: renders BOTH a 'Queue' dispatch button and a Stop control", async () => {
    touchMatches = true;
    const el = await mount(makeProps({ isRunning: true }));

    const queue = buttonWithText(el, "Queue");
    expect(
      queue,
      "expected a button with text 'Queue' while running (the Send button is unmounted today)",
    ).not.toBeNull();
    const stop = stopControl(el);
    expect(
      stop,
      "expected a Stop control (title/aria-label/text starting with 'Stop') while running",
    ).not.toBeNull();
  });

  // T2 — dispatch rides the existing onSend path, never onStop.
  // RED today: no "Queue" button exists while running.
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

  // T3 — DOM order: Stop immediately before dispatch; dispatch last.
  // RED today: no "Queue" button exists while running.
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

  // T4 — idle behaviour unchanged: dispatch reads "Send", no Stop.
  // Expected GREEN today (existing behaviour).
  it("idle: the dispatch button reads 'Send' and no Stop control exists", async () => {
    const el = await mount(makeProps({ isRunning: false }));

    expect(
      buttonWithText(el, "Send"),
      "expected a 'Send' dispatch button while idle",
    ).not.toBeNull();
    expect(stopControl(el), "expected no Stop control while idle").toBeNull();
  });

  // T5 — nothing to send → disabled (existing `canSend`), click is a no-op.
  // RED today: no "Queue" button exists while running.
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
  // Expected GREEN today (existing behaviour).
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
  // RED today: `pendingInput` is not a Composer prop and no such line exists.
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
