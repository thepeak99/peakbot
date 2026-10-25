// RED tests for the pure team-model module (selection-models plan §5.3 / §9.2,
// items 1–8). Written BEFORE the implementation: `./teamModels` currently
// exports signatures that throw "not implemented", so every test below fails
// at runtime for the right reason (feature absent), not at import time.
//
// Fixture = the catalog of `selectionChip.test.tsx`: `peakbot-dev › team`
// (model `sonnet`; subagents `reviewer` — sonnet, INHERITED — and `tester` —
// haiku, explicit) plus the single agent `review`.

import { describe, expect, it } from "vitest";
import {
  EMPTY_TEAM,
  anyOverride,
  boundTeamState,
  overrideCount,
  sameTeam,
  selectFrameFor,
  teamMembers,
  teamView,
  withModel,
  type Member,
  type TeamState,
} from "./teamModels";
import type { CatalogAgent, Selection } from "./types";

const TEAM: CatalogAgent = {
  name: "team",
  model: "sonnet",
  subagents: [
    { name: "reviewer", model: "sonnet", inherited: true },
    { name: "tester", model: "haiku", inherited: false },
  ],
};
const SOLO: CatalogAgent = { name: "review", model: "sonnet", subagents: [] };

const ORCH: Member = { kind: "orchestrator" };
const sub = (name: string): Member => ({ kind: "subagent", name });

const selection = (subagentModels: Record<string, string> = {}): Selection => ({
  profile: "peakbot-dev",
  agent: "team",
  subagentModels,
});

/** Recursively freeze, so any in-place mutation throws (modules are strict). */
function deepFreeze<T>(value: T): T {
  if (value && typeof value === "object") {
    for (const v of Object.values(value as object)) deepFreeze(v);
    Object.freeze(value);
  }
  return value;
}

const viewOf = (agent: CatalogAgent, team: TeamState, label: string) => {
  const found = teamView(agent, team).find((v) => v.label === label);
  if (!found) throw new Error(`no member view labelled ${label}`);
  return found;
};

describe("teamMembers", () => {
  it("teamMembers_orchestrator_first_then_subagents_with_captions", () => {
    expect(teamMembers(TEAM)).toEqual([
      { member: { kind: "orchestrator" }, label: "team", caption: "orchestrator" },
      { member: { kind: "subagent", name: "reviewer" }, label: "reviewer", caption: "subagent" },
      { member: { kind: "subagent", name: "tester" }, label: "tester", caption: "subagent" },
    ]);

    // A single agent has no team: the one member is captioned `main`.
    expect(teamMembers(SOLO)).toEqual([
      { member: { kind: "orchestrator" }, label: "review", caption: "main" },
    ]);
  });

  it("teamMembers_keeps_catalog_order_not_alphabetical", () => {
    const agent: CatalogAgent = {
      name: "t",
      model: "m",
      subagents: [
        { name: "zeta", model: "m", inherited: true },
        { name: "alpha", model: "m", inherited: true },
      ],
    };
    expect(teamMembers(agent).map((m) => m.label)).toEqual(["t", "zeta", "alpha"]);
  });
});

describe("boundTeamState", () => {
  it("boundTeamState_derives_orchestrator_override_from_activeAlias", () => {
    // activeAlias equal to the agent's model → not overridden.
    expect(boundTeamState(TEAM, selection(), "sonnet").orchestrator).toBeNull();
    // Empty alias (before the first stats frame) → not overridden.
    expect(boundTeamState(TEAM, selection(), "").orchestrator).toBeNull();
    // Anything else IS the orchestrator override.
    expect(boundTeamState(TEAM, selection(), "opus").orchestrator).toBe("opus");
  });

  it("boundTeamState_copies_subagent_overrides_from_the_selection", () => {
    const sel = selection({ tester: "opus" });
    const team = boundTeamState(TEAM, sel, "sonnet");
    expect(team.subagents).toEqual({ tester: "opus" });
    expect(team.subagents, "a copy, not the selection's own object").not.toBe(
      sel.subagentModels,
    );
  });

  it("boundTeamState_with_no_overrides_equals_the_empty_team", () => {
    expect(boundTeamState(TEAM, selection(), "sonnet")).toEqual(EMPTY_TEAM);
  });
});

describe("EMPTY_TEAM", () => {
  it("EMPTY_TEAM_is_the_unoverridden_team", () => {
    expect(EMPTY_TEAM).toEqual({ orchestrator: null, subagents: {} });
  });
});

