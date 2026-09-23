// Tests for the Composer's Pause/Resume button (pause-subagents feature).
//
// Locks the target behaviour from the spec:
//   - Hidden when idle (not running).
//   - Hidden while running but only the orchestrator runs (subAgent null).
//   - Hidden while running with a non-pausable sub-agent (pausable=false).
//   - "⏸ Pause" while pause === "running"; clicking sends `{"type":"pause"}`.
//   - "▶ Resume" while pause === "pausing" or "paused"; clicking sends
//     `{"type":"resume"}` — explicit frames, never a toggle.
//   - The Stop button stays rendered and still sends `{"type":"stop"}`.
//
// Harness modeled on CwdPicker.test.tsx (createRoot/act off
// react-dom/client; jsdom is auto-enabled for `src/components/**` by
// vitest.config.ts). Composer's `useMediaQuery` calls `window.matchMedia`,
// which jsdom doesn't implement, so we stub it (matches: false → the
// desktop fine-pointer path, which the button logic doesn't depend on).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Composer } from "./Composer";
import type { InboundMessage } from "../state";
import type { SubAgentRun } from "../types";

beforeAll(() => {
  // React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`.
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
  // jsdom has no matchMedia; Composer's useMediaQuery needs one.
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  }));
});

// ─── fixture ────────────────────────────────────────────────────────────

interface ComposerTestProps {
  isRunning: boolean;
  connected: boolean;
  commands: [];
  onSend: (text: string) => void;
  onStop: () => void;
  subAgent: SubAgentRun | null;
  onPause: () => void;
  onResume: () => void;
  watchingRole?: string | null;
  onClearWatch?: () => void;
}

const baseProps: ComposerTestProps = {
  isRunning: false,
  connected: true,
  commands: [],
  onSend: () => {},
  onStop: () => {},
  subAgent: null,
  onPause: () => {},
  onResume: () => {},
};

// A pausable sub-agent in the given pause state.
function subAgent(pause: SubAgentRun["pause"]): SubAgentRun {
  return { role: "researcher", pausable: true, pause };
}

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: ComposerTestProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<Composer {...props} />);
  });
  return container;
}

afterEach(() => {
  // No vi.restoreAllMocks() here: it would call mockRestore() on the
  // matchMedia vi.fn() stub and wipe its implementation for later tests.
  // (No spies to restore in this file.)
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
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

// ─── visibility ─────────────────────────────────────────────────────────

describe("Composer — Pause/Resume visibility", () => {
  it("hides the Pause button when idle", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: false,
      subAgent: subAgent("running"),
    });

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
  });

  it("hides the Pause button when only the orchestrator runs (subAgent null)", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: null,
    });

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
    // Stop still shows while running.
    expect(hasButton(el, "Stop")).toBe(true);
  });

  it("hides the Pause button when the sub-agent is not pausable", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: { role: "researcher", pausable: false, pause: "running" },
    });

    expect(hasButton(el, "Pause")).toBe(false);
    expect(hasButton(el, "Resume")).toBe(false);
    expect(hasButton(el, "Stop")).toBe(true);
  });
});

// ─── label + frames ─────────────────────────────────────────────────────

describe("Composer — Pause/Resume label and wire frames", () => {
  it("shows '⏸ Pause' with the pause tooltip while pause === 'running'", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("running"),
    });

    const btn = findButton(el, "Pause");
    expect(btn.textContent).toContain("⏸");
    expect(btn.title).toBe("Pause sub-agent after its current step");
    expect(hasButton(el, "Resume")).toBe(false);
  });

  it("clicking Pause sends the explicit {type:'pause'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("running"),
      onPause: () => sent.push({ type: "pause" }),
    });

    const btn = findButton(el, "Pause");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "pause" }]);
  });

  it("shows '▶ Resume' with the resume tooltip while pause === 'pausing'", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("pausing"),
    });

    const btn = findButton(el, "Resume");
    expect(btn.textContent).toContain("▶");
    expect(btn.title).toBe("Resume sub-agent");
    expect(hasButton(el, "Pause")).toBe(false);
  });

  it("clicking Resume (pausing) sends the explicit {type:'resume'} frame", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("pausing"),
      onResume: () => sent.push({ type: "resume" }),
    });

    const btn = findButton(el, "Resume");
    await act(async () => {
      btn.click();
    });

    expect(sent).toEqual([{ type: "resume" }]);
  });

  it("shows '▶ Resume' and sends {type:'resume'} while pause === 'paused'", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("paused"),
      onResume: () => sent.push({ type: "resume" }),
    });

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
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: subAgent("running"),
      onStop: () => sent.push({ type: "stop" }),
      onPause: () => sent.push({ type: "pause" }),
    });

    expect(hasButton(el, "Pause")).toBe(true);
    const stop = findButton(el, "Stop");
    await act(async () => {
      stop.click();
    });

    expect(sent).toEqual([{ type: "stop" }]);
  });
});
