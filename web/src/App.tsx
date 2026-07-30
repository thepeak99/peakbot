// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the component tree. Sending, Stop, the working spinner, and every side
// panel are driven by live state.
//
// Side panels live in a single `TabbedDrawer` (vertical tab handles pinned to
// the right edge; the body slides in). One responsive mechanism: on lg+ the
// body is a 288px rail, below sm it spans 94vw. Replaces the old static aside
// + separate mobile hamburger drawer.

import { useEffect, useState } from "react";
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
import { TranscriptNav } from "./components/TranscriptNav";
import { useAgent } from "./useAgent";
import { useTranscriptScroll } from "./useTranscriptScroll";
import { useTaskNotifications } from "./useTaskNotifications";
import { useFavicon } from "./useFavicon";
import type { ViewFilter } from "./types";
import {
  adaptBashPanel,
  adaptBg,
  adaptContext,
  adaptMessage,
  adaptStats,
  adaptWelcome,
  deriveSubAgentRoster,
  filesFromMessages,
  filterMessagesByView,
  laneOf,
  scopeStatsToView,
  todosByLane,
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
  // (see `useTranscriptScroll`); scrolled up to read history, they stay put and
  // `TranscriptNav` offers the way back.
  const messageCount = state?.chat.messages.length ?? 0;
  const {
    sectionRef,
    onScroll,
    pinned,
    unread,
    showTop,
    scrollToBottom,
    scrollToTop,
  } = useTranscriptScroll(messageCount);
  useEffect(() => {
    if (pinned) scrollToBottom();
  }, [messageCount, isRunning, pinned, scrollToBottom]);

  const notify = useTaskNotifications(isRunning);
  const hasTranscript = messageCount > 0;

  // Sub-agent watch. Two distinct facts, kept apart:
  //  * `pipelineAvailable` — is a pipeline *configured*? (boot-only config).
  //    Decides whether the opt-in is offered at all.
  //  * `subagentsEnabled` — did the user opt THIS conversation in? (default
  //    off, locked once the conversation has turns). Drives whether sub-agents
  //    actually run. `view` scopes the transcript, todos, files and stats to
  //    one lane via the message `source`.
  const pipelineAvailable = state?.pipeline_available ?? false;
  const subagentsEnabled = state?.subagents_enabled ?? false;
  const [view, setView] = useState<ViewFilter>("global");
  // Sub-agent views only make sense once the conversation actually opted in.
  const effectiveView: ViewFilter =
    pipelineAvailable && subagentsEnabled ? view : "global";

  // Sub-agent roster: one entry per delegation call, in call order (Phase 2 +
  // per-call). Zero backend — derived from the transcript. A role delegated to
  // more than once yields several entries; the panel suffixes those "#n".
  const roster = state ? deriveSubAgentRoster(state.chat.messages) : [];
  // The opt-in locks once the conversation has a real turn (any user or agent
  // message). System banners don't count — mirrors the backend's
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

  const stats = state ? adaptStats(state) : null;
  // Scope the Session panel to the watched view (Global keeps grand totals).
  const scopedStats = stats ? scopeStatsToView(stats, effectiveView) : null;
  const welcome = state ? adaptWelcome(state) : null;
  const bash = state ? adaptBashPanel(state.bash_panel) : null;
  // Todos & files derive from the transcript. In the global view todos show
  // every lane's list, each labeled by its lane (todosByLane); a scoped view
  // shows just that lane's list, unlabeled. Files stay global-mixed by design.
  const scopedMessages = state
    ? filterMessagesByView(state.chat.messages, effectiveView)
    : [];
  const todos =
    state && effectiveView === "global"
      ? todosByLane(state.chat.messages)
      : todosFromMessages(scopedMessages);
  const bg = state ? adaptBg(state) : [];
  const context = state ? adaptContext(state) : null;
  const files = filesFromMessages(scopedMessages);

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
          pipelineAvailable={pipelineAvailable}
          subagentsEnabled={subagentsEnabled}
          locked={conversationStarted}
          onToggle={(enabled) => send({ type: "set_subagents", enabled })}
          active={view}
          onSelect={setView}
          roster={roster}
        />
      ),
      badge:
        pipelineAvailable && subagentsEnabled ? roster.length : undefined,
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
          {/* The nav buttons float over the transcript, so they must live in a
              positioned wrapper *outside* the scrolling <section> — absolute
              inside it would scroll away with the content. */}
          <div className="relative flex min-h-0 flex-1 flex-col">
            <section
              ref={sectionRef}
              onScroll={onScroll}
              className="min-h-0 flex-1 overflow-y-auto mr-12 px-4 py-4 sm:px-6 md:px-8"
            >
              <div className="mx-auto max-w-5xl space-y-3">
                {welcome && messageCount === 0 && (
                  <WelcomeBanner welcome={welcome} />
                )}
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
              </div>
            </section>

            {hasTranscript && (
              <TranscriptNav
                pinned={pinned}
                unread={unread}
                showTop={showTop}
                onBottom={scrollToBottom}
                onTop={scrollToTop}
              />
            )}
          </div>

          <Composer
            isRunning={isRunning}
            connected={connected}
            commands={commands}
            onSend={(text) => {
              // Sending always re-pins: you expect to see your own message.
              send({ type: "send_message", text });
              scrollToBottom();
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
      />

      <TabbedDrawer tabs={tabs} />
    </div>
  );
}
