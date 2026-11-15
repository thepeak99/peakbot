import { useEffect, useRef, useState } from "react";
import type { DirListing, InboundMessage } from "../state";
import { basename } from "../paths";
import { DirectoryBrowser } from "./DirectoryBrowser";

// The working-directory chip + directory browser in the top bar. The chip
// shows the current cwd (basename prominent, full path on hover); clicking
// it opens a folder picker.
//
// Committing a new cwd calls `onSwitchCwd` — the App routes it (busy → fresh
// conversation, idle → `switch_cwd` on the socket) — so a non-empty
// transcript is confirmed first.
//
// The picker is purely state-frame-driven: the displayed cwd comes from the
// AppState `welcome.cwd`, and browsing rides the `list_dir` → `dir_listing`
// request/response (never local optimistic path state).

export function CwdPicker({
  cwd,
  hasTranscript,
  dirListing,
  recentDirs,
  send,
  onSwitchCwd,
  dropUp = false,
}: {
  cwd: string;
  hasTranscript: boolean;
  dirListing: DirListing | null;
  recentDirs: string[];
  send: (msg: InboundMessage) => void;
  onSwitchCwd: (path: string) => void;
  dropUp?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  return (
    <div className="relative" ref={ref}>
      <button
        data-testid="cwd-picker"
        onClick={() => setOpen((o) => !o)}
        className="flex min-w-0 max-w-[40vw] items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
        title={cwd}
      >
        <span className="shrink-0 text-zinc-500">📁</span>
        <span className="truncate font-medium">{basename(cwd)}</span>
        <span className="shrink-0 text-zinc-600">▾</span>
      </button>

      {open && (
        <div
          className={`absolute left-0 z-20 flex max-h-[calc(100dvh-8rem)] flex-col w-80 rounded-md border border-zinc-800 bg-zinc-900 shadow-xl max-sm:fixed max-sm:inset-x-2 max-sm:mt-0 max-sm:mb-0 max-sm:w-auto ${
            dropUp
              ? "bottom-full mb-1 max-sm:bottom-[3.75rem] max-sm:top-auto"
              : "top-full mt-1 max-sm:top-[3.75rem]"
          }`}
        >
          <DirectoryBrowser
            cwd={cwd}
            hasTranscript={hasTranscript}
            dirListing={dirListing}
            recentDirs={recentDirs}
            send={send}
            onSwitchCwd={onSwitchCwd}
            onDone={() => setOpen(false)}
          />
        </div>
      )}
    </div>
  );
}
