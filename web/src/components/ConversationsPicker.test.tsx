// Tests for the ConversationsPicker kill-error line.
//
// These lock the target behaviour from the spec:
//   1. When the `error` prop is set and the picker is not loading, a red
//      error line renders at the end of the dropdown panel (role="alert")
//      with the kill-refusal message.
//   2. The error line is hidden while a reopen is loading (opening the
//      picker re-requests the list; the stale error must not flash over
//      the spinner).
//   3. No alert renders when `error` is null.
//
// Harness modeled on CwdPicker.test.tsx (createRoot/act off
// react-dom/client; jsdom is auto-enabled for `src/components/**` by
// vitest.config.ts).
//
// IMPORTANT — expected to be RED before implementation: `error` is not
// declared in ConversationsPicker's props today, so the component ignores
// it and no alert ever renders. We pass props through an
// `as unknown as Parameters<typeof ConversationsPicker>[0]` cast so the
// tests still *run* under vitest (esbuild transform, no type check) and
// fail on the missing behaviour rather than refusing to execute — same
// technique as CwdPicker.test.tsx. `npm run typecheck` would also flag
// the fixture, which is expected and fine.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConversationsPicker } from "./ConversationsPicker";
import type { ConversationSummary } from "../state";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching CwdPicker.test.tsx's setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

// The real ConversationsPicker prop interface as declared today
// (ConversationsPicker.tsx:48-64) plus the single new `error` prop the
// feature adds.
interface ConversationsPickerPropsWithError {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  onOpen: () => void;
  onLoad: (id: string) => void;
  onKill: (id: string) => void;
  dropUp?: boolean;
  error: string | null;
}

const KILL_ERROR = "Can't kill a running conversation — press Stop first.";

const convo = (id: string, active = false): ConversationSummary => ({
  id,
  name: `convo ${id}`,
  updated_at: "2026-10-04T00:00:00Z",
  message_count: 1,
  model: "ollama/llama3",
  active,
});

const baseProps: ConversationsPickerPropsWithError = {
  conversations: [convo("a1", true), convo("b2")],
  hasTranscript: false,
  onOpen: () => {},
  onLoad: () => {},
  onKill: () => {},
  error: null,
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

/** Mount (or re-render, with a fresh `conversations` reference) the picker. */
async function render(props: ConversationsPickerPropsWithError): Promise<HTMLDivElement> {
  if (!root) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
    // `error` is not part of ConversationsPicker's declared props today, so
    // we cast through `unknown` to pass it anyway — the point of these tests
    // is to prove the prop is currently ignored, not to fight the type
    // system. Post-implementation this cast becomes a no-op identity.
    root!.render(
      <ConversationsPicker
        {...(props as unknown as Parameters<typeof ConversationsPicker>[0])}
      />,
    );
  });
  return container!;
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
});

/** Click the "conversations" chip to open the dropdown (the first button). */
async function openPicker(el: HTMLDivElement): Promise<void> {
  const chip = el.querySelector("button");
  expect(chip, "conversations chip button must render").not.toBeNull();
  await act(async () => {
    chip!.click();
  });
}

/** The kill-error line, once it exists: role="alert" inside the panel. */
function findAlert(el: HTMLDivElement): HTMLElement | null {
  return el.querySelector('[role="alert"]');
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("ConversationsPicker — kill error line", () => {
  // 1. The refused-kill message renders as an alert at the end of the panel.
  it("shows red error line under the list when error is set", async () => {
    const el = await render({ ...baseProps, error: KILL_ERROR });

    await openPicker(el);

    // Opening starts a reload; the alert only shows once a fresh list
    // (new `conversations` reference) lands and loading clears.
    await render({
      ...baseProps,
      error: KILL_ERROR,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });

    const alert = findAlert(el);
    expect(alert, "an alert element must render when error is set").not.toBeNull();
    expect(alert!.textContent).toContain("Can't kill a running conversation");
    expect(alert!.textContent).toContain("press Stop first");
  });

  // 2. The error is hidden while a reopen is loading: opening the picker
  //    re-requests the list (onOpen), and the stale error must not flash
  //    over the spinner. Once the fresh list lands (a new `conversations`
  //    reference) the error shows again; reopening hides it once more.
  it("hides error while a reopen is loading", async () => {
    const el = await render({ ...baseProps, error: KILL_ERROR });

    // Open: the reopen is in flight (onOpen is a no-op, so no list lands) →
    // the error line must be hidden behind the loading state.
    await openPicker(el);
    expect(el.textContent).toContain("Loading conversations…");
    expect(
      findAlert(el),
      "no alert while the reopen is loading",
    ).toBeNull();

    // The fresh list lands (new reference) → loading clears, error shows.
    await render({
      ...baseProps,
      error: KILL_ERROR,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });
    expect(findAlert(el), "alert returns once the list lands").not.toBeNull();

    // Reopen → loading again → the stale error hides.
    await openPicker(el);
    await openPicker(el); // close
    await openPicker(el); // reopen
    expect(
      findAlert(el),
      "no alert while the second reopen is loading",
    ).toBeNull();
  });

  // 3. No alert when there is no error.
  it("renders no alert when error is null", async () => {
    const el = await render({ ...baseProps, error: null });

    await openPicker(el);

    expect(findAlert(el)).toBeNull();
  });
});
