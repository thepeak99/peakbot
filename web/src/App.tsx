// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the component tree. Sending, Stop, the working spinner, and every side
// panel are driven by live state.
//
// Side panels live in a single `TabbedDrawer` (vertical tab handles pinned to
// the right edge; the body slides in). One responsive mechanism: on lg+ the
// body is a 288px rail, below sm it spans 94vw. Replaces the old static aside
// + separate mobile hamburger drawer.

import { useEffect, useRef } from "react";
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
import { useAgent } from "./useAgent";
import { useTaskNotifications } from "./useTaskNotifications";
import { useFavicon } from "./useFavicon";
import {
  adaptBashPanel,
  adaptBg,
  adaptContext,
  adaptFiles,
  adaptMessage,
  adaptStats,
  adaptTodos,
  adaptWelcome,
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
  const stats = state ? adaptStats(state) : null;
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
        stats && context ? (
          <StatsPanel
            stats={stats}
            context={context}
            peakbotVersion={welcome?.peakbotVersion}
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
              {state?.chat.messages.map((m, i) => (
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
