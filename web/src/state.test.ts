// Wire-contract tests for the 0.24 cutover: the single `select` frame
// (replacing `select_profile` / `select_pipeline`) and the catalog/selection
// fields on `AppState` (src/ui/app_state.rs + src/config/resolve.rs).
//
// The Rust side is the source of truth:
//   - `InboundMessage::Select { profile, agent }` (src/ui/wire.rs) —
//     `{"type":"select","profile":..,"agent":..}`.
//   - `AppState` carries `catalog: Vec<ProfileInfo>`, `selection: Selection`,
//     `profile_pinned: bool`, and the DERIVED `settings_available =
//     !profile_pinned` (all `#[serde(default)]`, so optional on the TS mirror
//     so old wire snapshots parse).
//
// `select` is a named export so the call site is unit-testable without
// mounting the socket (the same reason `selectProfile` was a named export).

import { describe, it, expect } from "vitest";
import { select, type AppState, type InboundMessage } from "./state";

describe("select — inbound frame builder (replaces select_profile/select_pipeline)", () => {
  it("builds exactly {type:'select', profile, agent} for a real pair", () => {
    expect(select("dev", "team")).toEqual({
      type: "select",
      profile: "dev",
      agent: "team",
    });
  });

  it("passes unicode profile/agent names through verbatim", () => {
    expect(select("生産チーム", "レビュー")).toEqual({
      type: "select",
      profile: "生産チーム",
      agent: "レビュー",
    });
  });

  it("passes an empty string through verbatim (no trimming/rejection at this layer)", () => {
    // Whether "" is a valid profile/agent is a server-side concern; the
    // client's job is to ship exactly what the caller asked for.
    expect(select("", "")).toEqual({ type: "select", profile: "", agent: "" });
  });

  it("round-trips through JSON exactly like a hand-built select frame would", () => {
    const json = JSON.stringify(select("dev", "team"));
    expect(JSON.parse(json)).toEqual({
      type: "select",
      profile: "dev",
      agent: "team",
    });
  });

  it("satisfies the InboundMessage union", () => {
    const frame: InboundMessage = select("dev", "team");
    expect(frame.type).toBe("select");
  });
});

describe("AppState — catalog/selection/profile_pinned/settings_available are optional wire fields", () => {
  // These are compile-time assertions: if any of the new fields is removed
  // from AppState, the `Pick` below stops type-checking. At runtime they
  // must all be *optional* (absent on old wire snapshots).
  it("a full 0.24 snapshot carries all four fields", () => {
    const full: Pick<
      AppState,
      "catalog" | "selection" | "profile_pinned" | "settings_available"
    > = {
      catalog: [
        {
          name: "dev",
          origin: "repo",
          overrides_global: true,
          agents: [
            {
              name: "team",
              model: "sonnet",
              subagents: [
                { name: "junior", model: "haiku", inherited: false },
              ],
            },
          ],
        },
      ],
      selection: { profile: "dev", agent: "team" },
      profile_pinned: false,
      settings_available: true,
    };
    expect(full.catalog).toHaveLength(1);
    expect(full.selection).toEqual({ profile: "dev", agent: "team" });
    expect(full.profile_pinned).toBe(false);
    expect(full.settings_available).toBe(true);
  });

  it("an old snapshot (none of the fields present) still parses", () => {
    const legacy: Pick<
      AppState,
      "catalog" | "selection" | "profile_pinned" | "settings_available"
    > = {};
    expect(legacy.catalog).toBeUndefined();
    expect(legacy.selection).toBeUndefined();
    expect(legacy.profile_pinned).toBeUndefined();
    expect(legacy.settings_available).toBeUndefined();
  });

  it("catalog accepts an empty array (no profiles configured)", () => {
    const empty: Pick<AppState, "catalog"> = { catalog: [] };
    expect(empty.catalog).toEqual([]);
  });

  it("selection accepts the (profile, agent) pair", () => {
    const s: Pick<AppState, "selection"> = {
      selection: { profile: "dev", agent: "team" },
    };
    expect(s.selection).toEqual({ profile: "dev", agent: "team" });
  });
});

describe("InboundMessage — select is part of the union, old frames are gone", () => {
  it("a hand-built select frame satisfies InboundMessage", () => {
    const frame: InboundMessage = { type: "select", profile: "dev", agent: "team" };
    expect(frame).toEqual({ type: "select", profile: "dev", agent: "team" });
  });
});
