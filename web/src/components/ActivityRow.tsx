// Live "working" row for focus mode (plan §6.4): one static bar (NOT inside
// the virtualized list) showing the newest hidden step; each new step
// replaces the content, never stacks. Pure display — no state, no timers
// (the dots are CSS-only).

import type { WireChatMessage, WireRole } from "../state";

// Display glyphs only — mirrors ROLE_META in Message.tsx, which is keyed by
// the adapted view roles, so importing it would cross the wire/view seam.
const GLYPH: Record<WireRole, string> = {
  user: "▷",
  agent: "✦",
  toolcall: "🔧",
  toolresult: "↳",
  system: "•",
  summary: "≡",
};

// First non-empty, trimmed line of the server-preformatted content — the
// thought line for tool calls, so the row reads like the step itself.
function firstLine(content: string | undefined): string {
  if (!content) return "";
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (trimmed !== "") return trimmed;
  }
  return "";
}

export function ActivityRow({ step }: { step: WireChatMessage | null }) {
  const label = step ? firstLine(step.content) || step.tool_name || step.role : "Thinking";
  const subRole = step?.source?.kind === "sub_agent" ? step.source.role : undefined;

  return (
    <div className="mr-12 px-4 pb-2 sm:px-6 md:px-8">
      <div
        className="mx-auto flex max-w-5xl items-center gap-2 rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-1.5 text-xs text-zinc-400"
        role="status"
        aria-live="polite"
      >
        <span aria-hidden>{step ? GLYPH[step.role] : "✦"}</span>
        {subRole && <span className="shrink-0">🧩 {subRole} ·</span>}
        <span className="truncate" title={label}>
          {label}
        </span>
        <span aria-hidden className="flex shrink-0 items-center">
          {[0, 200, 400].map((ms) => (
            <span key={ms} className="animate-pulse" style={{ animationDelay: `${ms}ms` }}>
              ·
            </span>
          ))}
        </span>
      </div>
    </div>
  );
}
