// Team-model logic for the selection chip (selection-models plan §5.3).
// Pure and React-free: members, effective / default / inherited / overridden,
// normalization, and building the `select` frame.
//
// The orchestrator's override is DERIVED from the session's active model
// alias (the server already owns it); only subagent overrides are stored in
// the team state, and only when they differ from the member's default.

import { select, type InboundMessage } from "./state";
import type { CatalogAgent, Selection } from "./types";

export type Member =
  | { kind: "orchestrator" }
  | { kind: "subagent"; name: string };

/** The model setup of ONE agent's team. */
export interface TeamState {
  /** null = the agent's configured model (not overridden). */
  orchestrator: string | null;
  /** Overridden subagents only. */
  subagents: Record<string, string>;
}

export interface MemberView {
  member: Member;
  /** Orchestrator → agent.name; subagent → its name. */
  label: string;
  caption: "orchestrator" | "main" | "subagent";
  /** Alias in effect. */
  effective: string;
  /** What "Reset to default" returns to (and where the `default` tag goes). */
  defaultAlias: string;
  overridden: boolean;
  /** Subagent, not overridden, `inherited` in the catalog → rides the orchestrator. */
  inherits: boolean;
}

export const EMPTY_TEAM: TeamState = { orchestrator: null, subagents: {} };

/** [orchestrator, …subagents in catalog order]. The lone member of a
 * single agent is captioned `main`, not `orchestrator`. */
export function teamMembers(
  agent: CatalogAgent,
): { member: Member; label: string; caption: MemberView["caption"] }[] {
  const members: { member: Member; label: string; caption: MemberView["caption"] }[] = [
    {
      member: { kind: "orchestrator" },
      label: agent.name,
      caption: agent.subagents.length > 0 ? "orchestrator" : "main",
    },
  ];
  for (const s of agent.subagents) {
    members.push({ member: { kind: "subagent", name: s.name }, label: s.name, caption: "subagent" });
  }
  return members;
}

/** The bound team, derived from server echoes: the orchestrator's override
 * is `activeAlias` whenever it differs from the agent's configured model
 * (an empty alias, before the first stats frame, is "not overridden");
 * subagent overrides are copied out of the selection. */
export function boundTeamState(
  agent: CatalogAgent,
  selection: Selection,
  activeAlias: string,
): TeamState {
  return {
    orchestrator: activeAlias !== "" && activeAlias !== agent.model ? activeAlias : null,
    subagents: { ...selection.subagentModels },
  };
}

/** What "Reset to default" returns to for `member`: the configured model —
 * except inherited subagents, whose default rides the orchestrator's
 * effective model (an orchestrator override moves it). */
function memberDefault(agent: CatalogAgent, team: TeamState, member: Member): string {
  if (member.kind === "orchestrator") return agent.model;
  const sub = agent.subagents.find((s) => s.name === member.name);
  if (!sub) return agent.model;
  return sub.inherited ? (team.orchestrator ?? agent.model) : sub.model;
}

/** One view per team member, orchestrator first (catalog order). */
export function teamView(agent: CatalogAgent, team: TeamState): MemberView[] {
  const orchEffective = team.orchestrator ?? agent.model;
  const views: MemberView[] = [
    {
      member: { kind: "orchestrator" },
      label: agent.name,
      caption: agent.subagents.length > 0 ? "orchestrator" : "main",
      effective: orchEffective,
      defaultAlias: agent.model,
      overridden: team.orchestrator !== null,
      inherits: false,
    },
  ];
  for (const s of agent.subagents) {
    const defaultAlias = s.inherited ? orchEffective : s.model;
    const overridden = s.name in team.subagents;
    views.push({
      member: { kind: "subagent", name: s.name },
      label: s.name,
      caption: "subagent",
      effective: team.subagents[s.name] ?? defaultAlias,
      defaultAlias,
      overridden,
      inherits: !overridden && s.inherited,
    });
  }
  return views;
}

/** Set or clear ONE member's model, normalized: an alias equal to the
 * member's current default (or `null`) clears the entry. Pure — returns a
 * new state, never mutates `team`. */
export function withModel(
  agent: CatalogAgent,
  team: TeamState,
  member: Member,
  alias: string | null,
): TeamState {
  if (member.kind === "orchestrator") {
    const next = alias !== null && alias !== agent.model ? alias : null;
    return { orchestrator: next, subagents: { ...team.subagents } };
  }
  const subagents = { ...team.subagents };
  if (alias === null || alias === memberDefault(agent, team, member)) {
    delete subagents[member.name];
  } else {
    subagents[member.name] = alias;
  }
  return { orchestrator: team.orchestrator, subagents };
}

/** The full desired-setup frame for the browsed profile's team agent. */
export function selectFrameFor(
  profile: string,
  agent: CatalogAgent,
  team: TeamState,
): InboundMessage {
  return select(profile, agent.name, {
    model: team.orchestrator ?? undefined,
    subagentModels: team.subagents,
  });
}

/** Subagent overrides only (the chip's `+N` badge). */
export function overrideCount(team: TeamState): number {
  return Object.keys(team.subagents).length;
}

/** Any override at all, orchestrator included (the Team pane's reset-all gate). */
export function anyOverride(team: TeamState): boolean {
  return team.orchestrator !== null || overrideCount(team) > 0;
}

/** Structural equality; subagent key order is irrelevant. */
export function sameTeam(a: TeamState, b: TeamState): boolean {
  if (a.orchestrator !== b.orchestrator) return false;
  const aKeys = Object.keys(a.subagents);
  const bKeys = Object.keys(b.subagents);
  if (aKeys.length !== bKeys.length) return false;
  return aKeys.every((k) => a.subagents[k] === b.subagents[k]);
}
