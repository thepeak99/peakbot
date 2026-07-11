import type { SessionStats } from "../types";
import type { DirListing, InboundMessage, ModelInfo } from "../state";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";

// Top status bar. Model switcher, the working spinner (AppState.is_running),
// a compact tokens/cost readout mirroring the TUI status line, and a
// connection indicator.
export function TopBar({
  stats,
  isRunning,
  connected,
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  send,
  onSwitchModel,
}: {
  stats: SessionStats | null;
  isRunning: boolean;
  connected: boolean;
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
}) {
  return (
    <header className="flex items-center gap-3 border-b border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-2">
        <span className="text-base">✦</span>
        <span className="font-semibold text-zinc-100">PeakBot</span>
      </div>

      <ModelSwitcher
        models={models}
        activeAlias={activeAlias}
        hasTranscript={hasTranscript}
        onSwitch={onSwitchModel}
      />

      {cwd && (
        <CwdPicker
          cwd={cwd}
          hasTranscript={hasTranscript}
          dirListing={dirListing}
          send={send}
        />
      )}

      {isRunning && (
        <span className="flex items-center gap-1.5 text-xs text-amber-400">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent" />
          working…
        </span>
      )}

      <div className="ml-auto flex items-center gap-4 font-mono text-[11px] tabular-nums text-zinc-500">
        {stats && (
          <>
            <span>{(stats.inputTokens + stats.outputTokens).toLocaleString()} tok</span>
            <span>${stats.costUsd.toFixed(4)}</span>
          </>
        )}
        <span
          className={`flex items-center gap-1.5 rounded px-1.5 py-0.5 ${
            connected ? "text-emerald-400" : "text-zinc-500"
          }`}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              connected ? "bg-emerald-400" : "bg-zinc-600"
            }`}
          />
          {connected ? "connected" : "offline"}
        </span>
      </div>
    </header>
  );
}
