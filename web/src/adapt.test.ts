import { describe, it, expect } from "vitest";
import {
  assignDelegationCalls,
  deriveSubAgentRoster,
  filterMessagesByView,
  flatTree,
  parseCallKey,
  todoTree,
  todosFromMessages,
  viewLabel,
} from "./adapt";
import type { WireChatMessage } from "./state";
import type { TodoNode } from "./types";

// Minimal transcript builder: a `todo` tool call carrying the given args JSON.
// Mirrors what SessionHook.on_tool_call TEEs into the transcript.
function todoCall(args: Record<string, unknown>): WireChatMessage {
  return {
    role: "toolcall",
    content: "",
    timestamp: "2026-01-01T00:00:00Z",
    tool_name: "todo",
    tool_args: JSON.stringify(args),
  };
}

// todosFromMessages must faithfully mirror the Rust `TodoList` transition
// function (src/tools/todo.rs): case-insensitive dedupe, monotonic id
// assignment, and `clear` dropping completed+cancelled and resetting the id
// counter to 1 when the list empties.
describe("todosFromMessages", () => {
  it("assigns monotonic ids and preserves first-touch order", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["alpha", "beta"] }),
    ]);
    expect(derived).toEqual([
      { id: 1, text: "alpha", status: "pending" },
      { id: 2, text: "beta", status: "pending" },
    ]);
  });

  it("dedupes case-insensitively without consuming an id", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["Build"] }),
      todoCall({ action: "add", tasks: ["build"] }), // dupe → no new id
      todoCall({ action: "add", tasks: ["Ship"] }), // must be id 2, not 3
    ]);
    expect(derived).toEqual([
      { id: 1, text: "Build", status: "pending" },
      { id: 2, text: "Ship", status: "pending" },
    ]);
  });

  it("updates status by id and ignores unknown ids", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a", "b"] }),
      todoCall({ action: "update", task_id: 2, status: "in_progress" }),
      todoCall({ action: "update", task_id: 99, status: "completed" }), // no-op
    ]);
    expect(derived).toEqual([
      { id: 1, text: "a", status: "pending" },
      { id: 2, text: "b", status: "inProgress" },
    ]);
  });

  it("ignores an update with an unknown status string (tool would error)", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a"] }),
      todoCall({ action: "update", task_id: 1, status: "bogus" }), // no-op
    ]);
    expect(derived).toEqual([{ id: 1, text: "a", status: "pending" }]);
  });

  it("removes by id", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a", "b", "c"] }),
      todoCall({ action: "remove", task_id: 2 }),
    ]);
    expect(derived).toEqual([
      { id: 1, text: "a", status: "pending" },
      { id: 3, text: "c", status: "pending" },
    ]);
  });

  it("clear drops completed+cancelled and keeps the rest", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a", "b", "c"] }),
      todoCall({ action: "update", task_id: 1, status: "completed" }),
      todoCall({ action: "update", task_id: 2, status: "cancelled" }),
      todoCall({ action: "clear" }),
    ]);
    expect(derived).toEqual([{ id: 3, text: "c", status: "pending" }]);
  });

  it("clear resets the id counter to 1 when the list empties", () => {
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a", "b"] }),
      todoCall({ action: "update", task_id: 1, status: "completed" }),
      todoCall({ action: "update", task_id: 2, status: "completed" }),
      todoCall({ action: "clear" }), // empties → next id resets to 1
      todoCall({ action: "add", tasks: ["fresh"] }),
    ]);
    expect(derived).toEqual([{ id: 1, text: "fresh", status: "pending" }]);
  });

  it("skips malformed tool_args without crashing", () => {
    const bad: WireChatMessage = {
      role: "toolcall",
      content: "",
      timestamp: "2026-01-01T00:00:00Z",
      tool_name: "todo",
      tool_args: "{not json",
    };
    const derived = todosFromMessages([
      todoCall({ action: "add", tasks: ["a"] }),
      bad,
    ]);
    expect(derived).toEqual([{ id: 1, text: "a", status: "pending" }]);
  });

  it("ignores non-todo tool calls and non-toolcall messages", () => {
    const derived = todosFromMessages([
      { role: "user", content: "hi", timestamp: "2026-01-01T00:00:00Z" },
      todoCall({ action: "add", tasks: ["a"] }),
      {
        role: "toolcall",
        content: "",
        timestamp: "2026-01-01T00:00:00Z",
        tool_name: "file_read",
        tool_args: JSON.stringify({ path: "/x" }),
      },
    ]);
    expect(derived).toEqual([{ id: 1, text: "a", status: "pending" }]);
  });
});

