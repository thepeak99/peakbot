// Step 10 — Review. The YAML the wizard would write, the restart split, and
// the Write config button.
//
// The YAML is rendered client-side from the draft (renderYaml), so this
// page is genuinely useful for judging the shape even before the network
// call. The restart split reads the *rendered* YAML's top-level keys
// rather than the draft, so the list can never disagree with the artifact.
// Per plan §A-Q7 / §E.13, persona is real and Locations/Start-on-boot are
// self-contained actions — nothing in the draft is "collected, not written"
// anymore, so that block is gone.

import { useState } from "react";
import { apiErrorMessage, writeConfig } from "../api";
import { classifyChange } from "../draft";
import type { StepProps } from "../steps";
import { renderYaml } from "../renderYaml";
import { buttonClass, ghostButtonClass } from "../ui";

type WriteState =
  | { kind: "idle" }
  | { kind: "busy" }
  | { kind: "ok"; path: string; backup: string | null }
  | { kind: "err"; lines: string[] };

function topLevelKeys(yaml: string): string[] {
  return yaml
    .split("\n")
    .map((line) => /^([a-z_][a-z0-9_]*):/.exec(line)?.[1])
    .filter((key): key is string => !!key);
}

export function ReviewStep({ draft }: StepProps) {
  const [write, setWrite] = useState<WriteState>({ kind: "idle" });
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);

  const yaml = renderYaml(draft);
  const keys = topLevelKeys(yaml);
  const reloadSafe = keys.filter((k) => classifyChange(k) === "reload-safe");
  const bootOnly = keys.filter((k) => classifyChange(k) === "boot-only");

  const submit = async () => {
    setWrite({ kind: "busy" });
    try {
      const body = renderYaml(draft, { mask: false });
      const res = await writeConfig(body);
      setWrite({ kind: "ok", path: res.path, backup: res.backup });
    } catch (err) {
      setWrite({ kind: "err", lines: apiErrorMessage(err) });
    }
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(yaml);
      setCopied("yes");
    } catch {
      setCopied("failed");
    }
  };

  return (
    <div className="space-y-4">
      <div className="space-y-1">
        <p className="text-xs font-medium text-zinc-400">config.yaml</p>
        {yaml ? (
          <pre className="max-h-96 overflow-auto rounded-md border border-zinc-800 bg-zinc-900 px-3 py-2 text-xs text-zinc-200">{yaml}</pre>
        ) : (
          <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">Nothing configured yet — add a provider and a model.</p>
        )}
        <p className="text-[11px] text-zinc-500">Secrets show as <code>****</code>. The real values stay in the draft and are sent verbatim to the write endpoint.</p>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="rounded-lg border border-zinc-800 p-3">
          <h3 className="text-sm font-medium text-zinc-200">Applies on the next /new</h3>
          <p className="mt-0.5 text-[11px] text-zinc-500">Session verbs re-read these — no restart.</p>
          <ul className="mt-2 space-y-0.5 text-xs text-zinc-400">
            {reloadSafe.length === 0 && <li className="text-zinc-600">None.</li>}
            {reloadSafe.map((k) => <li key={k}><code>{k}</code></li>)}
          </ul>
        </div>
        <div className="rounded-lg border border-zinc-800 p-3">
          <h3 className="text-sm font-medium text-zinc-200">Needs a restart</h3>
          <p className="mt-0.5 text-[11px] text-zinc-500">Read once at boot; a running process ignores changes.</p>
          <ul className="mt-2 space-y-0.5 text-xs text-zinc-400">
            {bootOnly.length === 0 && <li className="text-zinc-600">None.</li>}
            {bootOnly.map((k) => <li key={k}><code>{k}</code></li>)}
          </ul>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={submit} disabled={write.kind === "busy"} className={buttonClass}>
          {write.kind === "busy" ? "Writing…" : "Write config"}
        </button>
        <button type="button" onClick={copy} className={ghostButtonClass}>Copy YAML</button>
        {copied === "yes" && <span className="text-xs text-emerald-400">✓ Copied</span>}
        {copied === "failed" && <span className="text-xs text-zinc-500">Clipboard blocked — select the pane above and copy manually.</span>}
      </div>

      {write.kind === "err" && (
        <ul className="space-y-0.5 rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-xs text-red-300">
          {write.lines.map((l) => <li key={l}>{l}</li>)}
        </ul>
      )}

      {write.kind === "ok" && (
        <div className="space-y-2 rounded-md border border-emerald-800/60 bg-emerald-950/30 px-3 py-2 text-xs text-emerald-300">
          <p>Wrote <code>{write.path}</code>{write.backup && <> (backup: <code>{write.backup}</code>)</>}.</p>
          <p>Restart PeakBot to use this config.</p>
          <div className="flex flex-wrap items-center gap-2">
            <a href="/" className={buttonClass}>Open Shifu</a>
          </div>
        </div>
      )}
    </div>
  );
}
