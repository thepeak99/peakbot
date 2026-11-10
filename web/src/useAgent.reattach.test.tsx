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

// ─── URL follows the binding across popstate (mobile-chats design §3.4) ──
// After any popstate the entry we land on must show the BOUND conversation,
// not the one it was created with — otherwise a Back from the Chats layer
// would re-attach the wrong chat.
describe("useAgent — the URL follows the binding across popstate", () => {
  it("a marker entry shows the bound convo after a back-traversal", async () => {
    window.history.replaceState(null, "", "/");
    await mountAgent();

    // Bind c1: the base entry's URL.
    await act(async () => {
      conn!.switchConvo("c1", null);
      await Promise.resolve();
    });
    expect(new URL(window.location.href).searchParams.get("convo")).toBe("c1");

    // A marker entry (the mobile Chats layer's pushState, URL unchanged).
    window.history.pushState(window.history.state, "");

    // Rebind to c2: the top entry now carries c2.
    await act(async () => {
      conn!.switchConvo("c2", null);
      await Promise.resolve();
    });
    expect(new URL(window.location.href).searchParams.get("convo")).toBe("c2");

    // Back to the marker entry: it must show the bound convo (c2), not c1.
    const popped = new Promise<void>((resolve) => {
      const t = setTimeout(resolve, 250);
      window.addEventListener(
        "popstate",
        () => {
          clearTimeout(t);
          resolve();
        },
        { once: true },
      );
    });
    window.history.back();
    await act(async () => {
      await popped;
    });

    expect(
      new URL(window.location.href).searchParams.get("convo"),
      "the URL reflects the bound convo, not the entry's original",
    ).toBe("c2");
  });
});
