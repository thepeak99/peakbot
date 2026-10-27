// Error-path (segment array) → screen routing (spec §9.6 "Fix" rules).
// The segment array is NEVER split on "." — profile/agent names may contain
// dots and spaces (§4.2 name rule).

import type { ConfigErr, Seg } from "./types";

export type Section = "providers" | "mcps" | "profiles" | "general" | "system" | "desktop" | "raw";

export interface RouteTarget {
  section: Section;
  profile?: string; // ["profiles", p, …]
  agent?: string; // … "agents", a …
  subagent?: string; // … "subagents", s …
  field: Seg[]; // remaining segments after profile/agent/subagent
}

/**
 * Route a server error `path` to the screen that owns it:
 *   ["providers", …] → providers;  ["mcps", …] → mcps;
 *   ["profiles", p] → profile page; ["profiles", p, "agents", a, …] → agent
 *   page;  ["general", …] → general;  [] (root) → raw.
 */
export function route(path: Seg[]): RouteTarget {
  const head: Seg | undefined = path[0];

  if (head === "profiles") {
    const target: RouteTarget = { section: "profiles", field: [] };
    const name = path[1];
    if (typeof name !== "string") {
      // No profile name (a whole-`profiles:` error) — the list owns it.
      target.field = path.slice(1);
      return target;
    }
    target.profile = name;
    if (path[2] === "agents" && typeof path[3] === "string") {
      target.agent = path[3];
      if (path[4] === "subagents" && typeof path[5] === "string") {
        target.subagent = path[5];
        target.field = path.slice(6);
      } else {
        target.field = path.slice(4);
      }
    } else {
      target.field = path.slice(2);
    }
    return target;
  }

  if (head === "providers") return { section: "providers", field: path.slice(1) };
  if (head === "mcps") return { section: "mcps", field: path.slice(1) };
  if (head === "general") return { section: "general", field: path.slice(1) };
  if (head === "system") return { section: "system", field: path.slice(1) };

  // [] (document root) — and any head outside the four config blocks —
  // lands in the Raw YAML pane, which always shows the whole document.
  return { section: "raw", field: path };
}

/** `profiles.dev.agents.team.model` — the dot rendering for an error path. */
export function dotPath(path: Seg[]): string {
  return path.map(String).join(".");
}

/**
 * The message of the first validation error whose path EXACTLY matches
 * `path` (segment by segment — names may contain dots), or `undefined`.
 * Panes pass this to their widgets' `invalid` prop (spec §9.6 red fields).
 */
export function errAt(errors: ConfigErr[], path: Seg[]): string | undefined {
  return (
    errors.find(
      (e) => e.path.length === path.length && e.path.every((seg, i) => seg === path[i]),
    )?.message
  );
}
