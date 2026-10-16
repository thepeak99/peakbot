// Tests for the Focus-mode activity row (plan-focus-mode.md §5 case 24,
// §6.4) — written FIRST, expected to be RED until
// `web/src/components/ActivityRow.tsx` exists (the import below fails to
// resolve, so this file fails at collection for the right reason).
//
// The contract under test:
//
//   <ActivityRow step={WireChatMessage | null} />
//
//   - wrapper: role="status" + aria-live="polite"
//   - label = first non-empty, trimmed line of step.content;
//     falls back to step.tool_name when the content has no usable line;
//     step === null → the generic "Thinking" label
//   - sub-agent steps (source.kind === "sub_agent") get a
//     "🧩 <role> ·" prefix OUTSIDE the label span (D6)
//   - the label span carries title = the full first line — CSS
//     `truncate` clips visually, the title restores the full text
//
// Harness modeled on TopBar.test.tsx (createRoot/act off react-dom/client;
// jsdom is auto-enabled for src/components/** by vitest.config.ts).
// No timers: the animated dots are CSS-only per §6.4, so nothing here is
// time-dependent.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ActivityRow } from "./ActivityRow";
import type { WireChatMessage } from "../state";

// React 19's flushSync reads IS_REACT_ACT_ENVIRONMENT; set once so the
// console stays clean, matching the sibling component tests.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ──────────────────────────────────────────────────────────────

const T = "2026-01-01T00:00:00Z";

function step(
  partial: Pick<WireChatMessage, "role"> &
    Partial<Omit<WireChatMessage, "role">>,
): WireChatMessage {
  return { timestamp: T, ...partial };
}

// ─── mount helper ─────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(stepValue: WireChatMessage | null): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<ActivityRow step={stepValue} />);
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
});

// The label span is the only element that carries a title (§6.4 layout).
function labelOf(el: HTMLDivElement): HTMLSpanElement {
  const label = el.querySelector("span[title]");
  expect(label, "the label span must carry a title attribute").not.toBeNull();
  return label as HTMLSpanElement;
}

// ─── tests (plan §5 case 24) ─────────────────────────────────────────────

describe("ActivityRow — case 24", () => {
  it("renders the generic 'Thinking' label when step is null", async () => {
    const el = await mount(null);
    const status = el.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toContain("Thinking");
  });

  it("renders the first non-empty, trimmed line of a tool call's content as the label", async () => {
    // Server-side format_tool_call shape: an optional thought line first,
    // blank lines and indentation are noise the label must skip/trim.
    const s = step({
      role: "toolcall",
      tool_name: "bash",
      content: "💭 checking the layout\n\n  bash -c 'ls -la /tmp'  ",
    });
    const el = await mount(s);
    const label = labelOf(el);
    expect(label.textContent).toBe("💭 checking the layout");
    expect(label.getAttribute("title")).toBe("💭 checking the layout");
  });

  it("falls back to the tool name when the step has no usable content line", async () => {
    const elEmpty = await mount(step({ role: "toolcall", tool_name: "bash", content: "" }));
    expect(labelOf(elEmpty).textContent).toBe("bash");

    const elBlank = await mount(
      step({ role: "toolcall", tool_name: "web_search", content: "   \n\t \n" }),
    );
    expect(labelOf(elBlank).textContent).toBe("web_search");
  });

  it("prefixes sub-agent steps with '🧩 <role> ·' outside the label span (D6)", async () => {
    const s = step({
      role: "toolcall",
      tool_name: "bash",
      content: "curl https://example.com",
      source: { kind: "sub_agent", role: "researcher" },
    });
    const el = await mount(s);
    // The row as a whole shows the prefix…
    expect(el.textContent).toContain("🧩 researcher ·");
    // …but the label span itself is the bare label (CSS truncates THAT).
    const label = labelOf(el);
    expect(label.textContent).toBe("curl https://example.com");
    expect(label.textContent).not.toContain("🧩");
  });

  it("keeps the full long first line in the label span's title (truncation is CSS-only)", async () => {
    const long = `${"x".repeat(300)} end`; // 305 chars, single line
    const s = step({ role: "toolcall", tool_name: "bash", content: long });
    const el = await mount(s);
    const label = labelOf(el);
    // The DOM text is the full line; CSS `truncate` clips it visually.
    expect(label.textContent).toBe(long);
    expect(label.getAttribute("title")).toBe(long);
  });

  it("exposes the row as a polite status region (role=status + aria-live=polite)", async () => {
    const el = await mount(step({ role: "toolcall", tool_name: "bash", content: "ls" }));
    const status = el.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.getAttribute("aria-live")).toBe("polite");
  });
});