describe("teamView", () => {
  it("teamView_defaults", () => {
    const orch = viewOf(TEAM, EMPTY_TEAM, "team");
    expect(orch.member).toEqual({ kind: "orchestrator" });
    expect(orch.caption).toBe("orchestrator");
    expect(orch.effective).toBe("sonnet");
    expect(orch.defaultAlias).toBe("sonnet");
    expect(orch.overridden).toBe(false);
    expect(orch.inherits).toBe(false);

    const reviewer = viewOf(TEAM, EMPTY_TEAM, "reviewer");
    expect(reviewer.member).toEqual({ kind: "subagent", name: "reviewer" });
    expect(reviewer.caption).toBe("subagent");
    expect(reviewer.inherits).toBe(true);
    expect(reviewer.effective).toBe("sonnet");
    expect(reviewer.defaultAlias).toBe("sonnet");
    expect(reviewer.overridden).toBe(false);

    const tester = viewOf(TEAM, EMPTY_TEAM, "tester");
    expect(tester.effective).toBe("haiku");
    expect(tester.defaultAlias).toBe("haiku");
    expect(tester.inherits).toBe(false);
    expect(tester.overridden).toBe(false);
  });

  it("teamView_single_agent_is_one_main_member", () => {
    const views = teamView(SOLO, EMPTY_TEAM);
    expect(views).toHaveLength(1);
    expect(views[0].caption).toBe("main");
    expect(views[0].effective).toBe("sonnet");
  });

  it("teamView_inherited_follows_orchestrator_override", () => {
    const team: TeamState = { orchestrator: "opus", subagents: {} };

    const orch = viewOf(TEAM, team, "team");
    expect(orch.overridden).toBe(true);
    expect(orch.effective).toBe("opus");
    expect(orch.defaultAlias).toBe("sonnet");

    const reviewer = viewOf(TEAM, team, "reviewer");
    expect(reviewer.effective).toBe("opus");
    expect(reviewer.defaultAlias).toBe("opus");
    expect(reviewer.inherits).toBe(true);
    expect(reviewer.overridden).toBe(false);

    // An explicit subagent does NOT ride the orchestrator.
    const tester = viewOf(TEAM, team, "tester");
    expect(tester.effective).toBe("haiku");
    expect(tester.inherits).toBe(false);
  });

  it("teamView_subagent_override", () => {
    const team: TeamState = { orchestrator: null, subagents: { tester: "opus" } };
    const tester = viewOf(TEAM, team, "tester");
    expect(tester.overridden).toBe(true);
    expect(tester.effective).toBe("opus");
    expect(tester.defaultAlias).toBe("haiku");
    expect(tester.inherits).toBe(false);
  });

  it("teamView_overriding_an_inherited_subagent_stops_it_inheriting", () => {
    const team: TeamState = { orchestrator: null, subagents: { reviewer: "haiku" } };
    const reviewer = viewOf(TEAM, team, "reviewer");
    expect(reviewer.overridden).toBe(true);
    expect(reviewer.inherits).toBe(false);
    expect(reviewer.effective).toBe("haiku");
    expect(reviewer.defaultAlias).toBe("sonnet");
  });
});

describe("withModel", () => {
  it("withModel_sets_and_normalizes", () => {
    const base: TeamState = { orchestrator: null, subagents: {} };

    // A non-default alias is stored.
    expect(withModel(TEAM, base, sub("tester"), "opus")).toEqual({
      orchestrator: null,
      subagents: { tester: "opus" },
    });

    // The member's own default clears the entry.
    const overridden: TeamState = { orchestrator: null, subagents: { tester: "opus" } };
    expect(withModel(TEAM, overridden, sub("tester"), "haiku")).toEqual(EMPTY_TEAM);
    // `null` is "reset".
    expect(withModel(TEAM, overridden, sub("tester"), null)).toEqual(EMPTY_TEAM);

    // Orchestrator: the agent's configured model is "not overridden".
    const orchOver: TeamState = { orchestrator: "opus", subagents: {} };
    expect(withModel(TEAM, orchOver, ORCH, "sonnet")).toEqual(EMPTY_TEAM);
    expect(withModel(TEAM, base, ORCH, "opus")).toEqual({
      orchestrator: "opus",
      subagents: {},
    });
  });

  it("withModel_does_not_mutate_its_input_and_returns_a_new_object", () => {
    const team = deepFreeze<TeamState>({
      orchestrator: "opus",
      subagents: { tester: "opus" },
    });
    const next = withModel(TEAM, team, sub("reviewer"), "haiku");
    expect(next).not.toBe(team);
    expect(next.subagents).not.toBe(team.subagents);
    expect(team).toEqual({ orchestrator: "opus", subagents: { tester: "opus" } });
    // And the clearing path too.
    const cleared = withModel(TEAM, team, sub("tester"), null);
    expect(cleared).not.toBe(team);
    expect(team.subagents).toEqual({ tester: "opus" });
  });

  it("withModel_keeps_the_other_members_overrides", () => {
    const team: TeamState = { orchestrator: null, subagents: { tester: "opus" } };
    expect(withModel(TEAM, team, ORCH, "opus")).toEqual({
      orchestrator: "opus",
      subagents: { tester: "opus" },
    });
    expect(withModel(TEAM, team, sub("reviewer"), "haiku")).toEqual({
      orchestrator: null,
      subagents: { tester: "opus", reviewer: "haiku" },
    });
  });

  it("withModel_default_of_an_inherited_subagent_follows_the_orchestrator_override", () => {
    const team: TeamState = { orchestrator: "opus", subagents: { reviewer: "haiku" } };
    // The reviewer's default is now `opus` (it rides the orchestrator) →
    // picking opus is "back to default" → cleared.
    expect(withModel(TEAM, team, sub("reviewer"), "opus")).toEqual({
      orchestrator: "opus",
      subagents: {},
    });
    // …and picking `sonnet` (the agent's configured model) is now an OVERRIDE.
    expect(
      withModel(TEAM, { orchestrator: "opus", subagents: {} }, sub("reviewer"), "sonnet"),
    ).toEqual({ orchestrator: "opus", subagents: { reviewer: "sonnet" } });
  });
});

