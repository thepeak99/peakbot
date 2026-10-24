// Presentational panes of the selection popover (selection-models plan §5.3).
// Pure props: no state, no sending. SelectionChip owns the drill state and
// builds the `select` frames; these only render rows and forward clicks.

import type { ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";
import type { Member, MemberView } from "../teamModels";

const ROW_CLS =
  "flex w-full cursor-pointer flex-col items-start rounded px-2 py-1 text-left text-xs hover:bg-zinc-800 max-[800px]:py-2.5";
const RESET_CLS =
  "mt-1 w-full cursor-pointer rounded px-2 py-1.5 text-left text-[10px] text-amber-400/90 hover:bg-zinc-800";

function agentCaption(a: { subagents: unknown[] }, index: number): string {
  const base = a.subagents.length > 0 ? `${a.subagents.length} subagents` : "single";
  return index === 0 ? `${base} · default` : base;
}

/** Profile column rows. Click = browse only (the chip advances the drill). */
export function ProfilePane({
  profiles,
  selectedProfile,
  browsed,
  pinned,
  onBrowse,
}: {
  profiles: CatalogProfile[];
  selectedProfile: string;
  browsed: string;
  pinned: boolean;
  onBrowse(name: string): void;
}) {
  return (
    <>
      {profiles.map((p, i) => (
        <button
          key={p.name}
          data-testid={`selection-profile-${p.name}`}
          data-browsed={p.name === browsed ? "true" : undefined}
          type="button"
          onClick={() => onBrowse(p.name)}
          className={`${ROW_CLS} ${p.name === browsed ? "bg-zinc-800/60" : ""}`}
        >
          <span className={p.name === selectedProfile ? "font-medium text-emerald-400" : "text-zinc-200"}>
            {p.origin === "repo" && <span aria-hidden>📁 </span>}
            {p.name}
          </span>
          <span className="text-[10px] text-zinc-500">
            {pinned ? "pinned" : i === 0 ? "default" : ""}
          </span>
        </button>
      ))}
    </>
  );
}

/** Agent column rows of the browsed profile. Click = the binding pick. */
export function AgentPane({
  profile,
  selection,
  onPick,
}: {
  profile: CatalogProfile;
  selection: Selection;
  onPick(agentName: string): void;
}) {
  return (
    <>
      {profile.agents.map((a, i) => (
        <button
          key={a.name}
          data-testid={`selection-agent-${a.name}`}
          type="button"
          onClick={() => onPick(a.name)}
          className={ROW_CLS}
        >
          <span
            className={
              profile.name === selection.profile && a.name === selection.agent
                ? "font-medium text-emerald-400"
                : "text-zinc-200"
            }
          >
            {a.name}
          </span>
          <span className="text-[10px] text-zinc-500">{agentCaption(a, i)}</span>
        </button>
      ))}
    </>
  );
}

/** Team column rows: the orchestrator (or lone `main`) then the subagents,
 * each with the model it will run on. Click = open the Model pane. */
export function TeamPane({
  members,
  openMember,
  showResetAll,
  onPickMember,
  onResetAll,
}: {
  members: MemberView[];
  openMember: Member | null;
  showResetAll: boolean;
  onPickMember(member: Member): void;
  onResetAll(): void;
}) {
  const sameMember = (a: Member, b: Member | null): boolean => {
    if (!b || a.kind !== b.kind) return false;
    return a.kind === "subagent" && b.kind === "subagent" ? a.name === b.name : true;
  };
  return (
    <>
      {members.map((m) => (
        <button
          key={m.label}
          data-testid={
            m.member.kind === "orchestrator"
              ? "selection-member-orchestrator"
              : `selection-member-sub-${m.member.name}`
          }
          type="button"
          onClick={() => onPickMember(m.member)}
          className="flex w-full cursor-pointer items-center justify-between gap-2 rounded px-2 py-1.5 text-left text-xs hover:bg-zinc-800 max-[800px]:py-2.5"
        >
          <span className="min-w-0">
            <span
              className={`block truncate ${sameMember(m.member, openMember) ? "font-medium text-emerald-400" : "text-zinc-200"}`}
            >
              {m.label}
            </span>
            <span className="block text-[10px] text-zinc-500">{m.caption}</span>
          </span>
          <span
            data-testid="selection-member-model"
            data-state={m.inherits ? "inherited" : m.overridden ? "overridden" : "default"}
            className={`shrink-0 font-mono text-[11px] ${
              m.inherits ? "italic text-zinc-500" : m.overridden ? "font-medium text-zinc-100" : "text-zinc-500"
            }`}
          >
            {m.inherits ? `inherits ${m.effective}` : m.effective}
          </span>
        </button>
      ))}
      {showResetAll && (
        <button data-testid="selection-team-reset" type="button" onClick={onResetAll} className={RESET_CLS}>
          Reset all to defaults
        </button>
      )}
    </>
  );
}

/** Model column rows for one team member, in `models` order. Click = pick;
 * the reset button clears the member's override. */
export function ModelPane({
  member,
  models,
  onPick,
  onReset,
}: {
  member: MemberView;
  models: ModelInfo[];
  onPick(alias: string): void;
  onReset(): void;
}) {
  return (
    <>
      {models.map((md) => {
        const current = md.alias === member.effective;
        const isDefault = md.alias === member.defaultAlias;
        return (
          <button
            key={md.alias}
            data-testid={`selection-model-${md.alias}`}
            data-current={current ? "true" : undefined}
            type="button"
            onClick={() => onPick(md.alias)}
            className="flex w-full cursor-pointer flex-col items-start rounded px-2 py-1.5 text-left text-xs hover:bg-zinc-800 max-[800px]:py-2"
          >
            <span className={`flex items-center gap-1.5 ${current ? "font-medium text-emerald-400" : "text-zinc-200"}`}>
              <span>{current ? "→ " : ""}{md.alias}</span>
              {isDefault && (
                <span data-testid="selection-model-default-tag" className="rounded bg-zinc-800 px-1 text-[10px] font-normal text-zinc-400">
                  default
                </span>
              )}
            </span>
            <span className="text-[10px] text-zinc-500">
              {md.provider_name} · {md.model_name}
            </span>
          </button>
        );
      })}
      {member.overridden && (
        <button data-testid="selection-model-reset" type="button" onClick={onReset} className={RESET_CLS}>
          Reset to default
        </button>
      )}
    </>
  );
}
