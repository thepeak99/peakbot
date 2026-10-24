// D4 "Profiles list + profile page" (spec §9.2/§9.3, contract.ts
// "./profiles (T27)"). Both panes read the current scope's draft via
// useSettingsCtx() and write through `edit(path, value)` by segment array —
// profiles are editable in BOTH scopes (repo scope edits the repo draft).
// Ordering/naming logic lives in ./profileOps (pure, unit-tested).
//
// Master-detail: the shell navigates list → profile page → agent page (the
// agent page is ./agent, T28) and renders the breadcrumbs; the panes only
// report intent through their optional `onOpen*` callbacks.

import { useEffect, useRef, useState } from "react";
import { useSettingsCtx } from "./context";
import { get, type Doc } from "./yamlDoc";
import { parseFilter, serializeFilter } from "./filter";
import { duplicateInsertIndex, duplicateName, makeDefault } from "./profileOps";
import { useToast } from "./Toast";
import { Badge } from "./widgets";
import type { Seg } from "./types";

// ── shared derivations ───────────────────────────────────────────────────

function asRecord(v: unknown): Record<string, unknown> {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return {};
  return v as Record<string, unknown>;
}

function agentsOf(profileDef: unknown): Record<string, unknown> {
  return asRecord(asRecord(profileDef).agents);
}

function subAgentsOf(agentDef: unknown): Record<string, unknown> {
  return asRecord(asRecord(agentDef).subagents);
}

/** The first declared model alias in the draft (for new agents/subagents). */
function firstAliasOf(doc: Doc): string | undefined {
  const providers = get(doc, ["providers"]);
  if (!Array.isArray(providers)) return undefined;
  for (const p of providers) {
    const models = asRecord(p).models;
    if (!Array.isArray(models)) continue;
    for (const m of models) {
      const alias = asRecord(m).alias;
      if (typeof alias === "string" && alias !== "") return alias;
    }
  }
  return undefined;
}

/** `1 subagent · tester(↳)` / `3 subagents · a(qwen), b(opus)` / `single`. */
function subAgentSummary(agentDef: unknown): string {
  const subs = Object.entries(subAgentsOf(agentDef));
  if (subs.length === 0) return "single";
  const bits = subs.map(([name, def]) => {
    const model = asRecord(def).model;
    return typeof model === "string" && model !== "" ? `${name}(${model})` : `${name}(↳)`;
  });
  return `${subs.length} subagent${subs.length === 1 ? "" : "s"} · ${bits.join(", ")}`;
}

/** The mockup's `profileSummary` line (spec §9.2), from the profile def. */
function profileSummary(def: unknown): string[] {
  const p = asRecord(def);
  const bits: string[] = [];
  const tools = parseFilter(p.tools);
  bits.push(
    tools.mode === "all"
      ? "all tools"
      : tools.mode === "disabled"
        ? `tools −${tools.names.length}`
        : `tools ${tools.names.length} only`,
  );
  const mcps = parseFilter(p.mcps);
  bits.push(
    mcps.mode === "all"
      ? "all mcps"
      : mcps.mode === "only"
        ? mcps.names.length > 0
          ? `mcps: ${mcps.names.join(", ")}`
          : "no mcps"
        : `mcps −${mcps.names.length}`,
  );
  const envCount = Object.keys(asRecord(p.env)).length;
  if (envCount > 0) bits.push(`env ×${envCount}`);
  if (p.vector_db !== undefined && p.vector_db !== null) bits.push("📚 knowledge");
  return bits;
}

function mapWithout(map: Record<string, unknown>, name: string): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(map)) if (key !== name) out[key] = map[key];
  return out;
}

function insertAfter(
  map: Record<string, unknown>,
  names: string[],
  index: number,
  newName: string,
  def: unknown,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  names.forEach((key, i) => {
    out[key] = map[key];
    if (i + 1 === index) out[newName] = def;
  });
  if (index > names.length) out[newName] = def;
  return out;
}

const GHOST_BTN =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800";

// ── Profiles list ────────────────────────────────────────────────────────

export interface ProfilesPaneProps {
  /** Open the profile page (the shell's master-detail nav). */
  onOpenProfile?(name: string): void;
  /** Profile names of the OTHER scope's draft: the shadow/override badges. */
  otherScopeNames?: string[];
}