describe("selectFrameFor", () => {
  it("selectFrameFor_shapes", () => {
    // Nothing overridden → exactly the 3-key frame (no undefined `model`).
    expect(selectFrameFor("peakbot-dev", TEAM, EMPTY_TEAM)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
    });

    // Orchestrator + subagent overrides ride the frame.
    expect(
      selectFrameFor("peakbot-dev", TEAM, {
        orchestrator: "opus",
        subagents: { tester: "haiku" },
      }),
    ).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      model: "opus",
      subagent_models: { tester: "haiku" },
    });
  });

  it("selectFrameFor_omits_the_model_when_only_subagents_are_overridden", () => {
    expect(
      selectFrameFor("writer", TEAM, { orchestrator: null, subagents: { tester: "opus" } }),
    ).toStrictEqual({
      type: "select",
      profile: "writer",
      agent: "team",
      subagent_models: { tester: "opus" },
    });
  });

  it("selectFrameFor_omits_subagent_models_when_only_the_orchestrator_is_overridden", () => {
    expect(
      selectFrameFor("writer", TEAM, { orchestrator: "opus", subagents: {} }),
    ).toStrictEqual({ type: "select", profile: "writer", agent: "team", model: "opus" });
  });
});

describe("overrideCount / anyOverride", () => {
  it("overrideCount_counts_subagents_only", () => {
    expect(overrideCount(EMPTY_TEAM)).toBe(0);
    expect(overrideCount({ orchestrator: "opus", subagents: {} })).toBe(0);
    expect(overrideCount({ orchestrator: null, subagents: { a: "x" } })).toBe(1);
    expect(overrideCount({ orchestrator: "opus", subagents: { a: "x", b: "y" } })).toBe(2);
  });

  it("anyOverride_includes_orchestrator", () => {
    expect(anyOverride(EMPTY_TEAM)).toBe(false);
    expect(anyOverride({ orchestrator: "opus", subagents: {} })).toBe(true);
    expect(anyOverride({ orchestrator: null, subagents: { a: "x" } })).toBe(true);
    expect(anyOverride({ orchestrator: "opus", subagents: { a: "x" } })).toBe(true);
  });
});

describe("sameTeam", () => {
  it("sameTeam_compares_structurally", () => {
    expect(sameTeam(EMPTY_TEAM, { orchestrator: null, subagents: {} })).toBe(true);
    expect(
      sameTeam(
        { orchestrator: "opus", subagents: { a: "x", b: "y" } },
        { orchestrator: "opus", subagents: { b: "y", a: "x" } },
      ),
      "subagent key order is irrelevant",
    ).toBe(true);

    expect(sameTeam(EMPTY_TEAM, { orchestrator: "opus", subagents: {} })).toBe(false);
    expect(
      sameTeam(
        { orchestrator: null, subagents: { a: "x" } },
        { orchestrator: null, subagents: { a: "y" } },
      ),
      "a different alias",
    ).toBe(false);
    expect(
      sameTeam(
        { orchestrator: null, subagents: { a: "x" } },
        { orchestrator: null, subagents: { a: "x", b: "y" } },
      ),
      "an extra subagent",
    ).toBe(false);
  });
});
