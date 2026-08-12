// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the component tree. Sending, Stop, the working spinner, and every side
// panel are driven by live state.
//
// Side panels live in a single `TabbedDrawer` (vertical tab handles pinned to
// the right edge; the body slides in). One responsive mechanism: on lg+ the
// body is a 288px rail, below sm it spans 94vw. Replaces the old static aside
// + separate mobile hamburger drawer.

import { useRef, useState } from "react";
import { Transcript, type TranscriptHandle } from "./components/Transcript";
import { EmptyTranscript } from "./components/EmptyTranscript";
import { BashPanel } from "./components/BashPanel";
import { Composer } from "./components/Composer";
import { TopBar } from "./components/TopBar";
import { BottomBar } from "./components/BottomBar";
import { TabbedDrawer } from "./components/TabbedDrawer";
import { StatsPanel } from "./components/StatsPanel";
import { TodoPanel } from "./components/TodoPanel";
import { BgPanel } from "./components/BgPanel";
import { FilesPanel } from "./components/FilesPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { useAgent } from "./useAgent";
import { useTaskNotifications } from "./useTaskNotifications";
import { useFavicon } from "./useFavicon";
import { epochKey, nextEpoch, type EpochState } from "./transcriptEpoch";
import type { ViewFilter } from "./types";
import {
  adaptBashPanel,
  adaptBg,
  adaptContext,
  adaptStats,
  adaptWelcome,
  deriveSubAgentRoster,
  filesFromMessages,
  filterMessagesByView,
  flatTree,
  laneOf,
  messagesByLane,
  scopeStatsToView,
  todoTree,
  todosFromMessages,
  viewLabel,
} from "./adapt";

