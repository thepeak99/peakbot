// Regression + new-spec tests for the Composer's action row.
//
// ─── New spec (icon-only running controls, no media queries) ─────────────
// Let `hasContent` = the composer has non-whitespace text OR at least one
// done attachment (the existing `canSend` condition, minus the `connected`
// gate).
//
//   1. IDLE (not running): unchanged — a labelled "Send" button; no Stop,
//      no Pause/Resume.
//   2. RUNNING and NOT hasContent: icon-only controls — Pause/Resume (only
//      when the sub-agent is pausable, exactly as today) and Stop. No
//      visible text label; each carries its name via `aria-label` (and,
//      historically, `title`) so `getByRole('button', {name})`-style
//      lookups work. NO Send/Queue button is rendered.
//   3. RUNNING and hasContent: ONLY a labelled "Queue" button is shown.
//      Pause/Resume and Stop are NOT rendered.
//   4. Transitions: typing while running swaps Stop/Pause out for Queue;
//      clearing back to empty/whitespace swaps Queue back out for
//      Stop/Pause. Queue still sends (onSend), Stop still sends
//      {type:"stop"}, Pause/Resume still send the explicit frames.
//   5. The 📎 attach button is present in every state.
//
// This supersedes the OLD "both rendered while running" contract (the
// previous T1–T3), which pinned Queue *and* Stop coexisting whenever
// `isRunning`, regardless of content. That behaviour is now split across
// the "running + no content" / "running + has content" describe blocks
// below; nothing here asserts both a labelled dispatch button AND an
// icon-only Stop/Pause are present at once.
//
// Harness modeled on `TopBar.test.tsx` / `Transcript.test.tsx`: plain
// `createRoot` + `act` (jsdom is auto-enabled for `src/components/**` by
// `vitest.config.ts`).
//
// jsdom has no `window.matchMedia` and `useMediaQuery` (useMediaQuery.ts:13)
// calls it on mount, so a controllable stub is installed in `beforeEach`
// and restored in `afterEach`. `matches` is driven by the `touchMatches`
// flag set per test; only `(pointer: coarse)` is targeted, any other query
// is tolerated and reports `false`. (The new spec is explicitly viewport-
// independent — no media query gates the icon-only behaviour — so most new
// tests run with the default fine-pointer flag; the touch-Enter regression
// test opts into `touchMatches = true`.)

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Composer } from "./Composer";
import type { InboundMessage, SlashCommand, WireAttachment } from "../state";
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

// ─── navigator.userAgent stub ─────────────────────────────────────────────

// The attach-menu platform gate is the UA (design-attach-menu §2), so the
// Android/iOS/Windows cases pin a UA string before mount. Same precedent as
// ComposerPaste.test.tsx: spy on the getter, restore in afterEach.
let uaSpy: ReturnType<typeof vi.spyOn> | undefined;
const mockUA = (ua: string): void => {
  uaSpy = vi.spyOn(navigator, "userAgent", "get").mockReturnValue(ua);
};

// ─── fixture ──────────────────────────────────────────────────────────────

interface ComposerProps {
  isRunning: boolean;
  connected: boolean;
  commands: SlashCommand[];
  onSend: (text: string, attachmentIds: string[]) => void;
  onStop: () => void;
  subAgent: SubAgentRun | null;
  onPause: () => void;
  onResume: () => void;
  watchingRole?: string | null;
  onClearWatch?: () => void;
  pendingInput: number;
  /** Two-stage stop: a graceful stop is pending (mirrors `AppState.stop_pending`). */
  stopPending?: boolean;
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
  // Replace any previous mount (some tests render two variants in one test).
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
  // Restore the UA spy from the attach-menu tests (the platform gate is the UA).
  uaSpy?.mockRestore();
  uaSpy = undefined;
});

// ─── DOM query helpers ────────────────────────────────────────────────────

const allButtons = (el: HTMLElement): HTMLButtonElement[] =>
  Array.from(el.querySelectorAll("button"));

/** A button whose visible text is exactly `text` (trimmed). Only valid for
 *  buttons the spec says carry a *labelled* text (Send/Queue/Clear) — icon-
 *  only controls (Stop, Pause, Resume under the new spec) must NOT be
 *  looked up this way, see `controlByName` below. */
const buttonWithText = (el: HTMLElement, text: string): HTMLButtonElement | null =>
  allButtons(el).find((b) => (b.textContent ?? "").trim() === text) ?? null;

/** The accessible name of a button, in the same precedence an
 *  accessibility tree (and `getByRole('button', {name})`) would use:
 *  `aria-label`, then `title`, then trimmed visible text. */
const accessibleName = (b: HTMLButtonElement): string =>
  b.getAttribute("aria-label") ?? b.getAttribute("title") ?? (b.textContent ?? "").trim();

/** Find a button whose accessible name starts with `prefix`. This is the
 *  ONLY reliable way to find Stop/Pause/Resume once they go icon-only:
 *  their visible text is gone (or, pre-fix, may still be present but must
 *  not be relied on), so the name has to come from `aria-label`/`title`.
 *  Stop's name is dynamic ("Stop" or "Stop and discard N queued
 *  message(s)"), hence prefix rather than exact match. */
const controlByName = (el: HTMLElement, prefix: string): HTMLButtonElement | null =>
  allButtons(el).find((b) => accessibleName(b).startsWith(prefix)) ?? null;

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

// ─── 1. IDLE: unchanged ───────────────────────────────────────────────────

describe("Composer — action buttons: idle (spec 1)", () => {
  it("idle: the dispatch button reads 'Send' and no Stop control exists", async () => {
    const el = await mount(makeProps({ isRunning: false }));

    expect(
      buttonWithText(el, "Send"),
      "expected a 'Send' dispatch button while idle",
    ).not.toBeNull();
    expect(
      controlByName(el, "Stop"),
      "expected no Stop control while idle",
    ).toBeNull();
  });

  it("idle: no Pause/Resume control exists even with a pausable subAgent prop set", async () => {
    // `subAgent` is only meaningful while running; idle must never show it.
    const el = await mount(
      makeProps({ isRunning: false, subAgent: subAgent("running") }),
    );

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
  });

  it("idle: the 'Send' button is disabled with no text and no images, and clicking it is a no-op", async () => {
    const props = makeProps({ isRunning: false });
    const el = await mount(props);

    const send = buttonWithText(el, "Send")!;
    expect(send.disabled).toBe(true);
    await click(send);
    expect(props.onSend).not.toHaveBeenCalled();
  });
});

// ─── 2. RUNNING and NOT hasContent: icon-only Pause/Resume + Stop ─────────

