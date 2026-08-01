// Step 2 — Locations. Read-only machine facts and a real Install button
// (POST /api/setup/install). config.yaml lives where the binary says it
// does; the checkbox for PATH is gone — the install action reports PATH
// state verbatim.

import { useState } from "react";
import { apiErrorMessage, installBinary, type SetupInfo } from "../api";
import type { StepProps } from "../steps";
import { Field, buttonClass } from "../ui";

type InstallResult = { kind: "ok"; action: string; target: string; path: string; notes: string[] }
  | { kind: "err"; lines: string[] };

function pathLabel(info: SetupInfo): string {
  const p = info.install.path;
  if (p.status === "on_path") return "On PATH";
  if (p.status === "shadowed") return `Shadowed by ${p.by}`;
  return "Not on PATH";
}
function pathHint(info: SetupInfo): string | undefined {
  const p = info.install.path;
  if (p.status === "absent") return p.hint;
  if (p.status === "shadowed") return "Another peakbot wins the PATH lookup — invoke it by absolute path.";
  return undefined;
}
function describePath(p: { status: string; by?: string; hint?: string }): string {
  if (p.status === "on_path") return "On PATH";
  if (p.status === "shadowed") return `Shadowed by ${p.by}`;
  return p.hint ?? "Not on PATH";
}

export function LocationsStep({ info }: StepProps) {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<InstallResult | null>(null);
  if (!info) return <p className="text-xs text-zinc-500">Loading machine facts…</p>;
  const state = info.install.state;

  const run = async () => {
    setBusy(true);
    setResult(null);
    try {
      const res = await installBinary();
      setResult({ kind: "ok", action: res.action, target: res.target, path: describePath(res.path), notes: res.notes });
    } catch (err) {
      setResult({ kind: "err", lines: apiErrorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-lg border border-zinc-800 p-3 text-xs sm:grid-cols-3">
        {[
          ["Config", info.config_path],
          ["Data", info.data_dir ?? "—"],
          ["Cache", info.cache_dir ?? "—"],
          ["Skills", info.skills_dir ?? "—"],
          ["Binary now", info.exe_path ?? "—"],
          ["Install to", info.install.target],
        ].map(([label, value]) => (
          <div key={label}>
            <dt className="text-zinc-500">{label}</dt>
            <dd className="truncate text-zinc-300" title={value}>{value}</dd>
          </div>
        ))}
      </div>

      <Field label="PATH state" hint={pathHint(info)}>
        <span className="text-sm text-zinc-200">{pathLabel(info)}</span>
      </Field>

      <div className="space-y-2 rounded-lg border border-zinc-800 p-3">
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" onClick={run} disabled={busy} className={buttonClass}>
            {busy ? "Installing…" : state === "current" ? "Reinstall (already current)" : "Install PeakBot"}
          </button>
          <span className="text-[11px] text-zinc-500">Runs now — copies the running binary to {info.install.target}.</span>
        </div>
        {result?.kind === "ok" && (
          <div className="space-y-1 text-xs text-zinc-300">
            <p><span className="text-zinc-500">Action:</span> {result.action}</p>
            <p><span className="text-zinc-500">Target:</span> {result.target}</p>
            <p><span className="text-zinc-500">PATH:</span> {result.path}</p>
            {result.notes.length > 0 && (
              <ul className="list-disc space-y-0.5 pl-5 text-zinc-400">
                {result.notes.map((n) => <li key={n}>{n}</li>)}
              </ul>
            )}
          </div>
        )}
        {result?.kind === "err" && (
          <ul className="space-y-0.5 text-xs text-red-300">
            {result.lines.map((l) => <li key={l}>{l}</li>)}
          </ul>
        )}
      </div>
    </div>
  );
}
