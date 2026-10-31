// Wire-mirror contract for the per-profile tab colour (remote-profiles
// design T1/T2, §6.1): the TS mirror in `state.ts` must accept
//   - `ConversationSummary.profile?: string | null` (Rust
//     `#[serde(default)] profile: Option<String>`),
//   - `WireProfileInfo.color: TabColor` (RP8: always set server-side),
//   - `type TabColor` = the 8 lowercase palette names.
//
// The runtime assertions below are trivially true — the contract is the
// TYPES. This file typechecks (RED → GREEN) only once `state.ts` mirrors
// the wire; `npm run typecheck` is its gate, not vitest.

import { describe, expect, it } from "vitest";
import type { ConversationSummary, TabColor, WireProfileInfo } from "./state";

const PALETTE: TabColor[] = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink"];

describe("state.ts — tab-colour wire mirror (remote-profiles T1/T2)", () => {
  it("ConversationSummary accepts a profile (string, null, or absent)", () => {
    const withProfile: ConversationSummary = {
      id: "i",
      name: "n",
      updated_at: "t",
      message_count: 0,
      model: "m",
      active: false,
      running: false,
      profile: "home-dev",
    };
    const legacyNull: ConversationSummary = { ...withProfile, profile: null };
    const absent: ConversationSummary = {
      id: "i",
      name: "n",
      updated_at: "t",
      message_count: 0,
      model: "m",
      active: false,
    };
    expect(withProfile.profile).toBe("home-dev");
    expect(legacyNull.profile).toBeNull();
    expect(absent.profile).toBeUndefined();
  });

  it("WireProfileInfo carries the resolved colour (RP8: always set)", () => {
    const entry: WireProfileInfo = {
      name: "dev",
      origin: "global",
      overrides_global: false,
      agents: [],
      color: "teal",
    };
    expect(entry.color).toBe("teal");
    expect(entry.name).toBe("dev");
  });

  it("WireProfileInfo origin is the locked 3-value union (incl. remote, task 9)", () => {
    // Type-level pin: the mirror must accept "remote" even though the
    // server only emits remote entries from task 9 (design §6.1).
    const remoteEntry: WireProfileInfo = {
      name: "home-dev",
      origin: "remote",
      overrides_global: false,
      agents: [],
      color: "red",
    };
    expect(remoteEntry.origin).toBe("remote");
  });

  it("the palette is exactly the 8 locked names, each a TabColor", () => {
    expect(PALETTE).toEqual(["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink"]);
    expect(new Set(PALETTE).size).toBe(8);
  });
});