describe("Composer — action buttons: running, no content (spec 2)", () => {
  it("running + empty input, no subAgent: renders an icon-only Stop control and NO Send/Queue button", async () => {
    const el = await mount(makeProps({ isRunning: true, subAgent: null }));

    const stop = controlByName(el, "Stop");
    expect(stop, "expected an icon-only Stop control").not.toBeNull();
    expect(
      buttonWithText(el, "Queue"),
      "expected NO labelled 'Queue' button while running with no content",
    ).toBeNull();
    expect(
      buttonWithText(el, "Send"),
      "expected NO labelled 'Send' button while running",
    ).toBeNull();
  });

  it("running + empty input, no subAgent: no Pause/Resume control is rendered", async () => {
    const el = await mount(makeProps({ isRunning: true, subAgent: null }));

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
  });

  it("running + empty input, pausable subAgent (pause='running'): renders icon-only Pause AND Stop, no Queue/Send", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );

    expect(controlByName(el, "Pause"), "expected an icon-only Pause control").not.toBeNull();
    expect(controlByName(el, "Stop"), "expected an icon-only Stop control").not.toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();
    expect(buttonWithText(el, "Send")).toBeNull();
  });

  it("running + empty input, pausable subAgent (pause='paused'): renders icon-only Resume AND Stop, no Queue/Send", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("paused") }),
    );

    expect(controlByName(el, "Resume"), "expected an icon-only Resume control").not.toBeNull();
    expect(controlByName(el, "Stop"), "expected an icon-only Stop control").not.toBeNull();
    expect(controlByName(el, "Pause")).toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();
  });

  it("running + empty input, non-pausable subAgent: no Pause/Resume, Stop still shows, no Queue/Send", async () => {
    const el = await mount(
      makeProps({
        isRunning: true,
        subAgent: { role: "researcher", pausable: false, pause: "running" },
      }),
    );

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
    expect(controlByName(el, "Stop")).not.toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();
  });

  it("running + whitespace-only text ('   '): treated as NOT hasContent — Stop shows, no Queue", async () => {
    const el = await mount(makeProps({ isRunning: true, subAgent: null }));
    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "   ");

    expect(
      controlByName(el, "Stop"),
      "expected Stop to still show with whitespace-only text",
    ).not.toBeNull();
    expect(
      buttonWithText(el, "Queue"),
      "expected NO Queue button with whitespace-only text",
    ).toBeNull();
  });

  it("icon-only Stop and Pause/Resume carry NO visible text label (their name comes from aria-label/title only)", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );

    const stop = controlByName(el, "Stop")!;
    const pause = controlByName(el, "Pause")!;
    expect(
      (stop.textContent ?? "").trim(),
      "Stop must not render the visible word 'Stop' while running with no content",
    ).not.toMatch(/Stop/);
    expect(
      (pause.textContent ?? "").trim(),
      "Pause must not render the visible word 'Pause' while running with no content",
    ).not.toMatch(/Pause/);
  });

  it("Stop, Pause and Resume each expose their name via aria-label (not just title)", async () => {
    const running = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );
    const stop = controlByName(running, "Stop")!;
    const pause = controlByName(running, "Pause")!;
    const stopAria = stop.getAttribute("aria-label");
    const pauseAria = pause.getAttribute("aria-label");
    expect(stopAria, "expected Stop to expose an aria-label").not.toBeNull();
    expect(
      stopAria,
      "expected Stop's aria-label to start with 'Stop'",
    ).toMatch(/^Stop/);
    expect(pauseAria, "expected Pause to expose an aria-label").not.toBeNull();
    expect(
      pauseAria,
      "expected Pause's aria-label to start with 'Pause'",
    ).toMatch(/^Pause/);

    const paused = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("paused") }),
    );
    const resume = controlByName(paused, "Resume")!;
    const resumeAria = resume.getAttribute("aria-label");
    expect(resumeAria, "expected Resume to expose an aria-label").not.toBeNull();
    expect(
      resumeAria,
      "expected Resume's aria-label to start with 'Resume'",
    ).toMatch(/^Resume/);
  });

  it("clicking the icon-only Stop control sends {type:'stop'} and not {type:'send_message'}", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(
      makeProps({
        isRunning: true,
        subAgent: null,
        onStop: () => sent.push({ type: "stop" }),
      }),
    );

    const stop = controlByName(el, "Stop")!;
    await click(stop);

    expect(sent).toEqual([{ type: "stop" }]);
  });

  it("clicking the icon-only Pause control sends {type:'pause'}", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(
      makeProps({
        isRunning: true,
        subAgent: subAgent("running"),
        onPause: () => sent.push({ type: "pause" }),
      }),
    );

    const pause = controlByName(el, "Pause")!;
    await click(pause);

    expect(sent).toEqual([{ type: "pause" }]);
  });

  it("clicking the icon-only Resume control sends {type:'resume'}", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(
      makeProps({
        isRunning: true,
        subAgent: subAgent("paused"),
        onResume: () => sent.push({ type: "resume" }),
      }),
    );

    const resume = controlByName(el, "Resume")!;
    await click(resume);

    expect(sent).toEqual([{ type: "resume" }]);
  });

  // T7 (kept from the original suite — unaffected by the icon-only change,
  // this is about the separate hint LINE below the input row, not a button).
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

// ─── 3. RUNNING and hasContent: ONLY a labelled Queue button ──────────────

describe("Composer — action buttons: running, has content (spec 3)", () => {
  it("running + non-whitespace text: ONLY a labelled 'Queue' button is shown; Stop/Pause/Resume are absent", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );
    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "queue me up");

    expect(
      buttonWithText(el, "Queue"),
      "expected a labelled 'Queue' button while running with content",
    ).not.toBeNull();
    expect(controlByName(el, "Stop"), "expected Stop to be absent").toBeNull();
    expect(controlByName(el, "Pause"), "expected Pause to be absent").toBeNull();
    expect(controlByName(el, "Resume"), "expected Resume to be absent").toBeNull();
  });

  it("running + done attachment, no text: hasContent via attachment alone hides Stop and shows Queue", async () => {
    // Upload state is owned by App and passed in as the `attachments` prop
    // (T12); a standalone Composer mounts with the inert no-op default, so
    // the old file-input → FileReader path no longer produces chips here.
    // A done chip with an id is what makes hasContent true.
    const api = attachmentsApi({
      pending: [doneImageChip("id-img")],
      ids: ["id-img"],
      ready: true,
    });
    const el = await t12Mount(
      makeProps({ isRunning: true, subAgent: null }),
      api,
      true,
    );

    expect(
      buttonWithText(el, "Queue"),
      "expected 'Queue' once an attachment is done, even with no typed text",
    ).not.toBeNull();
    expect(
      controlByName(el, "Stop"),
      "expected Stop to disappear once an attachment makes hasContent true",
    ).toBeNull();
  });

  it("clicking 'Queue' calls onSend once with the text, does not call onStop, and clears the textarea", async () => {
    const props = makeProps({ isRunning: true });
    const el = await mount(props);
    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "queue me up");

    const queue = buttonWithText(el, "Queue")!;
    await click(queue);

    expect(props.onSend).toHaveBeenCalledTimes(1);
    expect(props.onSend).toHaveBeenCalledWith("queue me up", []);
    expect(props.onStop).not.toHaveBeenCalled();
    expect(textarea.value).toBe("");
  });

  it("clicking 'Queue' with a done attachment and no text sends the attachment ids (no data: token) and resets the chips", async () => {
    // The redesign sends uploaded ids, never `[img:data:…]` tokens in the
    // text; chips clear via attachments.reset() after sending.
    const props = makeProps({ isRunning: true });
    const api = attachmentsApi({
      pending: [doneImageChip("id-img")],
      ids: ["id-img"],
      ready: true,
    });
    const el = await t12Mount(props, api, true);

    const queue = buttonWithText(el, "Queue")!;
    await click(queue);

    expect(props.onSend).toHaveBeenCalledTimes(1);
    expect(props.onSend).toHaveBeenCalledWith("", ["id-img"]);
    expect(props.onStop).not.toHaveBeenCalled();
    expect(
      api.reset,
      "expected reset() to clear the attachment chips after sending",
    ).toHaveBeenCalledTimes(1);
  });

  // T6 from the original suite — unaffected by the button-visibility change:
  // this asserts Enter never sends on touch, independent of which buttons
  // are mounted.
  it("touch + running + has content: Enter without shift does not call onSend (newline behaviour preserved)", async () => {
    touchMatches = true;
    const props = makeProps({ isRunning: true });
    const el = await mount(props);

    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "still typing");
    await pressEnter(textarea);

    expect(props.onSend).not.toHaveBeenCalled();
    // The newline path leaves the text in place — it was not consumed as a send.
    expect(textarea.value).toBe("still typing");
  });
});

// ─── 4. Transitions while running ──────────────────────────────────────────

describe("Composer — hasContent transitions while running (spec 4)", () => {
  it("typing text swaps Stop out for Queue; clearing back to empty swaps Queue back out for Stop", async () => {
    const el = await mount(makeProps({ isRunning: true, subAgent: null }));
    const textarea = el.querySelector("textarea")!;

    // Starting state: no content → Stop, no Queue.
    expect(controlByName(el, "Stop"), "expected Stop before typing").not.toBeNull();
    expect(buttonWithText(el, "Queue"), "expected no Queue before typing").toBeNull();

    // Typing content → Queue appears, Stop disappears.
    await typeInto(textarea, "hello");
    expect(
      buttonWithText(el, "Queue"),
      "expected Queue to appear once text is non-empty",
    ).not.toBeNull();
    expect(
      controlByName(el, "Stop"),
      "expected Stop to disappear once text is non-empty",
    ).toBeNull();

    // Clearing back to empty → Stop reappears, Queue disappears.
    await typeInto(textarea, "");
    expect(
      controlByName(el, "Stop"),
      "expected Stop to reappear once the textarea is cleared",
    ).not.toBeNull();
    expect(
      buttonWithText(el, "Queue"),
      "expected Queue to disappear once the textarea is cleared",
    ).toBeNull();

    // Typing whitespace only → same as empty: Stop stays, no Queue.
    await typeInto(textarea, "   ");
    expect(controlByName(el, "Stop")).not.toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();
  });

  it("with a pausable subAgent: typing swaps BOTH Pause and Stop out for Queue; clearing restores both", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );
    const textarea = el.querySelector("textarea")!;

    expect(controlByName(el, "Pause")).not.toBeNull();
    expect(controlByName(el, "Stop")).not.toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();

    await typeInto(textarea, "in progress note");
    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Stop")).toBeNull();
    expect(buttonWithText(el, "Queue")).not.toBeNull();

    await typeInto(textarea, "");
    expect(controlByName(el, "Pause")).not.toBeNull();
    expect(controlByName(el, "Stop")).not.toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();
  });
});

// ─── 5. Attach button present in every state ───────────────────────────────

