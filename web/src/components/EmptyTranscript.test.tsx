// Batch 3, items 3.10 (B32 keep-R): the empty transcript keeps the welcome
// card, and gains the mock's centred "Nothing here yet" hint. The hint is
// `lg:hidden` — the desktop render must be unchanged (R1), so that class IS
// the contract. The scoped "No messages from …" line is kept as well.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { EmptyTranscript } from "./EmptyTranscript";
import type { Welcome } from "../types";

const WELCOME: Welcome = {
  provider: "anthropic",
  model: "claude",
  maxTokens: 100000,
  builtinTools: 10,
  mcpTools: 1,
  skills: 2,
  searxngEnabled: false,
  costTracking: true,
  compactionEnabled: true,
  compactionThreshold: 0.8,
  cwd: "/home/exe",
  peakbotVersion: "0.28.0",
};

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

async function render(
  welcome: Welcome | null,
  scopeLabel: string | null,
): Promise<{ el: HTMLElement; unmount: () => Promise<void> }> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(EmptyTranscript, { welcome, scopeLabel }));
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

const hintOf = (el: HTMLElement): HTMLElement | undefined =>
  [...el.querySelectorAll<HTMLElement>("p")].find((p) =>
    p.textContent?.includes("Nothing here yet"),
  );

describe("EmptyTranscript (B32 keep-R)", () => {
  it("a fresh chat keeps the welcome card and adds the 'Nothing here yet — ask for something.' hint", async () => {
    const { el, unmount } = await render(WELCOME, null);
    try {
      expect(el.textContent, "the welcome card is kept").toContain("Welcome to Shifu");
      const hint = hintOf(el);
      expect(hint, "the hint renders").toBeDefined();
      expect(hint!.textContent).toContain("ask for something");
    } finally {
      await unmount();
    }
  });

  it("the hint is phone-only: it carries lg:hidden so the desktop render is unchanged", async () => {
    const { el, unmount } = await render(WELCOME, null);
    try {
      expect(hintOf(el)?.classList.contains("lg:hidden")).toBe(true);
    } finally {
      await unmount();
    }
  });

  it("the hint also shows with no welcome (a chat that already passed its banner)", async () => {
    const { el, unmount } = await render(null, null);
    try {
      expect(hintOf(el)).toBeDefined();
      expect(el.textContent).not.toContain("Welcome to Shifu");
    } finally {
      await unmount();
    }
  });

  it("a watched lane keeps 'No messages from ‹who› yet.' and shows no 'Nothing here yet' hint", async () => {
    const { el, unmount } = await render(null, "researcher");
    try {
      expect(el.textContent).toContain("No messages from researcher yet.");
      expect(hintOf(el), "the scoped empty line replaces the hint").toBeUndefined();
    } finally {
      await unmount();
    }
  });
});
