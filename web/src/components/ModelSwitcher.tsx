import { useEffect, useRef, useState } from "react";
import type { ModelInfo } from "../state";

// Model dropdown in the top bar. Lists the registry (from the on-connect
// `models_available` frame) and sends `switch_model` on select.
//
// Switching starts a *new* conversation on the chosen model (same as the
// TUI's `/model <alias>`), so a non-empty transcript is confirmed first —
// the TUI shows a confirm overlay; the web View must re-build that guard
// itself (it does not ride the shared UiAction).
export function ModelSwitcher({
  models,
  activeAlias,
  hasTranscript,
  onSwitch,
  // Open the dropdown upward instead of downward. Set when the chip lives in
  // the mobile bottom bar, where a downward menu would clip off-screen.
  dropUp = false,
  // When set, the chip renders greyed/disabled, clicking does NOT open the
  // dropdown, and a title tooltip explains why.
  lockedReason = null,
}: {
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  onSwitch: (alias: string) => void;
  dropUp?: boolean;
  lockedReason?: string | null;
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

  if (models.length === 0) return null;

  const select = (alias: string) => {
    setOpen(false);
    if (alias === activeAlias) return;
    if (
      hasTranscript &&
      !window.confirm(
        `Switch to "${alias}"? This starts a new conversation — the current transcript is cleared.`,
      )
    ) {
      return;
    }
    onSwitch(alias);
  };

  return (
    <div className="relative" ref={ref}>
      <button
        onClick={() => {
          if (!lockedReason) setOpen((o) => !o);
        }}
        className={`flex items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900 px-2 py-1 text-xs transition-colors ${
          lockedReason
            ? "cursor-not-allowed text-zinc-500"
            : "text-zinc-300 hover:bg-zinc-800"
        }`}
        title={lockedReason ?? "Switch model"}
      >
        <span className="h-1.5 w-1.5 rounded-full bg-zinc-600" />
        {activeAlias || "model"}
        <span className="text-zinc-600">▾</span>
      </button>

      {open && (
        <div
          className={`absolute left-0 z-20 max-h-80 w-72 overflow-y-auto rounded-md border border-zinc-800 bg-zinc-900 py-1 shadow-xl max-sm:fixed max-sm:inset-x-2 max-sm:mt-0 max-sm:mb-0 max-sm:w-auto ${
            dropUp
              ? "bottom-full mb-1 max-sm:bottom-[3.75rem] max-sm:top-auto"
              : "top-full mt-1 max-sm:top-[3.75rem]"
          }`}
        >
          {models.map((m) => (
            <button
              key={m.alias}
              onClick={() => select(m.alias)}
              className={`flex w-full flex-col items-start px-3 py-1.5 text-left text-xs hover:bg-zinc-800 ${
                m.alias === activeAlias ? "text-emerald-400" : "text-zinc-200"
              }`}
            >
              <span className="font-medium">
                {m.alias === activeAlias ? "→ " : ""}
                {m.alias}
              </span>
              <span className="text-[10px] text-zinc-500">
                {m.provider_name} · {m.model_name}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