export function ProfilesPane({ onOpenProfile, otherScopeNames }: ProfilesPaneProps = {}) {
  const { scope, meta, doc, edit } = useSettingsCtx();
  const { toast } = useToast();

  const profiles = asRecord(get(doc, ["profiles"]));
  const names = Object.keys(profiles);
  // Repo profiles exist for this cwd → the global "default" is not the
  // conversation default here (spec §9.2 GLOBAL DEFAULT rule).
  const repoPresent = scope === "global" && meta?.repo_exists === true;
  const dftLabel = repoPresent ? "GLOBAL DEFAULT" : "DEFAULT";

  function makeDefaultClick(name: string) {
    try {
      edit(["profiles"], makeDefault(profiles, name));
    } catch (e) {
      toast(e instanceof Error ? e.message : "No such profile", "error");
      return;
    }
    toast(`${name} is now the ${repoPresent ? "global " : ""}default profile`);
  }

  function addProfile() {
    let name = "new-profile";
    let n = 2;
    while (names.includes(name)) name = `new-profile-${n++}`;
    const first = firstAliasOf(doc);
    edit(["profiles"], { ...profiles, [name]: { agents: { main: { model: first ?? "" } } } });
  }

  return (
    <div data-testid="profiles-pane" className="space-y-3">
      <div className="rounded-lg border border-zinc-800 bg-zinc-900/40 px-3 py-2">
        <div className="flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">
            {scope === "global" ? "Global profiles" : "Repo profiles"}
          </h3>
          <span className="text-[10px] text-zinc-500">ordered — the first is the default</span>
        </div>
        <p className="mt-1 text-xs text-zinc-500">
          A profile = capabilities (tools, MCP access, env, memory, knowledge, UI) + its ordered
          agents. Capabilities apply identically to the agent and all its subagents.
        </p>
      </div>

      {names.map((name, i) => {
        const def = profiles[name];
        const agentEntries = Object.entries(agentsOf(def));
        const shadowed = otherScopeNames?.includes(name) ?? false;
        return (
          <div
            key={name}
            data-testid={`profile-card-${i}`}
            onClick={() => onOpenProfile?.(name)}
            className="cursor-pointer rounded-lg border border-zinc-800 bg-zinc-900/40 p-3 hover:border-zinc-600"
          >
            <div className="flex flex-wrap items-center gap-2">
              <span data-testid={`profile-name-${i}`} className="text-sm font-semibold">
                {name}
              </span>
              {i === 0 && (
                <Badge testid="profile-badge-default" tone="green">
                  {dftLabel}
                </Badge>
              )}
              {shadowed && (
                <Badge tone={scope === "global" ? "amber" : "green"}>
                  {scope === "global" ? "shadowed in this repo" : `overrides global ${name}`}
                </Badge>
              )}
              <div className="ml-auto flex items-center gap-2">
                {i > 0 && (
                  <button
                    data-testid={`profile-make-default-${i}`}
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      makeDefaultClick(name);
                    }}
                    className={GHOST_BTN}
                  >
                    {repoPresent ? "Make global default" : "Make default"}
                  </button>
                )}
                <span className="text-zinc-500">›</span>
              </div>
            </div>
            <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1">
              {agentEntries.map(([aName, aDef], j) => (
                <span key={aName} className="text-xs text-zinc-400">
                  {aName}{" "}
                  <span className="text-zinc-600">
                    · {String(asRecord(aDef).model ?? "")} · {subAgentSummary(aDef)}
                  </span>
                  {j === 0 && <span className="ml-1 text-[10px] text-emerald-500">DEFAULT</span>}
                </span>
              ))}
            </div>
            <div className="mt-2 flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-zinc-500">
              {profileSummary(def).map((b) => (
                <span key={b}>{b}</span>
              ))}
            </div>
          </div>
        );
      })}

      <button data-testid="profile-add" type="button" onClick={addProfile} className={GHOST_BTN}>
        + New profile
      </button>
    </div>
  );
}

// ── Profile page ─────────────────────────────────────────────────────────

export interface ProfilePageProps {
  profile: string;
  /** The profile the current conversation uses (Q8: confirm before hiding `settings` on it). */
  activeProfile?: string | null;
  /** The shell does the cross-scope copy (naming via profileOps.copyToRepoName) + scope switch. */
  onCopyToRepo?(name: string, def: Record<string, unknown>): void;
  /** A copy was created / the name changed — follow it in the nav. */
  onOpenProfile?(name: string): void;
  /** Open an agent page (the Agents card rows). */
  onOpenAgent?(profile: string, agent: string): void;
  /** Profile names of the OTHER scope's draft: the shadow/override badges. */
  otherScopeNames?: string[];
}

