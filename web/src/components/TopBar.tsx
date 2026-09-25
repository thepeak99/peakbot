import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { SubAgentRun } from "../types";
import { ModelSwitcher } from "./ModelSwitcher";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { NotifyToggle } from "./NotifyToggle";
import { ThemeToggle } from "./ThemeToggle";
import type { NotifyPermission } from "../useTaskNotifications";

// Top status bar. Sessions trio (conversations + model + cwd) live here on
// lg+ and migrate to the BottomBar on smaller screens. Right side carries the
// working spinner and connection indicator.
export function TopBar({
  isRunning,
  connected,
  statusMessage,
  subAgent = null,
  models,
  activeAlias,
  hasTranscript,
  cwd,
  dirListing,
  recentDirs,
  conversations,
  send,
  onSwitchModel,
  onLoadConversation,
  notifyEnabled,
  notifyPermission,
  onToggleNotify,
  lockedReason = null,
}: {
  isRunning: boolean;
  connected: boolean;
  statusMessage: string | null;
  /** The currently-running sub-agent, or null for orchestrator-only turns.
   * Renders the 🧩/⏸ chip next to "working…" while non-null. */
  subAgent?: SubAgentRun | null;
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  recentDirs: string[];
  conversations: ConversationSummary[];
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
  onLoadConversation: (id: string) => void;
  notifyEnabled: boolean;
  notifyPermission: NotifyPermission;
  onToggleNotify: () => void;
  lockedReason?: string | null;
}) {
  return (

    // `backdrop-blur` makes this header a stacking context, which traps the
    // pickers' `z-20` panels inside it — so the header itself must outrank the
    // transcript's `relative` wrapper, or the dropdowns render but can't be clicked.
    <header className="relative z-30 flex min-h-14 items-center gap-3 border-b border-zinc-800 bg-zinc-950/80 px-4 py-2 backdrop-blur">
      <div className="flex items-center gap-2">
        <img src="/logo_shifu.png" alt="" className="h-6 w-6 rounded-sm" />
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
          lockedReason={lockedReason}
        />
 

        {cwd && (
          <CwdPicker
            cwd={cwd}
            hasTranscript={hasTranscript}
            dirListing={dirListing}
            recentDirs={recentDirs}
            send={send}
          />
        )}
      </div>

      {isRunning && (
        <span className="flex items-center gap-1.5 text-xs text-amber-400">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent shrink-0" />
          working…
          {statusMessage && (
            <span className="truncate max-w-[16rem]" title={statusMessage}>
              · {statusMessage}
            </span>
          )}
        </span>
      )}

      {/* Sub-agent chip: which delegated agent is driving the turn and its
          pause lifecycle. Gated on isRunning like the status message — a
          stale sub_agent must not leak once the run has ended. Sky ties it to
          the sub-agent lane color used by the watching banner. */}
      {isRunning && subAgent && (
        <span className="flex items-center gap-1.5 text-xs text-sky-400">
          {subAgent.pause === "running" && (
            <>
              🧩 <span className="font-medium">{subAgent.role}</span>
            </>
          )}
          {subAgent.pause === "pausing" && (
            <>⏸ Pausing {subAgent.role} after current step…</>
          )}
          {subAgent.pause === "paused" && (
            <>
              ⏸ <span className="font-medium">{subAgent.role}</span> paused
            </>
          )}
        </span>
      )}

      <div className="ml-auto flex items-center gap-4 font-mono text-[11px] tabular-nums text-zinc-500">
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
          aria-label={connected ? "connected" : "offline"}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${
              connected ? "bg-emerald-400" : "bg-zinc-600"
            }`}
          />
          {connected ? (
            // Mobile (below lg): dot only — the label is redundant on a
            // phone's narrow top bar. The outer span's aria-label keeps the
            // status exposed to screen readers.
            <span className="hidden lg:inline">connected</span>
          ) : (
            "offline"
          )}
        </span>
      </div>
    </header>
  );
}