// --- Per-delegation segmentation (issue: repeated calls to one role must not
// collapse into a single label) --------------------------------------------

const TS = "2026-01-01T00:00:00Z";

// An orchestrator-lane `delegate` ToolCall for `role` (opens a delegation).
// `extra` merges into the JSON args object — used to add `parent_task_id` to
// fixtures where the orchestrator names a todo item, or any other arg. Keep
// the default body so the existing one-arg callers (`delegate("pm")`) stay
// byte-for-byte equivalent to the old fixture.
function delegate(
  role: string,
  extra: Record<string, unknown> = {},
): WireChatMessage {
  return {
    role: "toolcall",
    content: "",
    timestamp: TS,
    tool_name: "delegate",
    tool_args: JSON.stringify({ role, task: "do it", ...extra }),
  };
}

// A sub-agent lane turn for `role` (agent prose). `todo` lets a sub-agent turn
// also carry a todo tool call so we can test per-lane todo replay.
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

describe("parseCallKey", () => {
  it("splits role#n and treats a bare role as call 1", () => {
    expect(parseCallKey("pm#2")).toEqual({ key: "pm#2", role: "pm", n: 2 });
    expect(parseCallKey("pm")).toEqual({ key: "pm#1", role: "pm", n: 1 });
  });
  it("returns null for reserved views and malformed keys", () => {
    expect(parseCallKey("global")).toBeNull();
    expect(parseCallKey("orchestrator")).toBeNull();
    expect(parseCallKey("pm#0")).toBeNull();
    expect(parseCallKey("pm#x")).toBeNull();
  });
});

describe("assignDelegationCalls", () => {
  it("tags each sub-agent turn with its role's call number, in order", () => {
    const msgs = [
      delegate("pm"),
      subAgent("pm"),
      delegate("architect"),
      subAgent("architect"),
      delegate("pm"), // second pm delegation
      subAgent("pm"),
    ];
    const keys = assignDelegationCalls(msgs);
    expect(keys.get(1)).toBe("pm#1");
    expect(keys.get(3)).toBe("architect#1");
    expect(keys.get(5)).toBe("pm#2");
    // The delegate ToolCalls themselves are orchestrator-lane → untagged.
    expect(keys.has(0)).toBe(false);
  });

  it("interleaved pm → architect → pm yields pm#1, architect#1, pm#2", () => {
    const roster = deriveSubAgentRoster([
      delegate("pm"),
      subAgent("pm"),
      delegate("architect"),
      subAgent("architect"),
      delegate("pm"),
      subAgent("pm"),
      subAgent("pm"),
    ]);
    expect(roster.map((r) => r.key)).toEqual(["pm#1", "architect#1", "pm#2"]);
    expect(roster.find((r) => r.key === "pm#2")?.count).toBe(2);
  });
});

describe("filterMessagesByView (per-call)", () => {
  const msgs = [
    delegate("pm"),
    subAgent("pm"), // pm#1
    delegate("pm"),
    subAgent("pm"), // pm#2
    subAgent("pm"), // pm#2
  ];
  it("isolates a single delegation with role#n", () => {
    expect(filterMessagesByView(msgs, "pm#2")).toHaveLength(2);
    expect(filterMessagesByView(msgs, "pm#1")).toHaveLength(1);
  });
  it("a bare role keeps all of that role's turns", () => {
    expect(filterMessagesByView(msgs, "pm")).toHaveLength(3);
  });
});

describe("viewLabel", () => {
  it("labels reserved views and single vs multi-call delegations", () => {
    expect(viewLabel("global")).toBe("Global");
    expect(viewLabel("orchestrator")).toBe("Orchestrator");
    expect(viewLabel("pm#1", false)).toBe("pm");
    expect(viewLabel("pm#2", true)).toBe("pm · call 2");
  });
});

