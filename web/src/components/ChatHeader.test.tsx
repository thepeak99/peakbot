// Tests for `components/ChatHeader.tsx` — the mobile Conversation header
// (design §4.2, H1–H6; mobile-redesign design §2.5/§2.7: the subtitle IS the
// selection chip, `chat-machine` prefixes remote chats, the working state is
// a spinner with sr-only text, and `panels` adds the Panels button).
//
// Harness modeled on BottomBar.test.tsx (raw createRoot + act,
// querySelector). No shell marker is needed: the header is a plain presentational
// component.
//
// RED before implementation: `./ChatHeader` does not exist yet, so this file
// fails at import — the right reason (feature absent).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatHeader } from "./ChatHeader";
import type { ReactNode } from "react";
import type { PanelsButtonProps } from "./PanelsSheet";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// Mirrors the mobile-redesign ChatHeader props (design §2.5).
interface HeaderProps {
  title: string;
  machine: string | null;
  selector: ReactNode;
  badge: boolean;
  onBack(): void;
  connected: boolean;
  isRunning: boolean;
  focusMode: boolean;
  onToggleFocus(): void;
  panels?: PanelsButtonProps;
}

const SELECTOR = (
  <button type="button" data-testid="selection-chip">
    home › team › sonnet
  </button>
);

