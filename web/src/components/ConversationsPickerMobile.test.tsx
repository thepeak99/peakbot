// The header "+" in the mobile shell.
//
// The mobile shell's init script shims `window.open` to a top-level
// navigation: `+` opens `/?convo=<new>` in the SAME window, and Back
// returns to the previous conversation (verified on device: `+` adds a
// history entry, Back returns). So the "+" that used to
// be gated off in the mobile shell (with the tab strip, "the mobile shell
// owns new conversations") must be rendered again — the SPA, not the
// shell, owns new conversations.
//
// RED today: the button is gated `!desktop && !mobile`.
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
//   * `shell() === "mobile"`: the header "+" (aria-label "Open a new
//     conversation in a new tab") RENDERS, and clicking it calls
//     `window.open(window.location.pathname, "_blank", "noopener")` — the
//     shimmed open the mobile shell turns into the top-level navigation.
//   * desktop: no "+" (the tab strip has its own) — unchanged.
//   * browser: "+" renders — unchanged.
//
// Harness: ./ConversationsPicker.test.tsx conventions.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConversationsPicker } from "./ConversationsPicker";
import type { ConversationSummary } from "../state";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

const NEW_TAB_BUTTON = 'button[aria-label="Open a new conversation in a new tab"]';

const convo = (id: string): ConversationSummary => ({
  id,
  name: `convo ${id}`,
  updated_at: "2026-10-04T00:00:00Z",
  message_count: 1,
  model: "ollama/llama3",
  active: false,
});

const baseProps = {
  conversations: [convo("a1"), convo("b2")],
  hasTranscript: false,
  onOpen: () => {},
  onLoad: () => {},
  onKill: () => {},
  error: null,
};

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(): Promise<HTMLDivElement> {
  if (!root) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
    root!.render(<ConversationsPicker {...baseProps} />);
  });
  return container!;
}

function setShell(marker: "desktop" | "mobile" | "browser") {
  delete (window as unknown as { peakbotShell?: string }).peakbotShell;
  if (marker !== "browser") {
    (window as { peakbotShell?: string }).peakbotShell = marker;
  }
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
  setShell("browser");
  vi.restoreAllMocks();
});

describe("ConversationsPicker — header + per shell", () => {
  it("mobile: the + renders and opens the shimmed new conversation", async () => {
    setShell("mobile");
    const openSpy = vi.spyOn(window, "open").mockImplementation(() => null);
    const el = await render();

    const plus = el.querySelector<HTMLElement>(NEW_TAB_BUTTON);
    expect(plus, "the mobile shell must render the header +").not.toBeNull();

    await act(async () => {
      plus!.click();
    });

    // The same-origin open: the mobile shell's shim turns this into the
    // top-level navigation (Back returns to the previous conversation).
    expect(openSpy).toHaveBeenCalledTimes(1);
    expect(openSpy).toHaveBeenCalledWith(window.location.pathname, "_blank", "noopener");
  });

  it("desktop: the + stays hidden (the tab strip has its own)", async () => {
    setShell("desktop");
    const el = await render();
    expect(el.querySelector(NEW_TAB_BUTTON)).toBeNull();
  });

  it("browser: the + renders (unchanged)", async () => {
    const el = await render();
    expect(el.querySelector(NEW_TAB_BUTTON)).not.toBeNull();
  });
});
