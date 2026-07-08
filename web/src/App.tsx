// Phase 0 — the full static shell. This renders the *complete* PeakBot web
// UI layout (top bar, welcome banner, transcript with every message role,
// side rail with stats/context/todo/background panels, foreground bash
// strip, composer) driven entirely by hardcoded mock data (`./mock`).
//
// There is deliberately NO WebSocket, NO live AppState, and NO input
// handling here — that is Phase 1 (webui.md §5). The component tree is
// shaped so the Phase-1 swap is a data-source change, not a rewrite: each
// component already takes the same fields the Rust `AppState` carries.

import { Message } from "./components/Message";
import { WelcomeBanner } from "./components/WelcomeBanner";
import { StatsPanel } from "./components/StatsPanel";
import { TodoPanel } from "./components/TodoPanel";
import { BgPanel } from "./components/BgPanel";
import { BashPanel } from "./components/BashPanel";
import { Composer } from "./components/Composer";
import { TopBar } from "./components/TopBar";
import {
  bashPanel,
  bgProcesses,
  context,
  messages,
  stats,
  todos,
  welcome,
} from "./mock";

// Mock: pretend the agent is mid-turn so the working spinner + Stop button
// + live bash panel all show. Phase 1 reads this from AppState.is_running.
const IS_RUNNING = true;

export function App() {
  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-zinc-950 text-zinc-100">
      <TopBar stats={stats} isRunning={IS_RUNNING} />

      <div className="flex min-h-0 flex-1">
        <main className="flex min-w-0 flex-1 flex-col">
          <section className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
            <div className="mx-auto max-w-3xl space-y-3">
              <WelcomeBanner welcome={welcome} />
              {messages.map((m, i) => (
                <Message key={i} message={m} />
              ))}
            </div>
          </section>

          <BashPanel panel={bashPanel} />
          <Composer isRunning={IS_RUNNING} />
        </main>

        <aside className="hidden w-72 shrink-0 flex-col gap-5 overflow-y-auto border-l border-zinc-800 bg-zinc-950/60 p-4 lg:flex">
          <StatsPanel stats={stats} context={context} />
          <TodoPanel items={todos} />
          <BgPanel processes={bgProcesses} />
        </aside>
      </div>
    </div>
  );
}
