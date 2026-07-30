import { describe, it, expect } from "vitest";
import {
  adaptStats,
  assignDelegationCalls,
  deriveSubAgentRoster,
  filterMessagesByView,
  parseCallKey,
  todosByLane,
  todosFromMessages,
  viewLabel,
} from "./adapt";
import type { AppState, WireChatMessage, WireStats } from "./state";

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
function delegate(role: string): WireChatMessage {
  return {
    role: "toolcall",
    content: "",
    timestamp: TS,
    tool_name: "delegate",
    tool_args: JSON.stringify({ role, task: "do it" }),
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

  // A long conversation can lose its `delegate` ToolCalls (the ToolResult lands
  // thousands of messages later, so pair-sanitising drops it). Splitting must
  // still work: it keys off contiguous runs of a role's messages, not the
  // delegate call. Regression for "junior 1641 msg collapsed into one row".
  it("splits delegations with no delegate ToolCall present at all", () => {
    const roster = deriveSubAgentRoster([
      subAgent("junior"),
      subAgent("junior"),
      subAgent("senior"), // another lane spoke → junior's next run is call 2
      subAgent("junior"),
      todoCall({ action: "add", tasks: ["orchestrator turn"] }), // orch lane
      subAgent("junior"), // → call 3
    ]);
    expect(roster.map((r) => r.key)).toEqual([
      "junior#1",
      "senior#1",
      "junior#2",
      "junior#3",
    ]);
    expect(roster.find((r) => r.key === "junior#1")?.count).toBe(2);
    expect(roster.find((r) => r.key === "junior#3")?.count).toBe(1);
  });

  // Consecutive turns by one role are ONE delegation, however many messages it
  // spans — the run only breaks when a different lane speaks.
  it("keeps an uninterrupted run as a single delegation", () => {
    const roster = deriveSubAgentRoster([
      subAgent("tester"),
      subAgent("tester"),
      subAgent("tester"),
    ]);
    expect(roster).toHaveLength(1);
    expect(roster[0]).toMatchObject({ key: "tester#1", count: 3 });
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

describe("todosByLane", () => {
  it("replays each lane independently and labels every item", () => {
    const msgs = [
      todoCall({ action: "add", tasks: ["orchestrate"] }), // orchestrator lane
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["write prd"] }),
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["revise prd"] }),
    ];
    const todos = todosByLane(msgs);
    // Orchestrator first, then each pm delegation in call order.
    expect(todos.map((t) => ({ lane: t.lane, text: t.text, id: t.id }))).toEqual([
      { lane: "Orchestrator", text: "orchestrate", id: 1 },
      { lane: "pm · call 1", text: "write prd", id: 1 },
      { lane: "pm · call 2", text: "revise prd", id: 1 },
    ]);
  });

  it("keeps per-lane id spaces from colliding (each lane restarts at 1)", () => {
    const msgs = [
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["a", "b"] }),
      delegate("dev"),
      subAgent("dev", { action: "add", tasks: ["x"] }),
    ];
    const todos = todosByLane(msgs);
    // dev's single todo gets id 1, independent of pm's id space (1,2).
    const dev = todos.find((t) => t.lane === "dev");
    expect(dev).toMatchObject({ id: 1, text: "x", lane: "dev" });
  });

  it("labels a single-call role without a call suffix", () => {
    const todos = todosByLane([
      delegate("pm"),
      subAgent("pm", { action: "add", tasks: ["only"] }),
    ]);
    expect(todos).toEqual([
      { id: 1, text: "only", status: "pending", lane: "pm" },
    ]);
  });

  it("leaves todos unlabeled when no sub-agents ran (pre-agents behavior)", () => {
    // Subagents disabled / never used ⇒ no delegation in the transcript.
    // Todos must carry no lane so the panel renders no pill, exactly as before
    // the sub-agents feature existed.
    const todos = todosByLane([
      todoCall({ action: "add", tasks: ["a", "b"] }),
    ]);
    expect(todos).toEqual([
      { id: 1, text: "a", status: "pending" },
      { id: 2, text: "b", status: "pending" },
    ]);
    expect(todos.every((t) => t.lane === undefined)).toBe(true);
  });
});

describe("scoped todos carry no lane label", () => {
  it("todosFromMessages never sets lane (scoped view stays unlabeled)", () => {
    const scoped = todosFromMessages([todoCall({ action: "add", tasks: ["a"] })]);
    expect(scoped.every((t) => t.lane === undefined)).toBe(true);
  });
});

// The Session panel shows one "in"/"out" figure above the per-agent rows. The
// flat wire totals are LAST-request only, so reading them there would make the
// header contradict its own breakdown (the bug: junior 1641 vs session 425).
// adaptStats derives the session figure from the lanes instead.
describe("adaptStats reconciles session totals with the lane breakdown", () => {
  function stateWithLanes(lanes: WireStats["lanes"]): AppState {
    return {
      chat: { messages: [] },
      todo: { visible: false, items: [] },
      stats: {
        // Deliberately the LAST request's numbers, as the backend sends them.
        total_input_tokens: 425,
        total_output_tokens: 12,
        total_api_calls: 4,
        total_cost: 0.5,
        lanes,
        model: "m",
        provider_name: "p",
        model_alias: "a",
      },
      context: {
        current_usage: 0,
        window_size: 1000,
        compaction_enabled: true,
        compaction_threshold: 0.8,
      },
      conversation: null,
      is_running: false,
      is_loading: false,
      welcome: null,
      exit_requested: false,
      bg: { recent_summaries: [] },
      bash_panel: { visible: false, entries: [] },
    } as unknown as AppState;
  }

  it("sums the lanes rather than echoing the last request", () => {
    const stats = adaptStats(
      stateWithLanes([
        {
          lane: "orchestrator",
          input_tokens: 425,
          output_tokens: 12,
          api_calls: 2,
          cost: 0.2,
        },
        {
          lane: "junior",
          input_tokens: 1641,
          output_tokens: 88,
          api_calls: 2,
          cost: 0.3,
        },
      ]),
    );
    // The total is the sum of its parts — never smaller than a single row.
    expect(stats.inputTokens).toBe(2066);
    expect(stats.outputTokens).toBe(100);
    const rowSum = stats.lanes.reduce((a, l) => a + l.inputTokens, 0);
    expect(stats.inputTokens).toBe(rowSum);
  });

  it("falls back to the flat totals when no lanes are reported", () => {
    const stats = adaptStats(stateWithLanes([]));
    expect(stats.inputTokens).toBe(425);
    expect(stats.outputTokens).toBe(12);
  });
});
