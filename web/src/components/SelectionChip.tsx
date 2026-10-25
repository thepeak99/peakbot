// The selection chip — the ONE place that sets up the NEXT conversation:
// Profile › Agent › Team › Model drill-down (selection-models plan §5.3).
//
// The chip renders ONLY from server echoes (`selection`, `activeAlias`) and
// talks to the server through ONE frame: `select(profile, agent,
// {model?, subagentModels?})` — the full desired setup. The team's models
// live in teamModels.ts (pure); the columns are presentational panes.
//
// Layout is CSS-only, one DOM tree: four columns at ≥1100 px; at 801–1099 px
// the Model pane overlays the Profile column; at ≤800 px one pane at a time,
// full width, with ‹ Back — the drill state always advances, the breakpoint
// decides what is visible.
//
// Lock: once the conversation has a turn the chip is disabled, prefixed 🔒,
// drops the ▾, and never opens. Esc steps back out of the Model pane first,
// then closes (both preventDefault, so Settings' Esc handler stays open);
// mousedown outside closes.

import { useEffect, useRef, useState } from "react";
import { boundAgentOf } from "../adapt";
import { select } from "../state";
import type { CatalogAgent } from "../types";
import {
  EMPTY_TEAM,
  anyOverride,
  boundTeamState,
  overrideCount,
  sameTeam,
  selectFrameFor,
  teamView,
  withModel,
  type Member,
  type TeamState,
} from "../teamModels";
import type { SelectionChipProps } from "../settings/contract";
import { AgentPane, ModelPane, ProfilePane, TeamPane } from "./SelectionPanes";

// The popover's drill state. The Model pane can't be open without a member,
// so the illegal state is unrepresentable.
type Nav =
  | { pane: "profile" }
  | { pane: "agent" }
  | { pane: "team" }
  | { pane: "model"; member: Member };

const TITLE_CLS = "px-2 py-1 text-[10px] uppercase tracking-wide text-zinc-500 max-[800px]:hidden";
const HIDDEN_NARROW = "max-[800px]:hidden";

function sameMember(a: Member, b: Member): boolean {
  if (a.kind !== b.kind) return false;
  return a.kind === "subagent" && b.kind === "subagent" ? a.name === b.name : true;
}

function memberLabel(member: Member, agent: CatalogAgent): string {
  return member.kind === "orchestrator" ? agent.name : member.name;
}

