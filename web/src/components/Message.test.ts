import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { Message, sameMessage } from "./Message";
import type { AttachmentView, ChatMessage } from "../types";

// A canonical full message. Every test below mutates exactly one field off this
// base so each "differs in exactly one field" case is auditable.
function full(): ChatMessage {
  return {
    role: "agent",
    content: "hello world",
    timestamp: "10:14",
    toolName: "file_read",
    fromBackground: false,
    subAgentRole: undefined,
  };
}

describe("sameMessage", () => {
  it("returns true for two separately-constructed objects with equal values", () => {
    // Value comparison, not reference: same shape, distinct objects.
    expect(sameMessage(full(), full())).toBe(true);
  });

  it("returns true for the same reference (identity implies equality)", () => {
    const m = full();
    expect(sameMessage(m, m)).toBe(true);
  });

  it("returns false when role differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, role: "user" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when content differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, content: "different text" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when timestamp differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, timestamp: "10:15" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when toolName differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, toolName: "file_write" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when fromBackground differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, fromBackground: true };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when subAgentRole differs", () => {
    const a = full();
    const b: ChatMessage = { ...a, subAgentRole: "junior" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking is added to one but not the other", () => {
    // An assistant turn newly populated with thinking blocks must re-render
    // (the `<details>` flips from absent to present). Reference inequality is
    // the easy case here; the structural case below is the harder lock.
    const a = full();
    const b: ChatMessage = { ...a, thinking: ["step 1", "step 2"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking arrays differ in content", () => {
    // Two messages with structurally equal shape but different thinking text
    // must not be considered the same — the DOM would have stale text inside
    // the collapsed <details>.
    const a: ChatMessage = { ...full(), thinking: ["alpha", "beta"] };
    const b: ChatMessage = { ...full(), thinking: ["alpha", "gamma"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when thinking arrays differ in length", () => {
    const a: ChatMessage = { ...full(), thinking: ["only"] };
    const b: ChatMessage = { ...full(), thinking: ["only", "extra"] };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns true when thinking arrays are structurally equal but distinct references", () => {
    // adaptMessage allocates a fresh array per call, so two equal transcripts
    // must compare equal even though the array references differ. This is the
    // property memo relies on for the thinking path.
    const a: ChatMessage = { ...full(), thinking: ["x", "y"] };
    const b: ChatMessage = { ...full(), thinking: ["x", "y"] };
    expect(a.thinking).not.toBe(b.thinking);
    expect(sameMessage(a, b)).toBe(true);
  });

  it("returns true when thinking is undefined vs absent (loose equality)", () => {
    // Mirrors the existing "optional fields undefined vs absent" case for the
    // new field: `thinking: undefined` and a message without the field at all
    // describe the same DOM (no <details> rendered).
    const withUndefined: ChatMessage = { ...full(), thinking: undefined };
    const absent: ChatMessage = { ...full() };
    expect(sameMessage(withUndefined, absent)).toBe(true);
  });

  it("returns true when optional fields are undefined vs absent (loose equality)", () => {
    // Implementation choice (documented): an optional field explicitly set to
    // `undefined` is equivalent to the field being absent. Both messages below
    // describe the same shape; sameMessage must return true. If a future change
    // wants stricter semantics, update this case alongside it.
    const withUndefined: ChatMessage = {
      role: "user",
      content: "hi",
      timestamp: "10:14",
      toolName: undefined,
      fromBackground: undefined,
      subAgentRole: undefined,
    };
    const absent: ChatMessage = {
      role: "user",
      content: "hi",
      timestamp: "10:14",
    };
    expect(sameMessage(withUndefined, absent)).toBe(true);
  });

  it("returns true for two minimal messages with only the required fields", () => {
    const a: ChatMessage = { role: "system", content: "ready", timestamp: "10:14" };
    const b: ChatMessage = { role: "system", content: "ready", timestamp: "10:14" };
    expect(sameMessage(a, b)).toBe(true);
  });
});

// Render tests for markdown image links — same createRoot + act pattern as
// Transcript.test.tsx; jsdom comes from the vitest environmentMatchGlobs.

// React 19's `act` reads `globalThis.IS_REACT_ACT_ENVIRONMENT` and warns when
// it's unset — set it once so the console stays clean.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

/** Render a message into a fresh container; returns the container and an
 * async unmount for teardown. */
async function renderMessage(message: ChatMessage): Promise<{
  el: HTMLElement;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(createElement(Message, { message }));
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

describe("markdown image rendering (CachedImage)", () => {
  it("renders a resolved /images/ link as an <img> with that src", async () => {
    const { el, unmount } = await renderMessage({
      ...full(),
      content: "![x](/images/abc.png)",
    });
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      expect(img!.getAttribute("src")).toBe("/images/abc.png");
    } finally {
      await unmount();
    }
  });

  it("wraps the <img> in an anchor to the same src and sets loading=lazy", async () => {
    const { el, unmount } = await renderMessage({
      ...full(),
      content: "![x](/images/abc.png)",
    });
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      expect(img!.getAttribute("loading")).toBe("lazy");
      const anchor = img!.closest("a");
      expect(anchor).not.toBeNull();
      expect(anchor!.getAttribute("href")).toBe("/images/abc.png");
    } finally {
      await unmount();
    }
  });

  it("renders remote http(s) images as an <img> (deliberately not blocked)", async () => {
    // Pins the product decision NOT to block remote images for now.
    const { el, unmount } = await renderMessage({
      ...full(),
      content: "![x](https://example.com/x.png)",
    });
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      expect(img!.getAttribute("src")).toBe("https://example.com/x.png");
    } finally {
      await unmount();
    }
  });

  it("renders a bare local path as the muted fallback, not an <img>", async () => {
    const { el, unmount } = await renderMessage({
      ...full(),
      content: "![chart](out/chart.png)",
    });
    try {
      expect(el.querySelector("img")).toBeNull();
      const span = el.querySelector("span[title='out/chart.png']");
      expect(span).not.toBeNull();
      expect(span!.textContent).toContain("🖼 chart");
    } finally {
      await unmount();
    }
  });

  it("falls back to the muted text when the image fails to load", async () => {
    const { el, unmount } = await renderMessage({
      ...full(),
      content: "![chart](/images/deadbeef.png)",
    });
    try {
      const img = el.querySelector("img");
      expect(img).not.toBeNull();
      await act(async () => {
        img!.dispatchEvent(new Event("error"));
      });
      expect(el.querySelector("img")).toBeNull();
      expect(el.textContent).toContain("🖼 chart");
    } finally {
      await unmount();
    }
  });
});

// ─── T9: sameMessage compares attachment ids ─────────────────────────────
// RED: ChatMessage.attachments does not exist yet (§5.7). Contract:
// sameMessage compares attachment ids (length + per-index id), the same
// structural rule as `thinking` — fresh arrays per frame must compare equal.

const T9_CONVO = "11111111-1111-4111-8111-111111111111";

function attachment(id: string, over: Partial<AttachmentView> = {}): AttachmentView {
  return {
    id,
    convo: T9_CONVO,
    name: "spec.pdf",
    mime: "application/pdf",
    size: 2411724,
    kind: "file",
    url: `/api/uploads/${T9_CONVO}/${id}`,
    ...over,
  };
}

describe("sameMessage — attachments (T9)", () => {
  it("returns false when attachment ids differ", () => {
    const a: ChatMessage = {
      ...full(),
      role: "user",
      attachments: [attachment("id-1"), attachment("id-2")],
    };
    const b: ChatMessage = {
      ...full(),
      role: "user",
      attachments: [attachment("id-1"), attachment("id-3")],
    };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns true when attachment arrays are structurally equal but distinct references", () => {
    const a: ChatMessage = {
      ...full(),
      role: "user",
      attachments: [attachment("id-1"), attachment("id-2")],
    };
    const b: ChatMessage = {
      ...full(),
      role: "user",
      attachments: [attachment("id-1"), attachment("id-2")],
    };
    expect(a.attachments).not.toBe(b.attachments);
    expect(sameMessage(a, b)).toBe(true);
  });

  it("returns false when attachment array lengths differ", () => {
    const a: ChatMessage = { ...full(), role: "user", attachments: [attachment("id-1")] };
    const b: ChatMessage = {
      ...full(),
      role: "user",
      attachments: [attachment("id-1"), attachment("id-2")],
    };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns false when one side has attachments and the other has none", () => {
    const a: ChatMessage = { ...full(), role: "user", attachments: [attachment("id-1")] };
    const b: ChatMessage = { ...full(), role: "user" };
    expect(sameMessage(a, b)).toBe(false);
  });

  it("returns true when both sides have no attachments (absent vs undefined)", () => {
    const a: ChatMessage = { ...full(), role: "user", attachments: undefined };
    const b: ChatMessage = { ...full(), role: "user" };
    expect(sameMessage(a, b)).toBe(true);
  });
});

// ─── T13: attachment rendering + Lightbox ─────────────────────────────────
// RED: Message.tsx does not render ChatMessage.attachments yet. Contract
// (design T13): user rows render thumbnailable attachments as
// `<img loading="lazy">` above the text (click → Lightbox portal) and file
// attachments as `<a href={url} target="_blank" rel="noopener">`.

const IMG1 = attachment("11111111-aaaa-4aaa-8aaa-111111111111", {
  name: "cat.png",
  mime: "image/png",
  size: 1234,
  kind: "image",
});
const IMG2 = attachment("22222222-bbbb-4bbb-8bbb-222222222222", {
  name: "dog.png",
  mime: "image/png",
  size: 2345,
  kind: "image",
});
const PDF = attachment("33333333-cccc-4ccc-8ccc-333333333333");

function userWithAttachments(atts: AttachmentView[]): ChatMessage {
  return {
    role: "user",
    content: "what's in these?",
    timestamp: "10:14",
    attachments: atts,
  };
}

describe("attachment rendering (T13)", () => {
  it("renders 2 image attachments as <img loading=lazy> and the file as an <a href target=_blank> link", async () => {
    const { el, unmount } = await renderMessage(userWithAttachments([IMG1, IMG2, PDF]));
    try {
      const imgs = Array.from(el.querySelectorAll("img"));
      expect(imgs, "expected two image thumbnails").toHaveLength(2);
      expect(imgs.map((i) => i.getAttribute("src"))).toEqual([IMG1.url, IMG2.url]);
      for (const img of imgs) {
        expect(img.getAttribute("loading")).toBe("lazy");
      }

      const link = Array.from(el.querySelectorAll("a")).find(
        (a) => a.getAttribute("href") === PDF.url,
      );
      expect(link, "expected a link to the file attachment url").not.toBeNull();
      expect(link!.getAttribute("target")).toBe("_blank");
      expect(link!.getAttribute("rel")).toBe("noopener");
      expect(link!.textContent).toContain("spec.pdf");
    } finally {
      await unmount();
    }
  });

  it("opens the Lightbox when an image thumbnail is clicked", async () => {
    const { el, unmount } = await renderMessage(userWithAttachments([IMG1, IMG2, PDF]));
    try {
      const thumbs = Array.from(el.querySelectorAll("img"));
      expect(thumbs).toHaveLength(2);
      await act(async () => {
        thumbs[0].click();
      });
      // The Lightbox portals into document.body, so IMG1's url now appears
      // twice: the thumbnail + the lightbox image.
      expect(document.querySelectorAll(`img[src="${IMG1.url}"]`)).toHaveLength(2);
    } finally {
      await unmount();
    }
  });

  it("advances to the next image on ArrowRight and wraps around", async () => {
    const { el, unmount } = await renderMessage(userWithAttachments([IMG1, IMG2, PDF]));
    try {
      const thumbs = Array.from(el.querySelectorAll("img"));
      expect(thumbs).toHaveLength(2);
      await act(async () => {
        thumbs[0].click();
      });

      // Dispatch on document: a window-level listener still receives it via
      // bubbling, so either wiring is covered.
      await act(async () => {
        document.dispatchEvent(
          new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
        );
      });
      expect(
        document.querySelectorAll(`img[src="${IMG2.url}"]`),
        "ArrowRight should show the second image in the lightbox",
      ).toHaveLength(2);

      // Wrap: from the last image back to the first.
      await act(async () => {
        document.dispatchEvent(
          new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
        );
      });
      expect(document.querySelectorAll(`img[src="${IMG1.url}"]`)).toHaveLength(2);
    } finally {
      await unmount();
    }
  });

  it("closes the Lightbox on Escape", async () => {
    const { el, unmount } = await renderMessage(userWithAttachments([IMG1, IMG2, PDF]));
    try {
      const thumbs = Array.from(el.querySelectorAll("img"));
      expect(thumbs).toHaveLength(2);
      await act(async () => {
        thumbs[0].click();
      });
      expect(document.querySelectorAll(`img[src="${IMG1.url}"]`)).toHaveLength(2);

      await act(async () => {
        document.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
        );
      });
      expect(
        document.querySelectorAll(`img[src="${IMG1.url}"]`),
        "Escape should close the lightbox, leaving only the thumbnail",
      ).toHaveLength(1);
    } finally {
      await unmount();
    }
  });
});
