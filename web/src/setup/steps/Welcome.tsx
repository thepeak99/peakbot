// Step 1 — Welcome. Two paths: import an existing config, or start fresh.
//
// Dummy: the platform facts are fixtures, and Import reads nothing — neither
// the pasted text nor the picked file is parsed. It shows a canned summary and
// prefills the draft from `IMPORTED_PROVIDER`, which is what makes the rest of
// the wizard worth clicking through.

import { useState } from "react";
import type { StepProps } from "../steps";
import { IMPORTED_PROVIDER, IMPORT_RESULT, PLATFORM } from "../fixtures";
import {
  FakeActionButton,
  buttonClass,
  ghostButtonClass,
  inputClass,
} from "../ui";

export function WelcomeStep({ draft, patch, next }: StepProps) {
  const [pasted, setPasted] = useState("");
  const [pickedFile, setPickedFile] = useState<string | null>(null);
  const mode = draft.welcome.startMode;

  const startFresh = () => {
    patch({ welcome: { startMode: "fresh" } });
    next();
  };

  const applyImport = () => {
    const { defaultModel, ...provider } = IMPORTED_PROVIDER;
    patch({
      welcome: { startMode: "import", importedSummary: IMPORT_RESULT },
      providers: [provider],
      defaultModel,
    });
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <img src="/favicon.svg" alt="" className="h-10 w-10" />
        <p className="text-sm text-zinc-400">
          Point your agent at a provider, name a few models, and it is ready to
          work. Ten steps; six of them are optional.
        </p>
      </div>

      <dl className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-lg border border-zinc-800 p-3 text-xs sm:grid-cols-3">
        {[
          ["OS", PLATFORM.os],
          ["Arch", PLATFORM.arch],
          ["Binary", PLATFORM.exePath],
        ].map(([label, value]) => (
          <div key={label}>
            <dt className="text-zinc-500">{label}</dt>
            <dd className="truncate text-zinc-300" title={value}>
              {value}
            </dd>
          </div>
        ))}
      </dl>

      <div className="flex flex-wrap gap-2">
        <button type="button" onClick={startFresh} className={buttonClass}>
          Start fresh
        </button>
        <button
          type="button"
          onClick={() => patch({ welcome: { startMode: "import" } })}
          className={ghostButtonClass}
        >
          Import an existing config
        </button>
      </div>

      {mode === "import" && (
        <div className="space-y-3 rounded-lg border border-zinc-800 p-3">
          <p className="text-xs text-zinc-500">
            Paste a <code>config.yaml</code>, or pick the file. Nothing is read
            from disk in this preview — the summary below is canned.
          </p>
          <textarea
            value={pasted}
            onChange={(e) => setPasted(e.target.value)}
            rows={5}
            spellCheck={false}
            placeholder={"providers:\n  - name: openrouter\n    …"}
            className={`${inputClass} font-mono text-xs`}
          />
          <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-500">
            <input
              type="file"
              accept=".yaml,.yml"
              onChange={(e) => setPickedFile(e.target.files?.[0]?.name ?? null)}
              className="max-w-full cursor-pointer text-xs text-zinc-400 file:mr-2 file:cursor-pointer file:rounded file:border file:border-zinc-800 file:bg-zinc-900 file:px-2 file:py-1 file:text-zinc-300"
            />
            {pickedFile && <span>{pickedFile} — accepted, not read</span>}
          </div>
          <FakeActionButton
            label="Import"
            result={IMPORT_RESULT}
            onDone={applyImport}
          />
          {draft.welcome.importedSummary && (
            <p className="text-xs text-zinc-400">
              Draft prefilled — check the Provider and Models steps.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
