// D5 "Agent page" (spec §9.4, contract.ts "./agent (T28)"). Reads
// get(doc, ["profiles", p, "agents", a]) via useSettingsCtx() and writes
// through `edit(path, value)` by segment array. Both scopes are editable
// (repo scope edits the repo draft).
//
// Derived, never stored: the subagents toggle (non-empty map) and the
// "inherits" model option (absent `model` key). The Built-in persona chip
// CLEARS the key — it never writes a copy of the built-in text.

import { useState } from "react";
import { useSettingsCtx } from "./context";
import { get } from "./yamlDoc";
import { parseFilter, serializeFilter, type FilterState } from "./filter";
import { errAt } from "./paths";
import { useToast } from "./Toast";
import { Badge, FilterBlock, KvRows, ModelSelect, Toggle, type KvRow } from "./widgets";
import type { Seg } from "./types";

// Persona presets — the mockup's texts (v2/index.html PERSONAS, lines 748–752).
const NEUTRAL_PERSONA =
  "You are a coding agent working in the user's local filesystem. State what you are about to do in one line, do it, then report what changed. Prefer the smallest correct change.";
const TERSE_PERSONA =
  "You are a senior engineer reviewing and writing code. Be direct and brief. No filler. Name the file and the line. If something is wrong, say so plainly.";

const SELECT_CLS =
  "w-full cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";
const GHOST_BTN =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800";
const TEXTAREA_CLS =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-zinc-500 focus:outline-none";

function asRecord(v: unknown): Record<string, unknown> {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return {};
  return v as Record<string, unknown>;
}

function asString(v: unknown): string | undefined {
  return typeof v === "string" && v !== "" ? v : undefined;
}

function toRows(v: unknown): KvRow[] {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return [];
  return Object.entries(v as Record<string, unknown>).map(([k, val]) => ({
    key: k,
    value: typeof val === "string" ? val : String(val),
  }));
}

function toMap(rows: KvRow[]): Record<string, string> | undefined {
  const kept = rows.filter((r) => r.key.trim() !== "");
  if (kept.length === 0) return undefined;
  const out: Record<string, string> = {};
  for (const r of kept) out[r.key] = r.value;
  return out;
}

interface ModelOption {
  alias: string;
  qualified: string;
}

/** The draft's providers[].models as { alias, qualified = provider/name }, in
 *  declaration order (aliases are globally unique by validation). */
function modelOptionsOf(doc: Parameters<typeof get>[0]): ModelOption[] {
  const providers = get(doc, ["providers"]);
  if (!Array.isArray(providers)) return [];
  const out: ModelOption[] = [];
  const seen = new Set<string>();
  for (const p of providers) {
    const pname = asString(asRecord(p).name) ?? "";
    const models = asRecord(p).models;
    if (!Array.isArray(models)) continue;
    for (const m of models) {
      const alias = asString(asRecord(m).alias);
      if (!alias || seen.has(alias)) continue;
      seen.add(alias);
      out.push({ alias, qualified: `${pname}/${asString(asRecord(m).name) ?? ""}` });
    }
  }
  return out;
}

