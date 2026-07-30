// Step 5 — Persona. Preset cards with the full prompt behind a disclosure, or
// a custom prompt with a live character count.
//
// Dummy: presets come from fixtures. Note that `persona:` is not a config key
// yet (plan §8.B.5), so the review page lists this under "collected, not
// written" rather than inventing a key renderYaml would have to guess at.

import type { PersonaDraft } from "../draft";
import type { StepProps } from "../steps";
import { PERSONA_PRESETS } from "../fixtures";
import { inputClass } from "../ui";

export function PersonaStep({ draft, patch }: StepProps) {
  const set = (partial: Partial<PersonaDraft>) =>
    patch({ persona: { ...draft.persona, ...partial } });
  const { mode, presetId, custom } = draft.persona;

  return (
    <div className="space-y-3">
      {PERSONA_PRESETS.map((p) => (
        <div
          key={p.id}
          className={`rounded-lg border px-3 py-2 transition-colors ${
            mode === "preset" && presetId === p.id
              ? "border-zinc-600 bg-zinc-800"
              : "border-zinc-800"
          }`}
        >
          <label className="flex cursor-pointer items-start gap-2">
            <input
              type="radio"
              name="persona"
              checked={mode === "preset" && presetId === p.id}
              onChange={() => set({ mode: "preset", presetId: p.id })}
              className="mt-1 h-3.5 w-3.5 cursor-pointer accent-zinc-500"
            />
            <span>
              <span className="block text-sm text-zinc-200">{p.name}</span>
              <span className="block text-[11px] text-zinc-500">{p.blurb}</span>
            </span>
          </label>
          <details className="mt-2">
            <summary className="cursor-pointer text-[11px] text-zinc-500 hover:text-zinc-300">
              Show the full prompt
            </summary>
            <pre className="mt-2 overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1.5 text-xs whitespace-pre-wrap text-zinc-300">
              {p.prompt}
            </pre>
          </details>
        </div>
      ))}

      <div
        className={`space-y-2 rounded-lg border px-3 py-2 transition-colors ${
          mode === "custom" ? "border-zinc-600 bg-zinc-800" : "border-zinc-800"
        }`}
      >
        <label className="flex cursor-pointer items-center gap-2">
          <input
            type="radio"
            name="persona"
            checked={mode === "custom"}
            onChange={() => set({ mode: "custom" })}
            className="h-3.5 w-3.5 cursor-pointer accent-zinc-500"
          />
          <span className="text-sm text-zinc-200">Custom</span>
        </label>
        <textarea
          value={custom ?? ""}
          onChange={(e) => set({ mode: "custom", custom: e.target.value })}
          rows={6}
          placeholder="You are…"
          className={`${inputClass} text-xs`}
        />
        <p className="text-[11px] text-zinc-500">
          {(custom ?? "").length} characters
        </p>
      </div>
    </div>
  );
}
