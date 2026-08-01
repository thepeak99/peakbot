// Step 1 — Welcome. Live machine facts from GET /api/setup. Import is
// driven on mount in <Setup/> from the fetched `existing` block; this
// component just surfaces what was found and offers a Continue/Start
// action.

import type { StepProps } from "../steps";

export function WelcomeStep({ draft, info, next }: StepProps) {
  const imported = info?.existing.status === "ok";
  const parseError = info?.existing.status === "error" ? info.existing.message : null;
  const mode = draft.welcome.startMode;
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <img src="/favicon.svg" alt="" className="h-10 w-10" />
        <p className="text-sm text-zinc-400">
          Point your agent at a provider, name a few models, and it is ready to
          work. Ten steps; six of them are optional.
        </p>
      </div>

      {info ? (
        <dl className="grid grid-cols-2 gap-x-6 gap-y-1 rounded-lg border border-zinc-800 p-3 text-xs sm:grid-cols-3">
          {[
            ["OS", info.os],
            ["Arch", info.arch],
            ["Binary", info.exe_path ?? "—"],
            ["Config", info.config_path],
            ["Data dir", info.data_dir ?? "—"],
            ["Cache dir", info.cache_dir ?? "—"],
          ].map(([label, value]) => (
            <div key={label}>
              <dt className="text-zinc-500">{label}</dt>
              <dd className="truncate text-zinc-300" title={value}>{value}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-xs text-zinc-500">Loading machine facts…</p>
      )}

      {imported && mode === "import" && (
        <p className="rounded-md border border-emerald-800/60 bg-emerald-950/30 px-3 py-2 text-xs text-emerald-300">
          Imported existing config. Review the Provider and Models steps and continue.
        </p>
      )}
      {parseError && (
        <p className="rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-xs text-red-300">
          Could not parse existing config: {parseError}. Starting with a blank draft.
        </p>
      )}

      <button
        type="button"
        onClick={next}
        className="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition-colors hover:bg-zinc-700"
      >
        {imported ? "Continue" : "Start fresh"}
      </button>
    </div>
  );
}
