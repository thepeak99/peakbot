// Step 2 — Locations. Read-only machine facts and the PATH state verdict.
// The install action lives on the Review step; config.yaml lives where the
// binary says it does, and the install response reports PATH state verbatim.

import type { SetupInfo } from "../api";
import type { StepProps } from "../steps";
import { Field } from "../ui";

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

export function LocationsStep({ info }: StepProps) {
  if (!info) return <p className="text-xs text-zinc-500">Loading machine facts…</p>;

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
        <span className="block text-sm text-zinc-200">{pathLabel(info)}</span>
      </Field>
    </div>
  );
}
