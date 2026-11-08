// RED (remote-images): AttachmentChip's no-preview fallback src is hard-coded
// to `/api/uploads/{convo}/{id}`, which ignores the remote profile base
// (`/r/<encoded-profile>`, from `apiBase()` in state.ts). On a remote profile
// that URL hits the local server and 404s.
//
// ASSUMED CONTRACT (component has no base prop yet): an optional
// `base?: string` prop, default '' (local). When the chip has no `preview`,
// the fallback src is `${base}/api/uploads/${convo}/${id}`. A surviving blob
// preview always wins over the fallback.
//
// Same createRoot + act pattern as AttachmentChip.test.tsx.

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

/** Render with an optional `base`; `base` is passed via a loosely-typed props
 * object so this file compiles (and fails at runtime, not at typecheck) before
 * the prop exists. */
async function renderChipSrc(chip: Pending, base?: string): Promise<string | null> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  const props: Record<string, unknown> = {
    chip,
    supportsVision: true,
    onRemove: vi.fn(),
  };
  if (base !== undefined) props.base = base;
  await act(async () => {
    root.render(
      createElement(
        AttachmentChip as unknown as (p: Record<string, unknown>) => ReturnType<typeof createElement>,
        props,
      ),
    );
  });
  try {
    return el.querySelector("img")?.getAttribute("src") ?? null;
  } finally {
    await act(async () => {
      root.unmount();
    });
  }
}

describe("AttachmentChip fallback src with a remote base", () => {
  it("prefixes the fallback src with the remote base when there is no preview", async () => {
    const src = await renderChipSrc(doneImageChip(), "/r/home-dev");
    expect(src).toBe(`/r/home-dev/api/uploads/${CONVO}/${IMG_ID}`);
  });

  it("keeps a percent-encoded base verbatim in the fallback src", async () => {
    const src = await renderChipSrc(doneImageChip(), "/r/home%20dev");
    expect(src).toBe(`/r/home%20dev/api/uploads/${CONVO}/${IMG_ID}`);
  });

  it("prefers the blob preview over the fallback even on a remote base", async () => {
    // Guard: preview wins regardless of base (GREEN today).
    const preview = "blob:http://localhost/p9";
    const src = await renderChipSrc(doneImageChip({ preview }), "/r/home-dev");
    expect(src).toBe(preview);
  });
});

describe("AttachmentChip fallback src locally (regression guards, GREEN)", () => {
  it("uses /api/uploads/{convo}/{id} when no base prop is given", async () => {
    const src = await renderChipSrc(doneImageChip());
    expect(src).toBe(`/api/uploads/${CONVO}/${IMG_ID}`);
  });

  it("uses /api/uploads/{convo}/{id} when base is the empty string", async () => {
    const src = await renderChipSrc(doneImageChip(), "");
    expect(src).toBe(`/api/uploads/${CONVO}/${IMG_ID}`);
  });
});
