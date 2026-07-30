import type { SessionStats } from "../types";
import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { NotifyToggle } from "./NotifyToggle";
import { ThemeToggle } from "./ThemeToggle";
import type { NotifyPermission } from "../useTaskNotifications";

// Top status bar. Sessions trio (conversations + model + cwd) live here on
// lg+ and migrate to the BottomBar on smaller screens. Right side carries the
// working spinner, tokens/cost readout, and connection indicator.
export function TopBar({
  stats,
  isRunning,
  connected,
  pendingInput,
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  conversations,
  send,
  onSwitchModel,
  onLoadConversation,
  notifyEnabled,
  notifyPermission,
  onToggleNotify,
}: {
  stats: SessionStats | null;
  isRunning: boolean;
  connected: boolean;
  pendingInput: number;
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  conversations: ConversationSummary[];
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
  onLoadConversation: (id: string) => void;
  notifyEnabled: boolean;
  notifyPermission: NotifyPermission;
  onToggleNotify: () => void;
}) {
  return (
    // `backdrop-blur` makes this header a stacking context, which traps the
    // pickers' `z-20` panels inside it — so the header itself must outrank the
    // transcript's `relative` wrapper, or the dropdowns render but can't be clicked.
    <header className="relative z-30 flex min-h-14 items-center gap-3 border-b border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-2">
        <img src="/shifu-mark.png" alt="" className="h-6 w-6 rounded-sm" />
        <span className="font-semibold text-zinc-100">Shifu</span>
      </div>

      {/* Sessions trio — conversations + model + cwd. On mobile they move
          to the BottomBar to de-crowd the header, so hide them here below lg. */}
      <div className="hidden items-center gap-3 lg:flex">
        <ConversationsPicker
          conversations={conversations}
          hasTranscript={hasTranscript}
          onOpen={() => send({ type: "request_conversations" })}
          onLoad={onLoadConversation}
          onKill={(id) => send({ type: "kill_session", convo: id })}
        />

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
      </div>

      {isRunning && (
        <span className="flex items-center gap-1.5 text-xs text-amber-400">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent" />
          working…
        </span>
      )}

      {pendingInput > 0 && (
        <span
          className="flex items-center gap-1 rounded bg-zinc-800/80 px-1.5 py-0.5 text-xs text-zinc-400"
          title={`${pendingInput} message${pendingInput === 1 ? "" : "s"} queued while the agent is busy`}
        >
          ⏳ {pendingInput} queued
        </span>
      )}

      <div className="ml-auto flex items-center gap-4 font-mono text-[11px] tabular-nums text-zinc-500">
        {stats && (
          <>
            <span>{(stats.inputTokens + stats.outputTokens).toLocaleString()} tok</span>
            <span>${stats.costUsd.toFixed(4)}</span>
          </>
        )}
        <ThemeToggle />
        <NotifyToggle
          enabled={notifyEnabled}
          permission={notifyPermission}
          onToggle={onToggleNotify}
        />
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
