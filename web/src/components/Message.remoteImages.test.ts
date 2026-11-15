// RED (remote-images): on remote profiles the server relay re-roots markdown
// image URLs from `/images/<id>` to `/r/<percent-encoded-profile>/images/<id>`
// (src/ui/web/remote/relay.rs `localize_state`). `isRenderableImageSrc` in
// Message.tsx only accepts `/images/`, http(s):// and data:, so CachedImage
// renders the 🖼 text stub instead of an <img>.
//
// Contract: `/r/<segment>/images/<id>` (segment non-empty, no further slashes)
// is a renderable image src and is passed through UNCHANGED as the <img> src.
// Anything else under /r/ (non-image routes) or bare local paths stay stubs.
//
// Same createRoot + act + createElement pattern as Message.test.ts.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { Message } from "./Message";
import type { ChatMessage } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

const ID = `${"a".repeat(64)}.png`;

function msg(role: ChatMessage["role"], content: string): ChatMessage {
  return { role, content, at: 1732000000000 };
}

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

/** Render `![shot.png](src)` under `role`; report the <img> src (or null) and
 * whether the 🖼 stub is present. */
async function probe(role: ChatMessage["role"], src: string) {
  const { el, unmount } = await renderMessage(msg(role, `![shot.png](${src})`));
  try {
    const img = el.querySelector("img");
    return {
      imgSrc: img ? img.getAttribute("src") : null,
      hasStub: el.textContent?.includes("🖼") ?? false,
    };
  } finally {
    await unmount();
  }
}

describe("remote profile image URLs (/r/<profile>/images/<id>)", () => {
  it("renders an <img> with the exact src for an assistant (agent) image", async () => {
    const src = `/r/home-dev/images/${ID}`;
    const r = await probe("agent", src);
    expect(r.hasStub, "must not fall back to the 🖼 text stub").toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it("renders an <img> with the exact src for a tool-result (view_image) image", async () => {
    const src = `/r/home-dev/images/${ID}`;
    const r = await probe("toolResult", src);
    expect(r.hasStub, "must not fall back to the 🖼 text stub").toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it("renders an <img> when the profile segment is percent-encoded (space)", async () => {
    const src = `/r/home%20dev/images/${ID}`;
    const r = await probe("agent", src);
    expect(r.hasStub).toBe(false);
    expect(r.imgSrc, "src is passed through still percent-encoded").toBe(src);
  });

  it("renders an <img> when the profile segment is percent-encoded (unicode)", async () => {
    const src = `/r/${encodeURIComponent("café ☕")}/images/${ID}`;
    const r = await probe("toolResult", src);
    expect(r.hasStub).toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it("renders an <img> when the encoded segment contains %2F (not a path separator)", async () => {
    const src = `/r/${encodeURIComponent("a/b")}/images/${ID}`;
    expect(src).toContain("a%2Fb");
    const r = await probe("agent", src);
    expect(r.hasStub).toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it("opens the Lightbox with the same remote src when the image is clicked", async () => {
    const src = `/r/home-dev/images/${ID}`;
    const { el, unmount } = await renderMessage(msg("agent", `![shot.png](${src})`));
    try {
      const img = el.querySelector("img");
      expect(img, "remote image renders as <img>").not.toBeNull();
      await act(async () => {
        img!.click();
      });
      expect(document.querySelector('[role="dialog"][aria-modal="true"]')).not.toBeNull();
      expect(document.querySelectorAll(`img[src="${src}"]`)).toHaveLength(2);
    } finally {
      await unmount();
    }
  });
});

describe("image src regression guards (must stay GREEN)", () => {
  it("still renders an <img> for a local /images/<id> src", async () => {
    const src = `/images/${ID}`;
    const r = await probe("agent", src);
    expect(r.hasStub).toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it("still renders an <img> for a local /images/<id> src in a tool result", async () => {
    const src = `/images/${ID}`;
    const r = await probe("toolResult", src);
    expect(r.hasStub).toBe(false);
    expect(r.imgSrc).toBe(src);
  });

  it.each([
    ["/etc/passwd.png"],
    ["/foo/bar.png"],
    ["out/chart.png"],
  ])("renders the stub, not an <img>, for bare local path %s", async (src) => {
    const r = await probe("agent", src);
    expect(r.imgSrc).toBeNull();
    expect(r.hasStub).toBe(true);
  });

  it("renders the stub for a /r/<profile>/ route that is not an image route", async () => {
    const r = await probe("agent", "/r/home-dev/api/config");
    expect(r.imgSrc).toBeNull();
    expect(r.hasStub).toBe(true);
  });

  it("renders the stub for a /r/<profile>/ non-image route in a tool result", async () => {
    const r = await probe("toolResult", "/r/home-dev/api/config");
    expect(r.imgSrc).toBeNull();
    expect(r.hasStub).toBe(true);
  });

  it("renders the stub when /images/ is nested deeper than the profile segment", async () => {
    // Only `/r/<segment>/images/<id>` is a relay-rooted image URL.
    const r = await probe("agent", `/r/home-dev/extra/images/${ID}`);
    expect(r.imgSrc).toBeNull();
    expect(r.hasStub).toBe(true);
  });

  it("renders the stub for /r/ without a profile segment (/r/images/<id>)", async () => {
    // `images` here would be read as the profile segment, leaving no
    // `/images/` route after it.
    const r = await probe("agent", `/r/images/${ID}`);
    expect(r.imgSrc).toBeNull();
    expect(r.hasStub).toBe(true);
  });
});
