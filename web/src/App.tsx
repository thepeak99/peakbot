// Phase 1 — live web UI. Connects to the agent over WebSocket (`useAgent`),
// adapts each `AppState` frame into the view model (`adapt.ts`), and renders
// the component tree. Sending, Stop, the working spinner, and every side
// panel are driven by live state.
//
// Side panels: one `drawerTab` state, two surfaces — the desktop
// `TabbedDrawer` rail (≥1024px) or the phone's `PanelsSheet` (<1024px),
// picked by `useMediaQuery`. The Todo badge is per surface: the row count on
// desktop, the open count on a phone.

import { useCallback, useEffect, useRef, useState } from "react";
import { Transcript, type TranscriptHandle } from "./components/Transcript";
import { EmptyTranscript } from "./components/EmptyTranscript";
import { UpdateBanner } from "./components/UpdateBanner";
import { CloudRoot } from "./cloud/CloudRoot";
import { CLOUD_SETTINGS_EVENT } from "./cloud/openCloud";
import { useCloudTargets } from "./cloud/targets";
import { NoMachines } from "./cloud/NoMachines";
import { BashPanel } from "./components/BashPanel";
import {
  ComposerSlot,
  type ComposerLive,
  type ComposerSlotHandle,
} from "./components/ComposerSlot";
import { DEFAULT_UPLOAD_LIMITS } from "./useAttachments";
import { DropOverlay } from "./components/DropOverlay";
import { TopBar } from "./components/TopBar";
import { ConvoTabBar } from "./components/ConvoTabBar";
import { BottomBar } from "./components/BottomBar";
import { ActivityRow } from "./components/ActivityRow";
import { TabbedDrawer } from "./components/TabbedDrawer";
import { PanelsSheet } from "./components/PanelsSheet";
import { DirectoryBrowser } from "./components/DirectoryBrowser";
import { StatsPanel } from "./components/StatsPanel";
import { TodoPanel } from "./components/TodoPanel";
import { BgPanel } from "./components/BgPanel";
import { FilesPanel } from "./components/FilesPanel";
import { AgentsPanel } from "./components/AgentsPanel";
import { Settings, useSettingsToggle } from "./settings/Settings";
import { useAgent } from "./useAgent";
import { useBackDismiss } from "./useBackDismiss";
import { useSessionVerbs } from "./useSessionVerbs";
import { apiBase } from "./state";
import { useConvoTabs } from "./useConvoTabs";
import type { TabsState } from "./convoTabs";
import { useTaskNotifications } from "./useTaskNotifications";
import { useTabIndicator } from "./useTabIndicator";
import { useFocusMode } from "./useFocusMode";
import { useLayout } from "./useLayout";

