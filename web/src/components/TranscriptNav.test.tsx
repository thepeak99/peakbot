// B37 keep-R: the "↑ top" button survives the batch 3 restyle. Batch 3 only
// changes its face (the `.floatst` pill, item 3.11) — the copy, the
// aria-label and the onTop wiring stay, so this file locks their existence.
//
// Green today: a keep-R lock, not a RED contract.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { TranscriptNav } from "./TranscriptNav";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

async function renderNav(
  showTop: boolean,
  onTop: () => void,
): Promise<{ el: HTMLElement; unmount: () => Promise<void> }> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(
      createElement(TranscriptNav, {
        pinned: false,
        unread: 0,
        showTop,
        onBottom: () => {},
        onTop,
        drawerOpen: false,
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

describe("TranscriptNav — the '↑ top' button (B37 keep-R)", () => {
  it("renders '↑ top' and clicking it calls onTop", async () => {
    const onTop = vi.fn();
    const { el, unmount } = await renderNav(true, onTop);
    try {
      const btn = el.querySelector<HTMLElement>(
        'button[aria-label="Scroll to the top of the transcript"]',
      );
      expect(btn, "the '↑ top' button renders").not.toBeNull();
      expect(btn!.textContent).toContain("↑ top");
      await act(async () => {
        btn!.click();
      });
      expect(onTop, "clicking it scrolls to the top").toHaveBeenCalledTimes(1);
    } finally {
      await unmount();
    }
  });

  it("stays absent while pinned (showTop false)", async () => {
    const { el, unmount } = await renderNav(false, () => {});
    try {
      expect(
        el.querySelector('button[aria-label="Scroll to the top of the transcript"]'),
      ).toBeNull();
    } finally {
      await unmount();
    }
  });
});
