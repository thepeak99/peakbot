import { useState } from "react";
import type { FileEdit } from "../types";
import { ACTION_PILL, PANEL_HEAD, PANEL_SUB, PANEL_TITLE, ROW } from "./phoneUi";

// Files the agent touched this session (#126), derived from file tool calls in
// the transcript (see filesFromMessages). Read-only list — reverting a change needs
// backend undo support that doesn't exist yet, so no `[x]`. A "Copy git add"
// affordance copies the changed (created/modified) paths for staging.

const KIND_META: Record<
  FileEdit["kind"],
  { glyph: string; color: string; label: string }
> = {
  created: { glyph: "＋", color: "text-emerald-400", label: "created" },
  modified: { glyph: "✎", color: "text-sky-400", label: "modified" },
  read: { glyph: "👁", color: "text-zinc-500", label: "read" },
};

export function FilesPanel({ files, phone = false }: { files: FileEdit[]; phone?: boolean }) {
  const [copied, setCopied] = useState(false);
  // Only changed files are stage-worthy; a read leaves nothing to `git add`.
  const changed = files.filter((f) => f.kind !== "read").map((f) => f.path);

  const copyGitAdd = async () => {
    if (changed.length === 0) return;
    try {
      await navigator.clipboard.writeText(changed.join("\n"));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard blocked (insecure context / permission) — silently ignore.
    }
  };

  if (phone) {
    // Mock .frow: flat left-to-right paths; the ± counts are parked (no data).
    return (
      <section>
        <div className={PANEL_HEAD}>
          <h3 className={PANEL_TITLE}>Files</h3>
          <span className={PANEL_SUB}>{files.length} touched</span>
        </div>
        {files.length === 0 ? (
          <p className="py-8 text-center text-sm text-zinc-500">No files touched yet.</p>
        ) : (
          <>
            <ul>
              {files.map((f) => (
                <li key={f.path} className={ROW} title={KIND_META[f.kind].label}>
                  <span className="min-w-0 flex-1 truncate text-[13.5px] text-zinc-300">{f.path}</span>
                  {f.edits > 1 && (
                    <span className="shrink-0 font-mono text-[12.5px] font-semibold text-zinc-500">
                      ×{f.edits}
                    </span>
                  )}
                </li>
              ))}
            </ul>
            {changed.length > 0 && (
              <button
                type="button"
                onClick={copyGitAdd}
                className={`mt-3 ${ACTION_PILL} bg-sky-400/14 text-sky-400`}
              >
                {copied ? "Copied ✓" : `Copy ${changed.length} for git add`}
              </button>
            )}
          </>
        )}
      </section>
    );
  }

  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Files
        </h3>
        <span className="font-mono text-[11px] tabular-nums text-zinc-600">
          {files.length}
        </span>
      </div>

      {files.length === 0 ? (
        <p className="text-xs text-zinc-600">No files touched yet.</p>
      ) : (
        <>
          <ul className="space-y-1.5 text-xs">
            {files.map((f) => {
              const meta = KIND_META[f.kind];
              return (
                <li key={f.path} className="flex items-start gap-2">
                  <span className={`mt-px ${meta.color}`} title={meta.label}>
                    {meta.glyph}
                  </span>
                  <span
                    className="min-w-0 flex-1 truncate font-mono text-zinc-300"
                    title={f.path}
                    dir="rtl"
                  >
                    {f.path}
                  </span>
                  {f.edits > 1 && (
                    <span className="font-mono text-[10px] tabular-nums text-zinc-600">
                      ×{f.edits}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>

          {changed.length > 0 && (
            <button
              type="button"
              onClick={copyGitAdd}
              title="Copy changed paths, newline-joined, ready to paste after `git add`"
              className="mt-3 w-full cursor-pointer rounded border border-zinc-800 bg-zinc-900 px-2 py-1 text-[11px] text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
            >
              {copied ? "Copied ✓" : `Copy ${changed.length} for git add`}
            </button>
          )}
        </>
      )}
    </section>
  );
}
