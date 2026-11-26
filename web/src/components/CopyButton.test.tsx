// Contract for the shared `components/CopyButton.tsx` (phone/desktop message
// copy + any future copy affordance).
//
// RED before implementation: `./CopyButton` does not exist yet, so this file
// fails at import — the right reason (feature absent). Every assertion below
// is written against the locked interface:
//
//   <CopyButton getText={() => string} label={string} className?={string} />
//
// Behaviour:
// - icon-only <button type="button">, idle aria-label={label}, success
//   aria-label="Copied" for 1500 ms then back to {label};
// - getText() is read at CLICK time (never at render), and its exact string
//   (markdown, unicode, leading whitespace) reaches clipboard.writeText;
// - a missing clipboard or a rejected write leaves the button in the idle
//   state (no throw, no false success);
// - repeated clicks restart the 1500 ms window; unmount clears it.
//
// Harness: raw createRoot + act + querySelector, same as ChatHeader.test.tsx
// and Message.phone.test.tsx. jsdom comes from vitest.config.ts
// `environmentMatchGlobs` for `src/components/**`.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { CopyButton } from "./CopyButton";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let container: HTMLDivElement | null = null;
let root: Root | null = null;

interface CopyButtonProps {
  getText(): string;
  label: string;
  className?: string;
}

async function render(props: CopyButtonProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<CopyButton {...props} />);
  });
  return container;
}

function button(el: HTMLElement): HTMLButtonElement {
  const btn = el.querySelector("button");
  expect(btn, "CopyButton renders a <button>").not.toBeNull();
  return btn as HTMLButtonElement;
}

/** Click and let the clipboard promise + its `.then` state update settle.
 * Two microtask turns are enough for `writeText().then(...)`; act() flushes
 * the queued React update. */
async function click(btn: HTMLButtonElement): Promise<void> {
  await act(async () => {
    btn.click();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function unmount(): Promise<void> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
}

let writeText: ReturnType<typeof vi.fn>;

beforeEach(() => {
  writeText = vi.fn(() => Promise.resolve());
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
});

afterEach(async () => {
  await unmount();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("CopyButton — rendering", () => {
  it("is an icon-only type=button with aria-label={label} and no visible text", async () => {
    const el = await render({ getText: () => "x", label: "Copy message" });
    const btn = button(el);

    expect(btn.getAttribute("type"), "an explicit type=button keeps it out of forms").toBe("button");
    expect(btn.getAttribute("aria-label")).toBe("Copy message");
    expect(btn.querySelector("svg"), "the button is icon-only (an svg icon)").not.toBeNull();
    expect(btn.textContent ?? "", "no visible label text").toBe("");
  });

  it("forwards className to the button", async () => {
    const el = await render({
      getText: () => "x",
      label: "Copy message",
      className: "tap-copy",
    });
    expect(button(el).classList.contains("tap-copy")).toBe(true);
  });
});

describe("CopyButton — click-time text", () => {
  it("reads getText() at click time, once per click, not at render", async () => {
    let n = 0;
    const getText = vi.fn(() => `value-${++n}`);
    const el = await render({ getText, label: "Copy message" });
    const btn = button(el);

    expect(getText, "getText must not run during render").not.toHaveBeenCalled();

    await click(btn);
    expect(getText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenNthCalledWith(1, "value-1");

    await click(btn);
    expect(getText).toHaveBeenCalledTimes(2);
    expect(writeText).toHaveBeenNthCalledWith(2, "value-2");
  });

  it("writes the exact raw string: markdown, unicode and edge whitespace survive unchanged", async () => {
    const raw = "  ✨ héllo wörld\n```js\nconst π = 3.14;\n```\n\t";
    const el = await render({ getText: () => raw, label: "Copy message" });
    await click(button(el));

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(raw);
  });

  it("writes an empty string when getText() is empty (no trimming, no crash)", async () => {
    const el = await render({ getText: () => "", label: "Copy message" });
    await click(button(el));

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("");
  });
});

describe("CopyButton — success state", () => {
  it("swaps the aria-label to 'Copied' for exactly 1500ms, then reverts to {label}", async () => {
    vi.useFakeTimers();
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    expect(btn.getAttribute("aria-label"), "idle label").toBe("Copy message");

    await click(btn);
    expect(btn.getAttribute("aria-label")).toBe("Copied");
    expect(btn.textContent ?? "", "the success state stays icon-only").toBe("");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1499);
    });
    expect(btn.getAttribute("aria-label"), "still in the success window at 1499ms").toBe("Copied");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(btn.getAttribute("aria-label"), "reverted at 1500ms").toBe("Copy message");
  });

  it("restarts the 1500ms window on repeated clicks instead of letting the first timer fire", async () => {
    vi.useFakeTimers();
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    await click(btn); // t = 0, timer #1 at 1500
    expect(btn.getAttribute("aria-label")).toBe("Copied");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000); // t = 1000
    });

    await click(btn); // t = 1000, timer #2 at 2500
    expect(writeText).toHaveBeenCalledTimes(2);
    expect(btn.getAttribute("aria-label")).toBe("Copied");

    await act(async () => {
      // t = 2000. The first (uncleared) timer would have fired at 1500.
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(
      btn.getAttribute("aria-label"),
      "the earlier timer must be cleared when a new click starts a new window",
    ).toBe("Copied");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500); // t = 2500
    });
    expect(btn.getAttribute("aria-label")).toBe("Copy message");
  });
});

describe("CopyButton — clipboard failure", () => {
  it("stays in the idle state when writeText rejects (no false success)", async () => {
    writeText.mockImplementation(() => Promise.reject(new Error("denied")));
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    await click(btn);

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(
      btn.getAttribute("aria-label"),
      "a rejected write must not claim 'Copied'",
    ).toBe("Copy message");
  });

  it("does not throw and shows no success when navigator.clipboard is missing", async () => {
    vi.useFakeTimers();
    Object.defineProperty(navigator, "clipboard", {
      value: undefined,
      configurable: true,
    });
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    // jsdom routes listener errors to window.onerror; vitest fails the test on
    // an uncaught error either way. The label is the deterministic contract.
    btn.click();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1500);
    });

    expect(btn.getAttribute("aria-label")).toBe("Copy message");
  });

  it("does not throw when clipboard exists but writeText is not a function", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: undefined },
      configurable: true,
    });
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    btn.click();

    expect(btn.getAttribute("aria-label")).toBe("Copy message");
  });
});

describe("CopyButton — cleanup", () => {
  it("clears the pending success timer on unmount", async () => {
    vi.useFakeTimers();
    const el = await render({ getText: () => "payload", label: "Copy message" });
    const btn = button(el);

    await click(btn);
    expect(btn.getAttribute("aria-label")).toBe("Copied");
    const timersAfterClick = vi.getTimerCount();
    expect(timersAfterClick, "the 1500ms success timer is pending").toBeGreaterThan(0);

    await unmount();

    expect(
      vi.getTimerCount(),
      "unmount must clear the pending timer (nothing left to fire)",
    ).toBeLessThan(timersAfterClick);

    // And advancing time after unmount must be a no-op, not a crash.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
  });
});
