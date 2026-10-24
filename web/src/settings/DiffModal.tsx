// The diff modal (spec §9.6 "Diff modal"): a line diff of saved vs current
// text computed by the `diff` package (structuredPatch, context: 2). Hunks
// are rendered with `@@ … @@` gap markers and `--- a/<path>` /
// `+++ b/<path>` headers. No textual change → NO_TEXTUAL_CHANGE_MESSAGE.
// Esc or a backdrop click closes it.

import { useEffect, useMemo } from "react";
import { configDiff, diffIsEmpty, hunkHeader, NO_TEXTUAL_CHANGE_MESSAGE } from "./diff";

export interface DiffModalProps {
  /** The config file path shown in the header path tag and file headers. */
  path: string;
  oldText: string;
  newText: string;
  onClose(): void;
}

export function DiffModal({ path, oldText, newText, onClose }: DiffModalProps) {
  const patch = useMemo(() => configDiff(oldText, newText, path), [oldText, newText, path]);

  // Esc closes the modal FIRST (the spec's Esc order is modal → popover →
  // Settings): the handler runs in the capture phase and calls
  // preventDefault, so the Settings layer's keydown handler (which skips
  // defaultPrevented events) never sees this Esc.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  return (
    <div
      data-testid="diff-modal"
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/70 p-4 pt-[10vh]"
      onClick={onClose}
    >
      <div
        className="w-full max-w-3xl rounded-md border border-zinc-700 bg-zinc-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 border-b border-zinc-800 px-4 py-3">
          <h2 className="text-sm font-semibold text-zinc-100">Unsaved changes — diff</h2>
          <code className="rounded border border-zinc-700 bg-zinc-950 px-2 py-0.5 font-mono text-[11px] text-zinc-400">
            {path}
          </code>
          <button
            data-testid="diff-close"
            onClick={onClose}
            className="ml-auto cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
          >
            Close
          </button>
        </div>
        <div className="max-h-[60vh] overflow-auto p-4">
          {diffIsEmpty(patch) ? (
            <p data-testid="diff-empty" className="text-xs text-zinc-500">
              {NO_TEXTUAL_CHANGE_MESSAGE}
            </p>
          ) : (
            <pre data-testid="diff-body" className="overflow-x-auto font-mono text-[11px] leading-5">
              <div className="text-sky-400">--- {patch.oldFileName}</div>
              <div className="text-sky-400">+++ {patch.newFileName}</div>
              {patch.hunks.map((hunk) => (
                <div key={`${hunk.oldStart}-${hunk.newStart}`}>
                  <div className="text-sky-500">{hunkHeader(hunk)}</div>
                  {hunk.lines.map((line, i) => (
                    <div
                      key={i}
                      className={
                        line.startsWith("+")
                          ? "bg-emerald-950/40 text-emerald-300"
                          : line.startsWith("-")
                            ? "bg-red-950/40 text-red-300"
                            : "text-zinc-400"
                      }
                    >
                      {line}
                    </div>
                  ))}
                </div>
              ))}
            </pre>
          )}
        </div>
      </div>
    </div>
  );
}
