// Step 6 — Services (optional). SearXNG, the vector DB, and the built-in tool
// filter.
//
// The tool filter's XOR is a real UI constraint, not a warning: picking a mode
// sets one list and drops the other, so `disabled` and `only` can never both
// exist. Which list is present *is* the mode — no second copy of that state to
// drift out of sync. validateServices still enforces the rule, because an
// imported config can arrive with both.

import type { ServicesDraft } from "../draft";
import type { StepProps } from "../steps";
import { BUILTIN_TOOL_NAMES, EMBEDDING_MODELS } from "../fixtures";
import { Check, Field, RadioCards, Section, inputClass } from "../ui";

type ToolMode = "all" | "disabled" | "only";

function toNumber(raw: string): number | undefined {
  if (raw.trim() === "") return undefined;
  const n = Number(raw);
  return Number.isFinite(n) ? n : undefined;
}

export function ServicesStep({ draft, patch }: StepProps) {
  const set = (partial: Partial<ServicesDraft>) =>
    patch({ services: { ...draft.services, ...partial } });
  const { searxng, vectorDb, tools } = draft.services;

  const mode: ToolMode = tools?.only
    ? "only"
    : tools?.disabled
      ? "disabled"
      : "all";
  const active = mode === "only" ? (tools?.only ?? []) : (tools?.disabled ?? []);
  const toggleTool = (name: string) => {
    const next = active.includes(name)
      ? active.filter((n) => n !== name)
      : [...active, name];
    set({ tools: mode === "only" ? { only: next } : { disabled: next } });
  };

  const embeddings = vectorDb?.embeddings ?? {};
  const setEmbeddings = (partial: Partial<typeof embeddings>) =>
    set({ vectorDb: { ...vectorDb, embeddings: { ...embeddings, ...partial } } });
  const knownDimensions = EMBEDDING_MODELS.find(
    (m) => m.model === embeddings.model,
  )?.dimensions;

  return (
    <div className="space-y-4">
      <Section
        title="SearXNG"
        hint="Backs web_search. Reload-safe — a change applies on the next /new."
      >
        <Check
          label="Enable web search"
          checked={searxng?.enabled ?? false}
          onChange={(enabled) => set({ searxng: { ...searxng, enabled } })}
        />
        <Field label="base_url">
          <input
            value={searxng?.baseUrl ?? ""}
            onChange={(e) =>
              set({ searxng: { ...searxng, baseUrl: e.target.value } })
            }
            placeholder="https://searx.example.com"
            spellCheck={false}
            className={inputClass}
          />
        </Field>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="timeout_seconds">
            <input
              value={searxng?.timeoutSeconds ?? ""}
              onChange={(e) =>
                set({
                  searxng: {
                    ...searxng,
                    timeoutSeconds: toNumber(e.target.value),
                  },
                })
              }
              inputMode="numeric"
              placeholder="10"
              className={inputClass}
            />
          </Field>
          <Field label="max_results">
            <input
              value={searxng?.maxResults ?? ""}
              onChange={(e) =>
                set({
                  searxng: { ...searxng, maxResults: toNumber(e.target.value) },
                })
              }
              inputMode="numeric"
              placeholder="10"
              className={inputClass}
            />
          </Field>
        </div>
        <Field label="bearer_token" hint="Only if your instance requires one.">
          <input
            value={searxng?.bearerToken ?? ""}
            onChange={(e) =>
              set({ searxng: { ...searxng, bearerToken: e.target.value } })
            }
            type="password"
            autoComplete="off"
            className={inputClass}
          />
        </Field>
      </Section>

      <Section
        title="Vector DB"
        hint="Backs doc_index / doc_search. Boot-only — a change needs a restart."
      >
        <Check
          label="Enable the document index"
          checked={vectorDb?.enabled ?? false}
          onChange={(enabled) => set({ vectorDb: { ...vectorDb, enabled } })}
        />
        <Field
          label="db_path"
          hint="Relative paths resolve per session cwd; absolute stays global."
        >
          <input
            value={vectorDb?.dbPath ?? ""}
            onChange={(e) =>
              set({ vectorDb: { ...vectorDb, dbPath: e.target.value } })
            }
            placeholder="./.peakbot/vectors.db"
            spellCheck={false}
            className={inputClass}
          />
        </Field>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="embeddings.base_url">
            <input
              value={embeddings.baseUrl ?? ""}
              onChange={(e) => setEmbeddings({ baseUrl: e.target.value })}
              placeholder="https://api.openai.com/v1"
              spellCheck={false}
              className={inputClass}
            />
          </Field>
          <Field label="embeddings.api_key">
            <input
              value={embeddings.apiKey ?? ""}
              onChange={(e) => setEmbeddings({ apiKey: e.target.value })}
              type="password"
              autoComplete="off"
              className={inputClass}
            />
          </Field>
          <Field label="embeddings.model">
            <input
              list="embedding-models"
              value={embeddings.model ?? ""}
              onChange={(e) => {
                const known = EMBEDDING_MODELS.find(
                  (m) => m.model === e.target.value,
                );
                setEmbeddings({
                  model: e.target.value,
                  dimensions: known?.dimensions ?? embeddings.dimensions,
                });
              }}
              spellCheck={false}
              className={inputClass}
            />
            <datalist id="embedding-models">
              {EMBEDDING_MODELS.map((m) => (
                <option key={m.model} value={m.model} />
              ))}
            </datalist>
          </Field>
          <Field
            label="embeddings.dimensions"
            error={
              knownDimensions !== undefined &&
              embeddings.dimensions !== undefined &&
              embeddings.dimensions !== knownDimensions
                ? `${embeddings.model} emits ${knownDimensions} dimensions.`
                : null
            }
            hint="Must match the model AND any existing DB at db_path — a mismatch fails at boot."
          >
            <input
              value={embeddings.dimensions ?? ""}
              onChange={(e) =>
                setEmbeddings({ dimensions: toNumber(e.target.value) })
              }
              inputMode="numeric"
              placeholder="1536"
              className={inputClass}
            />
          </Field>
        </div>
      </Section>

      <Section
        title="Built-in tool filter"
        hint="Pick one list. A blocklist keeps everything else; an allowlist keeps only what you check."
      >
        <RadioCards<ToolMode>
          name="tool-mode"
          value={mode}
          onChange={(next) =>
            set({
              tools:
                next === "all"
                  ? undefined
                  : next === "only"
                    ? { only: [] }
                    : { disabled: [] },
            })
          }
          options={[
            { value: "all", label: "Every tool", hint: "The default." },
            { value: "disabled", label: "disabled:", hint: "Blocklist." },
            { value: "only", label: "only:", hint: "Allowlist." },
          ]}
        />
        {mode !== "all" && (
          <div className="grid grid-cols-2 gap-1 sm:grid-cols-3">
            {BUILTIN_TOOL_NAMES.map((name) => (
              <Check
                key={name}
                label={name}
                checked={active.includes(name)}
                onChange={() => toggleTool(name)}
              />
            ))}
          </div>
        )}
      </Section>
    </div>
  );
}