describe("Composer — Attach button always present (spec 5)", () => {
  const attachButton = (el: HTMLElement): HTMLButtonElement | null =>
    controlByName(el, "Attach");

  it("idle: the Attach button is present", async () => {
    const el = await mount(makeProps({ isRunning: false }));
    expect(attachButton(el)).not.toBeNull();
  });

  it("running + no content: the Attach button is present alongside the icon-only controls", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );
    expect(attachButton(el)).not.toBeNull();
  });

  it("running + has content: the Attach button is present alongside 'Queue'", async () => {
    const el = await mount(makeProps({ isRunning: true }));
    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "hi");
    expect(attachButton(el)).not.toBeNull();
    expect(buttonWithText(el, "Queue")).not.toBeNull();
  });
});

// ─── Pause/Resume visibility ─────────────────────────────────────────────

describe("Composer — Pause/Resume visibility", () => {
  it("hides the Pause button when idle", async () => {
    const el = await mount(makeProps({
      isRunning: false,
      subAgent: subAgent("running"),
    }));

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
  });

  it("hides the Pause button when only the orchestrator runs (subAgent null)", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: null,
    }));

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
    // Stop still shows while running with no content (spec 2). Looked up by
    // accessible name, not visible text — see the module header.
    expect(controlByName(el, "Stop")).not.toBeNull();
  });

  it("hides the Pause button when the sub-agent is not pausable", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: { role: "researcher", pausable: false, pause: "running" },
    }));

    expect(controlByName(el, "Pause")).toBeNull();
    expect(controlByName(el, "Resume")).toBeNull();
    expect(controlByName(el, "Stop")).not.toBeNull();
  });
});

// ─── Pause/Resume label + frames ─────────────────────────────────────────
//
// Rewritten to locate the buttons by accessible name (`controlByName`)
// instead of visible text: under the new spec these are icon-only while
// running with no content, so `findButton(el, "Pause")` (substring match on
// `textContent`) is no longer a valid lookup — see the module header.

describe("Composer — Pause/Resume label and wire frames", () => {
  it("shows the pause tooltip/aria-label ('Pause sub-agent…') while pause === 'running'", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
    }));

    const btn = controlByName(el, "Pause")!;
    expect(btn.title).toBe("Pause sub-agent after its current step");
    expect(controlByName(el, "Resume")).toBeNull();
  });

  it("clicking Pause sends the explicit {type:'pause'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
      onPause: () => sent.push({ type: "pause" }),
    }));

    const btn = controlByName(el, "Pause")!;
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "pause" }]);
  });

  it("shows the resume tooltip/aria-label ('Resume sub-agent') while pause === 'pausing'", async () => {
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("pausing"),
    }));

    const btn = controlByName(el, "Resume")!;
    expect(btn.title).toBe("Resume sub-agent");
    expect(controlByName(el, "Pause")).toBeNull();
  });

  it("clicking Resume (pausing) sends the explicit {type:'resume'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("pausing"),
      onResume: () => sent.push({ type: "resume" }),
    }));

    const btn = controlByName(el, "Resume")!;
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "resume" }]);
  });

  it("shows Resume and sends {type:'resume'} while pause === 'paused'", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("paused"),
      onResume: () => sent.push({ type: "resume" }),
    }));

    const btn = controlByName(el, "Resume")!;
    expect(btn.title).toBe("Resume sub-agent");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "resume" }]);
  });
});

// ─── Stop unchanged ─────────────────────────────────────────────────────

describe("Composer — Stop stays as is", () => {
  it("renders Stop next to Pause while a pausable sub-agent runs with no content, and Stop still sends {type:'stop'}", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount(makeProps({
      isRunning: true,
      subAgent: subAgent("running"),
      onStop: () => sent.push({ type: "stop" }),
      onPause: () => sent.push({ type: "pause" }),
    }));

    expect(controlByName(el, "Pause")).not.toBeNull();
    const stop = controlByName(el, "Stop")!;
    await act(async () => {
      stop.click();
    });

    expect(sent).toEqual([{ type: "stop" }]);
  });
});

// ─── T12: attachments (chips, send gating, paste, vision ⚠) ───────────────
// RED: Composer.tsx still uses the old image-only data-URI path. Contract
// (design T12 + §5.7): the useAttachments result is lifted into App.tsx and
// passed to Composer as the `attachments` prop; `supportsVision` carries
// AppState.supports_vision.
//
// ASSUMPTION: submit calls onSend(text, ids); App builds the
// {type:"send_message", text, attachments: ids} frame, and the Composer
// calls attachments.reset() after sending.

type PendingChip = { key: string; file: File; preview?: string } & (
  | { state: "uploading"; progress: number; abort: () => void }
  | { state: "done"; attachment: WireAttachment }
  | { state: "error"; message: string }
);

interface AttachmentsApi {
  pending: PendingChip[];
  add: (files: File[]) => void;
  remove: (key: string) => void;
  reset: () => void;
  ids: string[];
  ready: boolean;
}

const T12_CONVO = "11111111-1111-4111-8111-111111111111";

const pngFile = (): File => new File(["png-bytes"], "cat.png", { type: "image/png" });
const pdfFile = (): File => new File(["pdf-bytes"], "spec.pdf", { type: "application/pdf" });

function doneImageChip(id: string, preview?: string): PendingChip {
  return {
    key: `k-${id}`,
    file: pngFile(),
    preview,
    state: "done",
    attachment: {
      id,
      convo: T12_CONVO,
      name: "cat.png",
      mime: "image/png",
      size: 1234,
      kind: "image",
    },
  };
}

function donePdfChip(id: string): PendingChip {
  return {
    key: `k-${id}`,
    file: pdfFile(),
    state: "done",
    attachment: {
      id,
      convo: T12_CONVO,
      name: "spec.pdf",
      mime: "application/pdf",
      size: 2411724,
      kind: "file",
    },
  };
}

const uploadingChip = (): PendingChip => ({
  key: "k-up",
  file: pdfFile(),
  state: "uploading",
  progress: 0.4,
  abort: () => {},
});

const errorChip = (message: string): PendingChip => ({
  key: "k-err",
  file: pdfFile(),
  state: "error",
  message,
});

function attachmentsApi(over: Partial<AttachmentsApi> = {}): AttachmentsApi {
  return {
    pending: [],
    add: vi.fn(),
    remove: vi.fn(),
    reset: vi.fn(),
    ids: [],
    ready: true,
    ...over,
  };
}

// The new props are not on Composer's type yet (that is the point of the
// RED state); the cast keeps the mount call site clean while the
// runtime still forwards every prop through the spread.
const t12Mount = (
  base: ComposerProps,
  attachments: AttachmentsApi,
  supportsVision: boolean,
): Promise<HTMLDivElement> =>
  mount({ ...base, attachments, supportsVision } as unknown as ComposerProps);

// Mobile-attach part 2 rewords the non-vision warning (design decision 4).
// `NO_VISION` / `visionWarning` are not exported from AttachmentChip yet, so
// the tests pin the LITERAL sentence the spec locks in.
const NO_VISION_NOTICE =
  "This model can't view images — the picture is sent as a file it cannot see.";

