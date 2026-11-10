// GUARD (remote-profiles + mobile-attach part 2): a non-image attachment in a
// REMOTE conversation renders as a FileChip whose href is
// `/r/<L>/api/uploads/<convo>/<id>`.
//
// The URL is built in TWO existing seams this file pins:
//   1. `adaptMessage(m, base)` (adapt.ts) prefixes every wire attachment with
//      `${base}/api/uploads/${a.convo}/${a.id}` — `base` is the pre-encoded
//      `/r/<L>` relay prefix of the conversation's profile ("" = local);
//   2. `FileChip` (Message.tsx) renders `<a href={attachment.url}
//      target="_blank" rel="noopener">` verbatim.
//
// The remote relay serves the file at that path (src/ui/web/remote/proxy.rs),
// so a PDF attached on the phone must link through the profile's base, not
// the local server (which would 404).
//
// Same createRoot + act + createElement pattern as
// Message.remoteImages.test.ts / Message.test.ts.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { Message } from "./Message";
import { adaptMessage } from "../adapt";
import type { ChatMessage } from "../types";
import type { WireChatMessage } from "../state";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  document.body.innerHTML = "";
});

const CONVO = "11111111-1111-4111-8111-111111111111";
const ATTACH_ID = "22222222-2222-4222-8222-222222222222";

/** A user message carrying one PDF attachment (the wire shape the server
 * sends: attachments ride the user message, url is added by adaptMessage). */
function wirePdfMessage(): WireChatMessage {
  return {
    role: "user",
    content: "here is the report",
    timestamp: "2026-11-09T10:14:00Z",
    attachments: [
      {
        id: ATTACH_ID,
        convo: CONVO,
        name: "report.pdf",
        mime: "application/pdf",
        size: 2411724,
        kind: "file",
      },
    ],
  };
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

describe("remote profile file attachments (/r/<L>/api/uploads/<convo>/<id>)", () => {
  it("adaptMessage prefixes the PDF attachment's url with the relay base", () => {
    const message = adaptMessage(wirePdfMessage(), "/r/home-dev");
    expect(message.attachments).toHaveLength(1);
    expect(message.attachments![0].url).toBe(
      `/r/home-dev/api/uploads/${CONVO}/${ATTACH_ID}`,
    );
  });

  it("renders a FileChip link with href /r/home-dev/api/uploads/<convo>/<id> for a remote PDF", async () => {
    const message = adaptMessage(wirePdfMessage(), "/r/home-dev");
    const { el, unmount } = await renderMessage(message);
    try {
      const link = el.querySelector(
        `a[href="/r/home-dev/api/uploads/${CONVO}/${ATTACH_ID}"]`,
      );
      expect(
        link,
        "expected the PDF chip to link through the profile's relay base",
      ).not.toBeNull();
      expect(link!.getAttribute("target")).toBe("_blank");
      expect(link!.getAttribute("rel")).toBe("noopener");
      // A file chip is a link, never a thumbnail.
      expect(el.querySelector("img"), "a PDF must not render an <img>").toBeNull();
      expect(el.textContent).toContain("report.pdf");
    } finally {
      await unmount();
    }
  });

  it("keeps the local /api/uploads href with an empty base (unchanged)", async () => {
    const message = adaptMessage(wirePdfMessage(), "");
    expect(message.attachments![0].url).toBe(
      `/api/uploads/${CONVO}/${ATTACH_ID}`,
    );

    const { el, unmount } = await renderMessage(message);
    try {
      expect(
        el.querySelector(`a[href="/api/uploads/${CONVO}/${ATTACH_ID}"]`),
        "the local link must stay unprefixed",
      ).not.toBeNull();
    } finally {
      await unmount();
    }
  });
});