describe("todoTree", () => {
  // The four pre-existing cases were against `todosByLane` (a flat array of
  // TodoItem with a `lane` tag). The new API returns a TodoNode[] tree where
  // each row is `{ item: TodoItem, children: TodoItem[] }` — when the
  // delegate call has no `parent_task_id`, every item surfaces as a top-level
  // node with empty children, which is structurally identical to the old
  // flat lane list. The ports below preserve the original fixtures and
  // assertions (modulo the node wrapper) so the regression lock is exact.

  it("replays each lane independently and labels every item", () => {
    const msgs = [
      todoCall({ action: "add", tasks: ["orchestrate"] }), // orchestrator lane
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["write prd"] }),
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["revise prd"] }),
    ];
    const todos = todoTree(msgs);
    // Orchestrator first, then each pm delegation in call order — same shape
    // as the old todosByLane output, just wrapped in {item, children}.
    expect(todos.map((n) => ({ lane: n.item.lane, text: n.item.text, id: n.item.id }))).toEqual([
      { lane: "Orchestrator", text: "orchestrate", id: 1 },
      { lane: "pm · call 1", text: "write prd", id: 1 },
      { lane: "pm · call 2", text: "revise prd", id: 1 },
    ]);
    expect(todos.every((n) => n.children.length === 0)).toBe(true);
  });

  it("keeps per-lane id spaces from colliding (each lane restarts at 1)", () => {
    const msgs = [
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["a", "b"] }),
      delegate("dev"),
      subAgent("dev", { action: "add", tasks: ["x"] }),
    ];
    const todos = todoTree(msgs);
    // dev's single todo gets id 1, independent of pm's id space (1,2).
    const dev = todos.find((n) => n.item.lane === "dev");
    expect(dev).toMatchObject({ item: { id: 1, text: "x", lane: "dev" }, children: [] });
  });

  it("labels a single-call role without a call suffix", () => {
    const todos = todoTree([
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["only"] }),
    ]);
    expect(todos).toEqual([
      { item: { id: 1, text: "only", status: "pending", lane: "pm" }, children: [] },
    ]);
  });

  it("leaves todos unlabeled when no sub-agents ran (pre-agents behavior)", () => {
    // Subagents disabled / never used ⇒ no delegation in the transcript.
    // Todos must carry no lane so the panel renders no pill, exactly as before
    // the sub-agents feature existed.
    const todos = todoTree([
      todoCall({ action: "add", tasks: ["a", "b"] }),
    ]);
    expect(todos).toEqual([
      { item: { id: 1, text: "a", status: "pending" }, children: [] },
      { item: { id: 2, text: "b", status: "pending" }, children: [] },
    ]);
    expect(todos.every((n) => n.item.lane === undefined)).toBe(true);
  });

  // ── New cases (design §8.2) ─────────────────────────────────────────────

  it("nests a delegation's todos under the named parent", () => {
    // Orchestrator adds two items, then delegates `dev` with parent_task_id=2
    // (item 2 is the one being handed off). The `dev` sub-agent adds two
    // todos — they must surface as children of item 2 in call/list order,
    // each labeled with the bare `dev` lane.
    const msgs = [
      todoCall({ action: "add", tasks: ["task 1", "task 2"] }), // ids 1, 2
      delegate("dev", { parent_task_id: 2 }),
      subAgent("dev", { action: "add", tasks: ["step a", "step b"] }),
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      {
        item: { id: 1, text: "task 1", status: "pending", lane: "Orchestrator" },
        children: [],
      },
      {
        item: { id: 2, text: "task 2", status: "pending", lane: "Orchestrator" },
        children: [
          { id: 1, text: "step a", status: "pending", lane: "dev" },
          { id: 2, text: "step b", status: "pending", lane: "dev" },
        ],
      },
    ]);
  });

  it("concatenates two delegations of the same role onto one parent, in call order", () => {
    // Both dev calls point at item 2 → both sets of children concatenate
    // under item 2, each call tagged with its `dev · call N` lane label.
    const msgs = [
      todoCall({ action: "add", tasks: ["task 1", "task 2"] }), // ids 1, 2
      delegate("dev", { parent_task_id: 2 }),
      subAgent("dev", { action: "add", tasks: ["a", "b"] }),
      delegate("dev", { parent_task_id: 2 }),
      subAgent("dev", { action: "add", tasks: ["c"] }),
    ];
    const todos = todoTree(msgs);
    expect(todos).toHaveLength(2);
    expect(todos[0]).toEqual({
      item: { id: 1, text: "task 1", status: "pending", lane: "Orchestrator" },
      children: [],
    });
    expect(todos[1]).toEqual({
      item: { id: 2, text: "task 2", status: "pending", lane: "Orchestrator" },
      children: [
        { id: 1, text: "a", status: "pending", lane: "dev · call 1" },
        { id: 2, text: "b", status: "pending", lane: "dev · call 1" },
        { id: 1, text: "c", status: "pending", lane: "dev · call 2" },
      ],
    });
  });

  it("falls back to a top-level group when a delegate call lacks parent_task_id", () => {
    // Regression lock: an old-style transcript (no parent_task_id anywhere)
    // must produce the same structure as the pre-change `todosByLane` —
    // orchestrator item + one top-level group per unparented delegation,
    // each child node carrying its lane label and no children.
    const msgs = [
      delegate("dev"),
      subAgent("dev", { action: "add", tasks: ["a"] }),
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      { item: { id: 1, text: "a", status: "pending", lane: "dev" }, children: [] },
    ]);
  });

  it("surfaces a delegation as a top-level group when its parent was removed", () => {
    // Orchestrator delegates → sub-agent adds a todo → orchestrator removes
    // the parent item. The delegation's todos must NOT be dropped; they
    // fall back to a top-level lane group (the same fallback as the
    // unparented case), because the parent uid did not survive.
    const msgs = [
      todoCall({ action: "add", tasks: ["task 1"] }), // id 1
      delegate("dev", { parent_task_id: 1 }),
      subAgent("dev", { action: "add", tasks: ["step a"] }),
      todoCall({ action: "remove", task_id: 1 }), // parent item gone
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      { item: { id: 1, text: "step a", status: "pending", lane: "dev" }, children: [] },
    ]);
  });

  it("does NOT reattach a cleared delegation to a new item that reuses the id", () => {
    // Design §3.4: ids reset after `clear`; without uid binding the same
    // integer (#2) would silently re-parent the old delegation's todos onto
    // a new, unrelated task. This pins that the OLD delegation's todos
    // fall back to a top-level group even though `parent_task_id: 2` was
    // originally meaningful.
    const msgs = [
      todoCall({ action: "add", tasks: ["first", "second"] }), // ids 1, 2
      delegate("dev", { parent_task_id: 2 }), // bound to item 2 at call time
      subAgent("dev", { action: "add", tasks: ["old work"] }),
      todoCall({ action: "update", task_id: 1, status: "completed" }),
      todoCall({ action: "update", task_id: 2, status: "completed" }),
      todoCall({ action: "clear" }), // ids reset to 1
      todoCall({ action: "add", tasks: ["new1", "new2"] }), // new ids 1, 2
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      // Two new orchestrator items — neither carries the old delegation's
      // todos. new2 has children:[] despite the matching integer id 2.
      {
        item: { id: 1, text: "new1", status: "pending", lane: "Orchestrator" },
        children: [],
      },
      {
        item: { id: 2, text: "new2", status: "pending", lane: "Orchestrator" },
        children: [],
      },
      // The old delegation's todo falls back to a top-level group.
      { item: { id: 1, text: "old work", status: "pending", lane: "dev" }, children: [] },
    ]);
  });

  it.each([
    ["string", "abc"],
    ["negative", -1],
    ["out-of-range", 999],
  ])("treats a non-resolving parent_task_id (%s: %s) as unparented", (_label, badId) => {
    // Any non-resolving parent_task_id — wrong type, negative, or beyond the
    // live ids — must NOT throw. The delegation falls back to a top-level
    // lane group, the orchestrator item is childless.
    const msgs = [
      todoCall({ action: "add", tasks: ["task 1"] }), // id 1
      delegate("dev", { parent_task_id: badId }),
      subAgent("dev", { action: "add", tasks: ["a"] }),
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      {
        item: { id: 1, text: "task 1", status: "pending", lane: "Orchestrator" },
        children: [],
      },
      { item: { id: 1, text: "a", status: "pending", lane: "dev" }, children: [] },
    ]);
  });

  it("skips a delegation whose tool_args JSON is malformed, without throwing", () => {
    // Hand-crafted toolcall with non-parseable tool_args. The delegate call
    // is silently skipped — its (empty) lane does not appear at all, the
    // orchestrator item still renders, and the panel is otherwise correct.
    const msgs: WireChatMessage[] = [
      todoCall({ action: "add", tasks: ["task 1"] }), // id 1
      {
        role: "toolcall",
        content: "",
        timestamp: TS,
        tool_name: "delegate",
        tool_args: "{not json",
      },
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      {
        item: { id: 1, text: "task 1", status: "pending", lane: "Orchestrator" },
        children: [],
      },
    ]);
  });

  it("renders an empty delegation as an empty children list, with no stray top-level group", () => {
    // The orchestrator delegates, but the sub-agent never emits a todo.
    // The parent node must still appear (the work is happening) with
    // children:[] — no orphan top-level group surfaces for the empty lane.
    const msgs = [
      todoCall({ action: "add", tasks: ["task 1"] }), // id 1
      delegate("dev", { parent_task_id: 1 }),
      // no subAgent todo rows
    ];
    const todos = todoTree(msgs);
    expect(todos).toEqual([
      {
        item: { id: 1, text: "task 1", status: "pending", lane: "Orchestrator" },
        children: [],
      },
    ]);
    // No stray top-level group (no item with lane "dev" floating alone).
    expect(todos.some((n) => n.item.lane === "dev")).toBe(false);
  });

  it("returns flat childless nodes with no lane labels when no sub-agents ran", () => {
    // Pre-agents rendering preserved bit-for-bit (item has no `lane`, no
    // children). Equivalent to the "no sub-agents" case in the global view
    // when subagents are disabled or simply unused.
    const todos = todoTree([
      todoCall({ action: "add", tasks: ["a", "b"] }),
    ]);
    expect(todos).toEqual([
      { item: { id: 1, text: "a", status: "pending" }, children: [] },
      { item: { id: 2, text: "b", status: "pending" }, children: [] },
    ]);
    expect(todos.every((n) => n.item.lane === undefined && n.children.length === 0)).toBe(true);
  });
});

