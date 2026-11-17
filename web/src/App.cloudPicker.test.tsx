// @vitest-environment jsdom
//
// P5 — Task 3: App-level cloud picker flow.
// When noLocal is true, the App must:
//  • show NoMachines if there are no cloud targets
//  • auto-select the first cloud target (attach with profile=name@slug)
//  • leave local-profile behavior unchanged

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { AgentConnection } from "./useAgent";
import type { AppState } from "./state";

// Mock the useAgent hook
const mockSwitchConvo = vi.fn();
const mockState: AgentConnection = {
  connected: false,
  state: null,
  models: [],
  activeAlias: "",
  conversations: [],
  conversationsError: null,
  machines: [],
  commands: [],
  dirListing: null,
  recentDirs: [],
  noLocal: false,
  error: null,
  send: vi.fn(),
  convo: null,
  switchConvo: mockSwitchConvo,
} satisfies AgentConnection;

vi.mock("./useAgent", () => ({
  useAgent: () => mockState,
}));

// Mock cloud targets
const mockCloudTargets = {
  status: "ready" as const,
  rows: [] as { name: string; origin: string; overrides_global: boolean; agents: [] }[],
};
vi.mock("./cloud/targets", () => ({
  useCloudTargets: () => mockCloudTargets,
  toCloudRows: () => [],
}));

// Mock cloud store
vi.mock("./cloud/useCloud", () => ({
  useCloud: () => ({ snapshot: null, available: true, stale: false, busy: false, error: null, error_code: null, dismissed: new Set() }),
  useCloudDevices: () => [],
}));

import { App } from "./App";
import { COPY } from "./cloud/copy";

let root: Root | null = null;
let container: HTMLDivElement | null = null;

async function mountApp(): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<App />);
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));
});

beforeEach(() => {
  mockSwitchConvo.mockClear();
  mockState.noLocal = false;
  mockCloudTargets.rows = [];
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.unstubAllGlobals();
});

describe("App — cloud picker (noLocal=true)", () => {
  it("shows NoMachines when noLocal and no cloud targets", async () => {
    mockState.noLocal = true;
    mockCloudTargets.rows = [];
    await mountApp();
    expect(container!.textContent).toContain(COPY.empty.noMachines);
  });

  it("auto-selects first cloud target when noLocal and targets exist", async () => {
    mockState.noLocal = true;
    mockCloudTargets.rows = [
      { name: "alice@desk", origin: "remote", overrides_global: false, agents: [] },
    ];
    await mountApp();
    expect(mockSwitchConvo).toHaveBeenCalledWith(
      null,
      "alice@desk",
      undefined,
      expect.anything(),
    );
  });

  it("does not trigger cloud picker when local profiles exist", async () => {
    mockState.noLocal = false;
    mockState.state = {
      conversation: { id: "c1" },
      catalog: [{ name: "local", origin: "repo", overrides_global: false, agents: [] }],
      chat: [],
      todo: [],
      stats: null,
      context: null,
      slash_commands: [],
      agent: null,
    } as unknown as AppState;
    mockCloudTargets.rows = [];
    await mountApp();
    expect(mockSwitchConvo).not.toHaveBeenCalled();
  });
});