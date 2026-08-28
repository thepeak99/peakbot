import { useEffect, useRef, useState } from "react";
import type { DirListing, InboundMessage } from "../state";

// The working-directory chip + directory browser in the top bar, modelled on
// ModelSwitcher. The chip shows the current cwd (basename prominent, full path
// on hover); clicking it opens a folder picker.
//
// Committing a new cwd sends `switch_cwd`, which the backend resolves and
// validates, then resets the session (same seam as `/cd` / `/model`). So a
// non-empty transcript is confirmed first, exactly like ModelSwitcher.
//
// The picker is purely state-frame-driven: the displayed cwd comes from the
// AppState `welcome.cwd`, and browsing rides the `list_dir` → `dir_listing`
// request/response (never local optimistic path state).

/** Middle-ellipsised display of a path for the chip / breadcrumb. The browser
 * has no `$HOME`, so we show the absolute path the backend resolved. */
function displayPath(path: string): string {
  if (path.length <= 40) return path;
  const head = path.slice(0, 16);
  const tail = path.slice(-20);
  return `${head}…${tail}`;
}

function basename(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || "/";
}

export function CwdPicker({
  cwd,
  hasTranscript,
  dirListing,
  recentDirs,
  send,
  // Anchor the small-screen picker panel to the bottom of the viewport
  // (instead of the top) when the chip lives in the mobile bottom bar.
  dropUp = false,
}: {
  cwd: string;
  hasTranscript: boolean;
  dirListing: DirListing | null;
  /** Most-recently-used directories (newest-first, cwd excluded) shown as a
   * "Recent" quick-jump section at the top of the picker. */
  recentDirs: string[];
  send: (msg: InboundMessage) => void;
  dropUp?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  // The path currently being browsed in the modal (independent of the
  // committed cwd until "Use this directory"). Seeded from the backend's
  // resolved `dir_listing.path` so the UI never canonicalises itself.
  const [browsed, setBrowsed] = useState<string | null>(null);

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  // On open, browse the current cwd and fetch the recent list. Reset the
  // browsed anchor each time.
  const openPicker = () => {
    setOpen((o) => {
      const next = !o;
      if (next) {
        setBrowsed(null);
        send({ type: "list_dir", path: cwd });
        send({ type: "request_recent_dirs" });
      }
      return next;
    });
  };

  // Track the backend's resolved path as our browse anchor.
  useEffect(() => {
    if (open && dirListing && !dirListing.error) {
      setBrowsed(dirListing.path);
    }
  }, [open, dirListing]);

  const navigate = (path: string) => send({ type: "list_dir", path });

  const commit = (path: string) => {
    setOpen(false);
    if (path === cwd) return; // no-op (mirrors ModelSwitcher's alias check)
    if (
      hasTranscript &&
      !window.confirm(
        `Use "${path}" as the working directory? This starts a new conversation — the current transcript is cleared.`,
      )
    ) {
      return;
    }
    send({ type: "switch_cwd", path });
  };

  const dirs = dirListing?.entries.filter((e) => e.is_dir) ?? [];
  const files = dirListing?.entries.filter((e) => !e.is_dir) ?? [];

  return (
    <div className="relative" ref={ref}>
      <button
        onClick={openPicker}
        className="flex min-w-0 max-w-[40vw] items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
        title={cwd}
      >
        <span className="shrink-0 text-zinc-500">📁</span>
        <span className="truncate font-medium">{basename(cwd)}</span>
        <span className="shrink-0 text-zinc-600">▾</span>
      </button>

      {open && (
        // Direction is chosen by `dropUp` (down by default, up when the chip
        // lives in the bottom bar); the `max-sm:` prefix only widens the panel
        // and anchors it on phones. Placing it in the base class would silently
        // no-op `dropUp` on tablets, where the host swaps but the breakpoint
        // prefix does not.
        // max-h = viewport minus 8rem (3.75rem bottom bar + breathing room) so
        // the commit bar stays visible on short windows.
        <div
          className={`absolute left-0 z-20 flex max-h-[calc(100dvh-8rem)] flex-col w-80 rounded-md border border-zinc-800 bg-zinc-900 shadow-xl max-sm:fixed max-sm:inset-x-2 max-sm:mt-0 max-sm:mb-0 max-sm:w-auto ${
            dropUp
              ? "bottom-full mb-1 max-sm:bottom-[3.75rem] max-sm:top-auto"
              : "top-full mt-1 max-sm:top-[3.75rem]"
          }`}
        >
          {/* One scroll container for the recent list, breadcrumb, and browse
              list so they scroll together; the commit bar below stays pinned. */}
          <div className="min-h-0 flex-1 overflow-y-auto">
            {/* Recent — quick-jump to the most recently used directories.
                Rows reuse `commit`, so the confirm guard applies exactly as
                for the browse list. Hidden entirely when empty. */}
            {recentDirs.length > 0 && (
              <div className="border-b border-zinc-800">
                <div className="px-3 pb-1 pt-2 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
                  Recent
                </div>
                {recentDirs.map((path) => (
                  <button
                    key={path}
                    onClick={() => commit(path)}
                    className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-zinc-200 hover:bg-zinc-800"
                    title={path}
                  >
                    <span className="shrink-0 text-zinc-500">📁</span>
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-medium">
                        {basename(path)}
                      </span>
                      <span className="block truncate font-mono text-[10px] text-zinc-500">
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
                onClick={() =>
                  dirListing?.parent && navigate(dirListing.parent)
                }
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

            {/* Error (folded into the dir_listing frame) renders inline. */}
            {dirListing?.error && (
              <div className="px-3 py-2 text-xs text-red-300">
                {dirListing.error}
              </div>
            )}

            {/* Directory list. */}
            <div className="py-1">
              {dirs.map((e) => (
                <button
                  key={e.name}
                  onClick={() => navigate(`${browsed ?? cwd}/${e.name}`)}
                  className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-zinc-200 hover:bg-zinc-800"
                >
                  <span className="text-zinc-500">📁</span>
                  <span className="truncate">{e.name}</span>
                </button>
              ))}
              {files.map((e) => (
                <div
                  key={e.name}
                  className="flex items-center gap-2 px-3 py-1.5 text-xs text-zinc-600"
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

          {/* Commit. */}
          <div className="shrink-0 border-t border-zinc-800 px-3 py-2">
            <button
              onClick={() => commit(browsed ?? cwd)}
              className="w-full rounded bg-emerald-700 px-2 py-1 text-xs font-medium text-emerald-50 hover:bg-emerald-600"
            >
              Use this directory
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
