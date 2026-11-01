// TopBar wiring — remote kill frame (remote-profiles T9).
//
// RED until T9. Written against the design contract
// (remote-profiles-design §5.6, §6.4): killing a remote row from the
// conversation picker sends `kill_session{convo, profile: L}` (the server
// routes it to the remote's `POST /api/conversations/{id}/kill`); a local
// row keeps the profile-less frame. Today the wiring is
// `onKill={(id) => send({ type: "kill_session", convo: id })}` — the
// profile is dropped.
//
// Harness modeled on the TopBar section of selectionChip.test.tsx
// (barBase() props; jsdom for src/components/**).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { TopBar } from "./TopBar";
import type { ConversationSummary, InboundMessage, ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixtures ───────────────────────────────────────────────────────────

const CATALOG: CatalogProfile[] = [
  { name: "dev", origin: "global", overridesGlobal: false, agents: [], color: "teal" },
  { name: "home-dev", origin: "remote", overridesGlobal: false, agents: [], color: "pink" },
];

const SEL: Selection = { profile: "dev", agent: "team", subagentModels: {} };

const MODELS: ModelInfo[] = [
  { alias: "sonnet", provider_name: "anthropic", model_name: "claude-sonnet", context_size: 200000 },
];

const row = (
  id: string,
  over: Partial<ConversationSummary> & { running?: boolean } = {},
): ConversationSummary =>
  ({
    id,
    name: `convo ${id}`,
    updated_at: "2026-10-04T00:00:00Z",
    message_count: 1,
    model: "m",
    active: false,
    ...over,
  } as ConversationSummary);

function barBase(over: Partial<{
  conversations: ConversationSummary[];
  send: ReturnType<typeof vi.fn<(m: InboundMessage) => void>>;
}> = {}) {
  return {
    isRunning: false,
    connected: true,
    statusMessage: null,
    models: MODELS,
    activeAlias: "sonnet",
    hasTranscript: false,
    cwd: null,
    dirListing: null,
    recentDirs: [],
    conversations: [] as ConversationSummary[],
    conversationsError: null,
    send: vi.fn<(m: InboundMessage) => void>(),
    onLoadConversation: vi.fn(),
    notifyEnabled: false,
    notifyPermission: "default" as const,
    onToggleNotify: vi.fn(),
    focusMode: false,
    onToggleFocus: vi.fn(),
    ...over,
  };
}

const chatChips = {
  catalog: CATALOG,
  selection: SEL,
  profilePinned: false,
  onToast: vi.fn(),
};

// ─── mount plumbing ─────────────────────────────────────────────────────

let root: Root | null = null;
let host: HTMLElement | null = null;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  delete (window as unknown as { isTauri?: boolean }).isTauri;
  vi.restoreAllMocks();
});

/** Open the conversations dropdown with `props`, then let the list "land"
 * (a fresh `conversations` reference clears the loading state). */
async function openPicker(props: Parameters<typeof TopBar>[0]): Promise<void> {
  const chip = [...host!.querySelectorAll("button")].find((b) =>
    b.textContent?.includes("conversations"),
  );
  expect(chip, "the conversations chip renders").not.toBeNull();
  await act(async () => {
    chip!.click();
  });
  await act(async () => {
    root!.render(
      <TopBar {...props} conversations={[...props.conversations]} />,
    );
  });
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("TopBar — kill_session carries the row's profile (remote-profiles T9)", () => {
  it("killing a REMOTE row sends kill_session{convo, profile}", async () => {
    const send = vi.fn<(m: InboundMessage) => void>();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const props = {
      ...barBase({
        send,
        conversations: [row("a1", { active: true, profile: "home-dev" })],
      }),
      ...chatChips,
    } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    await openPicker(props);

    const killBtn = [...host!.querySelectorAll<HTMLElement>(".absolute button")].find((b) =>
      b.textContent?.includes("convo a1"),
    )!
      .closest("div.group")!
      .querySelector('button[title="Kill this live session"]');
    expect(killBtn, "the kill button renders on the active row").not.toBeNull();
    await act(async () => {
      (killBtn as HTMLElement).click();
    });

    expect(window.confirm).toHaveBeenCalled();
    const kills = send.mock.calls.map((c) => c[0]).filter(
      (m) => m.type === "kill_session",
    );
    expect(kills, "exactly one kill_session frame").toHaveLength(1);
    expect(kills[0]).toStrictEqual({
      type: "kill_session",
      convo: "a1",
      profile: "home-dev",
    });
  });

  it("killing a LOCAL row sends the profile-less kill_session (unchanged)", async () => {
    const send = vi.fn<(m: InboundMessage) => void>();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const props = {
      ...barBase({
        send,
        conversations: [row("b2", { active: true })],
      }),
      ...chatChips,
    } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    await openPicker(props);

    const killBtn = [...host!.querySelectorAll<HTMLElement>(".absolute button")].find((b) =>
      b.textContent?.includes("convo b2"),
    )!
      .closest("div.group")!
      .querySelector('button[title="Kill this live session"]');
    expect(killBtn, "the kill button renders on the active row").not.toBeNull();
    await act(async () => {
      (killBtn as HTMLElement).click();
    });

    const kills = send.mock.calls.map((c) => c[0]).filter(
      (m) => m.type === "kill_session",
    );
    expect(kills, "exactly one kill_session frame").toHaveLength(1);
    // No `profile` key at all — the local kill path is unchanged.
    expect(kills[0]).toStrictEqual({ type: "kill_session", convo: "b2" });
  });
});

// ─── pre-attach relay errors are visible in the remote tab ──────────────
//
// The relay answers attach-time failures (E-DOWN, E-VERSION, E-AUTH, …)
// with one `error` frame BEFORE `attached`/`state` — the tab has no
// transcript yet. §9 T9: that error must still be visible (the existing
// error UI), not swallowed by the empty state. App's banner renders
// `agent.error` unconditionally; this guards that against the no-state
// remote tab. (useAgent/useAttachments are mocked, App.tabs.test.tsx
// pattern — the point is the banner, not the socket.)

const hoisted = vi.hoisted(() => ({ agent: null as unknown }));

vi.mock("../useAgent", () => ({
  useAgent: () => hoisted.agent,
  setConvoInUrl: vi.fn(),
}));
vi.mock("../uploads", () => ({ uploadFile: vi.fn() }));

import { App } from "../App";
import type { AppState } from "../state";

describe("App — a pre-attach relay error is visible (remote-profiles T9)", () => {
  it("shows the relay's attach-time error with no state bound yet", async () => {
    // jsdom has no matchMedia (useMediaQuery in the tree needs it).
    if (!window.matchMedia) {
      window.matchMedia = ((q: string) => ({
        matches: false,
        media: q,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        onchange: null,
        dispatchEvent: () => false,
      })) as unknown as typeof window.matchMedia;
    }
    hoisted.agent = {
      connected: false,
      state: null as AppState | null,
      models: [],
      activeAlias: "",
      conversations: [],
      conversationsError: null,
      commands: [],
      dirListing: null,
      recentDirs: [],
      error: "Remote profile 'home-dev': remote PeakBot speaks protocol v0, this PeakBot speaks v1 — upgrade one side so they match.",
      send: vi.fn(),
      convo: null,
      switchConvo: vi.fn(),
    };

    mount(<App />);

    expect(host!.textContent).toContain(
      "Remote profile 'home-dev': remote PeakBot speaks protocol v0, this PeakBot speaks v1",
    );
  });
});
