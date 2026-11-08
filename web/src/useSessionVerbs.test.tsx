// @vitest-environment jsdom
//
// Tests for `useSessionVerbs.ts` (CONTRACT 3) — the hook that routes composer
// submits through `routeSessionVerb` and applies the result WITHOUT ever
// sending a session verb to a busy session.
//
// Written FIRST, expected to be RED until `web/src/useSessionVerbs.ts`
// exists: the import `./useSessionVerbs` does not resolve, so this file fails
// at collection for the RIGHT reason (feature absent).
//
// Contract (locked):
//   export function useSessionVerbs(a: {
//     agent: Pick<AgentConnection, "send" | "switchConvo" | "dirListing">;
//     busy: boolean; cwd: string;
//     loadConversation: (convo: string) => void;
//     onError: (msg: string) => void;
//   }): { submit(text: string, ids: string[]): void; switchCwd(path: string): void };
//
//   - switchCwd(p): busy → agent.switchConvo(null, undefined, undefined,
//     {type:"switch_cwd", path:p}); idle → agent.send({type:"switch_cwd", path:p}).
//   - submit(text, ids) routes through routeSessionVerb:
//       send        → the normal send_message frame ({type, text, attachments: ids})
//       cd          → set the pending-cd ref and send a list_dir frame
//       switch_cwd  → switchCwd(path)
//       load        → loadConversation(convo)
//       refuse      → onError(message)
//   - a useEffect on agent.dirListing acts only if a cd is pending, and clears
//     the pending flag. A reply with `error` → onError("/cd: " + error);
//     otherwise → switchCwd(reply.path), using busy as of reply time.
//
// Harness: a Probe component that captures the hook's returned {submit,
// switchCwd}. A controllable fake `agent` (send/switchConvo are spies,
// dirListing is mutable) is re-rendered to drive the dir_listing reply.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useSessionVerbs } from "./useSessionVerbs";
import type { DirListing } from "./state";

// ─── controllable fake agent ─────────────────────────────────────────────

interface FakeAgent {
  send: ReturnType<typeof vi.fn>;
  switchConvo: ReturnType<typeof vi.fn>;
  dirListing: DirListing | null;
}

function reply(path: string, error: string | null = null): DirListing {
  return { path, parent: null, entries: [], error };
}

// ─── probe + mount plumbing ──────────────────────────────────────────────

let api: { submit: (text: string, ids: string[]) => void; switchCwd: (path: string) => void } | null = null;
let agent: FakeAgent;
let busy: boolean;
const cwd = "/a";
let loadConversation: ReturnType<typeof vi.fn>;
let onError: ReturnType<typeof vi.fn>;

function Probe() {
  api = useSessionVerbs({ agent, busy, cwd, loadConversation, onError });
  return null;
}

let container: HTMLDivElement | null = null;
let root: Root | null = null;

function tree() {
  return <Probe />;
}

async function mount(over: { busy?: boolean } = {}): Promise<void> {
  agent = { send: vi.fn(), switchConvo: vi.fn(), dirListing: null };
  busy = over.busy ?? false;
  loadConversation = vi.fn();
  onError = vi.fn();
  api = null;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(tree());
  });
}

