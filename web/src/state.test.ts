// Wire-contract tests for the profiles feature (Layer E, E1).
//
// state.ts is a hand-typed mirror of the Rust `AppState` (src/ui/app_state.rs)
// and the `InboundMessage`/`OutboundMessage` wire envelopes (src/ui/wire.rs).
// The backend now carries three new AppState fields and one new inbound
// message; state.ts has no runtime behaviour of its own for the AppState
// fields (TypeScript types vanish at compile time), so these tests exercise
// them at the type level (assignability) PLUS one real runtime sender,
// `selectProfile`, modeled 1:1 on how `select_pipeline` is built and used at
// the App.tsx call site (App.tsx:210:
//   onSelectPipeline={(name) => send({ type: "select_pipeline", name })}
// ).
//
// New wire surface under test:
//   - `AppState.profiles: string[]` — selectable profile names, sorted;
//     empty when none are defined OR a `--profile` pin is active. Optional
//     on the TS side so an older server's snapshot (field absent) still
//     type-checks; the safe default is `[]` (adaptProfiles below, or
//     equivalent call-site `?? []`), NOT `undefined`.
//   - `AppState.active_profile: string | null` — optional the same way;
//     the safe default for an absent field is `null`.
//   - `AppState.visible_tabs: string[]` — optional the same way; the safe
//     default for an ABSENT field is ALL legacy tab ids (an older server
//     doesn't know about tab visibility yet, so nothing should hide) —
//     this defaulting rule is exercised in `tabs.test.ts` via
//     `filterTabs(tabs, undefined)`, not repeated here.
//   - `InboundMessage` gains `{ type: "select_profile"; name: string | null }`.
//   - A `selectProfile(name: string | null): InboundMessage` builder,
//     mirroring the inline arrow function pipelines use today. Nothing in
//     the existing codebase requires this to be a named export (pipelines
//     builds its frame inline in App.tsx), but the task brief asks for a
//     `selectProfile` sender we can unit test directly, so it is specified
//     as a new named export of state.ts.
//
// `selectProfile` does not exist yet — this file is expected to fail at
// import time ("does not provide an export named 'selectProfile'"), which
// is the correct RED reason (feature absent, not a typo here).

import { describe, it, expect } from "vitest";
import { selectProfile } from "./state";
import type { AppState, InboundMessage } from "./state";

describe("selectProfile — inbound frame builder (mirrors select_pipeline)", () => {
  it("builds exactly {type:'select_profile', name} for a real profile name", () => {
    expect(selectProfile("web")).toEqual({
      type: "select_profile",
      name: "web",
    });
  });

  it("builds {type:'select_profile', name: null} for 'None (base config)'", () => {
    expect(selectProfile(null)).toEqual({
      type: "select_profile",
      name: null,
    });
  });

  it("produces an object with EXACTLY the two keys type/name (no stray fields)", () => {
    const frame = selectProfile("web");
    expect(Object.keys(frame).sort()).toEqual(["name", "type"]);
  });

  it("round-trips through JSON exactly like a hand-built select_pipeline frame would", () => {
    // Same shape family as `{ type: "select_pipeline", name }` (state.ts:269)
    // — JSON.stringify must not reorder/drop keys or coerce null away.
    const json = JSON.stringify(selectProfile(null));
    expect(JSON.parse(json)).toEqual({ type: "select_profile", name: null });
  });

  it("passes an empty-string profile name through verbatim (no trimming/rejection at this layer)", () => {
    // Whether "" is a valid profile name is a server-side concern; the
    // builder is a dumb frame constructor and must not special-case it.
    expect(selectProfile("")).toEqual({ type: "select_profile", name: "" });
  });

  it("passes a unicode profile name through verbatim", () => {
    expect(selectProfile("生産チーム")).toEqual({
      type: "select_profile",
      name: "生産チーム",
    });
  });

  it("is assignable to InboundMessage (the frame belongs to the wire union)", () => {
    const frame: InboundMessage = selectProfile("web");
    expect(frame.type).toBe("select_profile");
  });
});

describe("AppState — profiles/active_profile/visible_tabs are optional wire fields", () => {
  // These are compile-time assertions: if `profiles`/`active_profile`/
  // `visible_tabs` are missing from the `AppState` interface entirely, this
  // file fails to typecheck. Vitest's esbuild transform does not run a full
  // type-check, so we ALSO assert a runtime shape below to give the suite a
  // real RED signal (not just a silently-erased type). `tsc --noEmit` is the
  // authoritative check for the type-level half of this contract.

  it("a full new-server snapshot (with all three fields) satisfies AppState", () => {
    const full: Pick<
      AppState,
      "profiles" | "active_profile" | "visible_tabs"
    > = {
      profiles: ["web", "cli"],
      active_profile: "web",
      visible_tabs: ["session", "agents", "profile"],
    };
    expect(full.profiles).toEqual(["web", "cli"]);
    expect(full.active_profile).toBe("web");
    expect(full.visible_tabs).toEqual(["session", "agents", "profile"]);
  });

  it("an old-server snapshot with all three fields OMITTED still satisfies AppState (optional)", () => {
    // If any of the three were required (non-optional), this object literal
    // would fail `tsc` with a missing-property error. Kept as a same-file
    // compile-time proof alongside the runtime one above.
    const legacy: Pick<
      AppState,
      "profiles" | "active_profile" | "visible_tabs"
    > = {};
    expect(legacy.profiles).toBeUndefined();
    expect(legacy.active_profile).toBeUndefined();
    expect(legacy.visible_tabs).toBeUndefined();
  });

  it("active_profile accepts an explicit null (no selectable profile pinned)", () => {
    const withNull: Pick<AppState, "active_profile"> = { active_profile: null };
    expect(withNull.active_profile).toBeNull();
  });

  it("profiles accepts an empty array (no profiles configured, or a --profile pin is active)", () => {
    const empty: Pick<AppState, "profiles"> = { profiles: [] };
    expect(empty.profiles).toEqual([]);
  });

  it("visible_tabs accepts an empty array (server computed zero visible tabs)", () => {
    const empty: Pick<AppState, "visible_tabs"> = { visible_tabs: [] };
    expect(empty.visible_tabs).toEqual([]);
  });
});

describe("InboundMessage — select_profile is part of the union", () => {
  it("a hand-built select_profile frame satisfies InboundMessage", () => {
    const frame: InboundMessage = { type: "select_profile", name: "web" };
    expect(frame).toEqual({ type: "select_profile", name: "web" });
  });

  it("a hand-built select_profile frame with null name satisfies InboundMessage", () => {
    const frame: InboundMessage = { type: "select_profile", name: null };
    expect(frame).toEqual({ type: "select_profile", name: null });
  });
});
