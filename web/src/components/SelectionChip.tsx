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
//
// `sheet` (the phone header's subtitle): the trigger is the bordered subtitle
// pill and the sheet shows three pressable groups (Profile/Agent/Model) at
// once; the proven sub-agent drill stays behind "Team models ›". The Sheet
// owns Esc/Back/backdrop, so the chip's own dismissal is off; Agent and
// Model picks bind, toast and close (mock behaviour, Q4).

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { boundAgentOf } from "../adapt";
import { chipAction, select } from "../state";
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
import { PILL_OFF, PILL_ON } from "./phoneUi";
import { AgentPane, ModelPane, ProfilePane, TeamPane } from "./SelectionPanes";
import { Sheet } from "./Sheet";

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

// The sheet's segmented groups (C16): one pressable pill per option, all
// three groups at once. Module-level only to keep the sheet JSX readable.
function SegGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="px-0.5 pb-2 text-[11px] font-semibold uppercase tracking-[0.08em] text-zinc-500">{label}</h3>
      <div className="flex flex-wrap gap-2">{children}</div>
    </section>
  );
}

function SegPill({
  testid,
  on,
  onClick,
  children,
}: {
  testid: string;
  on: boolean;
  onClick(): void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      data-testid={testid}
      aria-pressed={on}
      onClick={onClick}
      className={`flex h-11 flex-none cursor-pointer items-center rounded-[22px] border px-[18px] text-[14.5px] ${
        on ? PILL_ON : PILL_OFF
      }`}
    >
      {children}
    </button>
  );
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
  onReattach,
  sheet = false,
}: SelectionChipProps) {
  const [open, setOpen] = useState(false);
  const [nav, setNav] = useState<Nav>({ pane: "profile" });
  // The profile being browsed; reset to the bound profile on every open so a
  // half-explored browse never leaks into the next one.
  const [browsed, setBrowsed] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // Stable: the Sheet's back layer re-pushes on a new identity.
  const close = useCallback(() => setOpen(false), []);

  // Locking while the popover is open closes it (read-only state).
  useEffect(() => {
    if (locked && open) setOpen(false);
  }, [locked, open]);

  // Esc (step back from Model, else close; always preventDefault) and
  // mousedown-outside, only while open. Deps carry `nav` so the Esc handler
  // sees the current pane. Off in sheet mode: the portal would make every
  // in-sheet click "outside" rootRef, and the Sheet owns dismissal.
  useEffect(() => {
    if (!open || sheet) return;
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
  }, [open, nav, sheet]);

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
  // A remote profile whose agents are unknown offers one bare pill (AgentPane
  // rule), the remote resolves its own agent on attach.
  const bareRemote = browsedProfile.origin === "remote" && browsedProfile.agents.length === 0;

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
  // The separators and the ▾ are muted on the desktop chip; the sheet's
  // subtitle pill keeps them in the base colour (the model separator is `·`).
  const sep = sheet ? "" : "text-zinc-600";

  const toggle = () => {
    if (locked) return;
    if (!open) {
      setBrowsed(selection.profile);
      setNav({ pane: "profile" });
    }
    setOpen(!open);
  };

  // Profile click: browse — nothing is selected, nothing is sent. Exception:
  // a zero-agent remote profile is CONNECTED (onReattach without an agent)
  // so the relay can fill the remote's real agents; the bare row stays the
  // fallback until it does.
  const browse = (name: string) => {
    const entry = catalog.find((p) => p.name === name);
    if (entry && entry.origin === "remote" && entry.agents.length === 0 && onReattach && !locked) {
      onReattach(name);
    }
    setBrowsed(name);
    setNav({ pane: "agent" });
  };

  // Agent click: the binding pick (the 3-key frame — a new agent starts
  // clean), then drill into its Team. The popover stays open.
  const pickAgent = (agent: string) => {
    if (!(browsedProfile.name === selection.profile && agent === selection.agent)) {
      const target = { profile: browsedProfile.name, agent, subagentModels: {} };
      // Crossing local/remote needs a fresh socket: a `select` for a profile
      // on the other server would be refused (design §6.3 #10).
      if (onReattach && chipAction(selection, target, catalog) === "reattach") {
        // The bare remote row (agent "") rides the attach WITHOUT an agent —
        // the remote resolves its own (design §6.4); no `select` is sent.
        onReattach(target.profile, agent || undefined);
      } else {
        send(select(target.profile, agent));
      }
      onToast?.(
        agent
          ? `Next conversation: ${browsedProfile.name} › ${agent}`
          : `Next conversation: ${browsedProfile.name}`,
      );
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
    if (sheet) setOpen(false);
    else setNav({ pane: "team" });
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

  const panes = (
    <>
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

      <div className={`w-44 shrink-0 border-r border-zinc-800 py-1.5 max-[800px]:w-full ${nav.pane !== "profile" ? HIDDEN_NARROW : ""}`}>
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

      <div className={`w-60 shrink-0 border-r border-zinc-800 py-1.5 max-[800px]:w-full ${nav.pane !== "agent" ? HIDDEN_NARROW : ""}`}>
        <div data-testid="selection-title-agent" className={TITLE_CLS}>
          Agent
        </div>
        <AgentPane profile={browsedProfile} selection={selection} onPick={pickAgent} />
      </div>

      <div className={`max-h-[calc(100vh-7rem)] w-64 shrink-0 overflow-y-auto py-1.5 max-[800px]:w-full ${nav.pane !== "team" ? HIDDEN_NARROW : ""}`}>
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
    </>
  );

  // The sheet face (C16): Profile browses (stays open); Agent and Model bind,
  // toast and close. Sub-agent overrides keep the drill behind "Team models ›".
  const segments = (
    <div data-testid="selection-segments" className="flex flex-col gap-4 px-4 pb-2 pt-0.5">
      <SegGroup label="Profile">
        {profileList.map((p) => (
          <SegPill
            key={p.name}
            testid={`selection-profile-${p.name}`}
            on={p.name === browsedProfile.name}
            onClick={() => browse(p.name)}
          >
            {p.origin === "repo" && "📁 "}
            {p.name}
          </SegPill>
        ))}
      </SegGroup>
      <SegGroup label="Agent">
        {bareRemote ? (
          <SegPill
            testid="selection-agent-remote"
            on={browsedProfile.name === selection.profile && selection.agent === ""}
            onClick={() => {
              pickAgent("");
              close();
            }}
          >
            {browsedProfile.name}
          </SegPill>
        ) : (
          browsedProfile.agents.map((a) => (
            <SegPill
              key={a.name}
              testid={`selection-agent-${a.name}`}
              on={browsedProfile.name === selection.profile && a.name === selection.agent}
              onClick={() => {
                pickAgent(a.name);
                close();
              }}
            >
              {a.name}
            </SegPill>
          ))
        )}
      </SegGroup>
      {teamAgent && models.length > 0 && (
        <SegGroup label="Model">
          {models.map((m) => (
            <SegPill
              key={m.alias}
              testid={`selection-model-${m.alias}`}
              on={m.alias === teamViews[0].effective}
              onClick={() =>
                applyTeam(
                  withModel(teamAgent, team, { kind: "orchestrator" }, m.alias),
                  `Next conversation: ${teamAgent.name} on ${m.alias}`,
                )
              }
            >
              {m.alias}
            </SegPill>
          ))}
        </SegGroup>
      )}
      {teamAgent && teamAgent.subagents.length > 0 && (
        <button
          data-testid="selection-team-models"
          type="button"
          onClick={() => setNav({ pane: "team" })}
          className="flex h-11 items-center self-start px-0.5 text-[15px] font-semibold text-sky-400"
        >
          Team models ›
        </button>
      )}
    </div>
  );

  return (
    <div className={sheet ? "relative min-w-0" : "relative"} ref={rootRef}>
      <button
        data-testid="selection-chip"
        type="button"
        disabled={locked}
        title={locked ? "Locked after the first message" : "Switch profile / agent / team model"}
        onClick={toggle}
        className={
          sheet
            ? "flex h-[26px] max-w-full items-center gap-1.5 rounded-full border border-zinc-800 bg-zinc-900 px-3 text-xs text-zinc-400 transition-colors hover:text-zinc-200 disabled:cursor-default disabled:hover:text-zinc-400"
            : "flex items-center gap-1 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition-colors hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60 disabled:hover:bg-zinc-900"
        }
      >
        {locked && (
          <span aria-hidden className={sheet ? "text-[11px] leading-none" : undefined}>
            🔒
          </span>
        )}
        {selectedProfile?.origin === "repo" && <span aria-hidden>📁</span>}
        <span
          className={sheet ? "min-w-0 truncate" : "max-w-[9rem] truncate xl:max-w-[16rem]"}
          title={`${selection.profile} › ${selection.agent} › ${chipModel}`}
        >
          {selectedProfile?.color && (
            <span
              data-testid="selection-chip-color"
              aria-hidden
              className="inline-block h-2 w-2 shrink-0 rounded-full align-middle"
              style={{ backgroundColor: `var(--profile-${selectedProfile.color})` }}
            />
          )}
          {selection.profile} <span className={sep}>›</span> {selection.agent}{" "}
          <span className={sep}>{sheet ? "·" : "›"}</span>{" "}
          <span
            data-testid="selection-chip-model"
            data-state={chipModelState}
            className={chipModelState === "overridden" ? "font-medium text-zinc-100" : sep}
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
        {!locked && <span className={sep}>▾</span>}
      </button>

      {open && !locked && sheet && (
        <Sheet title="Selection" onClose={close}>
          {nav.pane === "profile" || nav.pane === "agent" ? (
            segments
          ) : (
            <div
              data-testid="selection-popover"
              data-pane={nav.pane}
              className="relative flex max-[800px]:flex-col"
            >
              {panes}
            </div>
          )}
        </Sheet>
      )}

      {open && !locked && !sheet && (
        <div
          data-testid="selection-popover"
          data-pane={nav.pane}
          className={`absolute left-0 z-40 flex overflow-hidden rounded-md border border-zinc-800 bg-zinc-900 shadow-xl max-w-[calc(100vw-2rem)] max-[800px]:w-[calc(100vw-1.5rem)] max-[800px]:max-h-[70vh] max-[800px]:flex-col ${
            dropUp ? "bottom-full mb-1" : "top-full mt-1"
          }`}
        >
          {panes}
        </div>
      )}
    </div>
  );
}