import { useMobileChats } from "./useMobileChats";
import { useScreenSlide } from "./useScreenSlide";
import { ChatHeader } from "./components/ChatHeader";
import { ChatList } from "./components/ChatList";
import { SelectionChip } from "./components/SelectionChip";
import { focusView } from "./focus";
import { epochKey, nextEpoch, type EpochState } from "./transcriptEpoch";
import { filterTabs, resolveActiveTab, settingsGearEnabled, tabVisible } from "./tabs";
import { isRemote } from "./state";
import { edition } from "./desktop";
import type { ViewFilter } from "./types";
import {
  adaptBashPanel,
  adaptBg,
  adaptCatalog,
  adaptContext,
  adaptSelection,
  adaptStats,
  adaptSubAgent,
  adaptWelcome,
  boundAgentOf,
  conversationHasTurns,
  deriveSubAgentRoster,
  filesFromMessages,
  filterMessagesByView,
  flatTree,
  laneOf,
  messagesByLane,
  scopeStatsToView,
  todoCounts,
  todoTree,
  todosFromMessages,
  viewLabel,
} from "./adapt";

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
    noLocal,
    error,
    send,
    switchConvo,
  } = agent;

  // P5 — cloud target picker (design §4). When noLocal is true (zero-local
  // config), derive cloud targets and either auto-select the first or show
  // the empty state.
  const cloudTargets = useCloudTargets();
  useEffect(() => {
    if (!noLocal) return;
    if (cloudTargets.status === "loading") return;
    if (cloudTargets.rows.length > 0) {
      // Auto-select the first cloud target (design: deterministic, no
      // last-used persistence).
      switchConvo(null, cloudTargets.rows[0].name, undefined, { type: "attach" });
    }
  }, [noLocal, connected, cloudTargets, switchConvo]);

  // The drop overlay on <main> feeds the active tab's composer slot.
  const activeSlot = useRef<ComposerSlotHandle>(null);

  const isRunning = state?.is_running ?? false;
  // A slash command (e.g. /compact) sets is_running WITHOUT adding a user row
  // — the trailing turn is already finished. Focus mode must not read that as
  // a live turn, or it hides the last answer. Composer/spinner/Stop keep the
  // plain is_running; only the focus view uses turnInFlight.
  const commandRunning = state?.command_running ?? false;
  const turnInFlight = isRunning && !commandRunning;
  // Tab favicon + title badge while the agent works.
  useTabIndicator(isRunning);

  // Follow the newest message, but only while the user is pinned to the bottom
  // — the transcript owns that now (see `Transcript` + `useTranscriptScroll`).
  const messageCount = state?.chat?.messages?.length ?? 0;
  const transcriptRef = useRef<TranscriptHandle>(null);

  const notify = useTaskNotifications(isRunning);
  const hasTranscript = messageCount > 0;
  const selectionLocked = conversationHasTurns(state?.chat.messages ?? []);
  // Offered once the conversation has a transcript; `state` always describes
  // the bound conversation (useAgent), so its name is the title.
  const convoTitle = hasTranscript ? (state?.conversation?.name ?? null) : null;
  const convoTabs = useConvoTabs(agent, initialTabs, convoTitle);
  // A picker row opens under ITS profile (null = local), so a remote row
  // reattaches as a relay rather than a local session (design §6.4).
  const loadConversation = convoTabs ? convoTabs.open : switchConvo;
  // The chip's local↔remote pick: the conversation is fresh (the chip locks
  // after the first turn), so start a new one on that pair. `agent` is
  // omitted for the bare remote row — the remote resolves its own.
  const reattach = useCallback(
    (profile: string, agent?: string) => switchConvo(null, profile, agent),
    [switchConvo],
  );
  const [drawerTab, setDrawerTab] = useState<string | null>(null);
  const drawerOpen = drawerTab !== null;
  // Settings is an in-app VIEW (not a route): this state lives here so the
  // chat and its WebSocket stay mounted underneath. Gear + Ctrl+, / ⌘+, +
  // Esc are owned by the hook.
  const settingsView = useSettingsToggle();
  // TrustBanner → "Open Cloud settings": open Settings AND jump to the Cloud
  // pane (counter prop, so repeat clicks re-trigger).
  const [cloudFocus, setCloudFocus] = useState(0);
  const showSettings = settingsView.show; // stable useCallback
  useEffect(() => {
    function openCloudPane(): void {
      showSettings();
      setCloudFocus((n) => n + 1);
    }
    window.addEventListener(CLOUD_SETTINGS_EVENT, openCloudPane);
    return () => window.removeEventListener(CLOUD_SETTINGS_EVENT, openCloudPane);
  }, [showSettings]);
  // Back closes the overlay instead of leaving the page (on a phone the OS
  // Back at the SPA root exits the app). `settingsView.close` is a stable
  // useCallback, as the hook requires.
  useBackDismiss(settingsView.open, settingsView.close);

  // Catalog and selection (0.24): `catalog` is the profiles/agents/sub-agents
  // this install declares; `selection` is the (profile, agent) this
  // conversation is bound to. `boundAgent` resolves the two — the roster the
  // Agents panel renders. `view` scopes the transcript, todos, files and
  // stats to one lane via the message `source`.
  const catalog = state ? adaptCatalog(state) : [];
  const selection = state ? adaptSelection(state) : null;
  const boundAgent = boundAgentOf(catalog, selection);
  // `/r/<L>` prefix for uploads and attachment URLs; derived, never stored.
  const base = apiBase(selection?.profile ?? null, catalog);
  const [view, setView] = useState<ViewFilter>("global");
  // Called once here (not in TopBar) so App can filter with the same value
  // the button renders — two useState copies would desync (plan §6.1).
  const focusMode = useFocusMode();
  // The desktop drawer rail (visible at any width in the desktop shell; the
  // shell's useLayout sets data-layout="desktop" regardless of viewport).
  // On phones: the Panels sheet instead.
  // The hook is always called, so hook order stays stable.
  
  const phone = useLayout() === "phone";

  // Sub-agent roster: one entry per delegation call, in call order (Phase 2 +
  // per-call). Zero backend — derived from the transcript. A role delegated to
  // more than once yields several entries; the panel suffixes those "#n".
  const roster = state ? deriveSubAgentRoster(state.chat?.messages ?? []) : [];
  const multiCall = new Set(
    roster.filter((r) => r.n > 1).map((r) => r.role),
  );
  const scopeLabel =
    view === "global"
      ? null
      : viewLabel(view, multiCall.has(laneOf(view)));

  const chat = Array.isArray(state?.chat) ? state.chat : (state?.chat?.messages ?? []);
  const visibleMessages = state
    ? filterMessagesByView(chat, view)
    : [];

  // Focus is a transcript-rendering concern only — the side panels keep
  // deriving from the full message list (D12). Global-only: a scoped lane
  // view means "show me everything" (D11).
  const focusing = focusMode.focus && view === "global";
  const focused = focusing ? focusView(visibleMessages, turnInFlight) : null;
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
  // The cwd the user may see and switch, or null. The Local edition has no cwd
  // of its own (the shell owns it) unless the profile is remote; a profile hides
  // it via ui.tabs "directory". This one value drives the phone row item, the
  // Directory tab, the desktop TopBar picker and the mobile-shell footer picker.
  const browsableCwd =
    welcome?.cwd &&
    tabVisible("directory", state?.visible_tabs) &&
    (edition() !== "local" || isRemote(selection?.profile ?? null, catalog))
      ? welcome.cwd
      : null;
  const bash = state?.bash_panel ? adaptBashPanel(state.bash_panel) : null;
  // Todos & files derive from the transcript. In the global view todos form a
  // one-level tree: each lane's list labeled by lane, sub-agent todos nested
  // under the orchestrator item they were delegated from (todoTree). A scoped
  // view shows just that lane's flat, unlabeled list. Files stay global-mixed
  // by design.
  const todos =
    state && view === "global"
      ? todoTree(chat)
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
  // A session verb typed while a turn runs (or input is queued) would be
  // server-queued and lost on Stop — the verb hook re-attaches instead.
  const busy = isRunning || pendingInput > 0;

  // One-shot toast (e.g. `Next conversation: …` from the selection chip).
  // A minimal local sink — the app has no global toast system; the Settings
  // view keeps its own.
  const [toast, setToast] = useState<string | null>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const showToast = useCallback((message: string) => {
    setToast(message);
    if (toastTimer.current) clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(null), 4000);
  }, []);
  useEffect(() => () => {
    if (toastTimer.current) clearTimeout(toastTimer.current);
  }, []);

  // Composer submits and cwd-picker commits route through the session-verb
  // hook so a session verb never reaches a busy session.
  const { submit, switchCwd } = useSessionVerbs({
    agent,
    busy,
    cwd: welcome?.cwd ?? "",
    loadConversation,
    onError: showToast,
  });

  // Stable for the Sheet's back layer (useBackDismiss re-pushes on a new
  // identity).
  const closePanels = useCallback(() => setDrawerTab(null), []);

  const directoryTab =
    phone && browsableCwd
      ? {
          id: "directory",
          label: "Directory",
          content: (
            <DirectoryBrowser
              phone
              cwd={browsableCwd}
              hasTranscript={hasTranscript}
              dirListing={dirListing}
              recentDirs={recentDirs}
              send={send}
              onSwitchCwd={switchCwd}
              onDone={closePanels}
            />
          ),
        }
      : null;
  const tabs = [
    ...(directoryTab ? [directoryTab] : []),
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
            phone={phone}
          />
        ) : null,
    },
    {
      id: "todo",
      label: "Todo",
      content: <TodoPanel nodes={todos} phone={phone} />,
      // Per surface: every rendered row on desktop, the OPEN count on a
      // phone (the pill badge and the panels badge both read it).
      badge: !phone
        ? todos.reduce((n, node) => n + 1 + node.children.length, 0)
        : todoCounts(todos).open,
    },
    {
      id: "files",
      label: "Files",
      content: <FilesPanel files={files} phone={phone} />,
      badge: files.length,
    },
    {
      id: "tasks",
      label: "Tasks",
      content: <BgPanel processes={bg} phone={phone} />,
      badge: runningBg,
    },
    {
      id: "bash",
      label: "Bash",
      content: bash ? (
        <BashPanel panel={bash} phone={phone} />
      ) : (
        <p className="text-sm text-zinc-600 phone:py-8 phone:text-center phone:text-zinc-500">
          No bash command yet.
        </p>
      ),
      // Desktop-only: the phone mock's Bash pill carries no badge (C17).
      badge: !phone && bash?.status === "running" ? 1 : undefined,
    },
    {
      id: "agents",
      label: "Agents",
      content: (
        <AgentsPanel
          boundAgent={boundAgent}
          active={view}
          // On a phone, Watch also closes the sheet to reveal the banner (X6).
          onSelect={!phone ? setView : (v) => { setView(v); closePanels(); }}
          roster={roster}
          laneCalls={laneCalls}
          laneMessages={laneMessages}
          phone={phone}
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

  // The phone's combined badge: running tasks + open todos, each counted only
  // when its tab is visible (a count that opens a hidden tab is a lie). The
  // button opens the FIRST visible tab; a count tap opens its own.
  const shown = (id: string): boolean => visibleTabIds.includes(id);
  const tasksN = shown("tasks") ? runningBg : 0;
  const { open, total } = shown("todo") ? todoCounts(todos) : { open: 0, total: 0 };
  const panels =
    phone && visibleTabs.length > 0
      ? { count: tasksN + open, onOpen: () => setDrawerTab(visibleTabIds[0]) }
      : undefined;

  // The gear (and Ctrl+,) exist only when the server says Settings is
  // available (`settings_available`, false under a --profile pin) AND the
  // bound profile's `visible_tabs` still lists `settings` (§9.12). Both
  // fields are absent on older servers / before the first state frame:
  // absent settings_available = available; absent visible_tabs = no rule.
  // The rule itself lives in tabs.settingsGearEnabled (pure, unit-tested).
  const settingsEnabled = settingsGearEnabled(
    state?.settings_available,
    state?.visible_tabs,
  );

  // The phone's two screens: the list overlay plus the always-mounted
  // conversation. `showToast` is stable; `null` outside the mobile shell.
  const chats = useMobileChats(agent, showToast);

  // Each overlay stays mounted (inert) through its WAAPI exit and unmounts
  // once its state is `gone`.
  const chatListRef = useRef<HTMLElement>(null);
  const chatListSlide = useScreenSlide(chats?.screen === "chats", chatListRef, "left");
  const settingsRef = useRef<HTMLDivElement>(null);
  const settingsSlide = useScreenSlide(
    settingsView.open && settingsEnabled,
    settingsRef,
    "right",
  );

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

  const live: ComposerLive = {
    isRunning,
    connected,
    commands,
    onSend: (text, ids) => {
      // Sending always re-pins: you expect to see your own message.
      submit(text, ids);
      transcriptRef.current?.jumpToLatest();
    },
    onStop: () => send({ type: "stop" }),
    subAgent,
    onPause: () => send({ type: "pause" }),
    onResume: () => send({ type: "resume" }),
    watchingRole: scopeLabel,
    onClearWatch: () => setView("global"),
    pendingInput,
    supportsVision: state?.supports_vision ?? true,
    settingsEnabled,
    stopPending: state?.stop_pending ?? false,
    // One switch, decided once: the phone composer face (fidelity batch 1).
    phone,
    uploadLimits: state?.upload_limits ?? DEFAULT_UPLOAD_LIMITS,
  };

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-zinc-950 text-zinc-100">
      {/* While the Chats list covers the conversation, the composer blurs
          (keyboard closes) and the screen is hidden from screen readers. */}
      <div className="contents" inert={chats?.screen === "chats"}>
      {chats ? (
        <ChatHeader
          title={convoTitle ?? "New chat"}
          selector={
            <SelectionChip
              sheet
              catalog={catalog}
              selection={selection}
              models={models}
              activeAlias={stats?.modelAlias || activeAlias}
              locked={selectionLocked}
              pinned={state?.profile_pinned ?? false}
              send={send}
              onToast={showToast}
              onReattach={reattach}
            />
          }
          onBack={chats.showChats}
          connected={connected}
          isRunning={isRunning}
          focusMode={focusMode.focus}
          onToggleFocus={focusMode.toggle}
          panels={panels}
        />
      ) : (
        <TopBar
        isRunning={isRunning}
        connected={connected}
        statusMessage={state?.status_message ?? null}
        subAgent={subAgent}
        models={models}
        activeAlias={stats?.modelAlias || activeAlias}
        hasTranscript={hasTranscript}
        cwd={browsableCwd}
        dirListing={dirListing}
        recentDirs={recentDirs}
        conversations={conversations}
        conversationsError={conversationsError}
        send={send}
        onSwitchCwd={switchCwd}
        onLoadConversation={loadConversation}
        onReattach={reattach}
        notify={notify}
        focusMode={focusMode.focus}
        onToggleFocus={focusMode.toggle}
        onOpenSettings={settingsEnabled ? settingsView.toggle : undefined}
        catalog={catalog}
        selection={selection}
        selectionLocked={selectionLocked}
        profilePinned={state?.profile_pinned ?? false}
        onToast={showToast}
        stopPending={state?.stop_pending ?? false}
        panels={panels}
      />
      )}
      <UpdateBanner />
      <CloudRoot />
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
        {noLocal && cloudTargets.status === "ready" && cloudTargets.rows.length === 0 ? (
          <NoMachines />
        ) : (
          <DropOverlay onAdd={(f) => activeSlot.current?.add(f)}>
          {/* The scope banner is a static bar above the transcript: the
              scrolling element's only child must be the virtualizer's sized
              container, or the row offsets stop matching the scroll offset. */}
          {scopeLabel && (
            <div className="desktop:mr-12 px-4 pt-4 phone:p-0 sm:px-6 md:px-8">
              <div className="mx-auto flex max-w-5xl items-center justify-between rounded-md border border-sky-900/60 bg-sky-950/30 px-3 py-1.5 text-xs text-sky-300 phone:mx-0 phone:h-11 phone:max-w-none phone:gap-2 phone:rounded-none phone:border-0 phone:border-b phone:border-sky-400/35 phone:bg-sky-400/12 phone:px-4 phone:py-0 phone:text-[13.5px] phone:text-zinc-100 phone:light:bg-sky-700/8">
                <span>
                  👁 Watching <span className="font-medium phone:font-semibold phone:text-sky-400">{scopeLabel}</span>
                  <span className="ml-2 text-sky-500/70 phone:hidden">
                    {visibleMessages.length} message
                    {visibleMessages.length === 1 ? "" : "s"}
                  </span>
                </span>
                <button
                  onClick={() => setView("global")}
                  className="cursor-pointer rounded px-1.5 py-0.5 text-sky-400 hover:bg-sky-900/40 phone:h-8 phone:rounded-2xl phone:bg-sky-400/18 phone:px-3 phone:py-0 phone:text-[13px] phone:font-semibold"
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
              base={base}
              phone={phone}
              ref={transcriptRef}
            />
          ) : (
            <EmptyTranscript
              welcome={messageCount === 0 ? welcome : null}
              scopeLabel={scopeLabel}
            />
          )}

          {!phone && focusing && turnInFlight && (
            <ActivityRow line={{ kind: "step", step: focused?.activity ?? null }} />
          )}
          {phone && (
            <ActivityRow
              line={
                isRunning
                  ? focusing && turnInFlight
                    ? { kind: "step", step: focused?.activity ?? null }
                    : { kind: "working" }
                  : { kind: "idle" }
              }
              status={{
                tasks: tasksN,
                todosDone: total - open,
                todosTotal: total,
                cwd: browsableCwd,
                onOpen: setDrawerTab,
              }}
            />
          )}

          {convoTabs ? (
            // Keep-alive: every tab's composer stays mounted, only the active
            // one is shown and wired to live state.
            convoTabs.state.tabs.map((t, i) => {
              const on = i === convoTabs.state.active;
              return (
                <ComposerSlot
                  key={t.id}
                  convo={t.convo}
                  base={on ? base : apiBase(t.profile, catalog)}
                  live={on ? live : null}
                  ref={on ? activeSlot : undefined}
                />
              );
            })
          ) : (
            // One composer for every conversation: the draftKey parks each
            // conversation's half-typed text (see Composer). Mobile shell only
            // — a plain browser keeps the keep-alive contract, where a
            // server-side conversation move (e.g. /new) keeps the typed text.
            <ComposerSlot
              ref={activeSlot}
              convo={state?.conversation?.id ?? null}
              base={base}
              live={live}
              draftKey={chats ? agent.convo : undefined}
            />
          )}
        </DropOverlay>
        )}
      </div>

   {/* The phone app (mobileChats) drops the footer entirely at phone width —
       cwd is reached via the activity-row 📁 → Directory tab. At ≥lg the
       footer stays (existing behavior). */}
      {chats === null && (
        <BottomBar
          conversations={conversations}
          conversationsError={conversationsError}
          models={models}
          activeAlias={stats?.modelAlias || activeAlias}
          hasTranscript={hasTranscript}
          cwd={browsableCwd}
          dirListing={dirListing}
          recentDirs={recentDirs}
          send={send}
          onLoadConversation={loadConversation}
          onReattach={reattach}
          onSwitchCwd={switchCwd}
          catalog={catalog}
          selection={selection}
          selectionLocked={selectionLocked}
          profilePinned={state?.profile_pinned ?? false}
          onToast={showToast}
        />
      )}

      {/* One open-panel state, two surfaces; the relocation effect above
          covers both. */}
      {!phone ? (
        <TabbedDrawer
          tabs={visibleTabs}
          active={drawerTab}
          onActiveChange={setDrawerTab}
        />
      ) : (
        drawerTab !== null && (
          <PanelsSheet
            tabs={visibleTabs}
            active={drawerTab}
            onActiveChange={setDrawerTab}
            onClose={closePanels}
            running={isRunning}
            onStop={() => send({ type: "stop" })}
          />
        )
      )}
      </div>

      {chats && chatListSlide !== "gone" && (
        <ChatList
          ref={chatListRef}
          leaving={chatListSlide === "leaving"}
          conversations={conversations}
          catalog={catalog}
          loaded={chats.loaded}
          unread={chats.unread}
          connected={connected}
          error={error}
          onOpen={chats.open}
          onNew={chats.newChat}
          onOpenSettings={settingsEnabled ? settingsView.toggle : undefined}
          machineStatus={agent.machines}
        />
      )}

      {settingsSlide !== "gone" && (
        <Settings
          ref={settingsRef}
          leaving={settingsSlide === "leaving"}
          convo={state?.conversation?.id ?? null}
          activeProfile={selection?.profile ?? null}
          onClose={settingsView.close}
          cloudFocus={cloudFocus}
        />
      )}

      {toast && (
        <div className="fixed bottom-16 left-1/2 z-40 -translate-x-1/2 rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-xs text-zinc-200 shadow-xl">
          {toast}
        </div>
      )}
    </div>
  );
}
