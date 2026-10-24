// Team-model logic for the selection chip (selection-models plan §5.3).
// Pure and React-free: members, effective / default / inherited / overridden,
// normalization, and building the `select` frame.
//
// RED STUB (T7): signatures + the spec-literal `EMPTY_TEAM` only. Every
// function throws until T8 lands the implementation (the contract is
// `src/teamModels.test.ts`).

import type { InboundMessage } from "./state";
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

function notImplemented(name: string, args: unknown[]): never {
  throw new Error(
    `teamModels.${name}: not implemented (T8) (${args.length} args)`,
  );
}

export const EMPTY_TEAM: TeamState = { orchestrator: null, subagents: {} };

export function teamMembers(
  agent: CatalogAgent,
): { member: Member; label: string; caption: MemberView["caption"] }[] {
  return notImplemented("teamMembers", [agent]);
}

export function boundTeamState(
  agent: CatalogAgent,
  selection: Selection,
  activeAlias: string,
): TeamState {
  return notImplemented("boundTeamState", [agent, selection, activeAlias]);
}

export function teamView(agent: CatalogAgent, team: TeamState): MemberView[] {
  return notImplemented("teamView", [agent, team]);
}

export function withModel(
  agent: CatalogAgent,
  team: TeamState,
  member: Member,
  alias: string | null,
): TeamState {
  return notImplemented("withModel", [agent, team, member, alias]);
}

export function selectFrameFor(
  profile: string,
  agent: CatalogAgent,
  team: TeamState,
): InboundMessage {
  return notImplemented("selectFrameFor", [profile, agent, team]);
}

export function overrideCount(team: TeamState): number {
  return notImplemented("overrideCount", [team]);
}

export function anyOverride(team: TeamState): boolean {
  return notImplemented("anyOverride", [team]);
}

export function sameTeam(a: TeamState, b: TeamState): boolean {
  return notImplemented("sameTeam", [a, b]);
}
