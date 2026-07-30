// Step 4 — Models. Repeatable rows plus the `default_model` radio.
//
// The validation here is real (draft.ts), because it mirrors the documented
// rules: aliases unique and matching `^[A-Za-z0-9_./:-]+$`, the literal
// `unknown` reserved, `default_model` required iff any model is declared. It's
// the step most likely to be filled in wrong, so the feedback is honest even
// in a preview.

import type { ModelDraft } from "../draft";
import { ALIAS_PATTERN, RESERVED_ALIAS } from "../draft";
import type { StepProps } from "../steps";
import { MODEL_PRESETS, PROVIDER_TYPES } from "../fixtures";
import { Check, Field, ghostButtonClass, inputClass } from "../ui";

/** The one alias problem to show under *this* row. Duplicates blame the later
 *  row, so the first declaration stays clean. */
function aliasError(models: ModelDraft[], index: number): string | null {
  const alias = models[index].alias;
  if (!alias) return null;
  if (!ALIAS_PATTERN.test(alias)) {
    return `Outside the allowed charset ${ALIAS_PATTERN.source}.`;
  }
  if (alias === RESERVED_ALIAS) return `"${RESERVED_ALIAS}" is reserved.`;
  if (models.slice(0, index).some((m) => m.alias === alias)) {
    return "Already used by an earlier model.";
  }
  return null;
}

/** Empty string → undefined, so a cleared number field drops the key. */
function toNumber(raw: string): number | undefined {
  if (raw.trim() === "") return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

export function ModelsStep({ draft, patch }: StepProps) {
  const provider = draft.providers[0] ?? {};
  const models = provider.models ?? [];
  const presets = MODEL_PRESETS[provider.type ?? PROVIDER_TYPES[0].id];

  const setModels = (next: ModelDraft[]) =>
    patch({
      providers: [{ ...provider, models: next }, ...draft.providers.slice(1)],
    });
  const setModel = (index: number, partial: Partial<ModelDraft>) =>
    setModels(models.map((m, i) => (i === index ? { ...m, ...partial } : m)));

  const addModel = (model: ModelDraft) => {
    setModels([...models, model]);
    // First model declared wins default_model — it is required anyway.
    if (!draft.defaultModel && model.alias) patch({ defaultModel: model.alias });
  };

  return (
    <div className="space-y-4">
      {models.length === 0 && (
        <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">
          No models yet. Add one, or start from a preset below.
        </p>
      )}

      {models.map((m, i) => (
        <div key={i} className="space-y-3 rounded-lg border border-zinc-800 p-3">
          <div className="flex items-center justify-between gap-2">
            <label className="flex cursor-pointer items-center gap-2 text-xs text-zinc-400">
              <input
                type="radio"
                name="default_model"
                checked={!!m.alias && draft.defaultModel === m.alias}
                disabled={!m.alias}
                onChange={() => patch({ defaultModel: m.alias })}
                className="h-3.5 w-3.5 cursor-pointer accent-zinc-500"
              />
              default_model
            </label>
            <button
              type="button"
              onClick={() => setModels(models.filter((_, j) => j !== i))}
              className={ghostButtonClass}
            >
              Remove
            </button>
          </div>

          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Model name" hint="Exactly as the provider spells it.">
              <input
                value={m.name ?? ""}
                onChange={(e) => setModel(i, { name: e.target.value })}
                placeholder="anthropic/claude-sonnet-4.5"
                spellCheck={false}
                className={inputClass}
              />
            </Field>
            <Field
              label="Alias"
              hint="Optional. Without one, address it as <provider>/<model>."
              error={aliasError(models, i)}
            >
              <input
                value={m.alias ?? ""}
                onChange={(e) => {
                  const alias = e.target.value || undefined;
                  // Keep default_model pointing at this row while it is renamed.
                  if (draft.defaultModel && draft.defaultModel === m.alias) {
                    patch({ defaultModel: alias });
                  }
                  setModel(i, { alias });
                }}
                placeholder="sonnet"
                spellCheck={false}
                className={inputClass}
              />
            </Field>
            <Field label="max_tokens">
              <input
                value={m.maxTokens ?? ""}
                onChange={(e) =>
                  setModel(i, { maxTokens: toNumber(e.target.value) })
                }
                inputMode="numeric"
                placeholder="8192"
                className={inputClass}
              />
            </Field>
            <Field label="temperature" hint="Leave empty for the provider default.">
              <input
                value={m.temperature ?? ""}
                onChange={(e) =>
                  setModel(i, { temperature: toNumber(e.target.value) })
                }
                inputMode="decimal"
                placeholder="0.7"
                className={inputClass}
              />
            </Field>
            <Field
              label="context_window_override"
              hint="Only when auto-detection gets it wrong."
            >
              <input
                value={m.contextWindowOverride ?? ""}
                onChange={(e) =>
                  setModel(i, {
                    contextWindowOverride: toNumber(e.target.value),
                  })
                }
                inputMode="numeric"
                placeholder="200000"
                className={inputClass}
              />
            </Field>
            <div className="self-end pb-1">
              <Check
                label="vision"
                hint="Force image support on. Omit for auto-detection."
                checked={m.vision ?? false}
                onChange={(vision) =>
                  setModel(i, { vision: vision ? true : undefined })
                }
              />
            </div>
          </div>
        </div>
      ))}

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => addModel({})}
          className={ghostButtonClass}
        >
          + Add model
        </button>
        {presets.map((p) => (
          <button
            key={p.name}
            type="button"
            onClick={() => addModel({ ...p })}
            className={ghostButtonClass}
            title={p.name}
          >
            + {p.alias}
          </button>
        ))}
      </div>
    </div>
  );
}