describe("Composer — attachments (T12)", () => {
  it("renders a done image attachment as a thumbnail chip (img with the preview src)", async () => {
    const api = attachmentsApi({ pending: [doneImageChip("id-img", "blob:preview-1")] });
    const el = await t12Mount(makeProps(), api, true);

    const img = el.querySelector('img[alt="cat.png"]');
    expect(img, "expected a thumbnail chip for the image attachment").not.toBeNull();
    expect(img!.getAttribute("src")).toBe("blob:preview-1");
  });

  it("renders a done pdf attachment as an icon chip with name and size, no <img>", async () => {
    const api = attachmentsApi({ pending: [donePdfChip("id-pdf")] });
    const el = await t12Mount(makeProps(), api, true);

    expect(el.querySelector("img"), "file chips must not render an <img>").toBeNull();
    const chipEl = Array.from(el.querySelectorAll("*")).find((n) =>
      (n.textContent ?? "").includes("spec.pdf"),
    );
    expect(chipEl, "expected a chip carrying the file name").toBeDefined();
    expect(chipEl!.textContent).toContain("2.3 MB");

    // The file input no longer restricts types (accept removed, any type).
    const input = el.querySelector('input[type="file"]');
    expect(input).not.toBeNull();
    expect(input!.getAttribute("accept")).toBeNull();
  });

  it("disables Send while a chip is uploading", async () => {
    const api = attachmentsApi({ pending: [uploadingChip()], ready: false });
    const el = await t12Mount(makeProps(), api, true);

    expect(buttonWithText(el, "Send")!.disabled).toBe(true);
  });

  it("disables Send while an error chip exists", async () => {
    const api = attachmentsApi({
      pending: [errorChip("Too large (61.2 MB · max 50 MB)")],
      ready: false,
    });
    const el = await t12Mount(makeProps(), api, true);

    expect(buttonWithText(el, "Send")!.disabled).toBe(true);
  });

  it("enables Send with only a done attachment and empty text", async () => {
    const api = attachmentsApi({
      pending: [donePdfChip("id-pdf")],
      ids: ["id-pdf"],
      ready: true,
    });
    const el = await t12Mount(makeProps(), api, true);

    expect(buttonWithText(el, "Send")!.disabled).toBe(false);
  });

  it("submit sends the text with the attachment ids and resets the chips", async () => {
    // ASSUMPTION (1 line): submit calls onSend(text, ids); App builds the
    // {type:"send_message", text, attachments: ids} frame.
    const onSend = vi.fn();
    const api = attachmentsApi({
      pending: [doneImageChip("id-a"), donePdfChip("id-b")],
      ids: ["id-a", "id-b"],
      ready: true,
    });
    const el = await t12Mount(makeProps({ onSend }), api, true);

    const textarea = el.querySelector("textarea")!;
    await typeInto(textarea, "what's in these?");
    await click(buttonWithText(el, "Send")!);

    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend).toHaveBeenCalledWith("what's in these?", ["id-a", "id-b"]);
    expect(api.reset).toHaveBeenCalledTimes(1);
  });

  it("attaches non-image clipboard files on paste", async () => {
    const add = vi.fn();
    const api = attachmentsApi({ add });
    const el = await t12Mount(makeProps(), api, true);
    const textarea = el.querySelector("textarea")!;

    const pdf = pdfFile();
    const png = pngFile();
    const files = [pdf, png];
    const ev = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(ev, "clipboardData", { value: { files, items: [] } });

    await act(async () => {
      textarea.dispatchEvent(ev);
    });

    expect(add).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith(files);
  });

  it("shows a ⚠ with the non-vision title for an image when supports_vision=false", async () => {
    const api = attachmentsApi({ pending: [doneImageChip("id-img")] });
    const el = await t12Mount(makeProps(), api, false);

    const warn = Array.from(el.querySelectorAll("[title]")).find((n) =>
      (n.getAttribute("title") ?? "").includes(NO_VISION_NOTICE),
    );
    expect(warn, "expected a ⚠ title for an image on a non-vision model").toBeDefined();
  });

  it("shows no vision warning for an image when supports_vision=true", async () => {
    const api = attachmentsApi({ pending: [doneImageChip("id-img")] });
    const el = await t12Mount(makeProps(), api, true);

    const warn = Array.from(el.querySelectorAll("[title]")).find((n) =>
      (n.getAttribute("title") ?? "").includes(NO_VISION_NOTICE),
    );
    expect(warn ?? null).toBeNull();
  });
});

// ─── Mobile attach: one 📎 button, Android attach menu ─────────────────────
//
// RED until the Composer implements design-attach-menu (PR #443):
//   1. The main hidden file input keeps `multiple` and loses `accept` in
//      EVERY edition (the server's 415 is the real gate, now with an
//      actionable message).
//   2. `isAndroid()` (web/src/platform.ts: UA contains "Android") — NOT
//      `(pointer: coarse)` — gates the menu: on Android (app WebView and
//      browsers alike) 📎 toggles a role="menu" with three menuitems
//      ("Take a photo" / "Choose from gallery" / "Pick a file") that click
//      the camera (`accept="image/*" capture="environment"`, NO multiple),
//      gallery (`accept="image/*" multiple`, NO capture) and plain inputs.
//      Camera + gallery sit AFTER the plain one so the first
//      `input[type=file]` stays the plain picker; the old 📷 "Take photo"
//      button is deleted everywhere. Every other platform (desktop, iPhone,
//      iPad-reporting-as-Mac, Windows touch laptops, unknown UAs) keeps
//      master's exact DOM: one input, no menu, no extra aria attributes.
//   3. Non-vision notice: when any DONE image chip's visionWarning is the
//      NO_VISION sentence, the Composer renders it ONCE under the chip row
//      with role="status". Sending stays enabled; it is hidden for vision
//      models and when only non-image files are attached — on a remote
//      base too.
//
// `NO_VISION` / `visionWarning` are not exported yet, so the notice is
// pinned by its literal text (see NO_VISION_NOTICE above).

/** All `input[type=file]` in DOM order — the first must be the main picker. */
const fileInputs = (el: HTMLElement): HTMLInputElement[] =>
  Array.from(el.querySelectorAll('input[type="file"]'));

/** The camera input, if any: the file input carrying a `capture` attribute. */
const cameraInput = (el: HTMLElement): HTMLInputElement | null =>
  fileInputs(el).find((i) => i.getAttribute("capture") !== null) ?? null;

/** Every role="status" element in the subtree. */
const statusEls = (el: HTMLElement): HTMLElement[] =>
  Array.from(el.querySelectorAll('[role="status"]'));

// Realistic full UA strings per platform (the gate is the UA, not the pointer).
const ANDROID_UA =
  "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36";
const IPHONE_UA =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const IPAD_AS_MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const WINDOWS_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/** The 📎 attach button (accessible name "Attach files"). */
const attachButtonOf = (el: HTMLElement): HTMLButtonElement | null =>
  controlByName(el, "Attach");

/** The Android attach menu, if open. */
const menuOf = (el: HTMLElement): HTMLElement | null =>
  el.querySelector('[data-testid="attach-menu"]');

/** The menu's role="menuitem" buttons in DOM order. */
const menuItemsOf = (el: HTMLElement): HTMLButtonElement[] =>
  Array.from(el.querySelectorAll('[role="menuitem"]'));

/** The gallery input: image-only picker WITHOUT capture (the camera has it). */
const galleryInputOf = (el: HTMLElement): HTMLInputElement | null =>
  el.querySelector('input[type="file"][accept="image/*"]:not([capture])');

/** Case 1's shared contract: master's plain-picker DOM, no menu extras. */
const expectPlainOnlyDom = async (el: HTMLElement): Promise<void> => {
  const inputs = fileInputs(el);
  expect(inputs, "exactly one file input (the plain picker)").toHaveLength(1);
  expect(inputs[0].hasAttribute("multiple"), "the plain picker is multiple").toBe(true);
  expect(inputs[0].hasAttribute("accept"), "no accept: the server's 415 is the gate").toBe(false);
  expect(inputs[0].hasAttribute("capture"), "no capture on the plain picker").toBe(false);

  const btn = attachButtonOf(el);
  expect(btn, "the 📎 button must exist").not.toBeNull();
  expect(btn!.hasAttribute("aria-haspopup"), "no aria-haspopup without a menu").toBe(false);
  expect(btn!.hasAttribute("aria-expanded"), "no aria-expanded without a menu").toBe(false);

  const plainClick = vi.spyOn(inputs[0], "click");
  await click(btn!);
  expect(plainClick, "📎 clicks the plain input exactly once").toHaveBeenCalledTimes(1);
  expect(menuOf(el), "no attach-menu in the DOM").toBeNull();
};

/** Case 3's shared contract: the three Android inputs, in DOM order. */
const expectAndroidInputs = (el: HTMLElement): void => {
  const inputs = fileInputs(el);
  expect(inputs, "expected plain, camera and gallery inputs").toHaveLength(3);
  const [plain, camera, gallery] = inputs;
  expect(plain.hasAttribute("multiple")).toBe(true);
  expect(plain.hasAttribute("accept")).toBe(false);
  expect(plain.hasAttribute("capture")).toBe(false);
  expect(camera.getAttribute("accept")).toBe("image/*");
  expect(camera.getAttribute("capture")).toBe("environment");
  expect(camera.hasAttribute("multiple"), "the camera picks ONE photo").toBe(false);
  expect(gallery.getAttribute("accept")).toBe("image/*");
  expect(gallery.hasAttribute("multiple")).toBe(true);
  expect(gallery.hasAttribute("capture"), "the gallery has no capture").toBe(false);
};

describe("Composer — non-Android: plain picker only (master DOM)", () => {
  it("default UA: one plain input, no menu attributes on 📎, 📎 clicks the plain input, no menu", async () => {
    const el = await mount(makeProps());
    await expectPlainOnlyDom(el);
  });

  it("iPhone (coarse pointer): still the plain picker only — pointer type is not the gate", async () => {
    mockUA(IPHONE_UA);
    touchMatches = true;
    const el = await mount(makeProps());
    await expectPlainOnlyDom(el);
  });

  it("iPad reporting as Macintosh (coarse pointer): plain picker only", async () => {
    mockUA(IPAD_AS_MAC_UA);
    touchMatches = true;
    const el = await mount(makeProps());
    await expectPlainOnlyDom(el);
  });

  it("Windows (coarse pointer, touch laptop): plain picker only", async () => {
    mockUA(WINDOWS_UA);
    touchMatches = true;
    const el = await mount(makeProps());
    await expectPlainOnlyDom(el);
  });

  it("the button row starts input[type=file] → 📎 button → textarea, and 📎 keeps exactly master's attributes (type, title, aria-label, class)", async () => {
    const el = await mount(makeProps());

    const plain = fileInputs(el)[0];
    expect(plain, "the row must start with the plain file input").not.toBeNull();
    const btn = plain.nextElementSibling as HTMLButtonElement | null;
    expect(btn?.getAttribute("aria-label"), "the input's next sibling is the 📎 button").toBe("Attach files");
    expect(btn?.nextElementSibling?.tagName, "the 📎 button's next sibling is the textarea").toBe("TEXTAREA");

    expect(
      Array.from(btn!.attributes).map((a) => a.name).sort(),
      "the 📎 button keeps exactly master's attribute set (connected: no disabled, no menu extras)",
    ).toEqual(["aria-label", "class", "title", "type"]);
  });
});

