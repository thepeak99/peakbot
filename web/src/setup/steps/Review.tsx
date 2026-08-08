// Step 10 — Review. The YAML the wizard would write, the restart split, and
// the single Install button that writes the configuration and then runs the
// binary install (POST /api/setup/install).
//
// The YAML is rendered client-side from the draft (renderYaml), so this
// page is genuinely useful for judging the shape even before the network
// call. The restart split reads the *rendered* YAML's top-level keys
// rather than the draft, so the list can never disagree with the artifact.
// Per plan §A-Q7 / §E.13, persona is real and Locations/Start-on-boot are
// self-contained actions — nothing in the draft is "collected, not written"
// anymore, so that block is gone.
//
// The handler runs the two API calls in sequence: writeConfig first (it
// validates server-side; if it fails we don't even try the install), then
// installBinary. If the config write succeeded but the install failed, the
// error panel reports that the config was written so the user can retry
// without losing it.

import { useState } from "react";
import { apiErrorMessage, installBinary, writeConfig } from "../api";
import { classifyChange } from "../draft";
import type { StepProps } from "../steps";
import { renderYaml } from "../renderYaml";
import { buttonClass, ghostButtonClass } from "../ui";

type InstallState =
  | { kind: "idle" }
  | { kind: "writing" }
  | { kind: "installing"; configPath: string; backup: string | null }
  | {
      kind: "ok";
      configPath: string;
      backup: string | null;
      installAction: string;
      installTarget: string;
      installPath: { status: string; by?: string; hint?: string };
      installNotes: string[];
    }
  | { kind: "err"; lines: string[]; configWritten: { path: string; backup: string | null } | null };

function topLevelKeys(yaml: string): string[] {
  return yaml
    .split("\n")
    .map((line) => /^([a-z_][a-z0-9_]*):/.exec(line)?.[1])
    .filter((key): key is string => !!key);
}

function pathVerdict(p: { status: string; by?: string; hint?: string }): string {
  if (p.status === "on_path") return "On PATH";
  if (p.status === "shadowed") return `Shadowed by ${p.by}`;
  return "Not on PATH";
}

export function ReviewStep({ draft }: StepProps) {
  const [install, setInstall] = useState<InstallState>({ kind: "idle" });

  const yaml = renderYaml(draft);
  const keys = topLevelKeys(yaml);
  const reloadSafe = keys.filter((k) => classifyChange(k) === "reload-safe");
  const bootOnly = keys.filter((k) => classifyChange(k) === "boot-only");

  const run = async () => {
    const body = renderYaml(draft, { mask: false });
    setInstall({ kind: "writing" });
    let configPath: string;
    let backup: string | null;
    try {
      const write = await writeConfig(body);
      configPath = write.path;
      backup = write.backup;
    } catch (err) {
      setInstall({ kind: "err", lines: apiErrorMessage(err), configWritten: null });
      return;
    }
    setInstall({ kind: "installing", configPath, backup });
    try {
      const res = await installBinary();
      setInstall({
        kind: "ok",
        configPath,
        backup,
        installAction: res.action,
        installTarget: res.target,
        installPath: res.path,
        installNotes: res.notes,
      });
    } catch (err) {
      setInstall({
        kind: "err",
        lines: apiErrorMessage(err),
        configWritten: { path: configPath, backup },
      });
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
        <button
          type="button"
          onClick={run}
          disabled={install.kind === "writing" || install.kind === "installing"}
          className={buttonClass}
        >
          {install.kind === "writing"
            ? "Writing config…"
            : install.kind === "installing"
              ? "Installing…"
              : "Install"}
        </button>
        <a href="/" className={ghostButtonClass}>Cancel</a>
      </div>

      {install.kind === "err" && (
        <div className="space-y-2 rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-xs text-red-300">
          <ul className="space-y-0.5">
            {install.lines.map((l) => <li key={l}>{l}</li>)}
          </ul>
          {install.configWritten && (
            <p>
              Note: your config was written to <code className="text-red-200">{install.configWritten.path}</code>
              {install.configWritten.backup && (
                <> (backup: <code className="text-red-200">{install.configWritten.backup}</code>)</>
              )}. Retry the install above — your config is preserved.
            </p>
          )}
        </div>
      )}

      {install.kind === "ok" && (
        <div className="space-y-2 rounded-md border border-emerald-800/60 bg-emerald-950/30 px-3 py-2 text-xs text-emerald-300">
          <p>
            Wrote <code>{install.configPath}</code>
            {install.backup && <> (backup: <code>{install.backup}</code>)</>}.
          </p>
          <p>
            Installed to <code>{install.installTarget}</code> ({install.installAction}). PATH: {pathVerdict(install.installPath)}.
          </p>
          {install.installNotes.length > 0 && (
            <ul className="list-disc space-y-0.5 pl-5 text-emerald-200/80">
              {install.installNotes.map((n) => <li key={n}>{n}</li>)}
            </ul>
          )}
          <p>Restart PeakBot to use this config.</p>
          <div className="flex flex-wrap items-center gap-2">
            <a href="/" className={buttonClass}>Open Shifu</a>
          </div>
        </div>
      )}
    </div>
  );
}
