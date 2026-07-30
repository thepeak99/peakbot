// Step 3 — Provider. Type, base URL, API key, and a Test connection button.
//
// The draft holds a provider *list* (agents.md supports several), but the
// wizard edits one: a second provider only matters once you are juggling
// aliases across them, and that is not a first-run problem. `providers[0]` is
// the whole step.
//
// Dummy: Test connection never leaves the browser.

import { useState } from "react";
import type { ProviderWithModelsDraft } from "../draft";
import type { StepProps } from "../steps";
import { PROVIDER_TYPES, TEST_CONNECTION_RESULT } from "../fixtures";
import {
  FakeActionButton,
  Field,
  ghostButtonClass,
  inputClass,
} from "../ui";

export function ProviderStep({ draft, patch }: StepProps) {
  const [revealed, setRevealed] = useState(false);
  const provider = draft.providers[0] ?? {};
  const spec = PROVIDER_TYPES.find((t) => t.id === provider.type);

  const set = (partial: Partial<ProviderWithModelsDraft>) =>
    patch({ providers: [{ ...provider, ...partial }, ...draft.providers.slice(1)] });

  return (
    <div className="space-y-4">
      <Field
        label="Provider type"
        hint="Decides which API dialect is spoken. Local runtimes need no key."
      >
        <select
          value={provider.type ?? ""}
          onChange={(e) => {
            const next = PROVIDER_TYPES.find((t) => t.id === e.target.value);
            if (!next) return;
            // Switching type re-prefills the endpoint and names the provider
            // after it, which is what `providers[].name` is for.
            set({
              type: next.id,
              baseUrl: next.defaultBaseUrl || undefined,
              name: provider.name || next.id,
            });
          }}
          className={inputClass}
        >
          <option value="">Choose…</option>
          {PROVIDER_TYPES.map((t) => (
            <option key={t.id} value={t.id}>
              {t.label}
            </option>
          ))}
        </select>
      </Field>

      <Field
        label="Name"
        hint="How models address this provider: <name>/<model> when a model has no alias."
      >
        <input
          value={provider.name ?? ""}
          onChange={(e) => set({ name: e.target.value })}
          placeholder="openrouter"
          spellCheck={false}
          className={inputClass}
        />
      </Field>

      <Field
        label="Base URL"
        hint={
          spec?.defaultBaseUrl
            ? `Leave as-is unless you proxy it. Default: ${spec.defaultBaseUrl}`
            : "Optional — OpenRouter uses its own endpoint."
        }
      >
        <input
          value={provider.baseUrl ?? ""}
          onChange={(e) => set({ baseUrl: e.target.value })}
          placeholder={spec?.defaultBaseUrl || "https://…"}
          spellCheck={false}
          className={inputClass}
        />
      </Field>

      {spec?.needsApiKey !== false && (
        <Field
          label="API key"
          hint="Stored in config.yaml (0600). The review page shows it masked."
        >
          <div className="flex gap-2">
            <input
              value={provider.apiKey ?? ""}
              onChange={(e) => set({ apiKey: e.target.value })}
              type={revealed ? "text" : "password"}
              autoComplete="off"
              spellCheck={false}
              placeholder="sk-…"
              className={inputClass}
            />
            <button
              type="button"
              onClick={() => setRevealed((r) => !r)}
              className={`${ghostButtonClass} shrink-0`}
            >
              {revealed ? "Hide" : "Reveal"}
            </button>
          </div>
        </Field>
      )}

      <FakeActionButton
        label="Test connection"
        result={TEST_CONNECTION_RESULT}
        disabled={!provider.type}
      />
    </div>
  );
}