describe("Composer — Android: three inputs, 📎 menu attributes", () => {
  it("coarse pointer: plain, camera, gallery in order with the spec attributes; 📎 has aria-haspopup=menu, aria-expanded=false; no menu", async () => {
    mockUA(ANDROID_UA);
    touchMatches = true;
    const el = await mount(makeProps());

    expectAndroidInputs(el);
    const btn = attachButtonOf(el)!;
    expect(btn.getAttribute("aria-haspopup")).toBe("menu");
    expect(btn.getAttribute("aria-expanded"), "the menu starts closed").toBe("false");
    expect(menuOf(el), "no menu while closed").toBeNull();
  });

  it("fine pointer: the same three inputs and menu attributes (the UA is the gate, not the pointer)", async () => {
    mockUA(ANDROID_UA);
    const el = await mount(makeProps());

    expectAndroidInputs(el);
    const btn = attachButtonOf(el)!;
    expect(btn.getAttribute("aria-haspopup")).toBe("menu");
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    expect(menuOf(el)).toBeNull();
  });

  // Re-gated from the old coarse-pointer block onto the Android UA:
  it("a camera input exists with accept='image/*', capture='environment' and no multiple", async () => {
    mockUA(ANDROID_UA);
    const el = await mount(makeProps());

    const camera = cameraInput(el);
    expect(
      camera,
      "expected a camera file input on an Android device",
    ).not.toBeNull();
    expect(camera!.getAttribute("accept")).toBe("image/*");
    expect(camera!.getAttribute("capture")).toBe("environment");
    expect(
      camera!.hasAttribute("multiple"),
      "the camera input picks ONE photo, not a file list",
    ).toBe(false);
  });

  it("the main input is still the FIRST input[type=file] and has no accept", async () => {
    mockUA(ANDROID_UA);
    const el = await mount(makeProps());

    const inputs = fileInputs(el);
    expect(inputs.length, "expected at least one file input").toBeGreaterThanOrEqual(1);
    const main = inputs[0];
    expect(
      main.getAttribute("capture"),
      "the first file input must be the main picker, not the camera",
    ).toBeNull();
    expect(
      main.hasAttribute("accept"),
      "the main picker accepts any file type (the server's 415 is the gate)",
    ).toBe(false);
  });

  it("firing change on the camera input with one file calls attachments.add with [file]", async () => {
    mockUA(ANDROID_UA);
    const add = vi.fn();
    const api = attachmentsApi({ add });
    const el = await t12Mount(makeProps(), api, true);

    const camera = cameraInput(el);
    expect(camera, "the camera input must exist to fire change on it").not.toBeNull();
    const photo = new File(["jpeg-bytes"], "photo.jpg", { type: "image/jpeg" });
    await act(async () => {
      Object.defineProperty(camera!, "files", { value: [photo], configurable: true });
      camera!.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(
      add,
      "the camera input rides the same pick handler as the main input",
    ).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([photo]);
  });
});

describe("Composer — the 📷 'Take photo' button is gone", () => {
  it.each([
    ["desktop", WINDOWS_UA],
    ["iPhone", IPHONE_UA],
    ["Android", ANDROID_UA],
  ])("%s: no button named 'Take photo' (even on a coarse pointer)", async (_platform, ua) => {
    mockUA(ua);
    touchMatches = true; // the old 📷 button rendered on every coarse pointer
    const el = await mount(makeProps());

    expect(controlByName(el, "Take photo"), "the Take photo button is deleted").toBeNull();
  });
});

describe("Composer — Android attach menu: open, close, pick", () => {
  // Android mount with all three inputs click-spied and the menu opened.
  // The input-count check runs BEFORE the spies so a missing input fails for
  // the right reason (menu absent) instead of crashing on an undefined node.
  const openAttachMenu = async (): Promise<{
    el: HTMLElement;
    clicks: { plain: ReturnType<typeof vi.spyOn>; camera: ReturnType<typeof vi.spyOn>; gallery: ReturnType<typeof vi.spyOn> };
  }> => {
    mockUA(ANDROID_UA);
    const el = await mount(makeProps());
    const [plain, camera, gallery] = fileInputs(el);
    expect(
      fileInputs(el),
      "Android renders the plain, camera and gallery inputs",
    ).toHaveLength(3);
    const clicks = {
      plain: vi.spyOn(plain, "click"),
      camera: vi.spyOn(camera, "click"),
      gallery: vi.spyOn(gallery, "click"),
    };
    await click(attachButtonOf(el)!);
    return { el, clicks };
  };

  it("clicking 📎 opens the menu: role=menu aria-label=Attach, three items in order with exact text, aria-expanded=true, NO input clicked; clicking 📎 again closes it", async () => {
    const { el, clicks } = await openAttachMenu();

    const menu = menuOf(el);
    expect(menu, "the menu must be open after clicking 📎").not.toBeNull();
    expect(menu!.getAttribute("role")).toBe("menu");
    expect(menu!.getAttribute("aria-label")).toBe("Attach");
    const items = menuItemsOf(el);
    expect(
      items.map((b) => (b.textContent ?? "").trim()),
      "three menuitems in the spec order with exact text",
    ).toEqual(["Take a photo", "Choose from gallery", "Pick a file"]);
    expect(
      items.every((b) => b.getAttribute("role") === "menuitem"),
      "each item carries role=menuitem",
    ).toBe(true);
    expect(
      attachButtonOf(el)!.getAttribute("aria-expanded"),
      "📎 reports the menu as expanded",
    ).toBe("true");
    for (const spy of Object.values(clicks))
      expect(spy, "opening the menu must not click any input").not.toHaveBeenCalled();

    await click(attachButtonOf(el)!);
    expect(menuOf(el), "clicking 📎 again closes the menu").toBeNull();
    expect(attachButtonOf(el)!.getAttribute("aria-expanded")).toBe("false");
  });

  it("pointerdown on document.body closes the open menu", async () => {
    const { el } = await openAttachMenu();
    expect(menuOf(el), "menu open before the outside press").not.toBeNull();

    await act(async () => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true, cancelable: true }));
    });

    expect(menuOf(el), "a pointerdown outside menu and 📎 closes the menu").toBeNull();
  });

  it("pointerdown inside the menu does NOT close it", async () => {
    const { el } = await openAttachMenu();
    const item = menuItemsOf(el)[0];
    expect(item, "the menu has items to press").not.toBeNull();

    await act(async () => {
      item!.dispatchEvent(new Event("pointerdown", { bubbles: true, cancelable: true }));
    });

    expect(menuOf(el), "a pointerdown inside the menu keeps it open").not.toBeNull();
  });

  it("Escape keydown on document closes the open menu", async () => {
    const { el } = await openAttachMenu();

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });

    expect(menuOf(el), "Escape closes the menu").toBeNull();
  });

  it("picking 'Take a photo' clicks ONLY the camera input once and closes the menu", async () => {
    const { el, clicks } = await openAttachMenu();
    const [camItem] = menuItemsOf(el);
    expect(camItem, "the camera item is first").not.toBeNull();

    await click(camItem!);

    expect(clicks.camera, "the camera input is clicked once").toHaveBeenCalledTimes(1);
    expect(clicks.plain, "the plain input is not clicked").not.toHaveBeenCalled();
    expect(clicks.gallery, "the gallery input is not clicked").not.toHaveBeenCalled();
    expect(menuOf(el), "the menu closes after picking").toBeNull();
  });

  it("picking 'Choose from gallery' clicks ONLY the gallery input once and closes the menu", async () => {
    const { el, clicks } = await openAttachMenu();
    const [, galItem] = menuItemsOf(el);
    expect(galItem, "the gallery item is second").not.toBeNull();

    await click(galItem!);

    expect(clicks.gallery, "the gallery input is clicked once").toHaveBeenCalledTimes(1);
    expect(clicks.plain, "the plain input is not clicked").not.toHaveBeenCalled();
    expect(clicks.camera, "the camera input is not clicked").not.toHaveBeenCalled();
    expect(menuOf(el), "the menu closes after picking").toBeNull();
  });

  it("picking 'Pick a file' clicks ONLY the plain input once and closes the menu", async () => {
    const { el, clicks } = await openAttachMenu();
    const [, , fileItem] = menuItemsOf(el);
    expect(fileItem, "the file item is third").not.toBeNull();

    await click(fileItem!);

    expect(clicks.plain, "the plain input is clicked once").toHaveBeenCalledTimes(1);
    expect(clicks.camera, "the camera input is not clicked").not.toHaveBeenCalled();
    expect(clicks.gallery, "the gallery input is not clicked").not.toHaveBeenCalled();
    expect(menuOf(el), "the menu closes after picking").toBeNull();
  });
});

