// Performance budget: the phone transcript virtualizer's estimateSize must
// not call Date.parse for every row it estimates. The virtualizer calls
// estimateSize once per row on mount and once per new row on append.
// startsNewDay (which calls Date.parse twice per row) should only be computed
// at render time for the rows that actually render, not during estimation.
//
// N = 2000 synthetic rows spread over 5 local days. Budget: < N/10 = 200
// Date.parse calls per render. The current implementation calls it ~2×N =
// ~4000 times per render — it will fail. The fix (removing the calendar from
// the estimate) brings it to ~10 calls (one per day separator render).
//
// This test pins the budget, not the implementation. Wide margins so
// virtualizer overscan changes or React batching cannot flake it.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { Transcript } from "./Transcript";
import type { WireChatMessage } from "../state";

const PhoneTranscript = Transcript as unknown as ComponentType<
  Parameters<typeof Transcript>[0] & { phone?: boolean }
>;

// Build N synthetic rows across 5 local days. Use a simple pattern:
// 400 messages per day, all at the same local time (10:00 AM),
// on consecutive days starting from 2026-11-10.
function buildLargeTranscript(n: number): WireChatMessage[] {
  const messages: WireChatMessage[] = [];
  const perDay = Math.floor(n / 5);
  for (let day = 0; day < 5; day++) {
    const date = new Date(2026, 10, 10 + day, 10, 0, 0); // UTC time
    for (let i = 0; i < perDay; i++) {
      messages.push({
        role: "user" as const,
        content: `message ${day * perDay + i} on day ${day}`,
        timestamp: date.toISOString(),
      });
    }
  }
  // If n isn't evenly divisible by 5, add the remainder to the last day
  const remainder = n - perDay * 5;
  if (remainder > 0) {
    const date = new Date(2026, 10, 14, 10, 0, 0);
    for (let i = 0; i < remainder; i++) {
      messages.push({
        role: "user" as const,
        content: `remainder message ${i}`,
        timestamp: date.toISOString(),
      });
    }
  }
  return messages;
}

const N = 2000;
const BUDGET = Math.floor(N / 10); // 200

let restoreOffset: (() => void) | null = null;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  // jsdom has no layout: give the virtualizer a viewport so rows render.
  const desc = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get: () => 400,
  });
  restoreOffset = () => {
    if (desc) Object.defineProperty(HTMLElement.prototype, "offsetHeight", desc);
  };
  (globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
});

afterEach(() => {
  restoreOffset?.();
  document.body.innerHTML = "";
});

async function renderTranscript(
  messages: WireChatMessage[],
): Promise<{
  el: HTMLElement;
  root: ReturnType<typeof createRoot>;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(
      createElement(PhoneTranscript, {
        messages,
        drawerOpen: false,
        phone: true,
      }),
    );
  });
  return {
    el,
    root,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
    },
  };
}

describe("Transcript phone — virtualizer estimate performance budget", () => {
  it("phone: does not touch per-row timestamps for rows it never renders", async () => {
    const messages = buildLargeTranscript(N);

    // Spy on Date.parse to count calls during mount
    const spy = vi.spyOn(Date, "parse");
    try {
      const { unmount } = await renderTranscript(messages);

      // Count the calls made during the mount
      const mountCalls = spy.mock.calls.length;

      await unmount();

      // Some calls are expected (mounted rows render separators), but the
      // virtualizer should not call startsNewDay for every unrendered row.
      // The budget is N/10 = 200 for 2000 rows.
      expect(
        mountCalls,
        `Date.parse called ${mountCalls} times on mount (budget: ${BUDGET})`,
      ).toBeLessThan(BUDGET);
    } finally {
      spy.mockRestore();
    }
  });

  it("phone: appending one message does not re-estimate the whole transcript", async () => {
    const messages = buildLargeTranscript(N);

    // Render the initial transcript
    const { root, unmount } = await renderTranscript(messages);

    // Wait for the initial render to settle, then start spying
    // We do this AFTER the initial mount so we only count calls
    // triggered by the append, not the initial estimate.
    await act(async () => {
      // Small async tick to let any deferred layout settle
      await new Promise((r) => setTimeout(r, 0));
    });

    const spy = vi.spyOn(Date, "parse");
    try {
      // Append one new message
      const newMessages: WireChatMessage[] = [
        ...messages,
        {
          role: "agent" as const,
          content: "new appended message",
          timestamp: new Date().toISOString(),
        },
      ];

      // Rerender with the appended message
      await act(async () => {
        root.render(
          createElement(PhoneTranscript, {
            messages: newMessages,
            drawerOpen: false,
            phone: true,
          }),
        );
      });

      // Count the calls made during the append
      const appendCalls = spy.mock.calls.length;

      await unmount();

      // Appending one message should only estimate the new row (and its
      // day boundary), not re-estimate all 2000+ rows. Same budget.
      expect(
        appendCalls,
        `Date.parse called ${appendCalls} times on append (budget: ${BUDGET})`,
      ).toBeLessThan(BUDGET);
    } finally {
      spy.mockRestore();
    }
  });
});