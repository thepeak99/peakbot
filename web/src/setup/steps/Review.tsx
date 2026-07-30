// Step 10 — Review. The YAML the wizard would write, the restart split, and
// the Write config button.
//
// The YAML is rendered client-side from the draft (renderYaml), so this page is
// genuinely useful for judging the shape even though nothing is written. The
// restart split reads the *rendered* YAML's top-level keys rather than the
// draft, so the list can never disagree with the artifact above it.

import { useState } from "react";
import { classifyChange } from "../draft";
import type { StepProps } from "../steps";
import { renderYaml } from "../renderYaml";
import { PERSONA_PRESETS } from "../fixtures";
import {
  FakeActionButton,
  PreviewChip,
  buttonClass,
  ghostButtonClass,
} from "../ui";

/** Top-level keys of a rendered config: a key at column zero. */
function topLevelKeys(yaml: string): string[] {
  return yaml
    .split("\n")
    .map((line) => /^([a-z_]+):/.exec(line)?.[1])
    .filter((key): key is string => !!key);
}

export function ReviewStep({ draft }: StepProps) {
  const [written, setWritten] = useState(false);
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);

  const yaml = renderYaml(draft);
  const keys = topLevelKeys(yaml);
  const reloadSafe = keys.filter((k) => classifyChange(k) === "reload-safe");
  const bootOnly = keys.filter((k) => classifyChange(k) === "boot-only");

  // Steps the wizard collects that config.yaml has no key for yet — better
  // said out loud here than guessed at by renderYaml.
  const notYetConfigurable = [
    draft.locations.configDir && `directories → ${draft.locations.configDir}`,
    draft.locations.addToPath && "copy the binary onto PATH",
    draft.persona.mode === "preset" &&
      draft.persona.presetId &&
      `persona → ${PERSONA_PRESETS.find((p) => p.id === draft.persona.presetId)?.name ?? draft.persona.presetId}`,
    draft.persona.mode === "custom" && "persona → custom prompt",
    draft.startOnBoot.enabled && "start on boot",
  ].filter((x): x is string => !!x);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(yaml);
      setCopied("yes");
    } catch {
      // No clipboard permission (or a plain-http origin) — say so instead of
      // pretending the copy worked.
      setCopied("failed");
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2 rounded-md border border-amber-800/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-300">
        <PreviewChip />
        <p>
          This is a preview of the wizard. Nothing below has been written, and
          leaving the page discards the draft.
        </p>
      </div>

      <div className="space-y-1">
        <p className="text-xs font-medium text-zinc-400">config.yaml</p>
        {yaml ? (
          <pre className="max-h-96 overflow-auto rounded-md border border-zinc-800 bg-zinc-900 px-3 py-2 text-xs text-zinc-200">
            {yaml}
          </pre>
        ) : (
          <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">
            Nothing configured yet — add a provider and a model.
          </p>
        )}
        <p className="text-[11px] text-zinc-500">
          Secrets show as <code>****</code>. The real values stay in the draft.
        </p>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="rounded-lg border border-zinc-800 p-3">
          <h3 className="text-sm font-medium text-zinc-200">
            Applies on the next /new
          </h3>
          <p className="mt-0.5 text-[11px] text-zinc-500">
            Session verbs re-read these — no restart.
          </p>
          <ul className="mt-2 space-y-0.5 text-xs text-zinc-400">
            {reloadSafe.length === 0 && <li className="text-zinc-600">None.</li>}
            {reloadSafe.map((k) => (
              <li key={k}>
                <code>{k}</code>
              </li>
            ))}
          </ul>
        </div>
        <div className="rounded-lg border border-zinc-800 p-3">
          <h3 className="text-sm font-medium text-zinc-200">
            Needs a restart
          </h3>
          <p className="mt-0.5 text-[11px] text-zinc-500">
            Read once at boot; a running process ignores changes.
          </p>
          <ul className="mt-2 space-y-0.5 text-xs text-zinc-400">
            {bootOnly.length === 0 && <li className="text-zinc-600">None.</li>}
            {bootOnly.map((k) => (
              <li key={k}>
                <code>{k}</code>
              </li>
            ))}
          </ul>
        </div>
      </div>

      {notYetConfigurable.length > 0 && (
        <div className="rounded-lg border border-zinc-800 p-3">
          <h3 className="text-sm font-medium text-zinc-200">
            Collected, but not part of config.yaml yet
          </h3>
          <ul className="mt-2 space-y-0.5 text-xs text-zinc-400">
            {notYetConfigurable.map((x) => (
              <li key={x}>{x}</li>
            ))}
          </ul>
        </div>
      )}

      <FakeActionButton
        label="Write config"
        result="Preview only — nothing was written."
        onDone={() => setWritten(true)}
      />

      {written && (
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => window.location.assign("/")}
            className={buttonClass}
          >
            Open Shifu
          </button>
          <button type="button" onClick={copy} className={ghostButtonClass}>
            Copy YAML
          </button>
          {copied === "yes" && (
            <span className="text-xs text-emerald-400">✓ Copied</span>
          )}
          {copied === "failed" && (
            <span className="text-xs text-zinc-500">
              Clipboard blocked — select the pane above and copy manually.
            </span>
          )}
        </div>
      )}
    </div>
  );
}
