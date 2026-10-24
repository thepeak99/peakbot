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
import { parseFilter, serializeFilter, type FilterState } from "./filter";
import { duplicateInsertIndex, duplicateName, makeDefault } from "./profileOps";
import { useToast } from "./Toast";
import { Badge, Field, FilterBlock, KvRows, SecretInput, Toggle, type KvRow } from "./widgets";
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

/** The declared `mcps` names in the draft (the MCP access card universe). */
function mcpNamesOfDoc(doc: Doc): string[] {
  const raw = get(doc, ["mcps"]);
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const e of raw) {
    const n = asRecord(e).name;
    if (typeof n === "string" && n !== "") out.push(n);
  }
  return out;
}

/** A map value (env) → editable key/value rows. */
function toRows(v: unknown): KvRow[] {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return [];
  return Object.entries(v as Record<string, unknown>).map(([k, val]) => ({
    key: k,
    value: typeof val === "string" ? val : String(val),
  }));
}

/** Rows → a string map to write (blank keys dropped; no rows left = `undefined`). */
function toMap(rows: KvRow[]): Record<string, string> | undefined {
  const kept = rows.filter((r) => r.key.trim() !== "");
  if (kept.length === 0) return undefined;
  const out: Record<string, string> = {};
  for (const r of kept) out[r.key] = r.value;
  return out;
}

// ── filter-state seam for the Tools / MCP access cards ───────────────────
// The shell mutates the draft IN PLACE on every edit (same Doc identity), so
// the FilterBlock value is the pane's own state: it is (re-)adopted from the
// doc only when the doc identity changes (discard / raw reparse / scope
// switch) or the profile changes (rename / nav). Between edits the doc alone
// cannot say which tri-state the user chose — e.g. {disabled, []} and "all"
// both omit the key — so the pane keeps the state FilterBlock last emitted
// (`emit` records it, then writes the serialized value to the draft).
function useFilterValue(
  doc: Doc,
  profile: string,
  path: Seg[],
  onEmit: (s: FilterState) => void,
): { value: FilterState; emit: (s: FilterState) => void } {
  const parsed = parseFilter(get(doc, path));
  const ref = useRef<{ doc: Doc; profile: string; value: FilterState } | null>(null);
  if (
    ref.current === null ||
    ref.current.doc !== doc ||
    ref.current.profile !== profile
  ) {
    ref.current = { doc, profile, value: parsed };
  }
  const value = ref.current.value;
  const emit = (s: FilterState) => {
    ref.current = { doc: ref.current!.doc, profile, value: s };
    onEmit(s);
  };
  return { value, emit };
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
  /** Universe for the MCP access card, supplied by the shell from the GLOBAL
   *  draft (contract T27b): in repo scope the repo draft has no `mcps`, so
   *  the chips must come from the global names. When omitted the pane falls
   *  back to the names of get(doc, ["mcps"]). */
  globalMcpNames?: string[];
}

