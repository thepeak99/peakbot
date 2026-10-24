// T24 (RED): NameFilter tri-state ⇄ YAML serializers (spec §9.3 Tools/MCP
// cards, plan T24). The ./filter module does not exist yet — this file must
// fail with a missing-module import error until the builder implements
// parseFilter / serializeFilter / setMode per contract.ts.

import { describe, expect, it } from "vitest";
import {
  parseFilter,
  serializeFilter,
  setMode,
  type FilterState,
} from "./filter";

describe("filter (NameFilter tri-state)", () => {
  // Every UI state must survive a serialize → parse roundtrip. The
  // allowlist-empty case roundtrips through {enabled: false}.
  const ROUNDTRIPS: FilterState[] = [
    { mode: "all", names: [] },
    { mode: "only", names: ["playwright", "linear"] },
    { mode: "disabled", names: ["powershell"] },
    { mode: "only", names: [] }, // → {enabled: false} → back
  ];

  it("filter_tri_roundtrip_all_cases", () => {
    for (const state of ROUNDTRIPS) {
      const value = serializeFilter(state);
      expect(
        parseFilter(value),
        `roundtrip of ${JSON.stringify(state)} via ${JSON.stringify(value)}`,
      ).toEqual(state);
    }
  });

  // §9.3: "Allowlist with zero checked serializes as {enabled: false}".
  it("allowlist_empty_is_enabled_false", () => {
    expect(serializeFilter({ mode: "only", names: [] })).toEqual({
      enabled: false,
    });
    expect(parseFilter({ enabled: false })).toEqual({
      mode: "only",
      names: [],
    });
  });

  // §9.3 [fix vs mockup]: switching mode clears the checked set (keeping it
  // would silently invert meaning).
  it("mode_switch_clears", () => {
    expect(setMode({ mode: "only", names: ["a", "b"] }, "disabled")).toEqual({
      mode: "disabled",
      names: [],
    });
    expect(setMode({ mode: "disabled", names: ["x"] }, "only")).toEqual({
      mode: "only",
      names: [],
    });
    expect(setMode({ mode: "all", names: [] }, "only")).toEqual({
      mode: "only",
      names: [],
    });
    // Same mode is a no-op — the checked set is kept.
    expect(setMode({ mode: "only", names: ["a"] }, "only")).toEqual({
      mode: "only",
      names: ["a"],
    });
  });

  // §9.3 Tools card: "Blocklist with 0 → omitted".
  it("blocklist_empty_serializes_to_omitted", () => {
    expect(serializeFilter({ mode: "disabled", names: [] })).toBeUndefined();
  });

  it("parse_filter_edges", () => {
    // Absent key = "allow all".
    expect(parseFilter(undefined)).toEqual({ mode: "all", names: [] });
    expect(parseFilter(null)).toEqual({ mode: "all", names: [] });
    // Garbage is not a filter — fall back to "all" (the server would
    // reject it; the UI must not crash).
    expect(parseFilter("only")).toEqual({ mode: "all", names: [] });
    expect(parseFilter([1, 2])).toEqual({ mode: "all", names: [] });
    // Empty allowlist in the file is the allowlist-empty state.
    expect(parseFilter({ only: [] })).toEqual({ mode: "only", names: [] });
    // `only`/`disabled` win over a redundant enabled: false.
    expect(parseFilter({ enabled: false, only: ["a"] })).toEqual({
      mode: "only",
      names: ["a"],
    });
    expect(parseFilter({ enabled: false, disabled: ["b"] })).toEqual({
      mode: "disabled",
      names: ["b"],
    });
    // Output carries exactly the listed keys — no `enabled: true`, no empty
    // sibling list, order of names preserved.
    expect(serializeFilter({ mode: "only", names: ["a", "b"] })).toEqual({
      only: ["a", "b"],
    });
    expect(serializeFilter({ mode: "disabled", names: ["a"] })).toEqual({
      disabled: ["a"],
    });
    expect(serializeFilter({ mode: "all", names: [] })).toBeUndefined();
  });
});
