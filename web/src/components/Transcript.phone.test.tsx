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

const wire = (role: WireChatMessage["role"], content: string): WireChatMessage => ({
  role,
  content,
  timestamp: "2026-01-01T00:00:00Z",
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