const baseProps: HeaderProps = {
  title: "Fix the WS relay",
  machine: null,
  selector: SELECTOR,
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

  // Reverses H2 / batch 1 keep-R B3 (user decision after device testing): the
  // "other chat has news" dot on the back button is REMOVED. The `badge`
  // prop is cast through `unknown` because the builder may delete it.
  it("H2: the back button renders NO badge dot, whether badge is false or true", async () => {
    for (const badge of [false, true]) {
      const el = await render({ badge });
      expect(
        el.querySelector('[data-testid="chat-back-badge"]'),
        `no chat-back-badge testid when badge=${badge}`,
      ).toBeNull();
      const back = el.querySelector('button[data-testid="chat-back"]')!;
      expect(back, "the back button itself still renders").not.toBeNull();
      expect(
        back.querySelector(".rounded-full"),
        `no round dot of any kind inside the back button when badge=${badge}`,
      ).toBeNull();
      expect(
        back.querySelector("span"),
        "the back button holds only the chevron svg and the label text",
      ).toBeNull();
    }
  });

  it("the back button still reads '‹ Chats' (chevron svg + label) when badge is true", async () => {
    const el = await render({ badge: true });
    const back = el.querySelector('button[data-testid="chat-back"]')!;
    expect(back.querySelector("svg"), "the chevron stays").not.toBeNull();
    expect((back.textContent ?? "").trim()).toBe("Chats");
    expect(back.getAttribute("aria-label")).toBe("Back to chats");
  });

  it("H3: the title truncates and the subtitle hosts the selector node", async () => {
    const el = await render({ title: "Fix the WS relay" });
    const title = el.querySelector('[data-testid="chat-title"]');
    expect(title!.textContent).toBe("Fix the WS relay");
    expect(title!.classList.contains("truncate")).toBe(true);
    const subtitle = el.querySelector('[data-testid="chat-subtitle"]')!;
    expect(
      subtitle.querySelector('[data-testid="selection-chip"]'),
      "the selector (the chip) lives inside the subtitle",
    ).not.toBeNull();
    expect(subtitle.textContent).toContain("home › team › sonnet");
  });

  // Reverses H3 (user decision): the host appears only inside the pill (the
  // selector node), never as a separate prefix in the subtitle.
  it("H3: no machine prefix renders in the subtitle, for a local OR a remote chat", async () => {
    for (const machine of [null, "home-server.local:7823"]) {
      const el = await render({ machine });
      expect(
        el.querySelector('[data-testid="chat-machine"]'),
        `no chat-machine prefix when machine=${String(machine)}`,
      ).toBeNull();
      const subtitle = el.querySelector('[data-testid="chat-subtitle"]')!;
      expect(subtitle.textContent ?? "", "the host text is not in the subtitle").not.toContain("home-server");
      expect(subtitle.textContent ?? "", "no dangling '·' separator outside the pill").not.toMatch(/·\s*$/);
      expect(
        subtitle.querySelector('[data-testid="selection-chip"]'),
        "the pill (selector) still renders in the subtitle",
      ).not.toBeNull();
    }
  });

  it("H3: an empty selector renders no chip and no crash (before the first state)", async () => {
    const el = await render({ selector: null });
    expect(el.querySelector('[data-testid="chat-subtitle"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="selection-chip"]')).toBeNull();
  });

  it("H4: state is offline (wins), then working, then nothing when connected and idle", async () => {
    let el = await render({ connected: false, isRunning: true });
    let state = el.querySelector('[data-testid="chat-state"]');
    expect(state!.getAttribute("data-state"), "offline wins over working").toBe("offline");

    el = await render({ connected: true, isRunning: true });
    state = el.querySelector('[data-testid="chat-state"]');
    expect(state!.getAttribute("data-state")).toBe("working");
    // The word moved to the inline status line: here it is a spinner with
    // sr-only text, so the label stays for assistive tech.
    expect(state!.textContent).toContain("working…");
    expect(
      state!.querySelector(".sr-only")?.textContent,
      "the working text is sr-only (no visible word in the header)",
    ).toContain("working…");
    expect(state!.querySelector(".animate-spin"), "the spinner renders").not.toBeNull();

    // Batch 1 (B10): connected and idle renders NOTHING — the mock shows no
    // state node, and the idle dot was never on the keep list (the offline
    // text and the working spinner are).
    el = await render({ connected: true, isRunning: false });
    expect(
      el.querySelector('[data-testid="chat-state"]'),
      "connected and idle renders no chat-state node",
    ).toBeNull();
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

  it("H6: the header keeps min-h-14 + the top safe-area inset; no gear; a panels button iff panels", async () => {
    let el = await render();
    const header = el.querySelector("header");
    expect(header, "a header element must render").not.toBeNull();
    expect(header!.classList.contains("min-h-14")).toBe(true);
    // Batch 1 (B1): the top inset is the satop+10 calc, so the class carries
    // env(safe-area-inset-top) without pinning the bare form.
    expect(header!.className).toContain("safe-area-inset-top");
    expect(el.querySelector('button[aria-label="Settings"]'), "no gear").toBeNull();
    expect(el.querySelector('[data-testid="panels-button"]'), "no panels button without `panels`").toBeNull();

    const onOpen = vi.fn();
    el = await render({ panels: { count: 2, onOpen } });
    const btn = el.querySelector<HTMLButtonElement>('[data-testid="panels-button"]');
    expect(btn, "the panels button renders with `panels`").not.toBeNull();
    expect(el.querySelector('[data-testid="panels-badge"]')!.textContent).toBe("2");
    await act(async () => {
      btn!.click();
    });
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("the panels button hides its badge at 0 but stays", async () => {
    const el = await render({ panels: { count: 0, onOpen: () => {} } });
    expect(el.querySelector('[data-testid="panels-button"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="panels-badge"]')).toBeNull();
  });

  it("the right cluster order is state, focus, panels (batch 1, X3/B9)", async () => {
    const el = await render({ isRunning: true, panels: { count: 1, onOpen: () => {} } });
    const cluster = el.querySelector("header > div:last-of-type");
    expect(cluster, "the right cluster is the header's last child").not.toBeNull();
    // The three members in DOM order. The mock is [state][focus][panels]
    // (proto:376-382); the header must follow it.
    const names = [...cluster!.children].map((n) => {
      const c = n as HTMLElement;
      const id = c.getAttribute("data-testid");
      if (id === "chat-state") return "state";
      if (id === "panels-button") return "panels";
      if (c.matches('button[aria-label="Focus mode"]')) return "focus";
      return `?${c.tagName}`;
    });
    expect(names, "the cluster reads state, focus, panels").toEqual([
      "state",
      "focus",
      "panels",
    ]);
  });
});

// ─── Phone title-row logo ─────────────────────────────────────────────────
//
// Locked: the title area is a `data-testid="chat-title-row"` row holding the
// decorative `<img src="/logo_shifu.png" alt="">` (h-5 w-5) plus the existing
// truncating `h1[data-testid="chat-title"]`. The logo is decoration: it must
// not leak text into the h1, and it must not displace the back button from
// the header's first position. `ChatList`'s intentional no-logo contract is
// separate and is NOT touched here.

describe("ChatHeader — phone title-row logo", () => {
  it("the title row pairs the logo (alt='', h-5 w-5) with the truncating title", async () => {
    const el = await render({ title: "Fix the WS relay" });
    const header = el.querySelector("header");
    expect(header, "a header element renders").not.toBeNull();

    const row = el.querySelector<HTMLElement>('[data-testid="chat-title-row"]');
    expect(row, "the title row must render").not.toBeNull();
    expect(header!.contains(row), "the title row lives inside the header").toBe(true);

    const img = row!.querySelector("img");
    expect(img, "the logo renders inside the title row").not.toBeNull();
    expect(img!.getAttribute("src")).toBe("/logo_shifu.png");
    expect(img!.getAttribute("alt"), "the logo is decorative").toBe("");
    expect(img!.classList.contains("h-5"), "logo height is h-5").toBe(true);
    expect(img!.classList.contains("w-5"), "logo width is w-5").toBe(true);

    const title = row!.querySelector<HTMLElement>('[data-testid="chat-title"]');
    expect(title, "the existing chat-title h1 stays inside the row").not.toBeNull();
    expect(title!.tagName).toBe("H1");
    expect(title!.textContent).toBe("Fix the WS relay");
    expect(title!.classList.contains("truncate"), "the title still truncates").toBe(true);

    expect(
      el.querySelectorAll('img[src="/logo_shifu.png"]'),
      "exactly one logo in the header",
    ).toHaveLength(1);
  });

  it("a long title still truncates and the h1 holds only the title text", async () => {
    const long = "A very long conversation title that should ellipsize on a phone screen";
    const el = await render({ title: long });
    const h1 = el.querySelector<HTMLElement>('[data-testid="chat-title"]');
    expect(h1, "the title h1 renders").not.toBeNull();
    expect(h1!.textContent, "no logo alt text or decoration leaks into the title").toBe(long);
    expect(h1!.querySelector("img"), "the logo is a sibling, not inside the h1").toBeNull();
    expect(h1!.classList.contains("truncate")).toBe(true);
  });

  it("the back button remains the header's first element with its name and callback", async () => {
    const onBack = vi.fn();
    const el = await render({ onBack });
    const header = el.querySelector("header")!;
    const first = header.firstElementChild as HTMLElement | null;
    expect(first, "the header has a first element").not.toBeNull();
    expect(first!.getAttribute("data-testid"), "back button is still first").toBe("chat-back");
    expect(first!.getAttribute("aria-label")).toBe("Back to chats");

    await act(async () => {
      first!.click();
    });
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});
