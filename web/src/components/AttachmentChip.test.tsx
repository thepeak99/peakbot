// RED (image-lightbox): clicking a composer pending-attachment image chip
// does not open the Lightbox yet. Contract: the 64px thumbnail <img> gets
// `cursor-zoom-in` and clicking it opens the Lightbox with the chip's src
// (the blob preview when present, else /api/uploads/{convo}/{id}); the
// remove × button must NOT open the lightbox (it still removes);
// non-image chips never open a lightbox.
//
// Same createRoot + act + createElement pattern as Message.test.ts (T13
// block); jsdom comes from the vitest environmentMatchGlobs for
// src/components/**.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { AttachmentChip } from "./AttachmentChip";
import type { Pending } from "../useAttachments";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

const CONVO = "11111111-1111-4111-8111-111111111111";
const IMG_ID = "11111111-aaaa-4aaa-8aaa-111111111111";

/** A done image chip; `preview` simulates a surviving local blob preview. */
function doneImageChip(opts: { preview?: string } = {}): Pending {
  return {
    key: "k1",
    file: new File(["x"], "cat.png", { type: "image/png" }),
    preview: opts.preview,
    state: "done",
    attachment: {
      id: IMG_ID,
      convo: CONVO,
      name: "cat.png",
      mime: "image/png",
      size: 1234,
      kind: "image",
    },
  };
}

/** Render a chip into a fresh container; returns the container and an async
 * unmount for teardown. */
async function renderChip(
  chip: Pending,
  onRemove: () => void,
): Promise<{ el: HTMLElement; unmount: () => Promise<void> }> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(AttachmentChip, { chip, supportsVision: true, onRemove }));
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

const dialog = (): HTMLElement | null =>
  document.querySelector('[role="dialog"][aria-modal="true"]');

