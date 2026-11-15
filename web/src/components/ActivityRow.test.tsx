// Tests for the Focus-mode activity row (plan-focus-mode.md §5 case 24,
// §6.4) — written FIRST, expected to be RED until
// `web/src/components/ActivityRow.tsx` exists (the import below fails to
// resolve, so this file fails at collection for the right reason).
//
// The contract under test:
//
//   <ActivityRow line={ActivityLine} status?={PhoneStatus} />
//   ActivityLine = {kind:"step", step: WireChatMessage | null}
//                | {kind:"working"} | {kind:"idle"}     (mobile redesign §2.5)
//
//   - wrapper: role="status" + aria-live="polite"
//   - label = first non-empty, trimmed line of step.content;
//     falls back to step.tool_name when the content has no usable line;
//     step === null → the generic "Thinking" label
//   - sub-agent steps (source.kind === "sub_agent") get a
//     "🧩 <role> ·" prefix OUTSIDE the label span (D6)
//   - the label span carries title = the full first line — CSS
//     `truncate` clips visually, the title restores the full text
//
// Harness modeled on TopBar.test.tsx (createRoot/act off react-dom/client;
// jsdom is auto-enabled for src/components/** by vitest.config.ts).
// No timers: the animated dots are CSS-only per §6.4, so nothing here is
// time-dependent.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ActivityRow, type ActivityLine, type PhoneStatus } from "./ActivityRow";
import type { WireChatMessage } from "../state";

// React 19's flushSync reads IS_REACT_ACT_ENVIRONMENT; set once so the
// console stays clean, matching the sibling component tests.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ──────────────────────────────────────────────────────────────

const T = "2026-01-01T00:00:00Z";

function step(
  partial: Pick<WireChatMessage, "role"> &
    Partial<Omit<WireChatMessage, "role">>,
): WireChatMessage {
  return { timestamp: T, ...partial };
}

// ─── mount helper ─────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(stepValue: WireChatMessage | null): Promise<HTMLDivElement> {
  return mountLine({ kind: "step", step: stepValue });
}

async function mountLine(line: ActivityLine, status?: PhoneStatus): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<ActivityRow line={line} status={status} />);
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
});

// The label span is the only element that carries a title (§6.4 layout).
function labelOf(el: HTMLDivElement): HTMLSpanElement {
  const label = el.querySelector("span[title]");
  expect(label, "the label span must carry a title attribute").not.toBeNull();
  return label as HTMLSpanElement;
}

// ─── tests (plan §5 case 24) ─────────────────────────────────────────────

describe("ActivityRow — case 24", () => {
  it("renders the generic 'Thinking' label when step is null", async () => {
    const el = await mount(null);
    const status = el.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toContain("Thinking");
  });

  it("renders the first non-empty, trimmed line of a tool call's content as the label", async () => {
    // Server-side format_tool_call shape: an optional thought line first,
    // blank lines and indentation are noise the label must skip/trim.
    const s = step({
      role: "toolcall",
      tool_name: "bash",
      content: "💭 checking the layout\n\n  bash -c 'ls -la /tmp'  ",
    });
    const el = await mount(s);
    const label = labelOf(el);
    expect(label.textContent).toBe("💭 checking the layout");
    expect(label.getAttribute("title")).toBe("💭 checking the layout");
  });

  it("falls back to the tool name when the step has no usable content line", async () => {
    const elEmpty = await mount(step({ role: "toolcall", tool_name: "bash", content: "" }));
    expect(labelOf(elEmpty).textContent).toBe("bash");

    const elBlank = await mount(
      step({ role: "toolcall", tool_name: "web_search", content: "   \n\t \n" }),
    );
    expect(labelOf(elBlank).textContent).toBe("web_search");
  });

  it("prefixes sub-agent steps with '🧩 <role> ·' outside the label span (D6)", async () => {
    const s = step({
      role: "toolcall",
      tool_name: "bash",
      content: "curl https://example.com",
      source: { kind: "sub_agent", role: "researcher" },
    });
    const el = await mount(s);
    // The row as a whole shows the prefix…
    expect(el.textContent).toContain("🧩 researcher ·");
    // …but the label span itself is the bare label (CSS truncates THAT).
    const label = labelOf(el);
    expect(label.textContent).toBe("curl https://example.com");
    expect(label.textContent).not.toContain("🧩");
  });

  it("keeps the full long first line in the label span's title (truncation is CSS-only)", async () => {
    const long = `${"x".repeat(300)} end`; // 305 chars, single line
    const s = step({ role: "toolcall", tool_name: "bash", content: long });
    const el = await mount(s);
    const label = labelOf(el);
    // The DOM text is the full line; CSS `truncate` clips it visually.
    expect(label.textContent).toBe(long);
    expect(label.getAttribute("title")).toBe(long);
  });

  it("exposes the row as a polite status region (role=status + aria-live=polite)", async () => {
    const el = await mount(step({ role: "toolcall", tool_name: "bash", content: "ls" }));
    const status = el.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.getAttribute("aria-live")).toBe("polite");
  });
});