/** Re-render the hook with a (possibly) new busy flag and/or dirListing. */
async function set(over: { busy?: boolean; dirListing?: DirListing | null } = {}): Promise<void> {
  if (over.busy !== undefined) busy = over.busy;
  if (over.dirListing !== undefined) agent.dirListing = over.dirListing;
  await act(async () => {
    root!.render(tree());
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  api = null;
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
});

// ─── cd path (case 7) ────────────────────────────────────────────────────

describe("useSessionVerbs — /cd goes over list_dir, then switch_cwd on reply", () => {
  it("submit('/cd /x') sends a list_dir frame for that path", async () => {
    await mount({ busy: true });
    expect(api, "the probe captured the hook api").not.toBeNull();

    api!.submit("/cd /x", []);

    expect(agent.send).toHaveBeenCalledWith({ type: "list_dir", path: "/x" });
    // No conversation verb is sent to the busy session while we wait for the
    // listing.
    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
  });

  it("a dir_listing reply with path /r while BUSY re-attaches via switchConvo", async () => {
    await mount({ busy: true });
    api!.submit("/cd /x", []);

    await set({ dirListing: reply("/r"), busy: true });

    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(agent.switchConvo).toHaveBeenCalledWith(null, undefined, undefined, {
      type: "switch_cwd",
      path: "/r",
    });
    // …and NOT a switch_cwd on the (still busy) socket.
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
  });

  it("the same dir_listing reply while IDLE sends switch_cwd on the socket", async () => {
    await mount({ busy: false });
    api!.submit("/cd /x", []);

    await set({ dirListing: reply("/r"), busy: false });

    expect(agent.send).toHaveBeenCalledWith({ type: "switch_cwd", path: "/r" });
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("a dir_listing reply with an error calls onError and sends no switch_cwd", async () => {
    await mount({ busy: true });
    api!.submit("/cd /x", []);

    await set({ dirListing: reply("/r", "No such file or directory"), busy: true });

    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith("/cd: No such file or directory");
    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
  });

  it("after a cd reply is consumed, a later dir_listing (no pending cd) does nothing", async () => {
    await mount({ busy: true });
    api!.submit("/cd /x", []);

    await set({ dirListing: reply("/r"), busy: true });
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);

    // A second listing arrives with no pending cd → ignored.
    await set({ dirListing: reply("/s"), busy: true });
    expect(agent.switchConvo).toHaveBeenCalledTimes(1);
    expect(onError).not.toHaveBeenCalled();
  });
});

// ─── a dir_listing change with no pending cd does nothing (case 8) ───────

describe("useSessionVerbs — dir_listing with no pending cd is ignored", () => {
  it("a dirListing change with no pending cd does nothing", async () => {
    await mount({ busy: true });
    // No submit() has issued a /cd, so nothing is pending.
    await set({ dirListing: reply("/r"), busy: true });

    expect(agent.switchConvo).not.toHaveBeenCalled();
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
    expect(onError).not.toHaveBeenCalled();
  });
});

// ─── submit() routes every VerbAction ────────────────────────────────────

describe("useSessionVerbs — submit() applies each routed action", () => {
  it("send → the normal send_message frame (attachments included)", async () => {
    await mount({ busy: true });
    api!.submit("hello", ["att-1"]);

    expect(agent.send).toHaveBeenCalledWith({
      type: "send_message",
      text: "hello",
      attachments: ["att-1"],
    });
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("switch_cwd (busy /new) → switchConvo with a switch_cwd frame for cwd", async () => {
    await mount({ busy: true });
    api!.submit("/new", []);

    expect(agent.switchConvo).toHaveBeenCalledWith(null, undefined, undefined, {
      type: "switch_cwd",
      path: cwd,
    });
  });

  it("send (idle /new) → the normal send_message frame, unchanged", async () => {
    await mount({ busy: false });
    api!.submit("/new", []);

    // While idle, /new is NOT applied client-side: it goes to the server
    // unchanged, exactly like any other pass-through text, so the server's
    // real /new semantics (preserve model, profile, agent) apply.
    expect(agent.send).toHaveBeenCalledWith({
      type: "send_message",
      text: "/new",
      attachments: [],
    });
    // …and no switch_cwd frame on the socket, and no switchConvo at all.
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("load (busy /load <uuid>) → loadConversation(convo)", async () => {
    await mount({ busy: true });
    api!.submit("/load 77777777-7777-7777-7777-777777777777", []);

    expect(loadConversation).toHaveBeenCalledTimes(1);
    expect(loadConversation).toHaveBeenCalledWith("77777777-7777-7777-7777-777777777777");
    expect(agent.send).not.toHaveBeenCalled();
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });

  it("refuse (busy /load 3) → onError with the exact message", async () => {
    await mount({ busy: true });
    api!.submit("/load 3", []);

    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith(
      "/load by index needs the turn to finish — press Stop first.",
    );
    expect(agent.send).not.toHaveBeenCalled();
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });
});

// ─── switchCwd() direct ──────────────────────────────────────────────────

describe("useSessionVerbs — switchCwd(path)", () => {
  it("busy → switchConvo(null, undefined, undefined, {type:'switch_cwd', path})", async () => {
    await mount({ busy: true });
    api!.switchCwd("/r");

    expect(agent.switchConvo).toHaveBeenCalledWith(null, undefined, undefined, {
      type: "switch_cwd",
      path: "/r",
    });
    expect(agent.send).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "switch_cwd" }),
    );
  });

  it("idle → send({type:'switch_cwd', path})", async () => {
    await mount({ busy: false });
    api!.switchCwd("/r");

    expect(agent.send).toHaveBeenCalledWith({ type: "switch_cwd", path: "/r" });
    expect(agent.switchConvo).not.toHaveBeenCalled();
  });
});