describe("flatTree", () => {
  it("wraps items into childless nodes in the given order", () => {
    // flatTree is the scoped-view helper: items go in, TodoNodes come out,
    // order preserved, no lane labels, empty children arrays. The TodoPanel
    // uses it for non-global views where the tree isn't surfaced.
    const items = [
      { id: 1, text: "alpha", status: "pending" as const },
      { id: 2, text: "beta", status: "inProgress" as const },
      { id: 3, text: "gamma", status: "completed" as const },
    ];
    const nodes: TodoNode[] = flatTree(items);
    expect(nodes).toEqual([
      { item: { id: 1, text: "alpha", status: "pending" }, children: [] },
      { item: { id: 2, text: "beta", status: "inProgress" }, children: [] },
      { item: { id: 3, text: "gamma", status: "completed" }, children: [] },
    ]);
    expect(nodes.every((n) => n.children.length === 0)).toBe(true);
    expect(nodes.every((n) => n.item.lane === undefined)).toBe(true);
  });

  it("handles an empty items array", () => {
    expect(flatTree([])).toEqual([]);
  });
});

describe("scoped todos carry no lane label", () => {
  it("todosFromMessages never sets lane (scoped view stays unlabeled)", () => {
    const scoped = todosFromMessages([todoCall({ action: "add", tasks: ["a"] })]);
    expect(scoped.every((t) => t.lane === undefined)).toBe(true);
  });
});