// ─── AR1–AR7: the compact phone line (mobile redesign design §2.5/§2.7) ───
//
//   counts absent  ⇒ today's boxed desktop row (`data-variant="box"`), no counts.
//   counts present ⇒ the compact one-row line (`data-variant="line"`): an
//     optional "working…" label (only while a turn runs), then tappable
//     "⏳ N tasks" / "☑ x/y todos" segments, each shown only when its count
//     is > 0 (todos: total > 0). One row: `flex-nowrap whitespace-nowrap`.

function counts(over: Partial<PhoneStatus> = {}): PhoneStatus {
  return { tasks: 0, todosDone: 0, todosTotal: 0, cwd: null, onOpen: () => {}, ...over };
}

const q = (el: HTMLElement, id: string): HTMLElement | null =>
  el.querySelector<HTMLElement>(`[data-testid="${id}"]`);

describe("ActivityRow — AR1: the line variant", () => {
  it("counts present ⇒ data-variant=line on a polite status root", async () => {
    const el = await mountLine({ kind: "working" }, counts());
    const root_ = q(el, "activity-row")!;
    expect(root_, "the row root").not.toBeNull();
    expect(root_.getAttribute("data-variant")).toBe("line");
    expect(root_.getAttribute("role")).toBe("status");
    expect(root_.getAttribute("aria-live")).toBe("polite");
  });

  it("counts absent ⇒ data-variant=box (the desktop row)", async () => {
    const el = await mount(step({ role: "toolcall", tool_name: "bash", content: "ls" }));
    expect(q(el, "activity-row")!.getAttribute("data-variant")).toBe("box");
  });
});

describe("ActivityRow — AR2: 'working…' only while running", () => {
  it("a working line shows 'working…'", async () => {
    const el = await mountLine({ kind: "working" }, counts({ tasks: 2 }));
    expect(q(el, "activity-label")!.textContent).toContain("working…");
  });

  it("an idle line shows no 'working…' label at all", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 2 }));
    expect(el.textContent).not.toContain("working…");
    expect(q(el, "activity-label")).toBeNull();
  });

  it("a step line keeps the step label and appends the counts (focus mode, D)", async () => {
    const s = step({ role: "toolcall", tool_name: "bash", content: "ls -la" });
    const el = await mountLine({ kind: "step", step: s }, counts({ tasks: 1 }));
    expect(q(el, "activity-label")!.textContent).toBe("ls -la");
    expect(q(el, "activity-tasks"), "the counts ride the same row").not.toBeNull();
  });

  it("a step line with no step reads 'Thinking' in the line variant", async () => {
    const el = await mountLine({ kind: "step", step: null }, counts({ tasks: 1 }));
    expect(q(el, "activity-label")!.textContent).toContain("Thinking");
  });
});

describe("ActivityRow — AR3: counts alone when idle", () => {
  it("idle with tasks and todos shows both segments with their text", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 3, todosDone: 1, todosTotal: 4 }));
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 3 tasks");
    expect(q(el, "activity-todos")!.textContent).toBe("☑ 1/4 todos");
  });

  it("singularises one task", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 1 }));
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 1 task");
  });
});

