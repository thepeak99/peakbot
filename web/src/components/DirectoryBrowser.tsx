import { useEffect, useState } from "react";
import type { DirListing, InboundMessage } from "../state";
import { basename, displayPath } from "../paths";

// The browse/commit body shared by the desktop CwdPicker popover and the
// phone Directory tab. Returns a fragment [body, commitBar] so the host
// (popover or sheet) supplies the flex column layout.

export function DirectoryBrowser({
  cwd,
  hasTranscript,
  dirListing,
  recentDirs,
  send,
  onSwitchCwd,
  onDone,
  phone = false,
}: {
  cwd: string;
  hasTranscript: boolean;
  dirListing: DirListing | null;
  recentDirs: string[];
  send: (msg: InboundMessage) => void;
  onSwitchCwd: (path: string) => void;
  onDone: () => void;
  phone?: boolean;
}) {
  // The path currently being browsed. Seeded from the backend's resolved
  // `dir_listing.path` so the UI never canonicalises itself.
  const [browsed, setBrowsed] = useState<string | null>(null);

  // Mounted = open: request the listing and recent dirs.
  useEffect(() => {
    send({ type: "list_dir", path: cwd });
    send({ type: "request_recent_dirs" });
  }, [send, cwd]);

  // Track the backend's resolved path as the browse anchor.
  useEffect(() => {
    if (dirListing && !dirListing.error) {
      setBrowsed(dirListing.path);
    }
  }, [dirListing]);

  const navigate = (path: string) => send({ type: "list_dir", path });

  const commit = (path: string) => {
    onDone();
    if (path === cwd) return;
    if (
      hasTranscript &&
      !window.confirm(`Use "${path}"? This starts a new conversation.`)
    ) {
      return;
    }
    onSwitchCwd(path);
  };

  const dirs = dirListing?.entries.filter((e) => e.is_dir) ?? [];
  const files = dirListing?.entries.filter((e) => !e.is_dir) ?? [];

  const bodyClass = phone ? "grow" : "min-h-0 flex-1 overflow-y-auto";
  const rowClass = phone ? "min-h-11 text-[14px]" : "text-xs";
  const subPathClass = phone ? "text-[11px]" : "text-[10px]";
  const commitClass = phone
    ? "sticky bottom-0 mt-auto -mx-4 border-t border-zinc-800 bg-zinc-900 px-4 pt-3 pb-2"
    : "shrink-0 border-t border-zinc-800 px-3 py-2";
  const commitBtnClass = phone
    ? "h-11 w-full rounded-[10px] bg-emerald-700 text-[14px] font-semibold text-emerald-50 hover:bg-emerald-600"
    : "w-full rounded bg-emerald-700 px-2 py-1 text-xs font-medium text-emerald-50 hover:bg-emerald-600";

  return (
    <>
      <div className={bodyClass}>
        {/* Recent — quick-jump to the most recently used directories. */}
        {recentDirs.length > 0 && (
          <div className="border-b border-zinc-800">
            <div className="px-3 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
              Recent
            </div>
            {recentDirs.map((path) => (
              <button
                key={path}
                onClick={() => commit(path)}
                className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-zinc-200 hover:bg-zinc-800 ${rowClass}`}
                title={path}
              >
                <span className="shrink-0 text-zinc-500">📁</span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-medium">
                    {basename(path)}
                  </span>
                  <span className={`block truncate font-mono text-zinc-500 ${subPathClass}`}>
                    {displayPath(path)}
                  </span>
                </span>
              </button>
            ))}
          </div>
        )}

        {/* Current browse path + up. */}
        <div className="flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
          <button
            onClick={() => dirListing?.parent && navigate(dirListing.parent)}
            disabled={!dirListing?.parent}
            className="rounded px-1.5 py-0.5 text-xs text-zinc-300 hover:bg-zinc-800 disabled:opacity-30"
            title="Up one level"
          >
            ⬆ ..
          </button>
          <span
            className="min-w-0 flex-1 truncate font-mono text-[11px] text-zinc-400"
            title={browsed ?? cwd}
          >
            {displayPath(browsed ?? cwd)}
          </span>
        </div>

        {/* Error renders inline. */}
        {dirListing?.error && (
          <div className="px-3 py-2 text-xs text-red-300">{dirListing.error}</div>
        )}

        {/* Directory list. */}
        <div className="py-1">
          {dirs.map((e) => (
            <button
              key={e.name}
              onClick={() => navigate(e.full_path)}
              className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-zinc-200 hover:bg-zinc-800 ${rowClass}`}
            >
              <span className="text-zinc-500">📁</span>
              <span className="truncate">{e.name}</span>
            </button>
          ))}
          {files.map((e) => (
            <div
              key={e.name}
              className={`flex items-center gap-2 px-3 py-1.5 text-zinc-600 ${rowClass}`}
            >
              <span>📄</span>
              <span className="truncate">{e.name}</span>
            </div>
          ))}
          {dirListing && !dirListing.error && dirs.length === 0 && (
            <div className="px-3 py-2 text-xs text-zinc-500">
              No sub-directories.
            </div>
          )}
        </div>
      </div>

      {/* Commit bar. */}
      <div className={commitClass}>
        <button
          data-testid="use-directory"
          onClick={() => commit(browsed ?? cwd)}
          className={commitBtnClass}
        >
          Use this directory
        </button>
      </div>
    </>
  );
}