describe("Composer — Android attach menu: pick handler and gating", () => {
  it("firing change on the gallery input with two files calls attachments.add with both and resets the value to ''", async () => {
    mockUA(ANDROID_UA);
    const add = vi.fn();
    const api = attachmentsApi({ add });
    const el = await t12Mount(makeProps(), api, true);

    const gallery = galleryInputOf(el);
    expect(gallery, "the gallery input must exist on Android").not.toBeNull();
    const a = new File(["a-bytes"], "a.jpg", { type: "image/jpeg" });
    const b = new File(["b-bytes"], "b.png", { type: "image/png" });
    await act(async () => {
      Object.defineProperty(gallery!, "files", { value: [a, b], configurable: true });
      gallery!.dispatchEvent(new Event("change", { bubbles: true }));
    });

    expect(
      add,
      "the gallery input rides the same pick handler as the main input",
    ).toHaveBeenCalledTimes(1);
    expect(add).toHaveBeenCalledWith([a, b]);
    expect(gallery!.value, "the input resets so re-picking fires change again").toBe("");
  });

  it("not connected: 📎 is disabled and clicking it opens no menu", async () => {
    mockUA(ANDROID_UA);
    const el = await mount(makeProps({ connected: false }));

    const btn = attachButtonOf(el);
    expect(btn, "the 📎 button renders (disabled) while not connected").not.toBeNull();
    expect(btn!.disabled, "connected is the only gate, as today").toBe(true);
    await click(btn!);
    expect(menuOf(el), "a disabled 📎 opens nothing").toBeNull();
  });

  it("with the slash palette showing, clicking 📎 hides the palette while the menu is open", async () => {
    mockUA(ANDROID_UA);
    const el = await mount(
      makeProps({ commands: [{ name: "help", description: "Show help", takes_args: false }] }),
    );
    await typeInto(el.querySelector("textarea")!, "/h");
    const slashRows = allButtons(el).filter((b) => (b.textContent ?? "").trim().startsWith("/"));
    expect(slashRows, "the palette shows for a bare /prefix").toHaveLength(1);

    await click(attachButtonOf(el)!);

    expect(menuOf(el), "the menu is open").not.toBeNull();
    const rowsAfter = allButtons(el).filter((b) => (b.textContent ?? "").trim().startsWith("/"));
    expect(rowsAfter, "the two popups never show together").toHaveLength(0);
  });
});

describe("Composer — non-vision notice (role=status under the chip row)", () => {
  it("shows the notice (exact text, role=status) for a done image chip with supportsVision=false", async () => {
    const api = attachmentsApi({
      pending: [doneImageChip("id-img")],
      ids: ["id-img"],
      ready: true,
    });
    const el = await t12Mount(makeProps(), api, false);

    const notices = statusEls(el);
    expect(notices.length, "expected exactly one status notice").toBe(1);
    expect(
      notices[0].textContent,
      "the notice must be the NO_VISION sentence verbatim",
    ).toBe(NO_VISION_NOTICE);
    // Informational, not a gate: sending stays enabled.
    expect(buttonWithText(el, "Send")!.disabled).toBe(false);
  });

  it("shows the notice with a remote base (/r/home) too", async () => {
    const api = attachmentsApi({
      pending: [doneImageChip("id-img")],
      ids: ["id-img"],
      ready: true,
    });
    const el = await mount({
      ...makeProps(),
      attachments: api,
      supportsVision: false,
      base: "/r/home",
    } as unknown as ComposerProps);

    const notices = statusEls(el);
    expect(notices.length, "expected exactly one status notice on a remote base").toBe(1);
    expect(notices[0].textContent).toBe(NO_VISION_NOTICE);
  });

  it("shows no notice when supportsVision=true", async () => {
    const api = attachmentsApi({
      pending: [doneImageChip("id-img")],
      ids: ["id-img"],
      ready: true,
    });
    const el = await t12Mount(makeProps(), api, true);

    expect(statusEls(el), "no status notice for a vision model").toHaveLength(0);
  });

  it("shows no notice when the only chip is a PDF", async () => {
    const api = attachmentsApi({
      pending: [donePdfChip("id-pdf")],
      ids: ["id-pdf"],
      ready: true,
    });
    const el = await t12Mount(makeProps(), api, false);

    expect(
      statusEls(el),
      "non-image files never trigger the vision notice",
    ).toHaveLength(0);
  });
});

// ─── Two-stage stop: Stop button label by `stopPending` ──────────────────
//
// Composer's `stopPending` prop mirrors the wire `AppState.stop_pending`.
// While a graceful stop is pending the Stop control's label/aria-label reads
// "Stop now — stopping after the current tool call" (pressing again
// escalates); normally it is the existing "Stop" / "Stop and discard …".
// Clicking while pending still sends a single {type:"stop"} — the backend
// decides between finishing gracefully and aborting immediately.

const PENDING_STOP_LABEL = "Stop now — stopping after the current tool call";

function stopMount(props: ComposerProps, stopPending?: boolean): Promise<HTMLDivElement> {
  return mount({ ...props, stopPending });
}

describe("Composer — Stop label by stopPending (two-stage stop)", () => {
  it("normally (stopPending unset/false) the Stop label is 'Stop'", async () => {
    for (const stopPending of [undefined, false]) {
      const el = await stopMount(makeProps({ isRunning: true }), stopPending);
      const stop = controlByName(el, "Stop");
      expect(stop, "expected a Stop control while running").not.toBeNull();
      expect(
        accessibleName(stop!),
        `expected the plain 'Stop' label when stopPending=${stopPending}`,
      ).toBe("Stop");
    }
  });

  it("stopPending=true: label and aria-label read the escalation sentence", async () => {
    const el = await stopMount(makeProps({ isRunning: true }), true);
    const stop = el.querySelector(`[aria-label="${PENDING_STOP_LABEL}"]`);
    expect(
      stop,
      `expected the Stop control's label/aria-label to be "${PENDING_STOP_LABEL}" while pending`,
    ).not.toBeNull();
  });

  it("stopPending=true with queued input: the escalation sentence (not the discard warning) is the label", async () => {
    const el = await stopMount(makeProps({ isRunning: true, pendingInput: 2 }), true);
    const stop = el.querySelector(`[aria-label="${PENDING_STOP_LABEL}"]`);
    expect(stop, "pending stop must win over the discard warning").not.toBeNull();
  });

  it("clicking Stop while pending still sends exactly one {type:'stop'} frame (the backend escalates)", async () => {
    const sent: InboundMessage[] = [];
    const el = await stopMount(
      makeProps({ isRunning: true, onStop: () => sent.push({ type: "stop" }) }),
      true,
    );
    const stop = el.querySelector(
      `[aria-label="${PENDING_STOP_LABEL}"]`,
    ) as HTMLButtonElement | null;
    expect(stop, "the pending Stop control must exist").not.toBeNull();
    await click(stop!);
    expect(sent).toEqual([{ type: "stop" }]);
  });

  it("idle: no Stop control exists, so stopPending renders nothing", async () => {
    const el = await stopMount(makeProps({ isRunning: false }), true);
    expect(controlByName(el, "Stop")).toBeNull();
    expect(el.textContent).not.toContain("Stop now");
  });
});

// ─── Mobile: right-rail clearance + keep-focus buttons ───────────────────
//
// The fixed right-hand TabbedDrawer rail (40px handles) exists from lg up
// only (phones get the Panels sheet). Contract (Tailwind classes ARE the
// layout contract — jsdom has no layout engine):
//   1. the Composer's outer container carries `lg:pr-12` (rail clearance
//      from lg up) and NO bare `pr-12`, so phones use the full width;
//   2. Stop, Queue and Pause call preventDefault() on `pointerdown`, so the
//      textarea keeps focus (tapping them must not collapse the soft
//      keyboard, which would shift the layout and cancel the tap).

/** Dispatch a cancelable, bubbling pointerdown and report whether a handler
 *  prevented it. A plain Event is used on purpose: React's `onPointerDown`
 *  listens for the native "pointerdown" type, and this works whether or not
 *  the jsdom build ships a PointerEvent constructor. */
const pointerDown = async (el: HTMLElement): Promise<boolean> => {
  const ev = new Event("pointerdown", { bubbles: true, cancelable: true });
  await act(async () => {
    el.dispatchEvent(ev);
  });
  return ev.defaultPrevented;
};