export function ProfilePage({
  profile,
  activeProfile,
  onCopyToRepo,
  onOpenProfile,
  onOpenAgent,
  otherScopeNames,
  globalMcpNames,
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

  // ── Tools / MCP access cards (NameFilters over the draft) ──────────────
  const toolsPath: Seg[] = ["profiles", profile, "tools"];
  const mcpsPath: Seg[] = ["profiles", profile, "mcps"];
  const tools = useFilterValue(doc, profile, toolsPath, (s) => edit(toolsPath, serializeFilter(s)));
  const mcps = useFilterValue(doc, profile, mcpsPath, (s) => edit(mcpsPath, serializeFilter(s)));
  // Repo scope: the repo draft has no `mcps` — the shell supplies the
  // universe from the global draft (contract T27b); global scope falls back
  // to the draft's own declared names.
  const mcpUniverse = globalMcpNames ?? mcpNamesOfDoc(doc);
  const allowPost = asRecord(get(doc, ["profiles", profile, "fetch_url"])).allow_post === true;

  // ── Environment card (string map; the rows are pane state so a just-added
  // blank row survives the doc round trip — the doc drops blank keys) ─────
  const envPath: Seg[] = ["profiles", profile, "env"];
  const envRowsFromDoc = (d: Doc) => toRows(get(d, envPath));
  const [envRows, setEnvRows] = useState<KvRow[]>(() => envRowsFromDoc(doc));
  const lastEnvDoc = useRef(doc);
  useEffect(() => {
    if (lastEnvDoc.current !== doc) {
      lastEnvDoc.current = doc;
      setEnvRows(envRowsFromDoc(doc));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- adopt on doc identity only
  }, [doc]);

  // ── Memory card (absent block reads as enabled=true) ───────────────────
  const memory = asRecord(get(doc, ["profiles", profile, "memory"]));
  const memoryEnabled = memory.enabled !== false;
  const memoryThreshold =
    typeof memory.threshold_bytes === "number" ? memory.threshold_bytes : undefined;

  // ── Knowledge card (vector_db; presence = on) ──────────────────────────
  const vdbPath: Seg[] = ["profiles", profile, "vector_db"];
  const vdb = asRecord(get(doc, vdbPath));
  const vdbOn = get(doc, vdbPath) !== undefined;
  const vdbEmb = asRecord(vdb.embeddings);
  const vdbDbPath = typeof vdb.db_path === "string" && vdb.db_path !== "" ? vdb.db_path : undefined;
  const vdbBaseUrl =
    typeof vdbEmb.base_url === "string" && vdbEmb.base_url !== "" ? vdbEmb.base_url : undefined;
  const vdbApiKey =
    typeof vdbEmb.api_key === "string" && vdbEmb.api_key !== "" ? vdbEmb.api_key : undefined;
  const vdbModel =
    typeof vdbEmb.model === "string" && vdbEmb.model !== "" ? vdbEmb.model : undefined;
  const vdbDimensions =
    typeof vdbEmb.dimensions === "number" ? vdbEmb.dimensions : undefined;
  const vdbDbHint =
    scope === "repo"
      ? "Relative paths resolve against this repo."
      : "Relative paths resolve against each session's working directory.";

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

      {/* Tools card (NameFilter over meta.builtin_tools — never hard-coded) */}
      <div data-testid="profile-tools-card" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Tools</h3>
          <span className="text-[10px] text-zinc-500">built-in tools</span>
        </div>
        <FilterBlock
          testid="profile-tools"
          label="Built-in tools"
          universe={meta?.builtin_tools ?? []}
          value={tools.value}
          onChange={tools.emit}
          emptyHint="No built-in tools — the agent can only talk (and use MCP tools)."
        />
        <span
          data-testid="profile-tools-footnote"
          className="mt-2 block text-[10px] text-zinc-500"
        >
          delegate is on automatically when the agent has subagents — it is not in this list.
        </span>
        <div className="mt-3">
          <Toggle
            testid="profile-tools-allowpost"
            label="Let fetch_url send POST requests"
            checked={allowPost}
            onChange={(c) => edit(["profiles", profile, "fetch_url", "allow_post"], c)}
            hint="Not a security boundary while bash is enabled."
          />
        </div>
      </div>

      {/* MCP access card (NameFilter over the global mcps names) */}
      <div data-testid="profile-mcps-card" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">MCP access</h3>
          <span className="text-[10px] text-zinc-500">over the global mcps servers</span>
        </div>
        {mcpUniverse.length === 0 ? (
          <div data-testid="profile-mcps">
            <span data-testid="profile-mcps-empty" className="text-xs text-zinc-500">
              No MCP servers declared globally.
            </span>
          </div>
        ) : (
          <FilterBlock
            testid="profile-mcps"
            label="MCP servers"
            universe={mcpUniverse}
            value={mcps.value}
            onChange={mcps.emit}
            emptyHint="only [] — no MCP tools at all"
          />
        )}
      </div>

      {/* Environment card (base bash env; NOT MCP-launched tools) */}
      <div data-testid="profile-env-card" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Environment</h3>
          <span className="text-[10px] text-zinc-500">base bash env</span>
        </div>
        <p className="mb-2 text-[11px] text-zinc-500">
          Base bash env, exported to bash for the agent and its subagents. MCP servers get their
          own env from mcps[].env.
        </p>
        <KvRows
          testid="profile-env"
          label="env (KEY=value)"
          rows={envRows}
          onChange={(rows) => {
            setEnvRows(rows);
            edit(envPath, toMap(rows));
          }}
        />
      </div>

      {/* Memory card */}
      <div data-testid="profile-memory-card" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Memory</h3>
        </div>
        <div className="space-y-3">
          <Toggle
            testid="profile-memory-enabled"
            label="Memory"
            checked={memoryEnabled}
            onChange={(c) => edit(["profiles", profile, "memory", "enabled"], c)}
            hint="Injects memory.md and auto-compacts an oversized one."
          />
          <Field
            testid="profile-memory-threshold"
            label="threshold_bytes"
            type="number"
            value={memoryThreshold}
            onChange={(v) => edit(["profiles", profile, "memory", "threshold_bytes"], v)}
            hint="bytes — empty = default"
          />
        </div>
      </div>

      {/* Knowledge card (vector_db; presence = on) */}
      <div data-testid="profile-vdb-card" className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-baseline justify-between gap-2">
          <h3 className="text-sm font-semibold">Knowledge</h3>
          <Badge tone="green">reload-safe</Badge>
        </div>
        <Toggle
          testid="profile-vdb-toggle"
          label="Enable Knowledge (vector_db)"
          checked={vdbOn}
          onChange={(c) =>
            edit(vdbPath, c ? { embeddings: {} } : undefined)
          }
          hint="Absent = off: the doc_index / doc_search tools are skipped for this profile."
        />
        {vdbOn && (
          <div className="mt-3 space-y-3">
            <Field
              testid="profile-vdb-dbpath"
              label="db_path"
              value={vdbDbPath}
              onChange={(v) => edit([...vdbPath, "db_path"], v)}
              hint={vdbDbHint}
            />
            <Field
              testid="profile-vdb-baseurl"
              label="embeddings.base_url"
              value={vdbBaseUrl}
              onChange={(v) => edit([...vdbPath, "embeddings", "base_url"], v)}
            />
            <SecretInput
              testid="profile-vdb-apikey"
              label="embeddings.api_key"
              value={vdbApiKey}
              onChange={(v) => edit([...vdbPath, "embeddings", "api_key"], v)}
              hint="Optional for local servers."
            />
            <Field
              testid="profile-vdb-model"
              label="embeddings.model"
              value={vdbModel}
              onChange={(v) => edit([...vdbPath, "embeddings", "model"], v)}
              hint="e.g. bge-m3, text-embedding-3-small, nomic-embed-text"
            />
            <Field
              testid="profile-vdb-dimensions"
              label="embeddings.dimensions"
              type="number"
              value={vdbDimensions}
              onChange={(v) => edit([...vdbPath, "embeddings", "dimensions"], v)}
              hint="dims"
            />
            <p className="text-[10px] text-amber-400/80">
              Must match the model and any existing DB at db_path — a mismatch fails when the
              conversation starts.
            </p>
          </div>
        )}
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