describe("ActivityRow — AR4: hidden-segment rules", () => {
  it("tasks 0 hides the tasks segment", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 0, todosDone: 0, todosTotal: 2 }));
    expect(q(el, "activity-tasks")).toBeNull();
    expect(q(el, "activity-todos")).not.toBeNull();
  });

  it("todosTotal 0 hides the todos segment (even with todosDone 0)", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 2, todosTotal: 0 }));
    expect(q(el, "activity-todos")).toBeNull();
    expect(q(el, "activity-tasks")).not.toBeNull();
  });

  it("all zero while working shows only the label", async () => {
    const el = await mountLine({ kind: "working" }, counts());
    expect(q(el, "activity-label")).not.toBeNull();
    expect(q(el, "activity-tasks")).toBeNull();
    expect(q(el, "activity-todos")).toBeNull();
  });

  it("a completed-everything todo list still shows x/y", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ todosDone: 3, todosTotal: 3 }));
    expect(q(el, "activity-todos")!.textContent).toBe("☑ 3/3 todos");
  });
});

describe("ActivityRow — AR5: tapping a count opens its panel", () => {
  it("tasks → onOpen('tasks'), todos → onOpen('todo'), with their aria-labels", async () => {
    const onOpen = vi.fn();
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 2, todosDone: 0, todosTotal: 2, onOpen }));
    const tasks = q(el, "activity-tasks")!;
    const todos = q(el, "activity-todos")!;
    expect(tasks.tagName).toBe("BUTTON");
    expect(tasks.getAttribute("aria-label")).toBe("2 running tasks");
    expect(todos.tagName).toBe("BUTTON");
    expect(todos.getAttribute("aria-label")).toBe("0 of 2 todos done");

    await act(async () => {
      tasks.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onOpen).toHaveBeenLastCalledWith("tasks");
    await act(async () => {
      todos.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onOpen).toHaveBeenLastCalledWith("todo");
    expect(onOpen).toHaveBeenCalledTimes(2);
  });
});

describe("ActivityRow — AR8: the '·' separators (batch 1, B18)", () => {
  // DOM order of the row's meaningful children: segment testids plus the
  // '·' separators. Decorative children (the pulse dot, glyphs) carry no
  // testid and no '·' text, so they drop out of the sequence.
  const seqOf = (el: HTMLDivElement): string[] =>
    [...q(el, "activity-row")!.children]
      .map((c) => {
        const id = (c as HTMLElement).getAttribute("data-testid");
        if (id === "activity-sep") return "·";
        if (id) return id;
        return (c as HTMLElement).textContent?.trim() === "·" ? "·" : null;
      })
      .filter((x): x is string => x !== null);

  it("a '·' separator (aria-hidden) sits between the label and each count, and between the counts", async () => {
    const el = await mountLine(
      { kind: "working" },
      counts({ tasks: 2, todosDone: 1, todosTotal: 3 }),
    );
    expect(q(el, "activity-label"), "the working label renders").not.toBeNull();
    const seps = Array.from(
      q(el, "activity-row")!.querySelectorAll("span[aria-hidden]"),
    ).filter((s) => (s.textContent ?? "").trim() === "·");
    expect(seps.length, "one separator between each pair of segments").toBe(2);
    seps.forEach((s) =>
      expect(s.getAttribute("aria-hidden"), "the separators are aria-hidden").toBe("true"),
    );
    expect(seqOf(el), "the row reads label, ·, tasks, ·, todos").toEqual([
      "activity-label",
      "·",
      "activity-tasks",
      "·",
      "activity-todos",
    ]);
  });

  it("idle: a lone count stands alone, two counts keep exactly one '·' between them", async () => {
    const lone = await mountLine({ kind: "idle" }, counts({ tasks: 2 }));
    expect(seqOf(lone), "no separator around a lone count").toEqual(["activity-tasks"]);

    const pair = await mountLine(
      { kind: "idle" },
      counts({ tasks: 2, todosDone: 1, todosTotal: 3 }),
    );
    expect(seqOf(pair), "the counts read tasks, ·, todos").toEqual([
      "activity-tasks",
      "·",
      "activity-todos",
    ]);
  });
});