describe("Composer — rail clearance (desktop only)", () => {
  it("outer container has lg:pr-12 and no bare pr-12 (idle)", async () => {
    const el = await mount(makeProps());
    const outer = el.firstElementChild as HTMLElement;
    expect(outer.classList.contains("lg:pr-12")).toBe(true);
    expect(outer.classList.contains("pr-12"), "phones use the full width").toBe(false);
  });

  it("outer container keeps lg:pr-12 while a turn is running", async () => {
    const el = await mount(makeProps({ isRunning: true }));
    const outer = el.firstElementChild as HTMLElement;
    expect(outer.classList.contains("lg:pr-12")).toBe(true);
    expect(outer.classList.contains("pr-12")).toBe(false);
  });
});

describe("Composer — buttons keep textarea focus on pointerdown (mobile)", () => {
  it("Stop: pointerdown is default-prevented", async () => {
    const el = await mount(makeProps({ isRunning: true }));
    const stop = controlByName(el, "Stop")!;
    expect(await pointerDown(stop)).toBe(true);
  });

  it("Pause: pointerdown is default-prevented", async () => {
    const el = await mount(
      makeProps({ isRunning: true, subAgent: subAgent("running") }),
    );
    const pause = controlByName(el, "Pause")!;
    expect(pause).not.toBeNull();
    expect(await pointerDown(pause)).toBe(true);
  });

  it("Queue: pointerdown is default-prevented", async () => {
    const el = await mount(makeProps({ isRunning: true }));
    await typeInto(el.querySelector("textarea")!, "queue me");
    const queue = buttonWithText(el, "Queue")!;
    expect(queue).not.toBeNull();
    expect(await pointerDown(queue)).toBe(true);
  });

  it("Stop: textarea stays focused through pointerdown + click, and onStop still fires once", async () => {
    const props = makeProps({ isRunning: true });
    const el = await mount(props);
    const textarea = el.querySelector("textarea")!;
    const stop = controlByName(el, "Stop")!;

    // jsdom does not move focus on pointer input, so emulate the browser
    // default: unless the page cancelled pointerdown, focus goes to the
    // pressed button. (Document-level bubble listener runs after React's
    // root handler, so it sees the final `defaultPrevented`.)
    const emulateBrowserFocus = (e: Event) => {
      if (!e.defaultPrevented) (e.target as HTMLElement).focus();
    };
    document.addEventListener("pointerdown", emulateBrowserFocus);
    try {
      textarea.focus();
      expect(document.activeElement).toBe(textarea);

      await pointerDown(stop);
      await click(stop);

      expect(document.activeElement).toBe(textarea);
      expect(props.onStop).toHaveBeenCalledTimes(1);
    } finally {
      document.removeEventListener("pointerdown", emulateBrowserFocus);
    }
  });

  it("regression: clicking Stop still calls onStop exactly once", async () => {
    const props = makeProps({ isRunning: true });
    const el = await mount(props);
    await click(controlByName(el, "Stop")!);
    expect(props.onStop).toHaveBeenCalledTimes(1);
  });
});

// ─── `hidden` prop (desktop-tab-composers, keep-alive) ───────────────────
//
// Contract: `hidden?: boolean` (default false) sets the `hidden` ATTRIBUTE on
// the composer's root element and nothing else — the component stays mounted,
// so typed text and the slash-palette state (`selected`, `dismissed`) survive
// a hidden→visible round trip. The textarea auto-grow layout effect depends
// on `[text, hidden]` and returns early while hidden (a `display:none`
// textarea measures `scrollHeight === 0`, which would collapse it to MIN_H);
// when un-hidden the height is re-measured.
//
// jsdom has no layout, so `scrollHeight` is stubbed on the prototype and
// restored in afterEach.

const SLASH_COMMANDS: SlashCommand[] = [
  { name: "help", description: "Show help", takes_args: false },
  { name: "history", description: "Show history", takes_args: false },
  { name: "hooks", description: "List hooks", takes_args: false },
];

/** Re-render the CURRENT root with new props (keeps component state). */
async function rerenderComposer(props: ComposerProps & { hidden?: boolean }): Promise<void> {
  await act(async () => {
    root!.render(<Composer {...(props as ComposerProps)} />);
  });
}

const composerRoot = (el: HTMLElement): HTMLElement => el.firstElementChild as HTMLElement;
const textareaOf = (el: HTMLElement): HTMLTextAreaElement => el.querySelector("textarea")!;
/** Palette rows are the only buttons whose text starts with "/". */
const paletteRows = (el: HTMLElement): HTMLButtonElement[] =>
  allButtons(el).filter((b) => (b.textContent ?? "").trim().startsWith("/"));
const highlightedRow = (el: HTMLElement): string | null =>
  paletteRows(el).find((b) => b.classList.contains("bg-zinc-800"))?.textContent?.trim() ?? null;

let scrollHeightNow = 0;
function stubScrollHeight(): void {
  Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", {
    configurable: true,
    get: () => scrollHeightNow,
  });
}
afterEach(() => {
  delete (HTMLTextAreaElement.prototype as unknown as Record<string, unknown>)["scrollHeight"];
  scrollHeightNow = 0;
});

const keydown = async (el: HTMLElement, key: string): Promise<void> => {
  await act(async () => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  });
};

describe("Composer — hidden prop: root attribute", () => {
  it("renders the root WITHOUT a hidden attribute when the prop is omitted", async () => {
    const el = await mount(makeProps());
    expect(composerRoot(el).hasAttribute("hidden")).toBe(false);
  });

  it("renders the root WITHOUT a hidden attribute when hidden={false}", async () => {
    const el = await mount({ ...makeProps(), hidden: false } as unknown as ComposerProps);
    expect(composerRoot(el).hasAttribute("hidden")).toBe(false);
  });

  it("sets the hidden attribute on the root element when hidden={true}", async () => {
    const el = await mount({ ...makeProps(), hidden: true } as unknown as ComposerProps);
    expect(composerRoot(el).hasAttribute("hidden")).toBe(true);
  });

  it("stays mounted while hidden: the textarea and attach input are still in the DOM", async () => {
    const el = await mount({ ...makeProps(), hidden: true } as unknown as ComposerProps);
    expect(el.querySelector("textarea")).not.toBeNull();
    expect(el.querySelector('input[type="file"]')).not.toBeNull();
  });

  it("toggling hidden false → true → false flips the attribute each time", async () => {
    const props = makeProps();
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    expect(composerRoot(el).hasAttribute("hidden")).toBe(false);
    await rerenderComposer({ ...props, hidden: true });
    expect(composerRoot(el).hasAttribute("hidden")).toBe(true);
    await rerenderComposer({ ...props, hidden: false });
    expect(composerRoot(el).hasAttribute("hidden")).toBe(false);
  });

  it("keeps the existing root classes (lg:pr-12 clearance) alongside hidden", async () => {
    const el = await mount({ ...makeProps(), hidden: true } as unknown as ComposerProps);
    expect(composerRoot(el).classList.contains("lg:pr-12")).toBe(true);
    expect(composerRoot(el).classList.contains("pr-12")).toBe(false);
  });
});

describe("Composer — hidden prop: state survives hide/show", () => {
  it("keeps the typed text across hidden true → false", async () => {
    const props = makeProps();
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    const ta = textareaOf(el);
    await typeInto(ta, "half-written thought");

    await rerenderComposer({ ...props, hidden: true });
    expect(textareaOf(el)).toBe(ta); // same node: not remounted
    expect(ta.value).toBe("half-written thought");

    await rerenderComposer({ ...props, hidden: false });
    expect(textareaOf(el)).toBe(ta);
    expect(ta.value).toBe("half-written thought");
  });

  it("keeps multi-line and unicode text intact across hide/show", async () => {
    const props = makeProps();
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    const text = "línea 1\nline 2 — ✓ 日本語 🚀";
    await typeInto(textareaOf(el), text);
    await rerenderComposer({ ...props, hidden: true });
    await rerenderComposer({ ...props, hidden: false });
    expect(textareaOf(el).value).toBe(text);
  });

  it("keeps the highlighted palette row (selected index) across hide/show", async () => {
    const props = makeProps({ commands: SLASH_COMMANDS });
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    const ta = textareaOf(el);
    await typeInto(ta, "/h");
    expect(paletteRows(el)).toHaveLength(3);
    await keydown(ta, "ArrowDown");
    await keydown(ta, "ArrowDown");
    expect(highlightedRow(el)).toContain("/hooks");

    await rerenderComposer({ ...props, hidden: true });
    await rerenderComposer({ ...props, hidden: false });

    expect(ta.value).toBe("/h");
    expect(paletteRows(el)).toHaveLength(3);
    expect(highlightedRow(el)).toContain("/hooks");
  });

  it("keeps a dismissed palette dismissed across hide/show (Esc is remembered)", async () => {
    const props = makeProps({ commands: SLASH_COMMANDS });
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    const ta = textareaOf(el);
    await typeInto(ta, "/h");
    expect(paletteRows(el)).toHaveLength(3);
    await keydown(ta, "Escape");
    expect(paletteRows(el)).toHaveLength(0);

    await rerenderComposer({ ...props, hidden: true });
    await rerenderComposer({ ...props, hidden: false });

    expect(ta.value).toBe("/h");
    expect(paletteRows(el), "dismissal must survive a tab switch").toHaveLength(0);
  });

  it("keeps an OPEN palette open across hide/show", async () => {
    const props = makeProps({ commands: SLASH_COMMANDS });
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    await typeInto(textareaOf(el), "/he");
    expect(paletteRows(el).map((b) => b.textContent)).toHaveLength(1);

    await rerenderComposer({ ...props, hidden: true });
    await rerenderComposer({ ...props, hidden: false });

    expect(paletteRows(el)).toHaveLength(1);
    expect(paletteRows(el)[0].textContent).toContain("/help");
  });
});