export function SelectionChip({
  catalog,
  selection,
  models,
  activeAlias,
  locked,
  pinned,
  send,
  onToast,
  dropUp = false,
}: SelectionChipProps) {
  const [open, setOpen] = useState(false);
  const [nav, setNav] = useState<Nav>({ pane: "profile" });
  // The profile being browsed; reset to the bound profile on every open so a
  // half-explored browse never leaks into the next one.
  const [browsed, setBrowsed] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // Locking while the popover is open closes it (read-only state).
  useEffect(() => {
    if (locked && open) setOpen(false);
  }, [locked, open]);

  // Esc (step back from Model, else close; always preventDefault) and
  // mousedown-outside, only while open. Deps carry `nav` so the Esc handler
  // sees the current pane.
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (nav.pane === "model") setNav({ pane: "team" });
      else setOpen(false);
    }
    function onDown(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [open, nav]);

  // Nothing to show: no selection (old server / before first frame) or an
  // empty catalog.
  if (selection === null || catalog.length === 0) return null;

  const boundAgent = boundAgentOf(catalog, selection);
  const selectedProfile = catalog.find((p) => p.name === selection.profile);

  // Pinned (--profile): only the pinned profile is listed, captioned `pinned`;
  // agent switching under a pin still works.
  const pinnedProfile = pinned ? catalog.find((p) => p.name === selection.profile) : undefined;
  const profileList = pinnedProfile ? [pinnedProfile] : catalog;
  const browsedName = browsed ?? selection.profile;
  const browsedProfile = profileList.find((p) => p.name === browsedName) ?? profileList[0];

  // The agent the Team/Model panes apply to: the bound agent when browsing
  // the bound profile, else the browsed profile's first agent. A non-bound
  // team starts from the empty setup — acting on it moves the binding.
  const teamAgent: CatalogAgent | undefined =
    browsedProfile.name === selection.profile
      ? boundAgent ?? browsedProfile.agents[0]
      : browsedProfile.agents[0];
  const isBoundTeam = teamAgent != null && teamAgent === boundAgent;
  const team: TeamState = isBoundTeam && teamAgent
    ? boundTeamState(teamAgent, selection, activeAlias)
    : EMPTY_TEAM;
  const teamViews = teamAgent ? teamView(teamAgent, team) : [];

  // The chip face always mirrors the BOUND team (not the browsed one).
  const boundTeam = boundAgent ? boundTeamState(boundAgent, selection, activeAlias) : EMPTY_TEAM;
  const badgeN = overrideCount(boundTeam);
  const chipModel = activeAlias || boundAgent?.model || "";
  const chipModelState =
    activeAlias === "" || (boundAgent != null && activeAlias === boundAgent.model)
      ? "default"
      : "overridden";

  const toggle = () => {
    if (locked) return;
    if (!open) {
      setBrowsed(selection.profile);
      setNav({ pane: "profile" });
    }
    setOpen(!open);
  };

  // Profile click: browse only — nothing is selected, nothing is sent.
  const browse = (name: string) => {
    setBrowsed(name);
    setNav({ pane: "agent" });
  };

  // Agent click: the binding pick (the 3-key frame — a new agent starts
  // clean), then drill into its Team. The popover stays open.
  const pickAgent = (agent: string) => {
    if (!(browsedProfile.name === selection.profile && agent === selection.agent)) {
      send(select(browsedProfile.name, agent));
      onToast?.(`Next conversation: ${browsedProfile.name} › ${agent}`);
    }
    setNav({ pane: "team" });
  };

  const pickMember = (member: Member) => setNav({ pane: "model", member });

  // Model pick / reset: compose the FULL desired setup from the team state;
  // a no-change pick on the bound team sends nothing (and toasts nothing).
  const applyTeam = (next: TeamState, toastMsg: string) => {
    if (!(sameTeam(next, team) && isBoundTeam)) {
      send(selectFrameFor(browsedProfile.name, teamAgent!, next));
      onToast?.(toastMsg);
    }
    setNav({ pane: "team" });
  };

  const pickModel = (alias: string) => {
    if (!teamAgent || nav.pane !== "model") return;
    const next = withModel(teamAgent, team, nav.member, alias);
    applyTeam(next, `Next conversation: ${memberLabel(nav.member, teamAgent)} on ${alias}`);
  };

  const resetMember = () => {
    if (!teamAgent || nav.pane !== "model") return;
    const member = nav.member;
    const view = teamViews.find((v) => sameMember(v.member, member));
    const next = withModel(teamAgent, team, member, null);
    applyTeam(
      next,
      `Next conversation: ${view?.label ?? memberLabel(member, teamAgent)} on ${view?.defaultAlias ?? ""}`,
    );
  };

  const resetAll = () => {
    if (!teamAgent) return;
    send(select(browsedProfile.name, teamAgent.name));
    onToast?.(`Overrides reset for ${teamAgent.name}`);
  };

  const back = () => {
    setNav((n) =>
      n.pane === "model" ? { pane: "team" } : n.pane === "team" ? { pane: "agent" } : { pane: "profile" },
    );
  };

  const memberView =
    nav.pane === "model" ? teamViews.find((v) => sameMember(v.member, nav.member)) ?? null : null;
  const mobileTitle =
    nav.pane === "model" && memberView ? `Model · ${memberView.label}` : nav.pane.charAt(0).toUpperCase() + nav.pane.slice(1);

  return (
    <div className="relative" ref={rootRef}>
      <button
        data-testid="selection-chip"
        type="button"
        disabled={locked}
        title={locked ? "Locked after the first message" : "Switch profile / agent / team model"}
        onClick={toggle}
        className="flex items-center gap-1 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition-colors hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-zinc-900"
      >
        {locked && <span aria-hidden>🔒</span>}
        {selectedProfile?.origin === "repo" && <span aria-hidden>📁</span>}
        <span
          className="max-w-[9rem] truncate xl:max-w-[16rem]"
          title={`${selection.profile} › ${selection.agent} › ${chipModel}`}
        >
          {selection.profile} <span className="text-zinc-600">›</span> {selection.agent}{" "}
          <span className="text-zinc-600">›</span>{" "}
          <span
            data-testid="selection-chip-model"
            data-state={chipModelState}
            className={chipModelState === "overridden" ? "font-medium text-zinc-100" : "text-zinc-600"}
          >
            {chipModel}
          </span>
          {badgeN > 0 && (
            <span
              data-testid="selection-chip-overrides"
              title={badgeN === 1 ? "1 subagent model changed" : `${badgeN} subagent models changed`}
              className="rounded bg-emerald-500/15 px-1 text-[10px] font-medium text-emerald-400"
            >
              +{badgeN}
            </span>
          )}
        </span>
        {!locked && <span className="text-zinc-600">▾</span>}
      </button>

      {open && !locked && (
        <div
          data-testid="selection-popover"
          data-pane={nav.pane}
          className={`absolute left-0 z-40 flex overflow-hidden rounded-md border border-zinc-800 bg-zinc-900 shadow-xl max-w-[calc(100vw-2rem)] max-[800px]:w-[calc(100vw-1.5rem)] max-[800px]:max-h-[70vh] max-[800px]:flex-col ${
            dropUp ? "bottom-full mb-1" : "top-full mt-1"
          }`}
        >
          {/* Mobile header: one pane at a time, ‹ Back steps up the drill. */}
          <div className="hidden max-[800px]:flex shrink-0 items-center gap-1.5 border-b border-zinc-800 py-1 pl-1 pr-2">
            {nav.pane !== "profile" ? (
              <button
                data-testid="selection-back"
                type="button"
                onClick={back}
                className="flex shrink-0 items-center gap-1 rounded-md px-2 py-1.5 text-xs text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
              >
                ‹ Back
              </button>
            ) : (
              <span className="shrink-0 px-1" />
            )}
            <span className="text-[10px] uppercase tracking-wide text-zinc-500">{mobileTitle}</span>
          </div>

          <div className={`w-44 shrink-0 border-r border-zinc-800 py-1.5 ${nav.pane !== "profile" ? HIDDEN_NARROW : ""}`}>
            <div data-testid="selection-title-profile" className={TITLE_CLS}>
              Profile
            </div>
            <ProfilePane
              profiles={profileList}
              selectedProfile={selection.profile}
              browsed={browsedProfile.name}
              pinned={pinned}
              onBrowse={browse}
            />
          </div>

          <div className={`w-60 shrink-0 border-r border-zinc-800 py-1.5 ${nav.pane !== "agent" ? HIDDEN_NARROW : ""}`}>
            <div data-testid="selection-title-agent" className={TITLE_CLS}>
              Agent
            </div>
            <AgentPane profile={browsedProfile} selection={selection} onPick={pickAgent} />
          </div>

          <div className={`max-h-[calc(100vh-7rem)] w-64 shrink-0 overflow-y-auto py-1.5 ${nav.pane !== "team" ? HIDDEN_NARROW : ""}`}>
            <div data-testid="selection-title-team" className={TITLE_CLS}>
              Team
            </div>
            <TeamPane
              members={teamViews}
              openMember={nav.pane === "model" ? nav.member : null}
              showResetAll={anyOverride(team)}
              onPickMember={pickMember}
              onResetAll={resetAll}
            />
          </div>

          {nav.pane === "model" && memberView && (
            // Overlays the Profile column at 801–1099 px (it's chosen by then),
            // a fourth column at ≥1100, static full width at ≤800.
            <div className="absolute inset-y-0 left-0 z-10 w-56 overflow-y-auto border-r border-zinc-800 bg-zinc-900 py-1.5 min-[1100px]:static min-[1100px]:z-auto min-[1100px]:w-60 min-[1100px]:border-l min-[1100px]:border-r-0 max-[800px]:static max-[800px]:inset-auto max-[800px]:z-auto max-[800px]:w-full max-[800px]:border-r-0">
              <div data-testid="selection-title-model" className={TITLE_CLS}>
                Model · {memberView.label}
              </div>
              <ModelPane
                member={memberView}
                models={models}
                onPick={pickModel}
                onReset={resetMember}
              />
            </div>
          )}
        </div>
      )}
    </div>
  );
}
