// The 422 error panel (spec §9.6 "Errors"). Top panel: "Save failed —
// N problem(s)", then one item per error: `<code>path</code>: message
// [Fix]`. Paths use the dot rendering; an error in the repo file while in
// global scope shows "(in the repo config)" and its Fix switches scope.

import type { ConfigErr, Scope } from "./types";
import { dotPath } from "./paths";

export interface ErrorPanelProps {
  errors: ConfigErr[];
  /** The scope the user is looking at — decides the "(in the repo config)" note. */
  scope: Scope;
  /** Fix: the shell routes on the segment array (paths.ts) and, for
      repo-file errors in global scope, switches scope. */
  onFix(error: ConfigErr): void;
}

export function ErrorPanel({ errors, scope, onFix }: ErrorPanelProps) {
  if (errors.length === 0) return null;
  const n = errors.length;
  return (
    <div
      data-testid="error-panel"
      className="border-b border-red-900/60 bg-red-950/40 px-4 py-2"
    >
      <div data-testid="error-panel-title" className="text-xs font-medium text-red-300">
        ✕ Save failed — {n} problem{n === 1 ? "" : "s"}
      </div>
      <ul className="mt-1 space-y-1">
        {errors.map((e, i) => (
          <li key={`${dotPath(e.path)}-${i}`} className="flex flex-wrap items-baseline gap-x-2 text-xs text-red-200">
            <code className="rounded bg-red-950/70 px-1.5 py-0.5 font-mono text-[11px] text-red-300">
              {dotPath(e.path)}
            </code>
            {e.file === "repo" && scope === "global" && (
              <span className="text-red-400/80">(in the repo config)</span>
            )}
            <span>{e.message}</span>
            <button
              data-testid="error-fix"
              onClick={() => onFix(e)}
              className="cursor-pointer text-sky-400 underline-offset-2 hover:underline"
            >
              Fix
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
