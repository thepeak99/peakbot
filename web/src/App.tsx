// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the same component tree the Phase-0 mock shaped. Sending, Stop, the
// working spinner, and every side panel are driven by live state.
//
// Layout has a fixed `<lg` threshold:
//   - lg+ (≥1024px): the sidebar is the static right-rail aside.
//   - <lg:  the sidebar is hidden, behind a hamburger in the top bar that
//           opens a full-viewport drawer (no max-w, no backdrop — the aside
//           is the whole viewport, closed via the X button or Escape).
//
// The two Sidebar instances are independent (each has its own dropdown-open
// state); only one is ever on-screen at a time, so this costs nothing.

import { useEffect, useRef, useState } from "react";
import { Message } from "./components/Message";
import { WelcomeBanner } from "./components/WelcomeBanner";
import { BashPanel } from "./components/BashPanel";
import { Composer } from "./components/Composer";
import { TopBar } from "./components/TopBar";
import { BottomBar } from "./components/BottomBar";
import { Sidebar } from "./components/Sidebar";
import { useAgent } from "./useAgent";
import { useMediaQuery } from "./useMediaQuery";
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
    dirListing,
    error,
    send,
    switchConvo,
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
  const context = state ? adaptContext(state) : null;

  // The sidebar lives in a drawer below the lg breakpoint (1024px). Above
  // that it is the static right rail. The hamburger button only renders
  // when this is true.
  const smallViewport = useMediaQuery("(max-width: 1023px)");
  const [sidebarOpen, setSidebarOpen] = useState(false);
  // If the viewport grows past lg while the drawer is open, close it —
  // otherwise the next time the user shrinks back the drawer appears open
  // out of nowhere.
  useEffect(() => {
    if (!smallViewport) setSidebarOpen(false);
  }, [smallViewport]);

  // Escape closes the drawer (no-op on lg+ since the drawer never opens).
  useEffect(() => {
    if (!sidebarOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSidebarOpen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [sidebarOpen]);

  // Body scroll lock while the drawer is up — prevents the underlying
  // transcript from rubber-banding on iOS.
  useEffect(() => {
    if (!sidebarOpen) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [sidebarOpen]);

  return (
    <div className="flex h-dvh w-screen flex-col overflow-hidden bg-zinc-950 text-zinc-100">
      <TopBar
        stats={stats}
        isRunning={isRunning}
        connected={connected}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={welcome?.cwd ?? null}
        dirListing={dirListing}
        conversations={conversations}
        send={send}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
        onLoadConversation={(id) => switchConvo(id)}
        onToggleSidebar={
          smallViewport ? () => setSidebarOpen((o) => !o) : undefined
        }
      />

      {error && (
        <div className="bg-red-950/70 px-4 py-1.5 text-center text-xs text-red-300">
          {error}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <main className="flex min-w-0 flex-1 flex-col">
          <section className="min-h-0 flex-1 overflow-y-auto px-4 py-4 sm:px-6 md:px-8">
            <div className="mx-auto max-w-5xl space-y-3">
              {welcome && messageCount === 0 && <WelcomeBanner welcome={welcome} />}
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
          <Sidebar stats={stats} context={context} todos={todos} bg={bg} />
        </aside>
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

      {/* Mobile/tablet drawer — full viewport so the left edge meets the
          screen edge. Closed via the X button or Escape. */}
      {smallViewport && sidebarOpen && (
        <aside className="fixed inset-0 z-40 flex flex-col gap-5 overflow-y-auto bg-zinc-950 p-4 lg:hidden">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <span className="text-base">✦</span>
              <span className="font-semibold text-zinc-100">PeakBot</span>
            </div>
            <button
              type="button"
              onClick={() => setSidebarOpen(false)}
              aria-label="Close sidebar"
              title="Close (Esc)"
              className="flex h-8 w-8 items-center justify-center rounded-md text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="h-5 w-5"
                aria-hidden="true"
              >
                <line x1="6" y1="6" x2="18" y2="18" />
                <line x1="18" y1="6" x2="6" y2="18" />
              </svg>
            </button>
          </div>
          <Sidebar stats={stats} context={context} todos={todos} bg={bg} />
        </aside>
      )}
    </div>
  );
}
