// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the component tree. Sending, Stop, the working spinner, and every side
// panel are driven by live state.
//
// Side panels live in a single `TabbedDrawer` (vertical tab handles pinned to
// the right edge; the body slides in). One responsive mechanism: on lg+ the
// body is a 288px rail, below sm it spans 94vw. Replaces the old static aside
// + separate mobile hamburger drawer.

import { useEffect, useRef, useState } from "react";
import { Message } from "./components/Message";
import { WelcomeBanner } from "./components/WelcomeBanner";
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
import type { ViewFilter } from "./types";
import {
  adaptBashPanel,
  adaptBg,
  adaptContext,
  adaptFiles,
  adaptMessage,
  adaptStats,
  adaptTodos,
  adaptWelcome,
  deriveSubAgentRoster,
  filterMessagesByView,
  scopeStatsToView,
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

  // Keep the transcript pinned to the newest message.
  const bottomRef = useRef<HTMLDivElement>(null);
  const messageCount = state?.chat.messages.length ?? 0;
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messageCount, isRunning]);

  const notify = useTaskNotifications(isRunning);
  const hasTranscript = messageCount > 0;

  // Phase-1 dummy: subagent watch. `agentsEnabled` toggles the list; `view`
  // scopes the transcript (and, later, todo/stats) via the message `source`.
  const [agentsEnabled, setAgentsEnabled] = useState(false);
  const [view, setView] = useState<ViewFilter>("global");
  // If watching is disabled, force the transcript back to the global view.
  const effectiveView: ViewFilter = agentsEnabled ? view : "global";
  const scopeLabel =
    effectiveView === "global"
      ? null
      : effectiveView === "orchestrator"
        ? "Orchestrator"
        : effectiveView;

  const visibleMessages = state
    ? filterMessagesByView(state.chat.messages, effectiveView)
    : [];

  // Sub-agent roster derived live from the transcript (Phase 2). Zero backend.
  const roster = state ? deriveSubAgentRoster(state.chat.messages) : [];

  const stats = state ? adaptStats(state) : null;
  // Scope the Session panel to the watched view (Global keeps grand totals).
  const scopedStats = stats ? scopeStatsToView(stats, effectiveView) : null;
  const pipelineEnabled = state?.pipeline_enabled ?? false;
  const welcome = state ? adaptWelcome(state) : null;
  const bash = state ? adaptBashPanel(state.bash_panel) : null;
  const todos = state ? adaptTodos(state) : [];
  const bg = state ? adaptBg(state) : [];
  const context = state ? adaptContext(state) : null;
  const files = state ? adaptFiles(state) : [];

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
          />
        ) : null,
    },
    {
      id: "todo",
      label: "Todo",
      content: <TodoPanel items={todos} />,
      badge: todos.length,
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
          enabled={agentsEnabled}
          onToggleEnabled={setAgentsEnabled}
          active={view}
          onSelect={setView}
          roster={roster}
          pipelineEnabled={pipelineEnabled}
        />
      ),
      badge: agentsEnabled ? roster.length : undefined,
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
      />

      {error && (
        <div className="bg-red-950/70 px-4 py-1.5 text-center text-xs text-red-300">
          {error}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <main className="flex min-w-0 flex-1 flex-col">
          <section className="min-h-0 flex-1 overflow-y-auto mr-12 px-4 py-4 sm:px-6 md:px-8">
            <div className="mx-auto max-w-5xl space-y-3">
              {welcome && messageCount === 0 && <WelcomeBanner welcome={welcome} />}
              {scopeLabel && (
                <div className="flex items-center justify-between rounded-md border border-sky-900/60 bg-sky-950/30 px-3 py-1.5 text-xs text-sky-300">
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
              )}
              {scopeLabel && visibleMessages.length === 0 && (
                <p className="rounded-md border border-dashed border-zinc-800 px-3 py-6 text-center text-xs text-zinc-600">
                  No messages from {scopeLabel} yet.
                </p>
              )}
              {visibleMessages.map((m, i) => (
                <Message key={i} message={adaptMessage(m)} />
              ))}
              <div ref={bottomRef} />
            </div>
          </section>

          <Composer
            isRunning={isRunning}
            connected={connected}
            commands={commands}
            onSend={(text) => send({ type: "send_message", text })}
            onStop={() => send({ type: "stop" })}
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
      />

      <TabbedDrawer tabs={tabs} />
    </div>
  );
}
