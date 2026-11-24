// @vitest-environment jsdom
//
// P5 — regression: the zero-local cloud picker, end to end through the REAL
// cloud store (FakeEventSource), `useCloudDevices` and `useCloudTargets`.
// Only `useAgent` is mocked. Guards three bugs:
//  • peers arriving over SSE after mount must become rows (the server's
//    profiles body is `{ "profiles": [...] }`, not a bare array);
//  • a non-active snapshot must not cause a render loop (vitest hang);
//  • an identical state re-sent (new array identity) must not refetch.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { AgentConnection } from "../useAgent";
import type { AppState, WireProfileInfo } from "../state";
import { store } from "./store";
import {
  active,
  connecting,
  device,
  FakeEventSource,
  installFakeEventSource,
  json,
  mockFetch,
  selfDevice,
  uninstallFakeEventSource,
} from "./testkit";

const mockSwitchConvo = vi.fn();
const mockAgent: AgentConnection = {
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
  noLocal: true,
  error: null,
  send: vi.fn(),
  convo: null,
  switchConvo: mockSwitchConvo,
} satisfies AgentConnection;

vi.mock("../useAgent", () => ({ useAgent: () => mockAgent }));

import { App } from "../App";
import { COPY } from "./copy";

const SANDWORM = device({ id: "5a".repeat(32), slug: "sandworm", name: "sandworm" });

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let fetchMock: ReturnType<typeof mockFetch>;

const profileCalls = (): string[] =>
  fetchMock.mock.calls.map((c) => String(c[0])).filter((u) => u.endsWith("/profiles"));

/** Let pending promises (fetch → res.json → setState) run, inside act. */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

async function mountApp(): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<App />);
  });
  await flush(); // the store's availability probe → EventSource opens
}

async function push(body: unknown): Promise<void> {
  const es = FakeEventSource.last();
  expect(es).toBeDefined();
  await act(async () => {
    es!.emit("state", body);
  });
  await flush();
}

const showsNoMachines = (): boolean =>
  container!.textContent!.includes(COPY.empty.noMachines);

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  installFakeEventSource();
  FakeEventSource.reset();
  mockSwitchConvo.mockClear();
  fetchMock = mockFetch((url) => {
    if (url.endsWith("/profiles")) {
      // The real wire shape (src/ui/web/cloud_api.rs `profiles`).
      return Promise.resolve(json(200, { profiles: [{ name: "default", color: "blue" }] }));
    }
    return Promise.resolve(json(200, {}));
  });
});

afterEach(async () => {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  store.reset();
  uninstallFakeEventSource();
});

describe("App — cloud picker over the live cloud store", () => {
  it("(a) a peer arriving over SSE after mount is auto-selected; NoMachines goes away", async () => {
    await mountApp();
    expect(showsNoMachines()).toBe(true);

    await push(active([selfDevice(), SANDWORM]));

    expect(profileCalls()).toEqual([`/api/cloud/devices/${SANDWORM.id}/profiles`]);
    expect(mockSwitchConvo).toHaveBeenCalledWith(null, "default@sandworm", undefined, {
      type: "attach",
    });
    expect(showsNoMachines()).toBe(false);
  });

  it("(b) a non-active snapshot settles on NoMachines without a render loop", async () => {
    await mountApp();
    await push(connecting());

    expect(showsNoMachines()).toBe(true);
    expect(profileCalls()).toEqual([]);
    expect(mockSwitchConvo).not.toHaveBeenCalled();
  });

  it("(c) the identical state pushed twice fetches profiles once and selects once", async () => {
    await mountApp();
    await push(active([selfDevice(), SANDWORM]));
    await push(active([selfDevice(), SANDWORM])); // new array identity, same content

    expect(profileCalls()).toHaveLength(1);
    expect(mockSwitchConvo).toHaveBeenCalledTimes(1);
  });

  it("(d) self and offline devices are excluded; online:null counts as eligible", async () => {
    const offline = device({ id: "0f".repeat(32), slug: "attic", online: false });
    const unknown = device({ id: "3c".repeat(32), slug: "garage", online: null });
    await mountApp();
    await push(active([selfDevice({ id: "11".repeat(32), slug: "phone" }), offline, unknown]));

    expect(profileCalls()).toEqual([`/api/cloud/devices/${unknown.id}/profiles`]);
    expect(mockSwitchConvo).toHaveBeenCalledWith(null, "default@garage", undefined, {
      type: "attach",
    });
  });
});

// ─── Regression: a phone linked to two PCs, both carrying `default` ───────
//
// The server's AppState catalog holds only the ATTACHED target's stub
// (`default@desk`). The other online peer's rows live in
// `/api/cloud/devices/{id}/profiles`. Before the fix App renders
// `adaptCatalog(state)` alone, so the Selection chip lists one profile and
// the second host is invisible/unselectable.
//
// Expected after the fix: App merges `adaptCatalog(state)` with the ready
// `cloudTargets.rows` via `mergeCatalog(..., devices)`, so both rows render,
// labelled `default@<Device.name>`, with per-host colour dots. Clicking the
// other host reattaches with the exact route key (`default@phone`, no agent).

