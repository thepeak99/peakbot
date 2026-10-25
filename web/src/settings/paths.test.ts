// RED (T22 D1c): ./paths — error-path (segment array) → screen routing.
// Spec §9.6 "Fix" rules:
//   ["providers", …]            → Providers
//   ["mcps", …]                 → MCP
//   ["profiles", p, …]          → profile page
//   ["profiles", p, "agents", a, …]            → agent page
//   ["profiles", p, "agents", a, "subagents", s, …] → agent page (subagent)
//   ["general", …]              → General
//   [] (document root)          → Raw YAML
// The segment array is NEVER split on "." — names may contain dots and
// spaces (§4.2 name rule).
//
// Assumed API (see contract.ts):
//   route(path: Seg[]): RouteTarget
//   RouteTarget = { section, profile?, agent?, subagent?, field: Seg[] }
//
// Expected RED: `./paths` does not exist yet → import error.

import { describe, expect, it } from "vitest";
import { route } from "./paths";

describe("paths", () => {
  it("paths_route_agent_page", () => {
    // Profile page.
    expect(route(["profiles", "dev"])).toEqual({
      section: "profiles",
      profile: "dev",
      field: [],
    });

    // Agent page: segments 2–3 are "agents", a.
    expect(route(["profiles", "dev", "agents", "team", "model"])).toEqual({
      section: "profiles",
      profile: "dev",
      agent: "team",
      field: ["model"],
    });

    // Agent page with a subagent and a deeper field.
    expect(
      route(["profiles", "dev", "agents", "team", "subagents", "senior", "prompt"]),
    ).toEqual({
      section: "profiles",
      profile: "dev",
      agent: "team",
      subagent: "senior",
      field: ["prompt"],
    });

    // List indices are segment values, not parsed strings.
    expect(route(["providers", 0, "models", 2, "alias"])).toEqual({
      section: "providers",
      field: [0, "models", 2, "alias"],
    });
    expect(route(["mcps", 1, "url"])).toEqual({ section: "mcps", field: [1, "url"] });
    expect(route(["general", "web", "bind"])).toEqual({
      section: "general",
      field: ["web", "bind"],
    });
  });

  it("paths_names_with_dot_and_space", () => {
    // Dots and spaces inside names must survive routing untouched.
    expect(route(["profiles", "my.repo", "agents", "team lead", "model"])).toEqual({
      section: "profiles",
      profile: "my.repo",
      agent: "team lead",
      field: ["model"],
    });

    expect(route(["profiles", "a.b", "agents", "c d", "subagents", "e f", "prompt"])).toEqual({
      section: "profiles",
      profile: "a.b",
      agent: "c d",
      subagent: "e f",
      field: ["prompt"],
    });
  });

  it("root_routes_raw", () => {
    // [] = document root → the Raw YAML pane.
    expect(route([])).toEqual({ section: "raw", field: [] });
  });
});