describe("AttachmentChip image lightbox", () => {
  it("renders the image thumbnail with cursor-zoom-in", async () => {
    const { el, unmount } = await renderChip(
      doneImageChip({ preview: "blob:http://localhost/p1" }),
      vi.fn(),
    );
    try {
      const img = el.querySelector("img");
      expect(img, "a done image chip renders a thumbnail").not.toBeNull();
      expect(img!.getAttribute("src")).toBe("blob:http://localhost/p1");
      expect(img!.getAttribute("class")).toContain("cursor-zoom-in");
    } finally {
      await unmount();
    }
  });

  it("opens the Lightbox with the blob preview src when the thumbnail is clicked", async () => {
    const preview = "blob:http://localhost/p2";
    const onRemove = vi.fn();
    const { el, unmount } = await renderChip(doneImageChip({ preview }), onRemove);
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      expect(dialog()).toBeNull();
      await act(async () => {
        img!.click();
      });
      const d = dialog();
      expect(d, "clicking the chip thumbnail opens a modal dialog").not.toBeNull();
      expect(d!.parentElement, "the Lightbox portals into document.body").toBe(
        document.body,
      );
      expect(
        d!.querySelector(`img[src="${preview}"]`),
        "the lightbox shows the blob preview",
      ).not.toBeNull();
      expect(onRemove).not.toHaveBeenCalled();
    } finally {
      await unmount();
    }
  });

  it("opens the Lightbox with the /api/uploads url when the chip has no local preview", async () => {
    const chip = doneImageChip();
    const url = `/api/uploads/${CONVO}/${IMG_ID}`;
    const { el, unmount } = await renderChip(chip, vi.fn());
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      expect(img!.getAttribute("src")).toBe(url);
      await act(async () => {
        img!.click();
      });
      const d = dialog();
      expect(d, "clicking a preview-less chip opens a modal dialog").not.toBeNull();
      expect(
        d!.querySelector(`img[src="${url}"]`),
        "the lightbox shows the served upload url",
      ).not.toBeNull();
    } finally {
      await unmount();
    }
  });

  it("opens the Lightbox with the preview src for an uploading image chip", async () => {
    // src is the preview while the upload is still in flight too.
    const chip: Pending = {
      key: "k2",
      file: new File(["x"], "cat.png", { type: "image/png" }),
      preview: "blob:http://localhost/uploading",
      state: "uploading",
      progress: 0.5,
      abort: vi.fn(),
    };
    const { el, unmount } = await renderChip(chip, vi.fn());
    try {
      const img = el.querySelector("img");
      expect(img, "an uploading image chip renders its preview thumbnail").not.toBeNull();
      await act(async () => {
        img!.click();
      });
      const d = dialog();
      expect(d, "an uploading image chip's preview is lightboxable").not.toBeNull();
      expect(
        d!.querySelector('img[src="blob:http://localhost/uploading"]'),
        "the lightbox shows the preview",
      ).not.toBeNull();
    } finally {
      await unmount();
    }
  });

  it("closes the Lightbox with the ✕ close button when a chip image is open", async () => {
    const { el, unmount } = await renderChip(
      doneImageChip({ preview: "blob:http://localhost/p3" }),
      vi.fn(),
    );
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      await act(async () => {
        img!.click();
      });
      expect(dialog()).not.toBeNull();
      const close = document.querySelector<HTMLButtonElement>(
        '[role="dialog"] button[aria-label="Close"]',
      );
      expect(close, "the lightbox exposes an aria-label=Close button").not.toBeNull();
      await act(async () => {
        close!.click();
      });
      expect(dialog(), "the ✕ button closes the lightbox").toBeNull();
    } finally {
      await unmount();
    }
  });

  it("closes the Lightbox on Escape when a chip image is open", async () => {
    const { el, unmount } = await renderChip(
      doneImageChip({ preview: "blob:http://localhost/p4" }),
      vi.fn(),
    );
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      await act(async () => {
        img!.click();
      });
      expect(dialog()).not.toBeNull();
      // Dispatch on document: a window-level listener still receives it via
      // bubbling, so either wiring is covered.
      await act(async () => {
        document.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
        );
      });
      expect(dialog(), "Escape should close the lightbox").toBeNull();
    } finally {
      await unmount();
    }
  });

  it("does not open the Lightbox when the remove button is clicked (it removes)", async () => {
    const onRemove = vi.fn();
    const { el, unmount } = await renderChip(
      doneImageChip({ preview: "blob:http://localhost/p5" }),
      onRemove,
    );
    try {
      const remove = el.querySelector<HTMLButtonElement>('button[aria-label="Remove cat.png"]');
      expect(remove, "image chips keep their remove button").not.toBeNull();
      await act(async () => {
        remove!.click();
      });
      expect(dialog(), "the remove button must not open the lightbox").toBeNull();
      expect(onRemove).toHaveBeenCalledTimes(1);
    } finally {
      await unmount();
    }
  });

  it("renders non-image chips without a thumbnail and never opens a Lightbox", async () => {
    const onRemove = vi.fn();
    const chip: Pending = {
      key: "k3",
      file: new File(["x"], "spec.pdf", { type: "application/pdf" }),
      state: "done",
      attachment: {
        id: "33333333-cccc-4ccc-8ccc-333333333333",
        convo: CONVO,
        name: "spec.pdf",
        mime: "application/pdf",
        size: 2411724,
        kind: "file",
      },
    };
    const { el, unmount } = await renderChip(chip, onRemove);
    try {
      expect(el.querySelector("img"), "non-image chips render no thumbnail").toBeNull();
      const remove = el.querySelector<HTMLButtonElement>('button[aria-label="Remove spec.pdf"]');
      expect(remove).not.toBeNull();
      await act(async () => {
        remove!.click();
      });
      expect(dialog(), "a non-image chip must not open a lightbox").toBeNull();
      expect(onRemove).toHaveBeenCalledTimes(1);
    } finally {
      await unmount();
    }
  });
});
