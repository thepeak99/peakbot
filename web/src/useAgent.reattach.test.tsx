// @vitest-environment jsdom
//
// GREEN guard (remote-profiles T9 follow-up): pins the reattach TARGET the
// selection chip must call for a remote profile the local side knows no
// agents of — `switchConvo(null, profile)` with NO agent. Per design §5.1/
// §6.4 the fresh socket's `attach` frame is `{convo: null, profile}` with
// the `agent` key OMITTED (the remote resolves its own agent, "agent or
// the profile's first agent"), and the URL keeps `?profile=` so a refresh
// or reconnect is still remote (G7). The chip-side bug (a zero-agent
// remote profile is unselectable) is locked by
// components/selectionChip.remote.test.tsx; this file proves the reattach
// destination it must drive already works, no agent required.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";

// ─── fake WebSocket (useAgent.t9.test.tsx conventions) ──────────────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 1; // OPEN, like the real socket once `onopen` has fired
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
    queueMicrotask(() => this.onopen?.());
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }
}

// ─── mount plumbing ─────────────────────────────────────────────────────

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let conn: AgentConnection | null = null;

function Harness() {
  conn = useAgent();
  return null;
}

async function mountAgent(): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<Harness />);
  });
  // Flush the fake socket's onopen microtask (the attach frame is sent in it).
  await act(async () => {
    await Promise.resolve();
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  FakeWebSocket.instances = [];
  conn = null;
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

// ─── tests ──────────────────────────────────────────────────────────────

describe("useAgent — reattach target: a remote profile without an agent", () => {
  it("switchConvo(null, profile) attaches without an agent key and keeps ?profile= in the URL", async () => {
    window.history.replaceState(null, "", "/");
    await mountAgent();

    // The chip's reattach for a remote profile with no local agents.
    await act(async () => {
      conn!.switchConvo(null, "bonsol-dev");
      await Promise.resolve(); // the fresh socket's onopen microtask
    });

    const ws = FakeWebSocket.instances.at(-1)!;
    expect(ws, "a fresh socket was dialed").toBeDefined();
    expect(JSON.parse(ws.sent[0]), "the agent key is omitted — the remote resolves it").toStrictEqual({
      type: "attach",
      convo: null,
      profile: "bonsol-dev",
    });

    const url = new URL(window.location.href);
    expect(url.searchParams.get("profile"), "refresh/reconnect stay remote (G7)").toBe("bonsol-dev");
    expect(url.searchParams.has("convo")).toBe(false);
  });
});
