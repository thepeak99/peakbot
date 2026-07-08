// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the same component tree the Phase-0 mock shaped. Sending, Stop, the
// working spinner, and every side panel are driven by live state.

import { useEffect, useRef } from "react";
import { Message } from "./components/Message";
import { WelcomeBanner } from "./components/WelcomeBanner";
import { StatsPanel } from "./components/StatsPanel";
import { TodoPanel } from "./components/TodoPanel";
import { BgPanel } from "./components/BgPanel";
import { BashPanel } from "./components/BashPanel";
import { Composer } from "./components/Composer";
import { TopBar } from "./components/TopBar";
import { ConversationsPicker } from "./components/ConversationsPicker";
import { useAgent } from "./useAgent";
import {
  adaptBashPanel,
  adaptBg,
  adaptContext,
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
    error,
    send,
  } = useAgent();

  // Keep the transcript pinned to the newest message.
  const bottomRef = useRef<HTMLDivElement>(null);
  const messageCount = state?.chat.messages.length ?? 0;
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messageCount, state?.is_running]);

  const isRunning = state?.is_running ?? false;
  const hasTranscript = messageCount > 0;
  const stats = state ? adaptStats(state) : null;
  const welcome = state ? adaptWelcome(state) : null;
  const bash = state ? adaptBashPanel(state.bash_panel) : null;
  const todos = state ? adaptTodos(state) : [];
  const bg = state ? adaptBg(state) : [];

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-zinc-950 text-zinc-100">
      <TopBar
        stats={stats}
        isRunning={isRunning}
        connected={connected}
        models={models}
        activeAlias={activeAlias}
        hasTranscript={hasTranscript}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
      />

      {error && (
        <div className="bg-red-950/70 px-4 py-1.5 text-center text-xs text-red-300">
          {error}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <main className="flex min-w-0 flex-1 flex-col">
          <section className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
            <div className="mx-auto max-w-3xl space-y-3">
              {welcome && <WelcomeBanner welcome={welcome} />}
              {state?.chat.messages.map((m, i) => (
                <Message key={i} message={adaptMessage(m)} />
              ))}
              <div ref={bottomRef} />
            </div>
          </section>

          {bash && <BashPanel panel={bash} />}
          <Composer
            isRunning={isRunning}
            connected={connected}
            commands={commands}
            onSend={(text) => send({ type: "send_message", text })}
            onStop={() => send({ type: "stop" })}
          />
        </main>

        <aside className="hidden w-72 shrink-0 flex-col gap-5 overflow-y-auto border-l border-zinc-800 bg-zinc-950/60 p-4 lg:flex">
          <ConversationsPicker
            conversations={conversations}
            hasTranscript={hasTranscript}
            onOpen={() => send({ type: "request_conversations" })}
            onLoad={(id) => send({ type: "send_message", text: `/load ${id}` })}
          />
          {stats && state && (
            <StatsPanel stats={stats} context={adaptContext(state)} />
          )}
          <TodoPanel items={todos} />
          <BgPanel processes={bg} />
        </aside>
      </div>
    </div>
  );
}
