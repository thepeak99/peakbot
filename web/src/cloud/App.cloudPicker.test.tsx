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
