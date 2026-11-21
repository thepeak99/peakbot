// @vitest-environment jsdom
//
// P5 — Task 3: the SPA handles the no_local_profile frame.
// On receiving it, the client must NOT reconnect in a loop (drop handlers),
// and expose noLocal=true so the App can show the picker.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";

// ─── fake WebSocket ─────────────────────────────────────────────────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 1;
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
  window.history.replaceState(null, "", "/");
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

// ─── tests ──────────────────────────────────────────────────────────────

describe("useAgent — no_local_profile frame", () => {
  it("sets noLocal=true on receiving the frame", async () => {
    await mountAgent();
    const ws = FakeWebSocket.instances[0];

    await act(async () => {
      ws.onmessage?.({ data: JSON.stringify({ type: "no_local_profile" }) });
      await Promise.resolve();
    });

    expect(conn!.noLocal).toBe(true);
  });

  it("does NOT schedule a reconnect after the frame (no reconnect loop)", async () => {
    await mountAgent();
    const ws = FakeWebSocket.instances[0];

    await act(async () => {
      ws.onmessage?.({ data: JSON.stringify({ type: "no_local_profile" }) });
      await Promise.resolve();
    });

    // The socket should be detached (onclose is null) so the subsequent
    // server-side close doesn't trigger a reconnect.
    expect(ws.onclose, "handlers detached → no reconnect scheduled").toBeNull();
  });

  it("local profiles still work (noLocal stays false)", async () => {
    await mountAgent();
    const ws = FakeWebSocket.instances[0];

    // A normal state frame with a catalog
    await act(async () => {
      ws.onmessage?.({
        data: JSON.stringify({
          type: "state",
          state: { conversation: { id: "c1" }, catalog: [{ name: "p1", origin: "repo", overrides_global: false, agents: [] }] },
        }),
      });
      await Promise.resolve();
    });

    expect(conn!.noLocal).toBe(false);
  });
});