// @vitest-environment jsdom
//
// RED (remote-profiles T9): `useAgent` does not carry the profile from the
// URL into `attach` yet. Written against the design contract
// (remote-profiles-design §5.6, §6.1, §6.4, G7):
//
//   * The URL is `?convo=<id>&profile=<name>`; the FIRST frame of every
//     socket is `attach{convo, profile}` — the profile is the routing key
//     (a relay, not a local session) and is NEVER dropped.
//   * A reconnect (after a close, including after an `error` frame such as
//     the relay's E-DOWN/E-VERSION) re-sends the SAME `(convo, profile)` —
//     "the SPA never drops `profile` on reconnect, so a retry is also
//     remote" (G7 by construction).
//
// A fake WebSocket replaces the global; the backoff timer is faked so a
// reconnect is deterministic. No network is touched.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent } from "./useAgent";

// ─── fake WebSocket ─────────────────────────────────────────────────────

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

function Harness() {
  useAgent();
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

async function feed(frame: object): Promise<void> {
  const ws = FakeWebSocket.instances.at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
  });
}

async function dropSocket(): Promise<void> {
  const ws = FakeWebSocket.instances.at(-1)!;
  await act(async () => {
    ws.close();
  });
}

async function reconnect(): Promise<void> {
  await act(async () => {
    vi.advanceTimersByTime(600); // the 500 ms backoff
    await Promise.resolve(); // the fresh socket's onopen microtask
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

const CONVO = "77777777-7777-7777-7777-777777777777";
const PROFILE = "home-dev";

// ─── tests ──────────────────────────────────────────────────────────────

describe("useAgent — remote profile routing (remote-profiles T9)", () => {
  it("attach_carries_the_profile_from_the_url", async () => {
    window.history.replaceState(null, "", `/?convo=${CONVO}&profile=${PROFILE}`);
    await mountAgent();

    const ws = FakeWebSocket.instances[0];
    expect(ws, "the socket connected").toBeDefined();
    expect(JSON.parse(ws.sent[0])).toStrictEqual({
      type: "attach",
      convo: CONVO,
      profile: PROFILE,
    });
  });

  it("attach_carries_no_profile_param_for_a_local_url", async () => {
    window.history.replaceState(null, "", `/?convo=${CONVO}`);
    await mountAgent();

    expect(JSON.parse(FakeWebSocket.instances[0].sent[0])).toStrictEqual({
      type: "attach",
      convo: CONVO,
    });
  });

  it("reconnect_after_an_error_re_sends_the_same_convo_and_profile", async () => {
    window.history.replaceState(null, "", `/?convo=${CONVO}&profile=${PROFILE}`);
    await mountAgent();

    // The relay's failure (E-DOWN): an `error` frame, then the socket ends.
    await feed({
      type: "error",
      message: `Can't reach remote profile '${PROFILE}' at 127.0.0.1:1: connection refused.`,
    });
    await dropSocket();
    await reconnect();

    // a second socket was dialed
    expect(FakeWebSocket.instances).toHaveLength(2);
    expect(JSON.parse(FakeWebSocket.instances[1].sent[0]), "G7: the retry is also remote").toStrictEqual(
      { type: "attach", convo: CONVO, profile: PROFILE },
    );
  });

  it("reconnect_after_a_plain_close_re_sends_the_same_convo_and_profile", async () => {
    window.history.replaceState(null, "", `/?convo=${CONVO}&profile=${PROFILE}`);
    await mountAgent();

    await feed({ type: "attached", convo: CONVO });
    await dropSocket();
    await reconnect();

    expect(JSON.parse(FakeWebSocket.instances[1].sent[0])).toStrictEqual({
      type: "attach",
      convo: CONVO,
      profile: PROFILE,
    });
  });
});
