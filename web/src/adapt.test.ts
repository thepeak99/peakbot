import { describe, it, expect } from "vitest";
import { todosFromMessages } from "./adapt";
import type { WireChatMessage } from "./state";

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
