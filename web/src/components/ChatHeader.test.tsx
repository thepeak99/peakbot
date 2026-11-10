// Tests for `components/ChatHeader.tsx` — the mobile Conversation header
// (design §4.2, H1–H6).
//
// Harness modeled on ConversationsPickerMobile.test.tsx (raw createRoot + act,
// querySelector). No shell marker is needed: the header is a plain presentational
// component.
//
// RED before implementation: `./ChatHeader` does not exist yet, so this file
// fails at import — the right reason (feature absent).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatHeader } from "./ChatHeader";
import type { TabColor } from "../state";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// Mirrors design §4.2, so overrides like `profile: null` type-check.
interface HeaderProps {
  title: string;
  profile: string | null;
  profileColor?: TabColor;
  model: string;
  badge: boolean;
  onBack(): void;
  connected: boolean;
  isRunning: boolean;
  focusMode: boolean;
  onToggleFocus(): void;
}

const baseProps: HeaderProps = {
  title: "Fix the WS relay",
  profile: "home",
  profileColor: "teal",
  model: "sonnet",
  badge: false,
  onBack: () => {},
  connected: true,
  isRunning: false,
  focusMode: false,
  onToggleFocus: () => {},
};

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(over: Partial<HeaderProps> = {}): Promise<HTMLDivElement> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<ChatHeader {...baseProps} {...over} />);
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.restoreAllMocks();
});

// The design names no selector for the colour dot; the repo's established
// idiom (ConversationsPicker) is a span whose inline style carries
// `var(--profile-<color>)`, so that is the closest reading.
const hasColorDot = (el: HTMLElement, color: string): boolean =>
  [...el.querySelectorAll<HTMLElement>("*")].some((n) =>
    (n.getAttribute("style") ?? "").includes(`var(--profile-${color})`),
  );

describe("ChatHeader — H1–H6", () => {
  it("H1: the back button is labelled 'Back to chats', reads 'Chats', calls onBack once", async () => {
    const onBack = vi.fn();
    const el = await render({ onBack });
    const back = el.querySelector<HTMLButtonElement>('button[data-testid="chat-back"]');
    expect(back, "the back button must render").not.toBeNull();
    expect(back!.getAttribute("aria-label")).toBe("Back to chats");
    expect(back!.textContent).toContain("Chats");

    await act(async () => {
      back!.click();
    });
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it("H2: the back badge is present iff badge", async () => {
    let el = await render({ badge: false });
    expect(el.querySelector('[data-testid="chat-back-badge"]')).toBeNull();

    el = await render({ badge: true });
    expect(el.querySelector('[data-testid="chat-back-badge"]')).not.toBeNull();
  });

  it("H3: title (truncate) and subtitle; a null profile drops the profile", async () => {
    let el = await render({ title: "Fix the WS relay", profile: "home", model: "sonnet" });
    const title = el.querySelector('[data-testid="chat-title"]');
    expect(title!.textContent).toBe("Fix the WS relay");
    expect(title!.classList.contains("truncate")).toBe(true);
    expect(el.querySelector('[data-testid="chat-subtitle"]')!.textContent).toBe("home · sonnet");

    el = await render({ profile: null, model: "sonnet" });
    expect(el.querySelector('[data-testid="chat-subtitle"]')!.textContent).toBe("sonnet");
  });

  it("H3: the colour dot is present iff profileColor", async () => {
    let el = await render({ profileColor: "teal" });
    expect(hasColorDot(el, "teal")).toBe(true);

    el = await render({ profileColor: undefined });
    expect(hasColorDot(el, "teal")).toBe(false);
  });

  it("H4: state is offline (wins), then working, then connected", async () => {
    let el = await render({ connected: false, isRunning: true });
    let state = el.querySelector('[data-testid="chat-state"]');
    expect(state!.getAttribute("data-state"), "offline wins over working").toBe("offline");

    el = await render({ connected: true, isRunning: true });
    state = el.querySelector('[data-testid="chat-state"]');
    expect(state!.getAttribute("data-state")).toBe("working");
    expect(state!.textContent).toContain("working…");

    el = await render({ connected: true, isRunning: false });
    state = el.querySelector('[data-testid="chat-state"]');
    expect(state!.getAttribute("data-state")).toBe("connected");
  });

  it("H5: the focus button reflects focusMode via aria-pressed and calls onToggleFocus", async () => {
    const onToggleFocus = vi.fn();
    let el = await render({ focusMode: false, onToggleFocus });
    let focus = el.querySelector<HTMLButtonElement>('button[aria-label="Focus mode"]');
    expect(focus).not.toBeNull();
    expect(focus!.getAttribute("aria-pressed")).toBe("false");

    await act(async () => {
      focus!.click();
    });
    expect(onToggleFocus).toHaveBeenCalledTimes(1);

    el = await render({ focusMode: true, onToggleFocus });
    focus = el.querySelector<HTMLButtonElement>('button[aria-label="Focus mode"]');
    expect(focus!.getAttribute("aria-pressed")).toBe("true");
  });

  it("H6: the header keeps min-h-14 + the top safe-area inset; no gear, no pickers", async () => {
    const el = await render();
    const header = el.querySelector("header");
    expect(header, "a header element must render").not.toBeNull();
    expect(header!.classList.contains("min-h-14")).toBe(true);
    expect(header!.className).toContain("pt-[env(safe-area-inset-top)]");
    expect(el.querySelector('button[aria-label="Settings"]'), "no gear").toBeNull();
    expect(
      el.querySelector('button[title="Load a saved conversation"]'),
      "no pickers",
    ).toBeNull();
  });
});
