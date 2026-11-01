//
// RED (remote-profiles T9): `adaptMessage` has no base argument yet.
// Written against the design contract (remote-profiles-design §5.5, §6.4):
//
//   * The SPA builds attachment URLs as `${apiBase}/api/uploads/${convo}/${id}`
//     — `adapt.ts:81` gains the base. Local (base "") is unchanged.
//   * Images: `localize_state` (T6) already re-roots root-relative markdown
//     image URLs to `/r/<L>/…`; the SPA must NOT double-prefix them — the
//     message `content` passes through byte-for-byte.

import { describe, expect, it } from "vitest";
import { adaptMessage } from "./adapt";
import type { WireAttachment, WireChatMessage } from "./state";

const CONVO = "77777777-7777-7777-7777-777777777777";
const ATT_ID = "22222222-2222-4222-8222-222222222222";

const ATT: WireAttachment = {
  id: ATT_ID,
  convo: CONVO,
  name: "a.png",
  mime: "image/png",
  size: 42,
  kind: "image",
};

function msg(content: string, attachments?: WireAttachment[]): WireChatMessage {
  return { role: "agent", content, timestamp: "2026-10-30T12:00:00Z", attachments };
}

describe("adaptMessage — the /r/<L>/ base (remote-profiles T9)", () => {
  it("builds attachment URLs under the given base (remote)", () => {
    const m = adaptMessage(msg("see it", [ATT]), "/r/home-dev");
    expect(m.attachments?.[0].url).toBe(
      `/r/home-dev/api/uploads/${CONVO}/${ATT_ID}`,
    );
  });

  it("keeps the local /api/uploads URL with an empty base (unchanged)", () => {
    const m = adaptMessage(msg("see it", [ATT]), "");
    expect(m.attachments?.[0].url).toBe(`/api/uploads/${CONVO}/${ATT_ID}`);
  });

  it("keeps the local URL when no base is given (legacy call)", () => {
    const m = adaptMessage(msg("see it", [ATT]));
    expect(m.attachments?.[0].url).toBe(`/api/uploads/${CONVO}/${ATT_ID}`);
  });

  it("does not re-prefix image URLs the relay already re-rooted (no double /r/)", () => {
    // `localize_state` (T6) rewrote `/images/<id>` → `/r/home-dev/images/<id>`
    // server-side; the SPA must pass that through byte-for-byte.
    const content =
      "look ![diagram](/r/home-dev/images/" +
      "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567.png)";
    const m = adaptMessage(msg(content), "/r/home-dev");
    // content is never rewritten by the SPA
    expect(m.content).toBe(content);
  });
});
