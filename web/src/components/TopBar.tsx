import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { CatalogProfile, Selection, SubAgentRun } from "../types";
import { CwdPicker } from "./CwdPicker";
import { ConversationsPicker } from "./ConversationsPicker";
import { SelectionChip } from "./SelectionChip";
import { NotifyToggle } from "./NotifyToggle";
import { FocusToggle } from "./FocusToggle";
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
  conversationsError,
  send,
  onLoadConversation,
  notifyEnabled,
  notifyPermission,
  onToggleNotify,
  focusMode,
  onToggleFocus,
  onOpenSettings,
  catalog,
  selection,
  selectionLocked = false,
  profilePinned = false,
  onToast,
  stopPending = false,
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
  conversationsError: string | null;
  send: (msg: InboundMessage) => void;
  onLoadConversation: (id: string) => void;
  notifyEnabled: boolean;
  notifyPermission: NotifyPermission;
  onToggleNotify: () => void;
  focusMode: boolean;
  onToggleFocus: () => void;
  /** Opens the Settings view. The gear renders only when this is given —
   * App passes it iff `settings_available` and the profile's `visible_tabs`
   * include `settings`. */
  onOpenSettings?: () => void;
  // T32 chat chip (optional — absent ⇒ no chip, as on older servers): the
  // profiles/agents catalog and this conversation's (profile, agent)
  // selection. Both render null on their own when absent.
  catalog?: CatalogProfile[];
  selection?: Selection | null;
  /** The conversation has a user/agent turn (`conversationHasTurns`) — the
   * selection chip locks. Not `hasTranscript`: system notices don't lock. */
  selectionLocked?: boolean;
  /** `AppState.profile_pinned` — a --profile pin freezes the profile. */
  profilePinned?: boolean;
  /** Toast sink for `Next conversation: …`. */
  onToast?(message: string): void;
  /** Two-stage stop: a graceful stop is pending. While set, the working…
   * chip shows the escalation sentence INSTEAD of the statusMessage. */
  stopPending?: boolean;
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

      {/* Sessions trio — selection + conversations + cwd. The selection chip
          owns profile, agent AND team-model setup (model/agents chips are
          gone). On mobile they move to the BottomBar, so hide below lg. */}
      <div className="hidden items-center gap-3 lg:flex">
        <SelectionChip
          catalog={catalog ?? []}
          selection={selection ?? null}
          models={models}
          activeAlias={activeAlias}
          locked={selectionLocked}
          pinned={profilePinned}
          send={send}
          onToast={onToast}
        />
        <ConversationsPicker
          conversations={conversations}
          error={conversationsError}
          hasTranscript={hasTranscript}
          onOpen={() => send({ type: "request_conversations" })}
          onLoad={onLoadConversation}
          onKill={(id) => send({ type: "kill_session", convo: id })}
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
          {stopPending ? (
            // A pending graceful stop is the status the user needs: it
            // outranks the tool/compaction statusMessage.
            <span
              className="truncate max-w-[16rem]"
              title="Stopping after current tool call…"
            >
              · Stopping after current tool call…
            </span>
          ) : (
            statusMessage && (
              <span className="truncate max-w-[16rem]" title={statusMessage}>
                · {statusMessage}
              </span>
            )
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
        <FocusToggle enabled={focusMode} onToggle={onToggleFocus} />
        <ThemeToggle />
        <NotifyToggle
          enabled={notifyEnabled}
          permission={notifyPermission}
          onToggle={onToggleNotify}
        />
        {onOpenSettings && (
          <button
            type="button"
            onClick={onOpenSettings}
            title="Settings  Ctrl ,"
            aria-label="Settings"
            className="flex h-6 w-6 cursor-pointer items-center justify-center rounded text-zinc-500 transition-colors hover:text-zinc-300"
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              className="h-4 w-4"
            >
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
          </button>
        )}
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
