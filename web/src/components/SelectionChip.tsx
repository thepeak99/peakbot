// T32 (D11 chat, spec §9.1): the selection chip + two-column popover.
//
// The chip shows the (profile, agent) this conversation is bound to. Clicking
// it opens the popover: the LEFT column lists profiles (📁 on repo origin,
// `default` caption on row 0, `pinned` when the session is --profile pinned)
// and clicking a profile row BROWSES ONLY — it switches the right column,
// selects nothing, sends nothing. The RIGHT column lists the browsed
// profile's agents; clicking an agent row is the ONLY way the chip talks to
// the server: `send(select(profile, agent))`, then close + toast
// `Next conversation: <p> › <a>`.
//
// Lock (§6.5): once the conversation has a transcript (`locked`) the chip is
// disabled, prefixed 🔒, drops the ▾, and never opens — the selection is
// frozen for the life of the conversation.
//
// Esc order (modal → popover → Settings): Esc while open closes the popover
// AND preventDefaults, so the Settings view's Esc handler (which skips
// defaultPrevented events) stays open. Esc while closed is not swallowed.
// Mousedown outside closes.

import { useEffect, useRef, useState } from "react";
import { select } from "../state";
import type { CatalogAgent } from "../types";
import type { SelectionChipProps } from "../settings/contract";

const ROW_CLS =
  "flex w-full cursor-pointer flex-col items-start rounded px-2 py-1 text-left text-xs hover:bg-zinc-800";

function agentCaption(a: CatalogAgent, index: number): string {
  const base = a.subagents.length > 0 ? `${a.subagents.length} subagents` : "single";
  return index === 0 ? `${base} · default` : base;
}

export function SelectionChip({
  catalog,
  selection,
  locked,
  pinned,
  send,
  onToast,
  dropUp = false,
}: SelectionChipProps) {
  const [open, setOpen] = useState(false);
  // The profile whose agents the right column shows. Reset to the bound
  // profile every time the popover opens, so a half-explored browse never
  // leaks into the next open.
  const [browsed, setBrowsed] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // Locking while the popover is open closes it (read-only state).
  useEffect(() => {
    if (locked && open) setOpen(false);
  }, [locked, open]);

  // Esc (close + preventDefault) and mousedown-outside, only while open.
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
      }
    }
    function onDown(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    }
    window.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [open]);

  // Nothing to show: no selection (old server / before first frame) or an
  // empty catalog.
  if (selection === null || catalog.length === 0) return null;

  // Pinned: the profile is frozen (--profile) — only that row renders,
  // captioned `pinned`. Agent switching under a pin still works.
  const pinnedProfile = pinned ? catalog.find((p) => p.name === selection.profile) : undefined;
  const profileList = pinnedProfile ? [pinnedProfile] : catalog;
  const browsedName = browsed ?? selection.profile;
  const browsedProfile = profileList.find((p) => p.name === browsedName) ?? profileList[0];
  const selectedProfile = catalog.find((p) => p.name === selection.profile);

  const toggle = () => {
    if (locked) return;
    if (!open) setBrowsed(selection.profile);
    setOpen(!open);
  };

  const pickAgent = (agent: string) => {
    send(select(browsedProfile.name, agent));
    onToast?.(`Next conversation: ${browsedProfile.name} › ${agent}`);
    setOpen(false);
  };

  return (
    <div className="relative" ref={rootRef}>
      <button
        data-testid="selection-chip"
        type="button"
        disabled={locked}
        title={locked ? "Locked after the first message" : "Switch profile / agent"}
        onClick={toggle}
        className="flex items-center gap-1 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs text-zinc-300 transition-colors hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-60"
      >
        {locked && <span aria-hidden>🔒</span>}
        {selectedProfile?.origin === "repo" && <span aria-hidden>📁</span>}
        <span className="max-w-44 truncate">
          {selection.profile} <span className="text-zinc-600">›</span> {selection.agent}
        </span>
        {!locked && <span className="text-zinc-600">▾</span>}
      </button>

      {open && !locked && (
        <div
          data-testid="selection-popover"
          className={`absolute left-0 z-20 flex w-[26rem] overflow-hidden rounded-md border border-zinc-800 bg-zinc-900 shadow-xl ${
            dropUp ? "bottom-full mb-1" : "top-full mt-1"
          }`}
        >
          <div className="w-44 shrink-0 border-r border-zinc-800 py-1">
            {profileList.map((p, i) => (
              <button
                key={p.name}
                data-testid={`selection-profile-${p.name}`}
                data-browsed={p.name === browsedProfile.name ? "true" : undefined}
                type="button"
                onClick={() => setBrowsed(p.name)}
                className={ROW_CLS}
              >
                <span className={p.name === selection.profile ? "font-medium text-emerald-400" : "text-zinc-200"}>
                  {p.origin === "repo" && <span aria-hidden>📁 </span>}
                  {p.name}
                </span>
                <span className="text-[10px] text-zinc-500">
                  {pinned ? "pinned" : i === 0 ? "default" : ""}
                </span>
              </button>
            ))}
          </div>
          <div className="min-w-0 flex-1 py-1">
            <div data-testid="selection-agents-title" className="px-2 py-1 text-[10px] uppercase tracking-wide text-zinc-500">
              Agents · {browsedProfile.name}
            </div>
            {browsedProfile.agents.map((a, i) => (
              <button
                key={a.name}
                data-testid={`selection-agent-${a.name}`}
                type="button"
                onClick={() => pickAgent(a.name)}
                className={ROW_CLS}
              >
                <span
                  className={
                    a.name === selection.agent && browsedProfile.name === selection.profile
                      ? "font-medium text-emerald-400"
                      : "text-zinc-200"
                  }
                >
                  {a.name}
                </span>
                <span className="text-[10px] text-zinc-500">{agentCaption(a, i)}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
