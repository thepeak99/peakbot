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
import { Transcript, type TranscriptHandle } from "./components/Transcript";
import { EmptyTranscript } from "./components/EmptyTranscript";
import { BashPanel } from "./components/BashPanel";
import { Composer } from "./components/Composer";
import { DropOverlay } from "./components/DropOverlay";
import { TopBar } from "./components/TopBar";
import { ConvoTabBar } from "./components/ConvoTabBar";
import { BottomBar } from "./components/BottomBar";
import { ActivityRow } from "./components/ActivityRow";
import { TabbedDrawer } from "./components/TabbedDrawer";
import { StatsPanel } from "./components/StatsPanel";
import { TodoPanel } from "./components/TodoPanel";
import { BgPanel } from "./components/BgPanel";
import { FilesPanel } from "./components/FilesPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { Settings, useSettingsToggle } from "./settings/Settings";
import { useAgent } from "./useAgent";
import { useConvoTabs } from "./useConvoTabs";
import type { TabsState } from "./convoTabs";
import { useAttachments } from "./useAttachments";
import { useTaskNotifications } from "./useTaskNotifications";
import { useFavicon } from "./useFavicon";
import { useFocusMode } from "./useFocusMode";
import { focusView } from "./focus";
import { epochKey, nextEpoch, type EpochState } from "./transcriptEpoch";
import { filterTabs, resolveActiveTab } from "./tabs";
import type { ViewFilter } from "./types";
import { type UploadLimits } from "./state";
import {
  adaptBashPanel,
  adaptBg,
  adaptCatalog,
  adaptContext,
  adaptSelection,
  adaptStats,
  adaptSubAgent,
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

// Matches the server default (`uploads:` config) until the first state frame.
const DEFAULT_UPLOAD_LIMITS: UploadLimits = { max_file_mb: 50, max_files: 10 };

export function App({ initialTabs = null }: { initialTabs?: TabsState | null }) {
  const agent = useAgent();
  const {
    connected,
    state,
    models,
    activeAlias,
    conversations,
    conversationsError,
    commands,
    dirListing,
    recentDirs,
    error,
    send,
    switchConvo,
  } = agent;

  // Lifted out of Composer so the drop overlay on <main> feeds the same chips.
  const attachments = useAttachments(
    state?.conversation?.id ?? null,
    state?.upload_limits ?? DEFAULT_UPLOAD_LIMITS,
  );

  // Swap favicon to a spinning loader while the agent is working
  const isRunning = state?.is_running ?? false;
  useFavicon(isRunning);

  // Follow the newest message, but only while the user is pinned to the bottom
  // — the transcript owns that now (see `Transcript` + `useTranscriptScroll`).
  const messageCount = state?.chat.messages.length ?? 0;
  const transcriptRef = useRef<TranscriptHandle>(null);

  const notify = useTaskNotifications(isRunning);
  const hasTranscript = messageCount > 0;
  // Desktop conversation tabs (null in a browser). The title is only offered
  // once `state` describes the conversation the socket is bound to — right
  // after a switch it still describes the previous one.
  const convoTabs = useConvoTabs(
    agent,
    initialTabs,
    hasTranscript && state?.conversation?.id === agent.convo
      ? (state?.conversation?.name ?? null)
      : null,
  );
  const loadConversation = convoTabs ? convoTabs.open : switchConvo;
  const [drawerTab, setDrawerTab] = useState<string | null>(null);
  const drawerOpen = drawerTab !== null;
  // Settings is an in-app VIEW (not a route): this state lives here so the
  // chat and its WebSocket stay mounted underneath. Gear + Ctrl+, / ⌘+, +
  // Esc are owned by the hook.
  const settingsView = useSettingsToggle();

  // Catalog and selection (0.24): `catalog` is the profiles/agents/sub-agents
  // this install declares; `selection` is the (profile, agent) this
  // conversation is bound to. `boundAgent` resolves the two — the roster the
  // Agents panel renders. `view` scopes the transcript, todos, files and
  // stats to one lane via the message `source`.
  const catalog = state ? adaptCatalog(state) : [];
  const selection = state ? adaptSelection(state) : null;
  const boundAgent =
    selection === null
      ? null
      : catalog
          .find((p) => p.name === selection.profile)
          ?.agents.find((a) => a.name === selection.agent) ?? null;
  const [view, setView] = useState<ViewFilter>("global");
  // Called once here (not in TopBar) so App can filter with the same value
  // the button renders — two useState copies would desync (plan §6.1).
  const focusMode = useFocusMode();

  // Sub-agent roster: one entry per delegation call, in call order (Phase 2 +
  // per-call). Zero backend — derived from the transcript. A role delegated to
  // more than once yields several entries; the panel suffixes those "#n".
  const roster = state ? deriveSubAgentRoster(state.chat.messages) : [];
  const multiCall = new Set(
    roster.filter((r) => r.n > 1).map((r) => r.role),
  );
  const scopeLabel =
    view === "global"
      ? null
      : viewLabel(view, multiCall.has(laneOf(view)));

  const visibleMessages = state
    ? filterMessagesByView(state.chat.messages, view)
    : [];

  // Focus is a transcript-rendering concern only — the side panels keep
  // deriving from the full message list (D12). Global-only: a scoped lane
  // view means "show me everything" (D11).
  const focusing = focusMode.focus && view === "global";
  const focused = focusing ? focusView(visibleMessages, isRunning) : null;
  const transcriptMessages = focused ? focused.visible : visibleMessages;

  // Remount token for the transcript. Switching conversation or view, toggling
  // focus, or a transcript that got shorter (truncation/compaction),
  // invalidates every measured row height and every scroll position —
  // remounting is the whole reset story: fresh scroll element, fresh
  // virtualizer, fresh pin state.
  const id = `${state?.conversation?.id ?? "none"}|${view}|${focusing ? "focus" : "all"}`;
  const [epoch, setEpoch] = useState<EpochState>({ id, count: 0, epoch: 0 });
  const next = nextEpoch(epoch, id, transcriptMessages.length);
  if (next !== epoch) setEpoch(next); // React's documented adjust-during-render
  const resetKey = epochKey(next);

  const stats = state ? adaptStats(state) : null;
  // Scope the Session panel to the watched view (Global keeps grand totals).
  const scopedStats = stats ? scopeStatsToView(stats, view) : null;
  const welcome = state ? adaptWelcome(state) : null;
  const bash = state ? adaptBashPanel(state.bash_panel) : null;
  // Todos & files derive from the transcript. In the global view todos form a
  // one-level tree: each lane's list labeled by lane, sub-agent todos nested
  // under the orchestrator item they were delegated from (todoTree). A scoped
  // view shows just that lane's flat, unlabeled list. Files stay global-mixed
  // by design.
  const todos =
    state && view === "global"
      ? todoTree(state.chat.messages)
      : flatTree(todosFromMessages(visibleMessages));
  const bg = state ? adaptBg(state) : [];
  const context = state ? adaptContext(state) : null;
  // The running sub-agent (pause feature): null for orchestrator-only turns.
  // Drives the TopBar chip and the Composer's Pause/Resume button.
  const subAgent = state ? adaptSubAgent(state) : null;
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
          boundAgent={boundAgent}
          active={view}
          onSelect={setView}
          roster={roster}
          laneCalls={laneCalls}
          laneMessages={laneMessages}
        />
      ),
      badge: roster.length > 0 ? roster.length : undefined,
    },
  ];

  // Narrow the canonical tab list to the server's `visible_tabs` (config/
  // profile rule, computed server-side — never re-derived here). An absent
  // field (older server) means "no rule yet" → every tab shows.
  const visibleTabs = filterTabs(tabs, state?.visible_tabs);
  const visibleTabIds = visibleTabs.map((t) => t.id);

  // The gear (and Ctrl+,) exist only when the server says Settings is
  // available (`settings_available`, false under a --profile pin) AND the
  // bound profile's `visible_tabs` still lists `settings` (§9.12). Both
  // fields are absent on older servers / before the first state frame:
  // absent settings_available = available; absent visible_tabs = no rule.
  const settingsEnabled =
    (state?.settings_available ?? true) &&
    (state?.visible_tabs === undefined || state.visible_tabs.includes("settings"));

  // Keep an open drawer pointed at a tab the server still shows. When the
  // visible set changes (e.g. the bound profile's `tabs` drops a tab out
  // from under an open drawer), relocate the active tab to the first
  // still-visible one. A closed drawer (null) stays closed — resolveActiveTab
  // never reopens it.
  useEffect(() => {
    if (drawerTab !== null && !visibleTabIds.includes(drawerTab)) {
      setDrawerTab(resolveActiveTab(drawerTab, visibleTabIds));
    }
  }, [drawerTab, visibleTabIds]);

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-zinc-950 text-zinc-100">
    <TopBar
        isRunning={isRunning}
        connected={connected}
        statusMessage={state?.status_message ?? null}
        subAgent={subAgent}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={welcome?.cwd ?? null}
        dirListing={dirListing}
        recentDirs={recentDirs}
        conversations={conversations}
        conversationsError={conversationsError}
        send={send}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
        onLoadConversation={loadConversation}
        notifyEnabled={notify.enabled}
        notifyPermission={notify.permission}
        onToggleNotify={notify.toggle}
        focusMode={focusMode.focus}
        onToggleFocus={focusMode.toggle}
        onOpenSettings={settingsEnabled ? settingsView.toggle : undefined}
      />
      {convoTabs && (
        <ConvoTabBar
          tabs={convoTabs.views}
          onActivate={convoTabs.activate}
          onClose={convoTabs.close}
          onNew={convoTabs.newTab}
        />
      )}

      {error && (
        <div className="bg-red-950/70 px-4 py-1.5 text-center text-xs text-red-300">
          {error}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <DropOverlay onAdd={attachments.add}>
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

          {transcriptMessages.length > 0 ? (
            <Transcript
              key={resetKey}
              messages={transcriptMessages}
              drawerOpen={drawerOpen}
              ref={transcriptRef}
            />
          ) : (
            <EmptyTranscript
              welcome={messageCount === 0 ? welcome : null}
              scopeLabel={scopeLabel}
            />
          )}

          {focusing && isRunning && <ActivityRow step={focused?.activity ?? null} />}

          <Composer
            isRunning={isRunning}
            connected={connected}
            commands={commands}
            onSend={(text, ids) => {
              // Sending always re-pins: you expect to see your own message.
              send({ type: "send_message", text, attachments: ids });
              transcriptRef.current?.jumpToLatest();
            }}
            onStop={() => send({ type: "stop" })}
            subAgent={subAgent}
            onPause={() => send({ type: "pause" })}
            onResume={() => send({ type: "resume" })}
            watchingRole={scopeLabel}
            onClearWatch={() => setView("global")}
            pendingInput={pendingInput}
            attachments={attachments}
            supportsVision={state?.supports_vision ?? true}
          />
        </DropOverlay>
      </div>

   <BottomBar
        conversations={conversations}
        conversationsError={conversationsError}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={welcome?.cwd ?? null}
        dirListing={dirListing}
        recentDirs={recentDirs}
        send={send}
        onSwitchModel={(alias) => send({ type: "switch_model", alias })}
        onLoadConversation={loadConversation}
      />
   

      <TabbedDrawer
        tabs={visibleTabs}
        active={drawerTab}
        onActiveChange={setDrawerTab}
      />

      {settingsView.open && settingsEnabled && (
        <Settings convo={state?.conversation?.id ?? null} onClose={settingsView.close} />
      )}
    </div>
  );
}