describe("ActivityRow — AR6/AR7: one compact row", () => {
  it("the line root is a single non-wrapping row (flex-nowrap + whitespace-nowrap)", async () => {
    const el = await mountLine({ kind: "working" }, counts({ tasks: 12, todosDone: 10, todosTotal: 11 }));
    const cls = q(el, "activity-row")!.className;
    expect(cls).toContain("flex-nowrap");
    expect(cls).toContain("whitespace-nowrap");
    expect(cls, "no boxed-strip border in the line variant").not.toMatch(/(^|\s)border(\s|$)/);
  });

  it("the line is small and muted (text-xs or smaller, no full-width box)", async () => {
    const el = await mountLine({ kind: "idle" }, counts({ tasks: 1 }));
    // The size class may sit on the root or a child; the design only fixes 11–12px.
    expect(el.innerHTML).toMatch(/text-(\[1[12]px\]|xs)/);
    expect(q(el, "activity-row")!.className).not.toContain("rounded-md");
  });
});

describe("ActivityRow — C12: the box variant is unchanged with no counts", () => {
  it("keeps the boxed strip, the dots and no count segments", async () => {
    const el = await mount(step({ role: "toolcall", tool_name: "bash", content: "ls" }));
    const status = el.querySelector('[role="status"]') as HTMLElement;
    expect(status.className).toContain("border");
    expect(status.className).toContain("rounded-md");
    expect(q(el, "activity-tasks")).toBeNull();
    expect(q(el, "activity-todos")).toBeNull();
  });

  it("the wrapper reserves the rail only from lg (lg:mr-12, no bare mr-12)", async () => {
    const el = await mount(step({ role: "toolcall", tool_name: "bash", content: "ls" }));
    const wrapper = el.firstElementChild as HTMLElement;
    expect(wrapper.classList.contains("lg:mr-12")).toBe(true);
    expect(wrapper.classList.contains("mr-12")).toBe(false);
  });

});

// ─── PhoneStatus redesign (design §5.2, §5.3) ─────────────────────────────
//
// StatusCounts is renamed PhoneStatus and gains a `cwd` field.
// ActivityRow takes `status` instead of `counts`. The phone line becomes an
// item list joined by "·" separators; it returns null when empty.

function status(over: Partial<PhoneStatus> = {}): PhoneStatus {
  return {
    tasks: 0,
    todosDone: 0,
    todosTotal: 0,
    cwd: null,
    onOpen: () => {},
    ...over,
  };
}

