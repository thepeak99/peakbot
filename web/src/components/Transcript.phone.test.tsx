// Batch 3, item 3.1 (X2): `Transcript` takes `phone?: boolean` and forwards
// it to every `<Message>`. Without it the transcript renders today's desktop
// cards (R1 lock). Messages are real wire frames, so the full Transcript →
// adaptMessage → Message path is exercised.
//
// `phone` is passed through a typed alias until the prop lands, so the
// phone-path assertion fails on the DOM (the "Shifu" label is still there),
// not on a type error.

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { act, createElement, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { Transcript } from "./Transcript";
import type { WireChatMessage } from "../state";

const PhoneTranscript = Transcript as unknown as ComponentType<
  Parameters<typeof Transcript>[0] & { phone?: boolean }
>;

const wire = (role: WireChatMessage["role"], content: string, ts = "2026-01-01T00:00:00Z"): WireChatMessage => ({
  role,
  content,
  timestamp: ts,
});

const MESSAGES: WireChatMessage[] = [wire("user", "question"), wire("agent", "answer")];

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

async function render(phone: boolean | undefined): Promise<{
  el: HTMLElement;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(
      createElement(PhoneTranscript, {
        messages: MESSAGES,
        drawerOpen: false,
        ...(phone === undefined ? {} : { phone }),
      }),
    );
  });
  return {
    el,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
    },
  };
}

describe("Transcript — forwards `phone` to every Message (3.1)", () => {
  it("phone: the rows render without the 'You'/'Shifu' labels or timestamps", async () => {
    const { el, unmount } = await render(true);
    try {
      expect(el.querySelectorAll("[data-index]").length, "rows render").toBe(2);
      expect(el.textContent).toContain("question");
      expect(el.textContent).toContain("answer");
      expect(el.textContent, "no user label").not.toContain("You");
      expect(el.textContent, "no agent label").not.toContain("Shifu");
    } finally {
      await unmount();
    }
  });

  it("without phone: the desktop cards keep the 'You' and 'Shifu' labels (R1)", async () => {
    const { el, unmount } = await render(undefined);
    try {
      expect(el.textContent).toContain("You");
      expect(el.textContent).toContain("Shifu");
    } finally {
      await unmount();
    }
  });

  it("phone: false is the same as absent (the default is desktop)", async () => {
    const { el, unmount } = await render(false);
    try {
      expect(el.textContent).toContain("You");
      expect(el.textContent).toContain("Shifu");
    } finally {
      await unmount();
    }
  });

});

// ─── Day separators (design §7.1) ────────────────────────────────────────
//
// Phone transcripts render a day separator inside the first message row of
// each day. Separators are computed over the filtered list, not virtual rows.
// Desktop has no separators.

const T1 = "2026-11-14T10:00:00Z";
const T2 = "2026-11-14T15:00:00Z";
const T3 = "2026-11-15T09:00:00Z";

describe("Transcript — day separators (design §7.1)", () => {
  it("phone: messages on 2 local days → 2 separators, inside rows 0 and k", async () => {
    const msgs = [
      wire("user", "day 1 msg", T1),
      wire("agent", "day 1 reply", T2),
      wire("user", "day 2 msg", T3),
    ];
    const el = document.createElement("div");
    document.body.appendChild(el);
    const root = createRoot(el);
    await act(async () => {
      root.render(
        createElement(PhoneTranscript, {
          messages: msgs,
          drawerOpen: false,
          phone: true,
        }),
      );
    });
    try {
      const separators = el.querySelectorAll('[role="separator"]');
      expect(separators.length, "two day separators").toBe(2);
      // First separator is inside row 0
      const row0 = el.querySelector('[data-index="0"]');
      expect(row0!.contains(separators[0]), "separator inside first row").toBe(true);
      // Second separator is inside row 2 (first of day 2)
      const row2 = el.querySelector('[data-index="2"]');
      expect(row2!.contains(separators[1]), "separator inside day-2 row").toBe(true);
    } finally {
      await act(async () => {
        root.unmount();
      });
      el.remove();
    }
  });

  it("phone: separator labels come from dayLabel", async () => {
    const msgs = [
      wire("user", "yesterday", T1),
      wire("user", "today", T3),
    ];
    const el = document.createElement("div");
    document.body.appendChild(el);
    const root = createRoot(el);
    await act(async () => {
      root.render(
        createElement(PhoneTranscript, {
          messages: msgs,
          drawerOpen: false,
          phone: true,
        }),
      );
    });
    try {
      const separators = el.querySelectorAll('[role="separator"]');
      expect(separators.length).toBe(2);
      // Labels should be "Yesterday" and "Today" (or date strings depending on test time)
      expect(separators[0].getAttribute("aria-label")).not.toBeNull();
      expect(separators[1].getAttribute("aria-label")).not.toBeNull();
    } finally {
      await act(async () => {
        root.unmount();
      });
      el.remove();
    }
  });

  it("phone: row count still equals message count", async () => {
    const msgs = [wire("user", "a"), wire("agent", "b"), wire("user", "c")];
    const el = document.createElement("div");
    document.body.appendChild(el);
    const root = createRoot(el);
    await act(async () => {
      root.render(
        createElement(PhoneTranscript, {
          messages: msgs,
          drawerOpen: false,
          phone: true,
        }),
      );
    });
    try {
      expect(el.querySelectorAll("[data-index]").length).toBe(3);
    } finally {
      await act(async () => {
        root.unmount();
      });
      el.remove();
    }
  });

  it("desktop: no separators", async () => {
    const msgs = [wire("user", "a"), wire("user", "b")];
    const el = document.createElement("div");
    document.body.appendChild(el);
    const root = createRoot(el);
    await act(async () => {
      root.render(
        createElement(PhoneTranscript, {
          messages: msgs,
          drawerOpen: false,
          phone: false,
        }),
      );
    });
    try {
      expect(el.querySelectorAll('[role="separator"]').length).toBe(0);
    } finally {
      await act(async () => {
        root.unmount();
      });
      el.remove();
    }
  });
});
