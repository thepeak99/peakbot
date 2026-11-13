// @vitest-environment jsdom
//
// W2/W3 (mobile-redesign design §2.2, §6): `useAgent` exposes the machine
// statuses of the last `conversations_list` frame as `AgentConnection.machines`.
//   * a frame WITHOUT `machines` (an older server, or a server with no
//     remotes — the key is omitted when empty) → `[]`;
//   * a frame WITH `machines` → stored verbatim;
//   * each new frame replaces the previous statuses (a frame that drops the
//     key clears them: statuses are a snapshot, not an accumulation).
//
// Harness: useAgent.reattach.test.tsx (fake WebSocket, raw createRoot).
//
// RED before implementation: `AgentConnection.machines` is undefined.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";

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

/** Deliver one server frame to the live socket. */
async function deliver(frame: unknown): Promise<void> {
  const ws = FakeWebSocket.instances.at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
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
  window.history.replaceState(null, "", "/");
});

describe("useAgent — machines (W2/W3)", () => {
  it("starts with no machine statuses", async () => {
    await mountAgent();
    expect(conn!.machines).toEqual([]);
  });

  it("W2: a conversations_list without machines leaves machines []", async () => {
    await mountAgent();
    await deliver({ type: "conversations_list", items: [] });
    expect(conn!.machines).toEqual([]);
  });

  it("W3: a conversations_list with machines stores them verbatim", async () => {
    await mountAgent();
    const machines = [
      { id: "https://10.0.0.5:7823", online: false },
      { id: "https://home-server.local:7823", online: true },
    ];
    await deliver({ type: "conversations_list", items: [], machines });
    expect(conn!.machines).toEqual(machines);
  });

  it("a later frame without machines clears the previous statuses (a snapshot)", async () => {
    await mountAgent();
    await deliver({
      type: "conversations_list",
      items: [],
      machines: [{ id: "https://h:7823", online: true }],
    });
    expect(conn!.machines).toHaveLength(1);
    await deliver({ type: "conversations_list", items: [] });
    expect(conn!.machines).toEqual([]);
  });

  it("a later frame replaces the statuses (online flips)", async () => {
    await mountAgent();
    await deliver({
      type: "conversations_list",
      items: [],
      machines: [{ id: "https://h:7823", online: true }],
    });
    await deliver({
      type: "conversations_list",
      items: [],
      machines: [{ id: "https://h:7823", online: false }],
    });
    expect(conn!.machines).toEqual([{ id: "https://h:7823", online: false }]);
  });

  it("the items still land alongside the machines", async () => {
    await mountAgent();
    const item = {
      id: "c1",
      name: "n",
      updated_at: "2026-11-10T12:00:00.000Z",
      message_count: 1,
      model: "m",
      active: false,
    };
    await deliver({
      type: "conversations_list",
      items: [item],
      machines: [{ id: "https://h:7823", online: true }],
    });
    expect(conn!.conversations).toEqual([item]);
  });
});
