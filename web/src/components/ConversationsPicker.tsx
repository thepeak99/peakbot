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
//
// A sticky search input at the top of the dropdown filters the list
// client-side (name/model substring, id prefix). Keyboard: ↑/↓ navigate,
// Enter loads, Esc clears query then closes.

export interface FilteredConversation {
  conversation: ConversationSummary;
  originalIndex: number;
}

/**
 * Filter conversations by a search query.
 * - Case-insensitive substring match on `name` and `model`.
 * - Prefix match on `id` (so pasting a UUID prefix works).
 * - Empty query returns all conversations with their original index.
 * - Returns entries carrying the original (pre-filter) index so ordinals
 *   stay stable regardless of the active filter.
 */
export function filterConversations(
  list: ConversationSummary[],
  query: string,
): FilteredConversation[] {
  if (!query) {
    return list.map((c, i) => ({ conversation: c, originalIndex: i }));
  }
  const q = query.toLowerCase();
  return list
    .map((c, i) => ({ conversation: c, originalIndex: i }))
    .filter(
      ({ conversation: c }) =>
        c.name.toLowerCase().includes(q) ||
        c.model.toLowerCase().includes(q) ||
        c.id.toLowerCase().startsWith(q),
    );
}

export function ConversationsPicker({
  conversations,
  hasTranscript,
  onOpen,
  onLoad,
  onKill,
  // Open the dropdown upward instead of downward. Set when the chip lives in
  // the mobile bottom bar, where a downward menu would clip off-screen.
  dropUp = false,
}: {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  onOpen: () => void;
  onLoad: (id: string) => void;
  onKill: (id: string) => void;
  dropUp?: boolean;
}) {
  const [open, setOpen] = useState(false);
  // The list arrives async after onOpen(); show a loading hint until it does
  // instead of prematurely flashing "No saved conversations."
  const [loading, setLoading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [highlightedIndex, setHighlightedIndex] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
        setSearchQuery("");
        setHighlightedIndex(0);
        inputRef.current?.blur();
      }
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  // A fresh `conversations` reference means the requested list landed.
  useEffect(() => {
    setLoading(false);
  }, [conversations]);

  // Auto-focus the search input when the dropdown opens.
  useEffect(() => {
    if (open && inputRef.current) {
      inputRef.current.focus();
    }
  }, [open]);

  const toggle = () => {
    setOpen((o) => {
      if (!o) {
        setLoading(true);
        setSearchQuery("");
        setHighlightedIndex(0);
        onOpen(); // refresh the list each time it opens
      } else {
        setSearchQuery("");
        setHighlightedIndex(0);
        inputRef.current?.blur();
      }
      return !o;
    });
  };

  const load = (c: ConversationSummary) => {
    setOpen(false);
    setSearchQuery("");
    setHighlightedIndex(0);
    inputRef.current?.blur();
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

  // A fresh browser tab with no `?convo=` starts a brand-new conversation
  // (the server mints the session). Same-origin, so the auth cookie carries.
  const openNewTab = () => {
    window.open(window.location.pathname, "_blank", "noopener");
  };

  const filtered = filterConversations(conversations, searchQuery);

  // Keyboard navigation over the filtered list (pattern copied from the
  // Composer slash palette).
  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlightedIndex((i) => Math.min(i + 1, filtered.length - 1));
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlightedIndex((i) => Math.max(i - 1, 0));
      return;
    }
    if (e.key === "Enter" && filtered.length > 0) {
      e.preventDefault();
      load(filtered[highlightedIndex].conversation);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (searchQuery) {
        setSearchQuery("");
        setHighlightedIndex(0);
      } else {
        setOpen(false);
        setHighlightedIndex(0);
        inputRef.current?.blur();
      }
      return;
    }
  };

  const onSearchChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setSearchQuery(e.target.value);
    setHighlightedIndex(0);
  };

  return (
    <div className="relative flex items-center gap-1.5" ref={ref}>
      <button
        onClick={toggle}
        className="flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 hover:bg-zinc-800"
        title="Load a saved conversation"
      >
        <span className="text-zinc-500">☰</span>
        conversations
        <span className="text-zinc-600">▾</span>
      </button>

      <button
        onClick={openNewTab}
        title="Open a new conversation in a new tab"
        aria-label="Open a new conversation in a new tab"
        className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-md border border-zinc-800 bg-zinc-900 text-sm text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
      >
        +
      </button>

      {open && (
        <div
          className={`absolute left-0 z-20 max-h-96 w-80 overflow-y-auto rounded-md border border-zinc-800 bg-zinc-900 shadow-xl max-sm:fixed max-sm:inset-x-2 max-sm:mt-0 max-sm:mb-0 max-sm:w-auto ${
            dropUp
              ? "bottom-full mb-1 max-sm:bottom-[3.75rem] max-sm:top-auto"
              : "top-full mt-1 max-sm:top-[3.75rem]"
          }`}
        >
          <input
            ref={inputRef}
            type="text"
            autoFocus
            value={searchQuery}
            onChange={onSearchChange}
            onKeyDown={onKeyDown}
            placeholder="Search conversations…"
            className="sticky top-0 w-full border-b border-zinc-800 bg-zinc-900 px-3 py-2 text-xs text-zinc-200 placeholder-zinc-500 outline-none"
          />

          {loading ? (
            <div className="flex items-center gap-2 px-3 py-2 text-xs text-zinc-500">
              <span className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-zinc-600 border-t-zinc-300" />
              Loading conversations…
            </div>
          ) : conversations.length === 0 ? (
            <div className="px-3 py-2 text-xs text-zinc-500">
              No saved conversations.
            </div>
          ) : filtered.length === 0 ? (
            <div className="px-3 py-2 text-xs text-zinc-500">
              No conversations match "{searchQuery}".
            </div>
          ) : (
            filtered.map(({ conversation: c, originalIndex }, fi) => (
              <div
                key={c.id}
                className={`group flex items-center gap-1 px-1 ${
                  fi === highlightedIndex
                    ? "bg-zinc-800"
                    : c.active
                      ? "bg-emerald-950/40 hover:bg-emerald-900/40"
                      : "hover:bg-zinc-800"
                }`}
              >
                <button
                  onClick={() => load(c)}
                  className="flex min-w-0 flex-1 items-baseline gap-2 px-2 py-1.5 text-left text-xs text-zinc-200"
                >
                  <span className="w-4 shrink-0 text-right tabular-nums text-zinc-600">
                    {originalIndex + 1}
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
                          c.active
                            ? "font-semibold text-emerald-200"
                            : "font-medium"
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
