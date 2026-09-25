// Regression + new-spec tests for the Composer's action row.
//
// ─── New spec (icon-only running controls, no media queries) ─────────────
// Let `hasContent` = the composer has non-whitespace text OR at least one
// attached image (the existing `canSend` condition, minus the `connected`
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

/** Attaches a fake image via the hidden file input, driving the same path a
 *  user picking a file would (`onPickFiles` → `addFiles`, which reads it
 *  via `FileReader.readAsDataURL`). jsdom has no global `DataTransfer`, so
 *  the file input's `files` is stubbed directly with a minimal array-like
 *  `FileList` (indexable + `length` + iterable is all `Array.from` needs).
 *
 *  jsdom fires `FileReader.onload` on a later task (not a fixed number of
 *  ticks), so the helper polls — inside `act`, so the resulting `setImages`
 *  render is wrapped — until the attachment chip is committed. A valid
 *  under-size image is guaranteed to produce one; if it never appears the
 *  caller's own assertions (chip present / Queue enabled) fail. */
const attachImage = async (el: HTMLDivElement, name = "photo.png"): Promise<void> => {
  const input = el.querySelector('input[type="file"]') as HTMLInputElement;
  const file = new File(["fake-bytes"], name, { type: "image/png" });
  const fileList = {
    0: file,
    length: 1,
    item: (i: number) => (i === 0 ? file : null),
    [Symbol.iterator]: function* () {
      yield file;
    },
  } as unknown as FileList;
  Object.defineProperty(input, "files", { value: fileList, configurable: true });
  await act(async () => {
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const deadline = Date.now() + 2000;
  while (
    !el.querySelector(`img[alt="${name}"]`) &&
    Date.now() < deadline
  ) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
  }
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

  it("running + attached image, no text: hasContent via image alone hides Stop and shows Queue", async () => {
    // Skips nothing: the file-input stub (see `attachImage`) makes this
    // feasible without a real `DataTransfer` (jsdom doesn't implement one).
    const el = await mount(makeProps({ isRunning: true, subAgent: null }));

    // Sanity: before attaching, we're in the "no content" state.
    expect(controlByName(el, "Stop")).not.toBeNull();
    expect(buttonWithText(el, "Queue")).toBeNull();

    await attachImage(el, "shot.png");

    expect(
      buttonWithText(el, "Queue"),
      "expected 'Queue' once an image is attached, even with no typed text",
    ).not.toBeNull();
    expect(
      controlByName(el, "Stop"),
      "expected Stop to disappear once an image makes hasContent true",
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
    expect(props.onSend).toHaveBeenCalledWith("queue me up");
    expect(props.onStop).not.toHaveBeenCalled();
    expect(textarea.value).toBe("");
  });

  it("clicking 'Queue' with an attached image and no text sends the image token and clears the attachment chip", async () => {
    const props = makeProps({ isRunning: true });
    const el = await mount(props);
    await attachImage(el, "shot.png");
    expect(el.querySelector('img[alt="shot.png"]')).not.toBeNull();

    const queue = buttonWithText(el, "Queue")!;
    await click(queue);

    expect(props.onSend).toHaveBeenCalledTimes(1);
    expect((props.onSend as ReturnType<typeof vi.fn>).mock.calls[0][0]).toMatch(
      /^\[img:data:image\/png;base64,.*\]$/,
    );
    expect(props.onStop).not.toHaveBeenCalled();
    expect(
      el.querySelector('img[alt="shot.png"]'),
      "expected the attachment chip to clear after sending",
    ).toBeNull();
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
