// @vitest-environment jsdom
//
// RED (remote-profiles T9): `useConvoTabs` calls `switchConvo(convo)`
// without the tab's profile. Written against the design contract
// (remote-profiles-design §6.4): "Tabs … `profile` is set from
// `state.selection.profile` (active tab) and list rows (`applyList`)" and
// "`useAgent.switchConvo(convo | null, profile?, agent?)`" — opening a
// conversation from the picker re-attaches it WITH the tab's profile, so a
// remote tab reopens as a relay, not a local session.
//
// Harness modeled on useConvoTabs.test.tsx (a stub agent + the same
// `useConvoTabs` hook; node environment, like that file).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useConvoTabs } from "./useConvoTabs";
import type { ConvoTab, TabsState } from "./convoTabs";
import type { ConversationSummary, InboundMessage } from "./state";
import type { AgentConnection } from "./useAgent";

interface StubAgent {
  connected: boolean;
  state: { is_running: boolean; selection?: { profile: string } | null } | null;
  models: unknown[];
  activeAlias: string;
  conversations: ConversationSummary[];
  conversationsError: string | null;
  commands: unknown[];
  dirListing: null;
  recentDirs: string[];
  error: string | null;
  send: ReturnType<typeof vi.fn<(msg: InboundMessage) => void>>;
  convo: string | null;
  switchConvo: ReturnType<typeof vi.fn<(id: string | null) => void>>;
}

function makeAgent(over: Partial<StubAgent> = {}): StubAgent {
  return {
    connected: true,
    state: { is_running: false },
    models: [],
    activeAlias: "gpt",
    conversations: [],
    conversationsError: null,
    commands: [],
    dirListing: null,
    recentDirs: [],
    error: null,
    send: vi.fn(),
    convo: null,
    switchConvo: vi.fn(),
    ...over,
  };
}

let api: ReturnType<typeof useConvoTabs>;
let container: HTMLDivElement | null = null;
let root: Root | null = null;

function Harness({ agent, initial }: { agent: StubAgent; initial: TabsState | null }) {
  api = useConvoTabs(agent as unknown as AgentConnection, initial, null);
  return null;
}

async function renderHook(agent: StubAgent, initial: TabsState | null): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<Harness agent={agent} initial={initial} />);
  });
}

const tab = (convo: string | null, title = "", profile: string | null = null): ConvoTab => ({
  id: `id-${convo ?? "new"}`,
  convo,
  title,
  profile,
});

const state = (tabs: ConvoTab[], active: number): TabsState => ({ tabs, active });

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useConvoTabs — opening a tab carries its profile (remote-profiles T9)", () => {
  it("open() of a REMOTE tab re-attaches with the tab's profile", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B", "home-dev")], 0);

    await renderHook(agent, initial);

    (api as { open: (c: string) => void }).open("b");

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b", "home-dev");
  });

  it("open() of a local tab re-attaches with a null profile", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A", "dev"), tab("b", "B")], 0);

    await renderHook(agent, initial);

    (api as { open: (c: string) => void }).open("b");

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b", null);
  });

  it("activate() of a remote tab re-attaches with its profile too", async () => {
    const agent = makeAgent({ convo: "a" });
    const initial = state([tab("a", "A"), tab("b", "B", "home-dev")], 0);

    await renderHook(agent, initial);

    (api as { activate: (i: number) => void }).activate(1);

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith("b", "home-dev");
  });

  it("re-opening the active tab sends nothing", async () => {
    const agent = makeAgent({ convo: "b" });
    const initial = state([tab("a", "A"), tab("b", "B", "home-dev")], 1);

    await renderHook(agent, initial);

    (api as { open: (c: string) => void }).open("b");

    expect(agent.switchConvo).not.toHaveBeenCalled();
  });
});