describe("Composer — hidden prop: textarea auto-grow", () => {
  it("does not measure or resize the textarea while hidden (mount hidden, type)", async () => {
    stubScrollHeight();
    scrollHeightNow = 0; // what a display:none textarea reports
    const el = await mount({ ...makeProps(), hidden: true } as unknown as ComposerProps);
    const ta = textareaOf(el);

    expect(ta.style.height, "no height may be written on a hidden mount").toBe("");
    await typeInto(ta, "typed while hidden");
    expect(ta.style.height, "typing while hidden must not collapse the box to MIN_H").toBe("");
  });

  it("re-measures when un-hidden: height follows scrollHeight", async () => {
    stubScrollHeight();
    scrollHeightNow = 0;
    const props = makeProps();
    const el = await mount({ ...props, hidden: true } as unknown as ComposerProps);
    const ta = textareaOf(el);
    await typeInto(ta, "line1\nline2\nline3\nline4");

    scrollHeightNow = 96; // now visible, so the real height is measurable
    await rerenderComposer({ ...props, hidden: false });

    expect(ta.style.height).toBe("96px");
  });

  it("clamps the re-measured height to the 192px cap", async () => {
    stubScrollHeight();
    scrollHeightNow = 0;
    const props = makeProps();
    const el = await mount({ ...props, hidden: true } as unknown as ComposerProps);
    await typeInto(textareaOf(el), "x");
    scrollHeightNow = 900;
    await rerenderComposer({ ...props, hidden: false });
    expect(textareaOf(el).style.height).toBe("192px");
  });

  it("clamps the re-measured height up to the 40px minimum", async () => {
    stubScrollHeight();
    scrollHeightNow = 0;
    const props = makeProps();
    const el = await mount({ ...props, hidden: true } as unknown as ComposerProps);
    await typeInto(textareaOf(el), "x");
    scrollHeightNow = 12;
    await rerenderComposer({ ...props, hidden: false });
    expect(textareaOf(el).style.height).toBe("40px");
  });

  it("hiding does not clobber the last measured height (no 0 → MIN_H collapse)", async () => {
    stubScrollHeight();
    scrollHeightNow = 120;
    const props = makeProps();
    const el = await mount({ ...props, hidden: false } as unknown as ComposerProps);
    await typeInto(textareaOf(el), "a\nb\nc");
    expect(textareaOf(el).style.height).toBe("120px");

    scrollHeightNow = 0; // display:none
    await rerenderComposer({ ...props, hidden: true });

    expect(textareaOf(el).style.height).toBe("120px");
  });

  it("without the hidden prop the textarea still grows on text change (unchanged)", async () => {
    stubScrollHeight();
    scrollHeightNow = 64;
    const el = await mount(makeProps());
    await typeInto(textareaOf(el), "grow me");
    expect(textareaOf(el).style.height).toBe("64px");
  });
});

// ─── Batch 1: the phone contract (phone: true) ───────────────────────────
//
// The `phone` prop (default false; App derives it as `!isDesktop`) switches
// the shared Composer to the mock's phone face (fix-plan §3, items 1.2,
// 1.12–1.20). The desktop contract above is untouched: `phone` false or
// absent renders today's DOM exactly (R1).
//
// RED until the prop lands: `Composer` has no `phone` prop, the root has no
// `data-testid="composer"`, the placeholder is "Type a message…" and an
// idle empty box always shows a disabled Send — the phone assertions below
// fail on that missing behaviour, not on setup.

function phoneMount(props: ComposerProps): Promise<HTMLDivElement> {
  return mount({ ...props, phone: true } as unknown as ComposerProps);
}

const composerRootTestid = (el: HTMLElement): HTMLElement | null =>
  el.querySelector('[data-testid="composer"]');

const textareaPlaceholder = (el: HTMLElement): string =>
  el.querySelector("textarea")?.getAttribute("placeholder") ?? "";

describe("Composer — phone (phone: true)", () => {
  it("idle and empty: no Send button; typing shows a 'Send' button (B28)", async () => {
    const el = await phoneMount(makeProps({ isRunning: false }));
    expect(
      buttonWithText(el, "Send"),
      "an idle, empty phone composer shows no dispatch button",
    ).toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();

    await typeInto(el.querySelector("textarea")!, "hello");
    expect(
      buttonWithText(el, "Send"),
      "typing text brings the 'Send' button back",
    ).not.toBeNull();
  });

  it("placeholder reads 'Message' idle, 'Queue a message…' running, 'Connecting…' offline, 'Message the orchestrator…' while watching (B25)", async () => {
    expect(textareaPlaceholder(await phoneMount(makeProps()))).toBe("Message");
    expect(
      textareaPlaceholder(await phoneMount(makeProps({ isRunning: true }))),
    ).toBe("Queue a message…");
    expect(
      textareaPlaceholder(await phoneMount(makeProps({ connected: false }))),
    ).toBe("Connecting…");
    expect(
      textareaPlaceholder(await phoneMount(makeProps({ watchingRole: "researcher" }))),
    ).toBe("Message the orchestrator…");
  });

  it("running and empty: Pause/Stop by aria-label, no Send; running with text: a 'Queue' button (B26–B28)", async () => {
    const props = makeProps({ isRunning: true, subAgent: subAgent("running") });
    let el = await phoneMount(props);
    expect(controlByName(el, "Pause"), "Pause is present while working").not.toBeNull();
    expect(controlByName(el, "Stop"), "Stop is present while working").not.toBeNull();
    expect(buttonWithText(el, "Send"), "no Send while running").toBeNull();
    expect(buttonWithText(el, "Queue"), "no Queue while empty").toBeNull();

    el = await phoneMount(props);
    await typeInto(el.querySelector("textarea")!, "queue me");
    expect(
      buttonWithText(el, "Queue"),
      "running with text shows the 'Queue' button",
    ).not.toBeNull();
    expect(controlByName(el, "Stop")).toBeNull();
  });

  it("the root carries the bottom safe-area inset (B29)", async () => {
    const el = await phoneMount(makeProps());
    const composer = composerRootTestid(el);
    expect(
      composer,
      'the composer root carries data-testid="composer"',
    ).not.toBeNull();
    // The class IS the contract here (jsdom has no env() to resolve): the
    // phone bottom padding is env(safe-area-inset-bottom) + 18px.
    expect(
      composer!.className,
      "the composer region pads out the bottom safe area",
    ).toContain("safe-area-inset-bottom");
  });

  it("the watch note renders outside the pill and its Clear calls onClearWatch (B30)", async () => {
    const onClearWatch = vi.fn();
    const el = await phoneMount(makeProps({ watchingRole: "researcher", onClearWatch }));
    const note = el.querySelector('[data-testid="composer-watch-note"]');
    expect(note, "the watching note renders on the phone").not.toBeNull();
    expect(note!.textContent).toContain("your message goes to the orchestrator");
    // Above the pill: the first child of the relative wrapper, not inside
    // the input box.
    const box = el.querySelector(".rounded-xl");
    expect(box, "the composer pill renders").not.toBeNull();
    expect(
      note!.parentElement?.contains(box) && note === note.parentElement!.firstElementChild,
      "the note is the first child of the wrapper, above the pill",
    ).toBe(true);
    expect(box!.contains(note), "the note is NOT inside the pill").toBe(false);

    const clear = Array.from(note!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").trim() === "Clear",
    );
    expect(clear, "the note carries a 'Clear' button").not.toBeNull();
    await click(clear!);
    expect(onClearWatch).toHaveBeenCalledTimes(1);
  });
});

// ─── Batch 1: desktop lock (R1) ──────────────────────────────────────────

describe("Composer — desktop lock (R1): phone absent keeps the old contract", () => {
  it("without phone the placeholder stays 'Type a message…' and an empty idle box still shows a disabled 'Send'", async () => {
    const el = await mount(makeProps());
    expect(textareaPlaceholder(el)).toBe("Type a message…");
    const send = buttonWithText(el, "Send");
    expect(send, "the idle desktop box keeps its 'Send' button").not.toBeNull();
    expect(send!.disabled, "with no content it is disabled, as today").toBe(true);
    expect(
      el.querySelector('[data-testid="composer"]'),
      "no phone-only root testid on the desktop render",
    ).toBeNull();
  });
});