export function AgentPage({ profile, agent }: { profile: string; agent: string }) {
  const { doc, edit, meta, errors } = useSettingsCtx();
  const { toast } = useToast();

  const base: Seg[] = ["profiles", profile, "agents", agent];
  const a = asRecord(get(doc, base));
  const agents = asRecord(get(doc, ["profiles", profile, "agents"]));
  const isDefaultAgent = Object.keys(agents)[0] === agent;

  const options = modelOptionsOf(doc);
  const model = asString(a.model);
  const modelKnown = model === undefined || options.some((o) => o.alias === model);
  // An unknown current value stays selectable as `<x> ⚠`.
  const mainOptions: ModelOption[] = [
    ...(model !== undefined && !modelKnown ? [{ alias: model, qualified: "unknown ⚠" }] : []),
    ...options,
  ];

  const builtinPersona: string = meta?.builtin_persona ?? "";
  const builtinFirstLine = builtinPersona.split("\n")[0] ?? "";
  const persona = asString(a.persona);
  const [showBuiltin, setShowBuiltin] = useState(false);

  const prompt = asString(a.prompt);
  const skillsState = parseFilter(a.skills);
  const coreGuidance = a.core_guidance !== false;
  const agentsMd = a.agents_md !== false;

  // ── subagents ──────────────────────────────────────────────────────────
  const subsPath: Seg[] = [...base, "subagents"];
  const subs = asRecord(get(doc, subsPath));
  const subNames = Object.keys(subs);
  // The toggle is DERIVED (map non-empty); turning it off stashes the map in
  // the page's own state (this editing session) for the inline Undo.
  const [stash, setStash] = useState<Record<string, unknown> | null>(null);

  function setSubagents(next: Record<string, unknown> | undefined) {
    edit(subsPath, next);
  }

  function toggleSubs(on: boolean) {
    if (!on) {
      if (subNames.length === 0) return;
      setStash(subs);
      setSubagents(undefined);
      toast(`${subNames.length} subagent${subNames.length === 1 ? "" : "s"} removed from the draft`);
      return;
    }
    if (stash) {
      setSubagents(stash);
      setStash(null);
      return;
    }
    setSubagents({ helper: { prompt: "" } });
  }

  function undoSubs() {
    if (!stash) return;
    setSubagents(stash);
    setStash(null);
  }

  function addSub() {
    let name = "helper";
    let n = 2;
    while (subNames.includes(name)) name = `helper-${n++}`;
    setSubagents({ ...subs, [name]: { prompt: "" } });
  }

  function removeSub(name: string) {
    const out: Record<string, unknown> = {};
    for (const key of subNames) if (key !== name) out[key] = subs[key];
    setSubagents(out);
    toast(`Removed ${name}`);
  }

  function renameSub(i: number, newName: string) {
    const name = subNames[i];
    const target = newName.trim();
    if (target === "" || target === name || subNames.includes(target)) return;
    const out: Record<string, unknown> = {};
    subNames.forEach((key, j) => {
      out[j === i ? target : key] = subs[key];
    });
    setSubagents(out);
  }

  const modelError = errAt(errors, [...base, "model"]);

  return (
    <div data-testid="agent-page" className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold">Agent</h3>
        {isDefaultAgent && (
          <Badge tone="green">
            <span className="text-[10px]">DEFAULT</span>
          </Badge>
        )}
        <span className="text-xs text-zinc-500">
          in profile [{profile}] — its capabilities apply to this agent and every subagent.
        </span>
      </div>

      {/* Main card */}
      <div className="space-y-3 rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <h4 className="text-xs font-semibold text-zinc-300">{agent}</h4>

        <div className={modelKnown ? "" : "[&_select]:border-red-600"}>
          <ModelSelect
            testid="agent-model"
            label="model"
            options={mainOptions}
            value={model ?? ""}
            onChange={(alias) => edit([...base, "model"], alias === "" ? undefined : alias)}
          />
          <span className="mt-1 block text-[10px] text-zinc-500">
            Starting model — /model can switch it per conversation.
          </span>
          {model !== undefined && !modelKnown && (
            <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
              {modelError ?? `Unknown model alias ${model}`}
            </span>
          )}
        </div>

        <div data-testid="agent-persona-field" className="min-w-0">
          <span className="mb-1 block text-[11px] text-zinc-400">
            persona <span className="text-zinc-600">(empty = built-in persona)</span>
          </span>
          <textarea
            data-testid="agent-persona"
            value={persona ?? ""}
            placeholder={persona === undefined ? builtinFirstLine : ""}
            rows={3}
            onChange={(e) => edit([...base, "persona"], e.target.value === "" ? undefined : e.target.value)}
            className={TEXTAREA_CLS}
          />
          <div className="mt-1.5 flex flex-wrap gap-1.5">
            <button
              data-testid="agent-persona-chip-builtin"
              type="button"
              onClick={() => edit([...base, "persona"], undefined)}
              className={GHOST_BTN}
            >
              Built-in (Code Crusader)
            </button>
            <button
              data-testid="agent-persona-chip-neutral"
              type="button"
              onClick={() => edit([...base, "persona"], NEUTRAL_PERSONA)}
              className={GHOST_BTN}
            >
              Neutral engineer
            </button>
            <button
              data-testid="agent-persona-chip-terse"
              type="button"
              onClick={() => edit([...base, "persona"], TERSE_PERSONA)}
              className={GHOST_BTN}
            >
              Terse reviewer
            </button>
            {builtinPersona !== "" && (
              <button
                data-testid="agent-persona-builtin-toggle"
                type="button"
                onClick={() => setShowBuiltin((s) => !s)}
                className={GHOST_BTN}
              >
                {showBuiltin ? "Hide built-in persona" : "Show built-in persona"}
              </button>
            )}
          </div>
          {showBuiltin && builtinPersona !== "" && (
            <pre
              data-testid="agent-persona-builtin"
              className="mt-1.5 whitespace-pre-wrap rounded border border-zinc-800 bg-zinc-950/60 px-2 py-1.5 font-mono text-[11px] text-zinc-500"
            >
              {builtinPersona}
            </pre>
          )}
          {errAt(errors, [...base, "persona"]) && (
            <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
              {errAt(errors, [...base, "persona"])}
            </span>
          )}
        </div>

        <div data-testid="agent-prompt-field" className="min-w-0">
          <span className="mb-1 block text-[11px] text-zinc-400">
            Instructions (prompt){" "}
            <span className="text-zinc-600">(→ # Instructions in the system prompt)</span>
          </span>
          <textarea
            data-testid="agent-prompt"
            value={prompt ?? ""}
            placeholder="Task-level instructions appended to the system prompt…"
            rows={2}
            onChange={(e) => edit([...base, "prompt"], e.target.value === "" ? undefined : e.target.value)}
            className={TEXTAREA_CLS}
          />
          {errAt(errors, [...base, "prompt"]) && (
            <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
              {errAt(errors, [...base, "prompt"])}
            </span>
          )}
        </div>

        <FilterBlock
          testid="agent-skills"
          label="skills filter"
          universe={meta?.skills ?? []}
          value={skillsState}
          onChange={(state: FilterState) => edit([...base, "skills"], serializeFilter(state))}
        />

        <Toggle
          testid="agent-agentsmd"
          label="agents_md"
          checked={agentsMd}
          onChange={(c) => edit([...base, "agents_md"], c)}
          hint="Inject the repo's agents.md. Default on for the main agent."
        />

        <KvRows
          testid="agent-env"
          label="env (agent-level, on top of the profile's)"
          rows={toRows(a.env)}
          onChange={(rows) => edit([...base, "env"], toMap(rows))}
        />

        {/* Advanced */}
        <div className="rounded border border-zinc-800/60 p-2">
          <span className="mb-1 block text-[10px] font-semibold tracking-wider text-zinc-600">
            ADVANCED
          </span>
          <Toggle
            testid="agent-coreguidance"
            label="core_guidance"
            checked={coreGuidance}
            onChange={(c) => edit([...base, "core_guidance"], c)}
            hint={
              coreGuidance
                ? "On — PeakBot's built-in tool coaching follows your persona."
                : "Off — the persona is the whole system prompt head. No built-in coaching."
            }
          />
          {!coreGuidance && persona === undefined && (
            <span data-testid="agent-coreguidance-error" className="mt-1 block text-[11px] text-red-400">
              {errAt(errors, [...base, "core_guidance"]) ??
                "core_guidance: false needs a persona — with core guidance off, the persona is the whole prompt head"}
            </span>
          )}
        </div>
      </div>

      {/* Subagents card */}
      <div className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="mb-2 flex items-center justify-between gap-2">
          <h4 className="text-xs font-semibold text-zinc-300">
            Subagents {subNames.length > 0 && <span className="text-zinc-500">· {subNames.length} subagents</span>}
          </h4>
          {stash && (
            <button data-testid="agent-subs-undo" type="button" onClick={undoSubs} className={GHOST_BTN}>
              Undo
            </button>
          )}
        </div>
        <Toggle
          testid="agent-subs-toggle"
          label="Subagents"
          checked={subNames.length > 0}
          onChange={toggleSubs}
          hint={
            subNames.length > 0
              ? "delegate is on automatically when the agent has subagents."
              : "Single agent — no delegate tool."
          }
        />

        <div className="mt-3 space-y-2">
          {subNames.map((name, i) => {
            const s = asRecord(subs[name]);
            const sp: Seg[] = [...subsPath, name];
            const sModel = asString(s.model);
            const sModelKnown = sModel === undefined || options.some((o) => o.alias === sModel);
            const sPrompt = asString(s.prompt);
            const promptMissing = sPrompt === undefined || sPrompt.trim() === "";
            return (
              <div key={name} data-testid={`agent-sub-${i}`} className="space-y-2 rounded border border-zinc-800/60 p-2">
                <div className="grid gap-2 sm:grid-cols-2">
                  <div>
                    <span className="mb-1 block text-[11px] text-zinc-400">name</span>
                    <input
                      data-testid={`agent-sub-name-${i}`}
                      value={name}
                      placeholder="role key"
                      onChange={(e) => renameSub(i, e.target.value)}
                      className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-zinc-500 focus:outline-none"
                    />
                  </div>
                  <div>
                    <span className="mb-1 block text-[11px] text-zinc-400">model</span>
                    <select
                      data-testid={`agent-sub-model-${i}`}
                      value={sModel ?? ""}
                      onChange={(e) => edit([...sp, "model"], e.target.value === "" ? undefined : e.target.value)}
                      className={SELECT_CLS}
                    >
                      <option value="">{`inherits ${model ?? "the agent model"}`}</option>
                      {sModel !== undefined && !sModelKnown && (
                        <option value={sModel}>{`${sModel} ⚠`}</option>
                      )}
                      {options.map((o) => (
                        <option key={o.alias} value={o.alias}>
                          {o.alias} — {o.qualified}
                        </option>
                      ))}
                    </select>
                  </div>
                </div>

                <div>
                  <span className="mb-1 block text-[11px] text-zinc-400">
                    prompt <span className="text-red-400/80">REQUIRED</span>
                  </span>
                  <textarea
                    data-testid={`agent-sub-prompt-${i}`}
                    value={sPrompt ?? ""}
                    placeholder="What this subagent is for…"
                    rows={2}
                    onChange={(e) => edit([...sp, "prompt"], e.target.value)}
                    className={TEXTAREA_CLS + (promptMissing ? " border-red-600" : "")}
                  />
                  {promptMissing && (
                    <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
                      Required — a subagent needs a prompt.
                    </span>
                  )}
                </div>

                <FilterBlock
                  testid={`agent-sub-skills-${i}`}
                  label="skills filter"
                  universe={meta?.skills ?? []}
                  value={parseFilter(s.skills)}
                  onChange={(state: FilterState) => edit([...sp, "skills"], serializeFilter(state))}
                />

                <div className="grid gap-2 sm:grid-cols-2">
                  <Toggle
                    testid={`agent-sub-agentsmd-${i}`}
                    label="agents_md"
                    checked={s.agents_md === true}
                    onChange={(c) => edit([...sp, "agents_md"], c)}
                    hint="Default off for subagents."
                  />
                  <KvRows
                    testid={`agent-sub-env-${i}`}
                    label="env"
                    rows={toRows(s.env)}
                    onChange={(rows) => edit([...sp, "env"], toMap(rows))}
                  />
                </div>

                <div className="flex justify-end">
                  <button
                    data-testid={`agent-sub-remove-${i}`}
                    type="button"
                    title="Remove subagent"
                    onClick={() => removeSub(name)}
                    className="cursor-pointer rounded px-1 text-xs text-zinc-500 hover:text-red-400"
                  >
                    ✕ remove
                  </button>
                </div>
              </div>
            );
          })}
        </div>

        <button data-testid="agent-sub-add" type="button" onClick={addSub} className={`mt-2 ${GHOST_BTN}`}>
          + Add subagent
        </button>
      </div>
    </div>
  );
}