export function App() {
  const {
    connected,
    state,
    models,
    activeAlias,
    conversations,
    commands,
    dirListing,
    error,
    send,
    switchConvo,
  } = useAgent();

  // Swap favicon to a spinning loader while the agent is working
  const isRunning = state?.is_running ?? false;
  useFavicon(isRunning);

  // Follow the newest message, but only while the user is pinned to the bottom
  // — the transcript owns that now (see `Transcript` + `useTranscriptScroll`).
  const messageCount = state?.chat.messages.length ?? 0;
  const transcriptRef = useRef<TranscriptHandle>(null);

  const notify = useTaskNotifications(isRunning);
  const hasTranscript = messageCount > 0;
  const [drawerTab, setDrawerTab] = useState<string | null>(null);
  const drawerOpen = drawerTab !== null;

  // Pipelines. ONE fact drives everything: `selected_pipeline` — the team this
  // conversation is bound to, or null for single agent. `pipelines` is the
  // boot-time catalogue (what the user may pick). Sub-agents run iff a
  // pipeline is selected, so that single value also decides whether the
  // per-lane views mean anything. `view` scopes the transcript, todos, files
  // and stats to one lane via the message `source`.
  const pipelines = state?.pipelines ?? [];
  const selectedPipeline = state?.selected_pipeline ?? null;
 // When a pipeline is selected, the model selector is locked to the
  // orchestrator's model — the chip stays visible but is disabled.
  const modelLockedReason = selectedPipeline
    ? `fixed by pipeline ${selectedPipeline}`
    : null;
  const [view, setView] = useState<ViewFilter>("global");
  // Sub-agent views only make sense once a pipeline is actually selected.
  const effectiveView: ViewFilter = selectedPipeline ? view : "global";

  // Sub-agent roster: one entry per delegation call, in call order (Phase 2 +
  // per-call). Zero backend — derived from the transcript. A role delegated to
  // more than once yields several entries; the panel suffixes those "#n".
  const roster = state ? deriveSubAgentRoster(state.chat.messages) : [];
  // The pipeline selection locks once the conversation has a real turn (any
  // user or agent message). System banners don't count — mirrors the backend's
  // `conversation_has_turns`.
  const conversationStarted = state
    ? state.chat.messages.some((m) => m.role === "user" || m.role === "agent")
    : false;
  const multiCall = new Set(
    roster.filter((r) => r.n > 1).map((r) => r.role),
  );
  const scopeLabel =
    effectiveView === "global"
      ? null
      : viewLabel(effectiveView, multiCall.has(laneOf(effectiveView)));

  const visibleMessages = state
    ? filterMessagesByView(state.chat.messages, effectiveView)
    : [];

  // Remount token for the transcript. Switching conversation or view, or a
  // transcript that got shorter (truncation/compaction), invalidates every
  // measured row height and every scroll position — remounting is the whole
  // reset story: fresh scroll element, fresh virtualizer, fresh pin state.
  const id = `${state?.conversation?.id ?? "none"}|${effectiveView}`;
  const [epoch, setEpoch] = useState<EpochState>({ id, count: 0, epoch: 0 });
  const next = nextEpoch(epoch, id, visibleMessages.length);
  if (next !== epoch) setEpoch(next); // React's documented adjust-during-render
  const resetKey = epochKey(next);

  const stats = state ? adaptStats(state) : null;
  // Scope the Session panel to the watched view (Global keeps grand totals).
  const scopedStats = stats ? scopeStatsToView(stats, effectiveView) : null;
  const welcome = state ? adaptWelcome(state) : null;
  const bash = state ? adaptBashPanel(state.bash_panel) : null;
  // Todos & files derive from the transcript. In the global view todos form a
  // one-level tree: each lane's list labeled by lane, sub-agent todos nested
  // under the orchestrator item they were delegated from (todoTree). A scoped
  // view shows just that lane's flat, unlabeled list. Files stay global-mixed
  // by design.
  const todos =
    state && effectiveView === "global"
      ? todoTree(state.chat.messages)
      : flatTree(todosFromMessages(visibleMessages));
  const bg = state ? adaptBg(state) : [];
  const context = state ? adaptContext(state) : null;
  const files = filesFromMessages(visibleMessages);
  // API calls per lane, so the Agents panel can show them next to its message
  // counts — two different units that otherwise look like disagreement.
  const laneCalls = Object.fromEntries(
    (stats?.lanes ?? []).map((l) => [l.lane, l.apiCalls]),
  );
  // One derivation of per-lane message counts, shared by the Session cards and
  // the Agents panel — two panels reporting the same figure from one place.
  const laneMessages = messagesByLane(state?.chat.messages ?? []);

  const runningBg = bg.filter((p) => p.status === "running").length;
  const pendingInput = state?.pending_input_count ?? 0;

  const tabs = [
    {
      id: "session",
      label: "Session",
      content:
        scopedStats && context ? (
          <StatsPanel
            stats={scopedStats}
            context={context}
            peakbotVersion={welcome?.peakbotVersion}
            scopeLabel={scopeLabel}
            laneMessages={laneMessages}
          />
        ) : null,
    },
    {
      id: "todo",
      label: "Todo",
      content: <TodoPanel nodes={todos} />,
      // Every rendered row counts — parents plus their nested sub-agent todos.
      badge: todos.reduce((n, node) => n + 1 + node.children.length, 0),
    },
    {
      id: "files",
      label: "Files",
      content: <FilesPanel files={files} />,
      badge: files.length,
    },
    {
      id: "tasks",
      label: "Tasks",
      content: <BgPanel processes={bg} />,
      badge: runningBg,
    },
    {
      id: "bash",
      label: "Bash",
      content: bash ? (
        <BashPanel panel={bash} />
      ) : (
        <p className="text-sm text-zinc-600">No bash command yet.</p>
      ),
      badge: bash?.status === "running" ? 1 : undefined,
    },
    {
      id: "agents",
      label: "Agents",
      content: (
        <AgentsPanel
          pipelines={pipelines}
          selected={selectedPipeline}
          locked={conversationStarted}
          onSelectPipeline={(name) => send({ type: "select_pipeline", name })}
          active={view}
          onSelect={setView}
          roster={roster}
          laneCalls={laneCalls}
          laneMessages={laneMessages}
        />
      ),
      badge: selectedPipeline ? roster.length : undefined,
    },
  ];

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-zinc-950 text-zinc-100">
    <TopBar
        stats={stats}
        isRunning={isRunning}
        connected={connected}
        pendingInput={pendingInput}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={welcome?.cwd ?? null}
        dirListing={dirListing}
        conversations={conversations}
        send={send}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
        onLoadConversation={(id) => switchConvo(id)}
        notifyEnabled={notify.enabled}
        notifyPermission={notify.permission}
        onToggleNotify={notify.toggle}
        lockedReason={modelLockedReason}
      />
  

      {error && (
        <div className="bg-red-950/70 px-4 py-1.5 text-center text-xs text-red-300">
          {error}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <main className="flex min-w-0 flex-1 flex-col">
          {/* The scope banner is a static bar above the transcript: the
              scrolling element's only child must be the virtualizer's sized
              container, or the row offsets stop matching the scroll offset. */}
          {scopeLabel && (
            <div className="mr-12 px-4 pt-4 sm:px-6 md:px-8">
              <div className="mx-auto flex max-w-5xl items-center justify-between rounded-md border border-sky-900/60 bg-sky-950/30 px-3 py-1.5 text-xs text-sky-300">
                <span>
                  👁 Watching <span className="font-medium">{scopeLabel}</span>
                  <span className="ml-2 text-sky-500/70">
                    {visibleMessages.length} message
                    {visibleMessages.length === 1 ? "" : "s"}
                  </span>
                </span>
                <button
                  onClick={() => setView("global")}
                  className="cursor-pointer rounded px-1.5 py-0.5 text-sky-400 hover:bg-sky-900/40"
                >
                  Clear
                </button>
              </div>
            </div>
          )}

          {visibleMessages.length > 0 ? (
            <Transcript
              key={resetKey}
              messages={visibleMessages}
              drawerOpen={drawerOpen}
              ref={transcriptRef}
            />
          ) : (
            <EmptyTranscript
              welcome={messageCount === 0 ? welcome : null}
              scopeLabel={scopeLabel}
            />
          )}

          <Composer
            isRunning={isRunning}
            connected={connected}
            commands={commands}
            onSend={(text) => {
              // Sending always re-pins: you expect to see your own message.
              send({ type: "send_message", text });
              transcriptRef.current?.jumpToLatest();
            }}
            onStop={() => send({ type: "stop" })}
            watchingRole={scopeLabel}
            onClearWatch={() => setView("global")}
          />
        </main>
      </div>

   <BottomBar
        conversations={conversations}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={welcome?.cwd ?? null}
        dirListing={dirListing}
        send={send}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
        onLoadConversation={(id) => switchConvo(id)}
        lockedReason={modelLockedReason}
      />
   

      <TabbedDrawer
        tabs={tabs}
        active={drawerTab}
        onActiveChange={setDrawerTab}
      />
    </div>
  );
}