export function ProfilePage({
  profile,
  activeProfile,
  onCopyToRepo,
  onOpenProfile,
  onOpenAgent,
  otherScopeNames,
}: ProfilePageProps) {
  const { scope, meta, doc, edit } = useSettingsCtx();
  const { toast } = useToast();

  const profiles = asRecord(get(doc, ["profiles"]));
  const names = Object.keys(profiles);
  const def = profiles[profile];
  const first = names[0] === profile;
  const repoPresent = scope === "global" && meta?.repo_exists === true;
  const dftLabel = repoPresent ? "GLOBAL DEFAULT" : "DEFAULT";

  // ── header actions ─────────────────────────────────────────────────────
  function rename(newName: string) {
    const name = newName.trim();
    if (name === "" || name === profile) return;
    if (names.includes(name)) return; // a JS map would silently eat the other profile
    const out: Record<string, unknown> = {};
    for (const key of names) out[key === profile ? name : key] = profiles[key];
    edit(["profiles"], out);
    onOpenProfile?.(name);
  }

  function duplicate() {
    const name = duplicateName(profile, names);
    const index = duplicateInsertIndex(names, profile);
    edit(["profiles"], insertAfter(profiles, names, index, name, def));
    toast(`Duplicated as ${name}`);
    onOpenProfile?.(name);
  }

  function remove() {
    if (names.length <= 1) {
      toast("A config needs at least one profile", "error");
      return;
    }
    edit(["profiles"], mapWithout(profiles, profile));
    toast(`Deleted ${profile}`);
  }

  // ── Agents card (ordered — first is the default) ───────────────────────
  const agents = agentsOf(def);
  const agentNames = Object.keys(agents);

  function removeAgent(name: string) {
    if (agentNames.length <= 1) {
      toast("A profile needs at least one agent", "error");
      return;
    }
    edit(["profiles", profile, "agents"], mapWithout(agents, name));
  }

  function defaultAgent(name: string) {
    edit(["profiles", profile, "agents"], makeDefault(agents, name));
  }

  function addAgent() {
    let name = "agent";
    let n = 2;
    while (agentNames.includes(name)) name = `agent-${n++}`;
    const firstAlias = firstAliasOf(doc);
    edit(["profiles", profile, "agents"], { ...agents, [name]: { model: firstAlias ?? "" } });
  }

  // ── Interface card (ui.tabs — checked = visible) ───────────────────────
  const tabNames: string[] = meta?.tab_names ?? [];
  const tabsPath: Seg[] = ["profiles", profile, "ui", "tabs"];
  // The unchecked set, in universe order. The shell mutates the draft doc IN
  // PLACE (same reference) on every edit and bumps a version, so the pane
  // composes its OWN `hidden` state between edits and adopts the doc's value
  // only when the doc's IDENTITY changes — discard / raw-YAML reparse /
  // reload (the same semi-controlled pattern as the list widgets in
  // widgets.tsx).
  const hiddenFromDoc = (d: typeof doc) =>
    tabNames.filter((t) => parseFilter(get(d, tabsPath)).names.includes(t));
  const [hidden, setHidden] = useState<string[]>(() => hiddenFromDoc(doc));
  const lastDoc = useRef(doc);
  useEffect(() => {
    if (lastDoc.current !== doc) {
      lastDoc.current = doc;
      setHidden(hiddenFromDoc(doc));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- adopt on doc identity only
  }, [doc]);
  const [confirmOpen, setConfirmOpen] = useState(false);

  function applyToggleTab(tab: string) {
    const next = hidden.includes(tab) ? hidden.filter((x) => x !== tab) : [...hidden, tab];
    setHidden(next);
    // The `disabled` list is in meta.tab_names (universe) order; all visible
    // → undefined (key omitted, not `enabled: true`).
    edit(
      tabsPath,
      serializeFilter({ mode: "disabled", names: tabNames.filter((t) => next.includes(t)) }),
    );
  }

  function toggleTab(tab: string) {
    const willHide = !hidden.includes(tab);
    // Q8: hiding `settings` on the profile in use asks first.
    if (willHide && tab === "settings" && activeProfile === profile) {
      setConfirmOpen(true);
      return;
    }
    applyToggleTab(tab);
  }

  return (
    <div data-testid="profile-page" className="space-y-4">
      {/* Header row: editable name · badges · actions */}
      <div className="flex flex-wrap items-center gap-2">
        <input
          data-testid="profile-name"
          value={profile}
          onChange={(e) => rename(e.target.value)}
          aria-label="Profile name"
          className="w-44 rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-sm font-semibold text-zinc-100 focus:border-zinc-500 focus:outline-none"
        />
        {first && (
          <Badge testid="profile-badge-default" tone="green">
            {dftLabel}
          </Badge>
        )}
        {otherScopeNames?.includes(profile) && (
          <Badge tone={scope === "global" ? "amber" : "green"}>
            {scope === "global" ? "shadowed in this repo" : `overrides global ${profile}`}
          </Badge>
        )}
        <div className="ml-auto flex items-center gap-2">
          <button data-testid="profile-duplicate" type="button" onClick={duplicate} className={GHOST_BTN}>
            Duplicate
          </button>
          {scope === "global" && (
            <button
              data-testid="profile-copy-repo"
              type="button"
              onClick={() => onCopyToRepo?.(profile, asRecord(def))}
              className={GHOST_BTN}
            >
              Copy to this repo
            </button>
          )}
          <button
            data-testid="profile-delete"
            type="button"
            onClick={remove}
            className="cursor-pointer rounded border border-red-900/70 px-2 py-0.5 text-[11px] text-red-400 hover:bg-red-950/40"
          >
            Delete
          </button>
        </div>
      </div>

      <div className="rounded-lg border border-sky-900/60 bg-sky-950/30 px-3 py-2 text-xs text-sky-300">
        Capabilities below (Tools, MCP access, Environment, Memory, Knowledge, Interface) apply
        identically to the agent and all its subagents.
      </div>

      {/* Agents card */}
      <div data-testid="profile-agents" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Agents</h3>
          <span className="text-[10px] text-zinc-500">ordered — first is the default for this profile</span>
        </div>
        <div className="space-y-1.5">
          {agentNames.map((aName, i) => {
            const aDef = agents[aName];
            return (
              <div
                key={aName}
                data-testid={`profile-agent-row-${i}`}
                className="flex flex-wrap items-center gap-2 rounded border border-zinc-800/60 px-2 py-1.5"
              >
                <button
                  data-testid={`profile-agent-open-${i}`}
                  type="button"
                  onClick={() => onOpenAgent?.(profile, aName)}
                  className="cursor-pointer text-xs font-medium text-zinc-200 hover:text-sky-400"
                >
                  {aName}
                </button>
                {i === 0 && <Badge tone="green">DEFAULT</Badge>}
                <span className="rounded border border-zinc-700 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
                  {String(asRecord(aDef).model ?? "")}
                </span>
                <span className="text-[11px] text-zinc-500">{subAgentSummary(aDef)}</span>
                <div className="ml-auto flex items-center gap-1.5">
                  {i > 0 && (
                    <button
                      data-testid={`profile-agent-default-${i}`}
                      type="button"
                      onClick={() => defaultAgent(aName)}
                      className={GHOST_BTN}
                    >
                      Make default
                    </button>
                  )}
                  <button
                    data-testid={`profile-agent-remove-${i}`}
                    type="button"
                    title="Delete agent"
                    onClick={() => removeAgent(aName)}
                    className="cursor-pointer rounded px-1 text-xs text-zinc-500 hover:text-red-400"
                  >
                    ✕
                  </button>
                  <span className="text-zinc-500">›</span>
                </div>
              </div>
            );
          })}
        </div>
        <button
          data-testid="profile-agent-add"
          type="button"
          onClick={addAgent}
          className={`mt-2 ${GHOST_BTN}`}
        >
          + Add agent
        </button>
      </div>

      {/* Interface card (ui.tabs) */}
      <div data-testid="profile-tabs" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Interface</h3>
          <span className="text-[10px] text-zinc-500">ui.tabs — checked = visible</span>
        </div>
        <div className="flex flex-wrap gap-x-4 gap-y-1.5">
          {tabNames.map((t) => (
            <label
              key={t}
              className="inline-flex cursor-pointer items-center gap-1.5 text-xs text-zinc-300"
            >
              <input
                data-testid={`profile-tabs-chip-${t}`}
                type="checkbox"
                checked={!hidden.includes(t)}
                onChange={() => toggleTab(t)}
                className="h-3.5 w-3.5 cursor-pointer accent-emerald-600"
              />
              {t}
            </label>
          ))}
        </div>

        {confirmOpen && (
          <div
            data-testid="profile-tabs-confirm"
            className="mt-3 rounded-lg border border-amber-800/70 bg-amber-950/40 px-3 py-2 text-xs text-amber-200"
          >
            <p>
              Hide Settings for '{profile}'? This removes the ⚙ gear and Ctrl+, for the profile you
              are using. To get Settings back you will have to edit config.yaml by hand or switch
              profile.
            </p>
            <div className="mt-2 flex gap-2">
              <button
                data-testid="profile-tabs-confirm-hide"
                type="button"
                onClick={() => {
                  setConfirmOpen(false);
                  applyToggleTab("settings");
                }}
                className="cursor-pointer rounded border border-amber-700 px-2 py-0.5 text-[11px] text-amber-200 hover:bg-amber-900/40"
              >
                Hide Settings
              </button>
              <button
                data-testid="profile-tabs-confirm-cancel"
                type="button"
                onClick={() => setConfirmOpen(false)}
                className={GHOST_BTN}
              >
                Cancel
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