describe("ActivityRow — PhoneStatus (design §5.2)", () => {
  it("idle with tasks, todos, cwd shows all items in order", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ tasks: 2, todosDone: 6, todosTotal: 25, cwd: "/home/exe/infra" }),
    );
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 2 tasks");
    expect(q(el, "activity-todos")!.textContent).toBe("☑ 6/25 todos");
    expect(q(el, "activity-dir")!.textContent).toContain("📁 infra");
  });

  it("working line shows compact counts (no 'tasks'/'todos' words)", async () => {
    const el = await mountLine(
      { kind: "working" },
      status({ tasks: 2, todosDone: 6, todosTotal: 25, cwd: "/home/exe/infra" }),
    );
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 2");
    expect(q(el, "activity-todos")!.textContent).toBe("☑ 6/25");
    expect(q(el, "activity-dir")!.textContent).toContain("📁 infra");
  });

  it("step line shows compact counts", async () => {
    const s = step({ role: "toolcall", tool_name: "bash", content: "ls" });
    const el = await mountLine(
      { kind: "step", step: s },
      status({ tasks: 2, todosDone: 6, todosTotal: 25, cwd: "/home/exe/infra" }),
    );
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 2");
    expect(q(el, "activity-todos")!.textContent).toBe("☑ 6/25");
  });

  it("aria-labels are 'N running tasks' and 'x of y todos done' in both states", async () => {
    const idle = await mountLine(
      { kind: "idle" },
      status({ tasks: 2, todosDone: 6, todosTotal: 25 }),
    );
    expect(q(idle, "activity-tasks")!.getAttribute("aria-label")).toBe("2 running tasks");
    expect(q(idle, "activity-todos")!.getAttribute("aria-label")).toBe("6 of 25 todos done");

    const working = await mountLine(
      { kind: "working" },
      status({ tasks: 2, todosDone: 6, todosTotal: 25 }),
    );
    expect(q(working, "activity-tasks")!.getAttribute("aria-label")).toBe("2 running tasks");
    expect(q(working, "activity-todos")!.getAttribute("aria-label")).toBe("6 of 25 todos done");
  });

  it("singular '1 running task' and '⏳ 1 task'", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ tasks: 1 }),
    );
    expect(q(el, "activity-tasks")!.textContent).toBe("⏳ 1 task");
    expect(q(el, "activity-tasks")!.getAttribute("aria-label")).toBe("1 running task");
  });

  it("separators only between items (no leading/trailing)", async () => {
    const el = await mountLine(
      { kind: "working" },
      status({ tasks: 2, todosDone: 6, todosTotal: 25, cwd: "/home/exe/infra" }),
    );
    // 4 items → 3 separators
    const seps = el.querySelectorAll('[data-testid="activity-sep"]');
    expect(seps.length).toBe(3);
  });

  it("working only → 0 separators, no leading ·", async () => {
    const el = await mountLine(
      { kind: "working" },
      status({}),
    );
    expect(el.querySelectorAll('[data-testid="activity-sep"]').length).toBe(0);
    expect(el.textContent).not.toContain("·");
  });

  it("working + dir → 1 separator", async () => {
    const el = await mountLine(
      { kind: "working" },
      status({ cwd: "/home/exe/infra" }),
    );
    expect(el.querySelectorAll('[data-testid="activity-sep"]').length).toBe(1);
  });

  it("dir only → 0 separators", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ cwd: "/home/exe/infra" }),
    );
    expect(el.querySelectorAll('[data-testid="activity-sep"]').length).toBe(0);
  });

  it("step with sub-agent: the 🧩 role · is not counted as a separator", async () => {
    const s = step({
      role: "toolcall",
      tool_name: "bash",
      content: "curl https://example.com",
      source: { kind: "sub_agent", role: "mid" },
    });
    const el = await mountLine(
      { kind: "step", step: s },
      status({ cwd: "/home/exe/infra" }),
    );
    // One separator between step and dir; the "🧩 mid ·" is part of the step label
    expect(el.querySelectorAll('[data-testid="activity-sep"]').length).toBe(1);
  });

  it("nothing (idle, 0, 0, null) → renders nothing", async () => {
    const el = await mountLine({ kind: "idle" }, status({}));
    expect(el.querySelector('[data-testid="activity-row"]')).toBeNull();
  });

  it("tapping 📁 calls onOpen('directory')", async () => {
    const onOpen = vi.fn();
    const el = await mountLine(
      { kind: "idle" },
      status({ cwd: "/home/exe/infra", onOpen }),
    );
    const dir = q(el, "activity-dir")!;
    await act(async () => {
      dir.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onOpen).toHaveBeenCalledWith("directory");
  });

  it("dir item title is full path, text is basename", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ cwd: "/home/exe/infra" }),
    );
    const dir = q(el, "activity-dir")!;
    expect(dir.getAttribute("title")).toBe("/home/exe/infra");
    expect(dir.textContent).toContain("📁 infra");
    expect(dir.textContent).not.toContain("/home/exe");
  });

  it("Windows path basename works", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ cwd: "C:\\proj\\x" }),
    );
    expect(q(el, "activity-dir")!.textContent).toContain("📁 x");
  });

  it("📁 item absent when cwd is null", async () => {
    const el = await mountLine(
      { kind: "idle" },
      status({ tasks: 2, cwd: null }),
    );
    expect(q(el, "activity-dir")).toBeNull();
  });
});
