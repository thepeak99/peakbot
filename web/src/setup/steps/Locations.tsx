// Step 2 — Locations. Free-text directories plus the optional "put the binary
// on my PATH" action.
//
// Dummy: no existence checks, no writes. The `cp` command is shown verbatim so
// nothing about the PATH option is mysterious.

import type { LocationsDraft } from "../draft";
import type { StepProps } from "../steps";
import { PLATFORM } from "../fixtures";
import { Check, Field, inputClass } from "../ui";

export function LocationsStep({ draft, patch }: StepProps) {
  const set = (partial: Partial<LocationsDraft>) =>
    patch({ locations: { ...draft.locations, ...partial } });

  return (
    <div className="space-y-4">
      <Field
        label="Config directory"
        hint={`config.yaml lives here. Platform default: ${PLATFORM.configDir}`}
      >
        <input
          value={draft.locations.configDir ?? PLATFORM.configDir}
          onChange={(e) => set({ configDir: e.target.value })}
          spellCheck={false}
          className={inputClass}
        />
      </Field>

      <Field label="Data directory" hint="Conversations and memory.md.">
        <input
          value={draft.locations.dataDir ?? PLATFORM.dataDir}
          onChange={(e) => set({ dataDir: e.target.value })}
          spellCheck={false}
          className={inputClass}
        />
      </Field>

      <Field label="Cache directory" hint="TLS CA, MCP auth tokens, scratch.">
        <input
          value={draft.locations.cacheDir ?? PLATFORM.cacheDir}
          onChange={(e) => set({ cacheDir: e.target.value })}
          spellCheck={false}
          className={inputClass}
        />
      </Field>

      <Field
        label="Skills directories"
        hint="One per line. Scanned on every session verb, so new skills need no restart."
      >
        <textarea
          value={(draft.locations.skillsDirs ?? [PLATFORM.skillsDir]).join("\n")}
          onChange={(e) =>
            set({
              skillsDirs: e.target.value
                .split("\n")
                .map((s) => s.trim())
                .filter(Boolean),
            })
          }
          rows={3}
          spellCheck={false}
          className={`${inputClass} font-mono text-xs`}
        />
      </Field>

      <div className="space-y-2 rounded-lg border border-zinc-800 p-3">
        <Check
          label="Also put the binary on my PATH"
          checked={draft.locations.addToPath ?? false}
          onChange={(addToPath) => set({ addToPath })}
        />
        <pre className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1.5 text-xs text-zinc-300">
          {PLATFORM.installCommand}
        </pre>
        <p className="text-[11px] text-zinc-500">
          That is the exact command it would run. Nothing is copied in this
          preview.
        </p>
      </div>
    </div>
  );
}