const DESK = device({ id: "0d".repeat(32), slug: "desk", name: "Desktop", online: true });
const PIXEL = device({ id: "9e".repeat(32), slug: "phone", name: "Pixel 10 Pro", online: true });
const SELF = selfDevice({ id: "e5".repeat(32), slug: "my-phone", name: "My Phone" });

// The attached host's session stub, exactly as cloud_target_info sends it:
// route-key name, device slug in `machine`, resolved agents.
const ATTACHED_STUB: WireProfileInfo = {
  name: "default@desk",
  origin: "remote",
  overrides_global: false,
  agents: [{ name: "team", model: "sonnet", subagents: [] }],
  color: "blue",
  machine: "desk",
};

function attachedState(): AppState {
  return {
    conversation: { id: "c-attached", name: "default@desk" },
    catalog: [ATTACHED_STUB],
    selection: { profile: "default@desk", agent: "team", subagent_models: {} },
    chat: { messages: [] },
    todo: { visible: false, items: [] },
    stats: null,
    context: null,
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { recent_summaries: [] },
    bash_panel: { kind: "idle" },
  } as unknown as AppState;
}

function profileRows(el: HTMLElement): HTMLElement[] {
  return Array.from(
    el.querySelectorAll<HTMLElement>('button[data-testid^="selection-profile-"]'),
  );
}

async function openChip(el: HTMLElement): Promise<void> {
  const chip = el.querySelector<HTMLElement>('[data-testid="selection-chip"]');
  expect(chip, "the selection chip renders for a bound profile").not.toBeNull();
  await act(async () => {
    chip!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

async function clickProfile(el: HTMLElement, name: string): Promise<void> {
  const row = el.querySelector<HTMLElement>(`[data-testid="selection-profile-${name}"]`);
  expect(row, `profile row ${name} must render`).not.toBeNull();
  await act(async () => {
    row!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

describe("App — two cloud hosts, both profile `default`", () => {
  beforeEach(() => {
    mockAgent.state = attachedState();
    mockAgent.connected = true;
    mockAgent.noLocal = false; // the chip scenario, not the zero-local picker
    mockSwitchConvo.mockClear();
    fetchMock = mockFetch((url) => {
      if (url.endsWith(`/api/cloud/devices/${DESK.id}/profiles`)) {
        // Post-fix server shape: this host's color is derived from its slug.
        return Promise.resolve(json(200, { profiles: [{ name: "default", color: "red" }] }));
      }
      if (url.endsWith(`/api/cloud/devices/${PIXEL.id}/profiles`)) {
        return Promise.resolve(json(200, { profiles: [{ name: "default", color: "pink" }] }));
      }
      return Promise.resolve(json(200, { profiles: [] }));
    });
  });

  afterEach(() => {
    mockAgent.state = null;
    mockAgent.connected = false;
    mockAgent.noLocal = true;
  });

  it("renders both cloud profiles, deduping the attached host's stub", async () => {
    await mountApp();
    await push(active([SELF, DESK, PIXEL]));
    await openChip(container!);

    expect(
      profileRows(container!),
      "two hosts with the same profile must yield two rows, not one",
    ).toHaveLength(2);
    expect(
      container!.querySelector('[data-testid="selection-profile-default@desk"]'),
      "the attached host stays addressable by its route key",
    ).not.toBeNull();
    expect(
      container!.querySelector('[data-testid="selection-profile-default@phone"]'),
      "the other host is visible by its route key",
    ).not.toBeNull();
  });

  it("labels the rows with the human device names, not raw slugs", async () => {
    await mountApp();
    await push(active([SELF, DESK, PIXEL]));
    await openChip(container!);

    const desk = container!.querySelector<HTMLElement>(
      '[data-testid="selection-profile-default@desk"]',
    );
    const phone = container!.querySelector<HTMLElement>(
      '[data-testid="selection-profile-default@phone"]',
    );
    expect(desk!.textContent).toContain("default@Desktop");
    expect(phone!.textContent).toContain("default@Pixel 10 Pro");
  });

  it("gives the two hosts distinct colour dots", async () => {
    await mountApp();
    await push(active([SELF, DESK, PIXEL]));
    await openChip(container!);

    const dots = Array.from(
      container!.querySelectorAll<HTMLElement>('[data-testid="selection-profile-color"]'),
    );
    expect(dots, "one dot per remote profile").toHaveLength(2);
    const colours = dots.map((d) => d.getAttribute("style"));
    expect(new Set(colours).size, "per-host colors must not collapse").toBe(2);
  });

  it("reattaches with the exact route key (and no agent) when the other host is clicked", async () => {
    await mountApp();
    await push(active([SELF, DESK, PIXEL]));
    await openChip(container!);
    await clickProfile(container!, "default@phone");

    expect(mockSwitchConvo).toHaveBeenCalledWith(null, "default@phone", undefined);
    expect(
      mockSwitchConvo.mock.calls.some((c) => c[1] === "default@Pixel 10 Pro"),
      "the display label must never become the route key",
    ).toBe(false);
  });
});
