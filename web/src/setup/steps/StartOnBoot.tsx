// Step 8 — Start on boot. Live service control via /api/setup/service.
// The wizard never invents a sentence the backend didn't send: manager,
// artifact, run_state, survives_logout, commands[] and notes[] render
// straight from the JSON.

import { useEffect, useState } from "react";
import { apiErrorMessage, getService, installService, uninstallService, type ServiceReport } from "../api";
import type { StepProps } from "../steps";
import { buttonClass, ghostButtonClass } from "../ui";

type ViewState =
  | { kind: "loading" }
  | { kind: "ready"; report: ServiceReport; error?: string[] }
  | { kind: "error"; lines: string[] };

export function StartOnBootStep({ draft }: StepProps) {
  const [state, setState] = useState<ViewState>({ kind: "loading" });
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    setState({ kind: "loading" });
    try {
      const report = await getService();
      setState({ kind: "ready", report });
    } catch (err) {
      setState({ kind: "error", lines: apiErrorMessage(err) });
    }
  };

  useEffect(() => { void refresh(); }, []);

  const enable = async () => {
    setBusy(true);
    try {
      const bind = draft.access.mode === "lan" ? draft.access.bindAddress ?? "0.0.0.0:7823" : "127.0.0.1:7823";
      const token = draft.access.mode === "lan" ? draft.access.token : undefined;
      const report = await installService({ bind, token });
      setState({ kind: "ready", report });
    } catch (err) {
      const lines = apiErrorMessage(err);
      setState((prev) => prev.kind === "ready" ? { kind: "ready", report: prev.report, error: lines } : { kind: "error", lines });
    } finally {
      setBusy(false);
    }
  };

  const disable = async () => {
    setBusy(true);
    try {
      const report = await uninstallService();
      setState({ kind: "ready", report });
    } catch (err) {
      const lines = apiErrorMessage(err);
      setState((prev) => prev.kind === "ready" ? { kind: "ready", report: prev.report, error: lines } : { kind: "error", lines });
    } finally {
      setBusy(false);
    }
  };

  if (state.kind === "loading") return <p className="text-xs text-zinc-500">Loading service status…</p>;
  if (state.kind === "error") {
    return (
      <div className="space-y-2">
        <ul className="space-y-0.5 text-xs text-red-300">{state.lines.map((l) => <li key={l}>{l}</li>)}</ul>
        <button type="button" onClick={refresh} className={ghostButtonClass}>Retry</button>
      </div>
    );
  }
  const { report, error } = state;
  return (
    <div className="space-y-4">
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="rounded-lg border border-zinc-800 p-3 text-xs">
          <h3 className="text-sm font-medium text-zinc-200">Status</h3>
          <dl className="mt-2 space-y-1 text-zinc-300">
            <div><dt className="inline text-zinc-500">Manager: </dt><dd className="inline">{report.manager}</dd></div>
            <div><dt className="inline text-zinc-500">Installed: </dt><dd className="inline">{String(report.installed)}</dd></div>
            <div><dt className="inline text-zinc-500">Run state: </dt><dd className="inline">{report.run_state}</dd></div>
            <div><dt className="inline text-zinc-500">Survives logout: </dt><dd className="inline">{String(report.survives_logout)}</dd></div>
            <div><dt className="inline text-zinc-500">Name: </dt><dd className="inline">{report.name}</dd></div>
            {report.exe && <div className="truncate"><dt className="inline text-zinc-500">Exe: </dt><dd className="inline">{report.exe}</dd></div>}
            {report.artifact && <div className="truncate"><dt className="inline text-zinc-500">Artifact: </dt><dd className="inline">{report.artifact}</dd></div>}
          </dl>
        </div>
        <div className="rounded-lg border border-zinc-800 p-3 text-xs">
          <h3 className="text-sm font-medium text-zinc-200">Notes</h3>
          {report.notes.length === 0 ? <p className="mt-2 text-zinc-500">None.</p> : (
            <ul className="mt-2 list-disc space-y-0.5 pl-5 text-zinc-300">{report.notes.map((n) => <li key={n}>{n}</li>)}</ul>
          )}
        </div>
      </div>

      {report.commands.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs font-medium text-zinc-400">Commands run</p>
          <pre className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-2 text-xs text-zinc-300">{report.commands.join("\n")}</pre>
        </div>
      )}

      {error && <ul className="space-y-0.5 text-xs text-red-300">{error.map((l) => <li key={l}>{l}</li>)}</ul>}

      <div className="flex flex-wrap gap-2">
        {report.installed ? (
          <button type="button" onClick={disable} disabled={busy} className={ghostButtonClass}>{busy ? "Working…" : "Disable at login"}</button>
        ) : (
          <button type="button" onClick={enable} disabled={busy} className={buttonClass}>{busy ? "Working…" : "Enable at login"}</button>
        )}
        <button type="button" onClick={refresh} disabled={busy} className={ghostButtonClass}>Refresh</button>
      </div>
    </div>
  );
}
