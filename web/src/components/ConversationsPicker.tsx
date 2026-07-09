import { useEffect, useRef, useState } from "react";
import type { ConversationSummary } from "../state";

// Conversations picker in the top bar. Opening it requests a fresh list
// (`request_conversations`, answered off-band with `conversations_list`);
// clicking a row loads it via `/load <id>`.
//
// The user never types the machine-shaped conversation id — rows show a
// short ordinal + the title, and the click carries the id ("humans hold
// human-sized things"). Loading replaces this session's conversation, so a
// non-empty transcript is confirmed first (same guard as model switching).
export function ConversationsPicker({
  conversations,
  hasTranscript,
  onOpen,
  onLoad,
  onKill,
}: {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  onOpen: () => void;
  onLoad: (id: string) => void;
  onKill: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  // The list arrives async after onOpen(); show a loading hint until it does
  // instead of prematurely flashing "No saved conversations."
  const [loading, setLoading] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  // A fresh `conversations` reference means the requested list landed.
  useEffect(() => {
    setLoading(false);
  }, [conversations]);

  const toggle = () => {
    setOpen((o) => {
      if (!o) {
        setLoading(true);
        onOpen(); // refresh the list each time it opens
      }
      return !o;
    });
  };

  const load = (c: ConversationSummary) => {
    setOpen(false);
    if (
      hasTranscript &&
      !window.confirm(
        `Load "${c.name}"? This replaces the current transcript.`,
      )
    ) {
      return;
    }
    onLoad(c.id);
  };

  const kill = (c: ConversationSummary) => {
    if (
      window.confirm(
        `Kill the live session for "${c.name}"? Anyone connected to it will be disconnected.`,
      )
    ) {
      onKill(c.id);
    }
  };

  return (
    <div className="relative" ref={ref}>
      <button
        onClick={toggle}
        className="flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
        title="Load a saved conversation"
      >
        <span className="text-zinc-500">☰</span>
        conversations
        <span className="text-zinc-600">▾</span>
      </button>

      {open && (
        <div className="absolute left-0 top-full z-20 mt-1 max-h-96 w-80 overflow-y-auto rounded-md border border-zinc-800 bg-zinc-900 py-1 shadow-xl">
          {loading ? (
            <div className="flex items-center gap-2 px-3 py-2 text-xs text-zinc-500">
              <span className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-zinc-600 border-t-zinc-300" />
              Loading conversations…
            </div>
          ) : conversations.length === 0 ? (
            <div className="px-3 py-2 text-xs text-zinc-500">
              No saved conversations.
            </div>
          ) : (
            conversations.map((c, i) => (
              <div
                key={c.id}
                className={`group flex items-center gap-1 px-1 ${
                  c.active
                    ? "bg-emerald-950/40 hover:bg-emerald-900/40"
                    : "hover:bg-zinc-800"
                }`}
              >
                <button
                  onClick={() => load(c)}
                  className="flex min-w-0 flex-1 items-baseline gap-2 px-2 py-1.5 text-left text-xs text-zinc-200"
                >
                  <span className="w-4 shrink-0 text-right tabular-nums text-zinc-600">
                    {i + 1}
                  </span>
                  <span className="flex min-w-0 flex-col">
                    <span className="flex items-center gap-1.5">
                      {c.active && (
                        <span
                          className="inline-block h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-400"
                          title="Live session"
                        />
                      )}
                      <span
                        className={`truncate ${
                          c.active ? "font-semibold text-emerald-200" : "font-medium"
                        }`}
                      >
                        {c.name}
                      </span>
                    </span>
                    <span className="text-[10px] text-zinc-500">
                      {c.message_count} msgs · {c.model}
                      {c.active && " · live"}
                    </span>
                  </span>
                </button>
                {c.active && (
                  <button
                    onClick={() => kill(c)}
                    title="Kill this live session"
                    className="shrink-0 rounded px-1.5 py-1 text-xs text-zinc-500 opacity-0 hover:bg-red-900/50 hover:text-red-300 group-hover:opacity-100"
                  >
                    ✕
                  </button>
                )}
              </div>
            ))
          )}
        </div>
      )}
    </div>
  );
}
