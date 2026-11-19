// Pins the single-pass todoTree: byte-identical output on the equivalence
// fixture, and a message-visit budget (< 4N element reads). tool_args wire
// projection is covered by the Rust tests.

import { describe, it, expect, vi, afterEach } from "vitest";
import * as adapt from "./adapt";
import type { WireChatMessage } from "./state";

afterEach(() => {
  vi.restoreAllMocks();
});

const { todoTree } = adapt;

const TS = "2026-01-01T00:00:00Z";

// Helpers to construct test messages (copied from adapt.test.ts so this
// file is self-contained and can run independently).
function todoCall(args: Record<string, unknown>): WireChatMessage {
  return {
    role: "toolcall",
    content: "",
    timestamp: TS,
    tool_name: "todo",
    tool_args: JSON.stringify(args),
  };
}

function delegate(role: string, extra: Record<string, unknown> = {}): WireChatMessage {
  return {
    role: "toolcall",
    content: "",
    timestamp: TS,
    tool_name: "delegate",
    tool_args: JSON.stringify({ role, task: "do it", ...extra }),
  };
}

function subAgent(role: string, todo?: Record<string, unknown>): WireChatMessage {
  return {
    role: todo ? "toolcall" : "agent",
    content: "",
    timestamp: TS,
    tool_name: todo ? "todo" : undefined,
    tool_args: todo ? JSON.stringify(todo) : undefined,
    source: { kind: "sub_agent", role },
  };
}

// ─── EQUIVALENCE FIXTURE: byte-identical output on the full fixture ───────
//
// A comprehensive synthetic message set that exercises:
// - multiple sub-agent lanes
// - nested delegate calls (parent_task_id chains)
// - orchestrator-lane todos
// - background-source messages
// - delegate calls whose parent lane has no todos
// - empty input
// - todo calls with action add/update/remove and statuses

describe("todoTree equivalence fixture", () => {
  it("reproduces the exact same output as the current implementation", () => {
    // Build a rich transcript: 2 delegations from dev (one parented, one not),
    // 1 from pm (parented), 1 from tester (unparented), orchestrator todos
    // interleaved, a background message, an update, and a removal.
    const msgs: WireChatMessage[] = [
      // Orchestrator: add two tasks, then delegate dev with parent_task_id=2
      todoCall({ action: "add", tasks: ["plan the feature", "implement the feature"] }),
      delegate("dev", { parent_task_id: 2 }),
      // dev#1: adds two steps
      subAgent("dev", { action: "add", tasks: ["write backend", "write frontend"] }),
      // Orchestrator: update task 1 to completed
      todoCall({ action: "update", task_id: 1, status: "completed" }),
      // Orchestrator: delegate pm (unparented)
      delegate("pm"),
      // pm#1: adds one task
      subAgent("pm", { action: "add", tasks: ["write user story"] }),
      // Orchestrator: delegate dev again, parented to task 1 (completed)
      delegate("dev", { parent_task_id: 1 }),
      // dev#2: adds one step
      subAgent("dev", { action: "add", tasks: ["refactor"] }),
      // Background message (should be ignored by todoTree)
      {
        role: "user",
        content: "bg output",
        timestamp: TS,
        source: { kind: "background", proc_ids: [1] },
      } as unknown as WireChatMessage,
      // Orchestrator: remove task 1 (which dev#2 was parented to)
      todoCall({ action: "remove", task_id: 1 }),
      // Orchestrator: delegate tester (unparented)
      delegate("tester"),
      // tester#1: adds one task, then updates it
      subAgent("tester", { action: "add", tasks: ["test happy path"] }),
      subAgent("tester", { action: "update", task_id: 1, status: "in_progress" }),
    ];

    // Run the fixture; assert the exact output it produces, byte-for-byte.
    const actual = todoTree(msgs);

    // Expected output: "plan the feature" (task 1) is removed, leaving only
    // "implement the feature" (task 2) as an orchestrator item. The dev#1
    // steps are children of task 2. pm's task is unparented (top-level).
    // dev#2's parent was removed, so it falls back to top-level. tester's
    // task is unparented (top-level). Order of unparented items follows
    // delegation call order: pm, then dev#2, then tester.
    expect(actual).toEqual([
      {
        item: {
          id: 2,
          text: "implement the feature",
          status: "pending",
          lane: "Orchestrator",
        },
        children: [
          { id: 1, text: "write backend", status: "pending", lane: "dev · call 1" },
          { id: 2, text: "write frontend", status: "pending", lane: "dev · call 1" },
        ],
      },
      {
        item: { id: 1, text: "write user story", status: "pending", lane: "pm" },
        children: [],
      },
      {
        item: { id: 1, text: "refactor", status: "pending", lane: "dev · call 2" },
        children: [],
      },
      {
        item: { id: 1, text: "test happy path", status: "inProgress", lane: "tester" },
        children: [],
      },
    ]);
  });

  it("empty input produces empty output", () => {
    expect(todoTree([])).toEqual([]);
  });
});

// ─── COST GUARD: single-pass message-visit budget ─────────────────────────
//
// Spy seam: we wrap the messages array in a Proxy whose `get` trap counts
// every element access (index reads, iteration, Array.prototype.filter/map
// all go through it). adapt.ts has no injectable module-level hook — ES
// imports bind statically, so vi.spyOn on the export cannot intercept the
// internal call. A Proxy on the data measures the actual scanning the
// implementation performs, regardless of how it's structured.
//
// Budget: total message-element reads must stay under 4×N; the single pass
// is ~N + small per-lane work (< 4N = 16,000), versus ~L×N (160,000 for
// N=4000, L=40) with a per-lane-scan design.

describe("todoTree cost guard (single-pass rewrite)", () => {
  function buildLargeTranscript(numMessages: number, numLanes: number): WireChatMessage[] {
    const msgs: WireChatMessage[] = [];
    let msgIdx = 0;

    // Create delegations for each lane
    for (let lane = 0; lane < numLanes; lane++) {
      const role = `role${lane}`;
      msgs.push(delegate(role));
      msgIdx++;
    }

    // Distribute sub-agent messages evenly across lanes
    const msgsPerLane = Math.floor((numMessages - numLanes) / numLanes);
    for (let lane = 0; lane < numLanes; lane++) {
      const role = `role${lane}`;
      for (let i = 0; i < msgsPerLane; i++) {
        msgs.push(subAgent(role));
        msgIdx++;
      }
    }

    // Fill remaining messages with orchestrator todos
    while (msgIdx < numMessages) {
      msgs.push(todoCall({ action: "add", tasks: [`task ${msgIdx}`] }));
      msgIdx++;
    }

    return msgs;
  }

  it("uses O(N) message visits, not O(lanes × N)", () => {
    const N = 4000;
    const L = 40;
    const raw = buildLargeTranscript(N, L);

    // Count every element read on the array. Array.prototype.filter/map/
    // for...of all dispatch through the `get` trap for indexed reads.
    let visits = 0;
    const counted = new Proxy(raw, {
      get(target, prop, receiver) {
        if (typeof prop === "string" && /^\d+$/.test(prop)) {
          visits++;
        }
        const value = Reflect.get(target, prop, receiver);
        // Bind array methods to the Proxy so their internal element reads
        // also go through the trap (filter/map/forEach/@@iterator).
        if (typeof value === "function") {
          return value.bind(receiver);
        }
        return value;
      },
    });

    todoTree(counted as WireChatMessage[]);

    // Guard pins reads under 4N: the single pass is ~N + small per-lane work
    // (< 16,000), far below a per-lane-scan design's ~L×N (160,000).
    expect(visits).toBeLessThan(4 * N);
  });
});
