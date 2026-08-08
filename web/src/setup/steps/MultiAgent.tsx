// Step 9 — Multi-agent pipeline (optional). Name the team, preset the
// orchestrator, then one row per member role.
//
// The role's model select is fed by step 4's aliases — the flow's only real
// cross-step dependency, and the one thing here worth building properly. Delete
// an alias in Models and the rows that referenced it turn red immediately
// (validateMultiAgent), which is exactly what the binary would refuse at boot.
//
// An imported config that already declares `pipelines:` is rendered read-only:
// `pipelines` is not an owned key, so those teams pass through verbatim and
// this step has nothing to edit without silently deleting them.

import { useState } from "react";
import type { PipelineAgentDraft, PipelineDraft } from "../draft";
import { collectAliases, importedPipelines, pipelineName } from "../draft";
import type { StepProps } from "../steps";
import {
  Check,
  Field,
  ghostButtonClass,
  inputClass,
} from "../ui";

type SkillsMode = "all" | "only" | "disabled" | "off";

/** `KEY=value` lines → env record. Lines without `=` are ignored. */
function parseEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const eq = line.indexOf("=");
    if (eq <= 0) continue;
    env[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
  }
  return env;
}

function formatEnv(env: Record<string, string> | undefined): string {
  return Object.entries(env ?? {})
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

function splitList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** One role. The env and skills boxes keep their own raw text: parsing them on
 *  every keystroke is lossy (a half-typed `KEY` has no `=` yet), so the
 *  textarea must not be re-rendered from the parsed value. */
function RoleRow({
  agent,
  aliases,
  onChange,
  onRemove,
}: {
  agent: PipelineAgentDraft;
  aliases: string[];
  onChange: (partial: Partial<PipelineAgentDraft>) => void;
  onRemove: () => void;
}) {
  const [envText, setEnvText] = useState(() => formatEnv(agent.env));
  const [skillsText, setSkillsText] = useState(() =>
    (agent.skills?.only ?? agent.skills?.disabled ?? []).join(", "),
  );

  const skillsMode: SkillsMode =
    agent.skills?.enabled === false
      ? "off"
      : agent.skills?.only
        ? "only"
        : agent.skills?.disabled
          ? "disabled"
          : "all";
  const setSkillsMode = (mode: SkillsMode) => {
    const names = splitList(skillsText);
    onChange({
      skills:
        mode === "all"
          ? undefined
          : mode === "off"
            ? { enabled: false }
            : mode === "only"
              ? { only: names }
              : { disabled: names },
    });
  };

  const danglingAlias = !!agent.model && !aliases.includes(agent.model);

  return (
    <div className="space-y-3 rounded-lg border border-zinc-800 p-3">
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Role" hint="The key under pipelines[].agents — one word.">
          <input
            value={agent.role ?? ""}
            onChange={(e) => onChange({ role: e.target.value })}
            placeholder="researcher"
            spellCheck={false}
            className={inputClass}
          />
        </Field>
        <Field
          label="Model alias"
          hint="From the Models step. Omit to inherit default_model."
          error={
            danglingAlias
              ? `"${agent.model}" is not declared in the Models step.`
              : null
          }
        >
          <select
            value={agent.model ?? ""}
            onChange={(e) => onChange({ model: e.target.value || undefined })}
            className={inputClass}
          >
            <option value="">(default_model)</option>
            {aliases.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
            {/* Keep a removed alias selectable so the error is visible rather
                than silently snapping back to default_model. */}
            {danglingAlias && (
              <option value={agent.model}>{agent.model} (missing)</option>
            )}
          </select>
        </Field>
      </div>

      <Field label="Prompt" hint="This is the sub-agent's whole persona.">
        <textarea
          value={agent.prompt ?? ""}
          onChange={(e) => onChange({ prompt: e.target.value })}
          rows={3}
          className={`${inputClass} text-xs`}
        />
      </Field>

      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Skills" hint="only: XOR disabled:, or off entirely.">
          <select
            value={skillsMode}
            onChange={(e) => setSkillsMode(e.target.value as SkillsMode)}
            className={inputClass}
          >
            <option value="all">All skills</option>
            <option value="only">Only these…</option>
            <option value="disabled">All except these…</option>
            <option value="off">No skills</option>
          </select>
        </Field>
        {(skillsMode === "only" || skillsMode === "disabled") && (
          <Field label="Skill names" hint="Comma separated.">
            <input
              value={skillsText}
              onChange={(e) => {
                setSkillsText(e.target.value);
                const names = splitList(e.target.value);
                onChange({
                  skills:
                    skillsMode === "only"
                      ? { only: names }
                      : { disabled: names },
                });
              }}
              placeholder="github, gitea"
              spellCheck={false}
              className={inputClass}
            />
          </Field>
        )}
      </div>

      <Check
        label="agents_md"
        hint="Opt this role into the repo's agents.md (default off)."
        checked={agent.agentsMd ?? false}
        onChange={(agentsMd) => onChange({ agentsMd })}
      />

      <Field
        label="env"
        hint="KEY=value per line. Merged into THIS role's bash env only."
      >
        <textarea
          value={envText}
          onChange={(e) => {
            setEnvText(e.target.value);
            onChange({ env: parseEnv(e.target.value) });
          }}
          rows={2}
          spellCheck={false}
          className={`${inputClass} font-mono text-xs`}
        />
      </Field>

      <button type="button" onClick={onRemove} className={ghostButtonClass}>
        Remove role
      </button>
    </div>
  );
}

/** Best-effort name of a passthrough pipeline entry — the read-only notice
 *  shows what the wizard is leaving alone, so an unreadable entry still has to
 *  render something. */
function importedName(entry: unknown): string {
  if (entry && typeof entry === "object" && !Array.isArray(entry)) {
    const name = (entry as Record<string, unknown>).name;
    if (typeof name === "string" && name.trim()) return name;
  }
  return "(unnamed)";
}

export function MultiAgentStep({ draft, patch }: StepProps) {
  const aliases = collectAliases(draft);
  const imported = importedPipelines(draft);
  const agents = draft.pipeline.agents ?? [];
  const set = (partial: Partial<PipelineDraft>) =>
    patch({ pipeline: { ...draft.pipeline, ...partial } });

  // Stable row identity: with index keys, removing a middle role would hand
  // its in-progress env/skills text to the row that shifts up.
  const [rowKeys, setRowKeys] = useState<string[]>(() =>
    agents.map(() => crypto.randomUUID()),
  );

  const setAgents = (next: PipelineAgentDraft[]) => set({ agents: next });

  if (imported) {
    return (
      <div className="space-y-3">
        <p className="rounded-md border border-zinc-800 bg-zinc-900/50 px-3 py-2 text-xs text-zinc-400">
          {imported.length} pipeline{imported.length === 1 ? "" : "s"} already
          configured — the wizard won&apos;t touch them.
        </p>
        <ul className="space-y-1 text-xs text-zinc-500">
          {imported.map((entry, i) => (
            <li key={i} className="rounded-md border border-zinc-800 px-3 py-2">
              <code className="text-zinc-300">{importedName(entry)}</code>
            </li>
          ))}
        </ul>
        <p className="text-xs text-zinc-600">
          Edit them in config.yaml — they are written back exactly as imported.
        </p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <Check
        label="Add a sub-agent pipeline"
        hint="Boot-only: pipelines changes need a restart, not just /new."
        checked={draft.pipeline.include ?? false}
        onChange={(include) => set({ include })}
      />

      {draft.pipeline.include && (
        <>
          <Field
            label="Pipeline name"
            hint="What you type after /pipeline. No spaces; `none` is reserved."
          >
            <input
              value={pipelineName(draft.pipeline)}
              onChange={(e) => set({ name: e.target.value })}
              placeholder="default"
              spellCheck={false}
              className={inputClass}
            />
          </Field>

          <Field
            label="Orchestrator model"
            hint="Fixed for this pipeline — the UI can't change it later. Omit to inherit default_model."
          >
            <select
              value={draft.pipeline.orchestratorModel ?? ""}
              onChange={(e) =>
                set({ orchestratorModel: e.target.value || undefined })
              }
              className={inputClass}
            >
              <option value="">(default_model)</option>
              {aliases.map((a) => (
                <option key={a} value={a}>
                  {a}
                </option>
              ))}
              {/* Keep a removed alias selectable so the error stays visible
                  rather than silently snapping back to default_model. */}
              {draft.pipeline.orchestratorModel &&
                !aliases.includes(draft.pipeline.orchestratorModel) && (
                  <option value={draft.pipeline.orchestratorModel}>
                    {draft.pipeline.orchestratorModel} (missing)
                  </option>
                )}
            </select>
          </Field>

          <Field
            label="Orchestrator prompt"
            hint="Appended to the orchestrator's prompt. Sub-agents never see it."
          >
            <textarea
              value={draft.pipeline.orchestratorPrompt ?? ""}
              onChange={(e) =>
                set({ orchestratorPrompt: e.target.value || undefined })
              }
              rows={3}
              placeholder="You lead a small team. Delegate research and review…"
              className={`${inputClass} text-xs`}
            />
          </Field>

          <Field
            label="Orchestrator persona"
            hint="Optional: replaces the global persona while this pipeline runs."
          >
            <textarea
              value={draft.pipeline.orchestratorPersona ?? ""}
              onChange={(e) =>
                set({ orchestratorPersona: e.target.value || undefined })
              }
              rows={3}
              className={`${inputClass} text-xs`}
            />
          </Field>

          {agents.length === 0 && (
            <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">
              No roles yet. Each role becomes a delegate target — a pipeline
              needs at least one.
            </p>
          )}

          {agents.map((agent, i) => (
            <RoleRow
              key={rowKeys[i] ?? i}
              agent={agent}
              aliases={aliases}
              onChange={(partial) =>
                setAgents(
                  agents.map((a, j) => (i === j ? { ...a, ...partial } : a)),
                )
              }
              onRemove={() => {
                setAgents(agents.filter((_, j) => j !== i));
                setRowKeys((keys) => keys.filter((_, j) => j !== i));
              }}
            />
          ))}

          <button
            type="button"
            onClick={() => {
              setAgents([...agents, {}]);
              setRowKeys((keys) => [...keys, crypto.randomUUID()]);
            }}
            className={ghostButtonClass}
          >
            + Add role
          </button>
        </>
      )}
    </div>
  );
}
