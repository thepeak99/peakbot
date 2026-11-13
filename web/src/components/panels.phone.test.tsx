// Batch 4 phone contracts (fix-plan §4 BATCH 4, items 4.14–4.22): with
// `phone: true` the six panels render the phone faces — the Todo header +
// progress bar + circle rows (C6–C8), the Tasks header + dot rows without the
// parked Stop (C9/C10), the Session Cost/Tokens/Context rows (C11/C12), the
// left-to-right Files rows with the kept Copy-for-git-add (C13), the flat
// Agents rows with the Watch pill (C15) and the Full-edition Bash face (C14)
// — while `phone` absent keeps today's desktop drawer body exactly (R1 lock).
//
// The panels exist and are imported as-is; `phone` is passed through a typed
// alias so the file typechecks before the prop lands (4.13) and every new
// assertion fails on the DOM, not on an import.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { TodoPanel } from "./TodoPanel";
import { BgPanel } from "./BgPanel";
import { StatsPanel } from "./StatsPanel";
import { FilesPanel } from "./FilesPanel";
import { AgentsPanel } from "./AgentsPanel";
import { BashPanel as BashPanelView } from "./BashPanel";
import { setBootEdition } from "../desktop";
import type {
  BgProcess,
  BashPanel as BashPanelData,
  CatalogAgent,
  ContextUsage,
  FileEdit,
  SessionStats,
  TodoItem,
  TodoNode,
  TodoStatus,
  ViewFilter,
} from "../types";

// 4.13 adds `phone?: boolean` to each panel's props; until it lands the casts
// keep `tsc --noEmit` clean while the assertions below stay RED (feature
// absent).
const PhoneTodoPanel = TodoPanel as unknown as ComponentType<{ nodes: TodoNode[]; phone?: boolean }>;
const PhoneBgPanel = BgPanel as unknown as ComponentType<{ processes: BgProcess[]; phone?: boolean }>;

interface StatsPanelProps {
  stats: SessionStats;
  context: ContextUsage;
  peakbotVersion?: string;
  scopeLabel?: string | null;
  laneMessages: Record<string, number>;
}
const PhoneStatsPanel = StatsPanel as unknown as ComponentType<StatsPanelProps & { phone?: boolean }>;

const PhoneFilesPanel = FilesPanel as unknown as ComponentType<{ files: FileEdit[]; phone?: boolean }>;

interface AgentsPanelProps {
  boundAgent: CatalogAgent | null;
  active: ViewFilter;
  onSelect: (filter: ViewFilter) => void;
  roster: { key: string; role: string; n: number; count: number }[];
  laneCalls: Record<string, number>;
  laneMessages: Record<string, number>;
}
const PhoneAgentsPanel = AgentsPanel as unknown as ComponentType<AgentsPanelProps & { phone?: boolean }>;

const PhoneBashPanel = BashPanelView as unknown as ComponentType<{
  panel: BashPanelData;
  phone?: boolean;
}>;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let writeText: ReturnType<typeof vi.fn>;
beforeEach(() => {
  writeText = vi.fn();
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
});

afterEach(() => {
  // The edition is module state in desktop.ts; default it back.
  setBootEdition("full");
  document.body.innerHTML = "";
});

async function render(node: React.ReactNode): Promise<{
  el: HTMLElement;
  unmount: () => Promise<void>;
}> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  await act(async () => {
    root.render(node);
  });
  return {
    el,
    unmount: async () => {
      await act(async () => {
        root.unmount();
      });
    },
  };
}

/** The row (li) whose text contains the label; null when absent. */
const rowOf = (el: HTMLElement, label: string): HTMLElement | null =>
  [...el.querySelectorAll("li")].find((r) => (r.textContent ?? "").includes(label)) ?? null;

/** The span whose text ends with the item text (a lane label may precede it). */
const textEl = (row: HTMLElement, text: string): HTMLElement | null =>
  [...row.querySelectorAll("span")].find((s) => (s.textContent ?? "").endsWith(text)) ?? null;

// ─── fixtures ─────────────────────────────────────────────────────────────

const item = (id: number, text: string, status: TodoStatus, lane?: string): TodoItem =>
  lane ? { id, text, status, lane } : { id, text, status };

// 3 of 5 done: three completed, one in progress, one pending (todoCounts:
// open = pending | inProgress, done = total − open).
const TODO_NODES: TodoNode[] = [
  { item: item(1, "write the plan", "completed", "Orchestrator"), children: [] },
  { item: item(2, "dig into the logs", "completed", "researcher"), children: [] },
  { item: item(3, "patch the parser", "inProgress", "coder"), children: [] },
  { item: item(4, "run the suite", "pending", "coder"), children: [] },
  { item: item(5, "ship it", "completed", "Orchestrator"), children: [] },
];

const BG_PROCESSES: BgProcess[] = [
  { id: 1, command: "sleep 100", label: "night job", status: "running" },
  { id: 2, command: "ls", status: "exited", exitCode: 0 },
];

const STATS: SessionStats = {
  inputTokens: 18400,
  outputTokens: 4000,
  apiCalls: 12,
  costUsd: 0.4217,
  modelAlias: "sonnet",
  model: "claude-sonnet-4-5",
  provider: "anthropic",
  lanes: [],
};
const CONTEXT: ContextUsage = {
  currentUsage: 4600,
  windowSize: 10000,
  compactionThreshold: 0.8,
};

const FILES: FileEdit[] = [
  { path: "src/parser.rs", edits: 3, kind: "modified" },
  { path: "src/token.rs", edits: 1, kind: "created" },
  { path: "README.md", edits: 1, kind: "read" },
];

const ROSTER = [
  { key: "researcher#1", role: "researcher", n: 1, count: 3 },
  { key: "coder#1", role: "coder", n: 1, count: 5 },
  { key: "coder#2", role: "coder", n: 2, count: 2 },
];
const LANE_MESSAGES = { researcher: 4, coder: 8 };
const LANE_CALLS = { researcher: 2, coder: 3 };
const BOUND_AGENT: CatalogAgent = {
  name: "team",
  model: "sonnet",
  subagents: [
    { name: "researcher", model: "sonnet", inherited: true },
    { name: "coder", model: "opus", inherited: false },
  ],
};

const BASH_PANEL: BashPanelData = {
  command: "cargo test -p auth",
  status: "running",
  pid: 4242,
  elapsed: "3s",
  tail: ["running 12 tests", "test token::new ... ok"],
};

// ─── TodoPanel: phone ─────────────────────────────────────────────────────

describe("TodoPanel — phone (phone: true)", () => {
  it("header reads 'Todo' + '3 of 5 done' and the progressbar has aria-valuenow 3 / aria-valuemax 5 (C6/C7)", async () => {
    const { el, unmount } = await render(<PhoneTodoPanel nodes={TODO_NODES} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Todo");
      expect(el.textContent, "the done count line").toContain("3 of 5 done");
      const bar = el.querySelector<HTMLElement>('[role="progressbar"]');
      expect(bar, "the progress bar renders").not.toBeNull();
      expect(bar!.getAttribute("aria-label"), "the bar's accessible name").toBe("Todo progress");
      expect(bar!.getAttribute("aria-valuemin")).toBe("0");
      expect(bar!.getAttribute("aria-valuemax")).toBe("5");
      expect(bar!.getAttribute("aria-valuenow")).toBe("3");
    } finally {
      await unmount();
    }
  });

  it("a completed row's text is struck through and lane labels still render (C8)", async () => {
    const { el, unmount } = await render(<PhoneTodoPanel nodes={TODO_NODES} phone />);
    try {
      const done = rowOf(el, "write the plan");
      expect(done, "the completed row renders").not.toBeNull();
      const doneText = textEl(done!, "write the plan");
      expect(doneText, "the row's text cell").not.toBeNull();
      expect(doneText!.classList.contains("line-through"), "completed is struck through").toBe(true);
      const open = rowOf(el, "patch the parser");
      expect(
        textEl(open!, "patch the parser")!.classList.contains("line-through"),
        "open rows are not struck",
      ).toBe(false);
      const laneRow = rowOf(el, "dig into the logs");
      expect(
        [...laneRow!.querySelectorAll("span")].some((s) => s.textContent === "researcher"),
        "the lane label renders",
      ).toBe(true);
    } finally {
      await unmount();
    }
  });

  it("each row's circle marks its status: emerald check done, amber in progress, plain pending (C8)", async () => {
    const { el, unmount } = await render(<PhoneTodoPanel nodes={TODO_NODES} phone />);
    try {
      const circle = (row: HTMLElement): HTMLElement | null =>
        row.querySelector<HTMLElement>("span.rounded-full");
      const doneC = circle(rowOf(el, "write the plan")!);
      expect(doneC, "the completed circle renders").not.toBeNull();
      expect(doneC!.classList.contains("border-emerald-600"), "done is emerald").toBe(true);
      expect(doneC!.classList.contains("bg-emerald-600"), "done is filled").toBe(true);
      expect(doneC!.querySelector("svg"), "done carries the check").not.toBeNull();
      const progC = circle(rowOf(el, "patch the parser")!);
      expect(progC, "the in-progress circle renders").not.toBeNull();
      expect(progC!.classList.contains("border-amber-500"), "in progress is amber").toBe(true);
      expect(progC!.querySelector("svg"), "in progress has no check").toBeNull();
      const pendC = circle(rowOf(el, "run the suite")!);
      expect(pendC, "the pending circle renders").not.toBeNull();
      expect(pendC!.classList.contains("border-zinc-700"), "pending is plain").toBe(true);
      expect(pendC!.querySelector("svg"), "pending has no check").toBeNull();
    } finally {
      await unmount();
    }
  });
});

// ─── BgPanel: phone ───────────────────────────────────────────────────────

describe("BgPanel — phone (phone: true)", () => {
  it("header 'Tasks' + '1 running'; rows show name + status sub; no Stop button (C9/C10, parked lock)", async () => {
    const { el, unmount } = await render(<PhoneBgPanel processes={BG_PROCESSES} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Tasks");
      expect(el.textContent, "the running count").toContain("1 running");
      const run = rowOf(el, "sleep 100");
      expect(run, "the running row renders").not.toBeNull();
      expect(run!.querySelector("b")?.textContent, "the row's name").toBe("sleep 100");
      expect(run!.textContent, "the label + running sub").toContain("night job · running");
      const dead = rowOf(el, "ls");
      expect(dead, "the exited row renders").not.toBeNull();
      expect(dead!.textContent, "the exited sub").toContain("exited (code 0)");
      expect(el.querySelector("button"), "no Stop button (parked: no wire frame)").toBeNull();
    } finally {
      await unmount();
    }
  });
});

// ─── StatsPanel: phone ────────────────────────────────────────────────────

describe("StatsPanel — phone (phone: true)", () => {
  const base: StatsPanelProps = { stats: STATS, context: CONTEXT, laneMessages: LANE_MESSAGES };

  it("Cost shows 2 decimals; Tokens reads '18.4k in · 4.0k out'; Context shows a %; no 'calls' row (C11/C12)", async () => {
    const { el, unmount } = await render(<PhoneStatsPanel {...base} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Session");
      expect(el.textContent, "the global scope sub").toContain("this chat");
      expect(el.textContent, "the 2-decimal cost").toContain("$0.42");
      expect(el.textContent, "the combined tokens line").toContain("18.4k in · 4.0k out");
      expect(el.textContent, "the context percentage").toContain("46%");
      expect(el.textContent, "no calls row").not.toContain("calls");
      expect(el.textContent, "no model row").not.toContain("sonnet");
      expect(el.textContent, "no peakbot row").not.toContain("peakbot");
    } finally {
      await unmount();
    }
  });

  it("a scoped session names its lane in the header (C11)", async () => {
    const { el, unmount } = await render(<PhoneStatsPanel {...base} scopeLabel="researcher" phone />);
    try {
      expect(el.textContent, "the scope label stays as the header sub").toContain(
        "scoped to researcher",
      );
    } finally {
      await unmount();
    }
  });
});

// ─── FilesPanel: phone ────────────────────────────────────────────────────

describe("FilesPanel — phone (phone: true)", () => {
  it("the path is left-to-right (no dir=rtl) and 'Copy 2 for git add' is still there (C13 + keep)", async () => {
    const { el, unmount } = await render(<PhoneFilesPanel files={FILES} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Files");
      expect(el.textContent, "the touched count").toContain("3 touched");
      expect(el.querySelector('[dir="rtl"]'), "no rtl path").toBeNull();
      expect(el.textContent, "the paths render").toContain("src/parser.rs");
      expect(el.textContent, "the edits count").toContain("×3");
      const copy = [...el.querySelectorAll("button")].find((b) =>
        (b.textContent ?? "").includes("Copy 2 for git add"),
      );
      expect(copy, "the Copy-for-git-add button is kept").toBeDefined();
      await act(async () => {
        copy!.click();
      });
      expect(writeText, "copying writes the changed paths").toHaveBeenCalledTimes(1);
      expect(writeText).toHaveBeenCalledWith("src/parser.rs\nsrc/token.rs");
    } finally {
      await unmount();
    }
  });
});

// ─── AgentsPanel: phone ───────────────────────────────────────────────────

describe("AgentsPanel — phone (phone: true)", () => {
  const base: AgentsPanelProps = {
    boundAgent: BOUND_AGENT,
    active: "coder",
    onSelect: () => {},
    roster: ROSTER,
    laneCalls: LANE_CALLS,
    laneMessages: LANE_MESSAGES,
  };

  it("flat rows: Global, Main agent and each role have a 'Watch' button, per-call rows under their role (C15)", async () => {
    const { el, unmount } = await render(<PhoneAgentsPanel {...base} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Agents");
      const rows = [...el.querySelectorAll("li")];
      expect(
        rows.map((r) => r.querySelector("b")?.textContent),
        "the flat row labels, in order",
      ).toEqual(["Global", "Main agent", "researcher", "coder", "coder · call 1", "coder · call 2"]);
      for (const row of rows) {
        expect(row.querySelector("button"), "each row carries the Watch pill").not.toBeNull();
      }
      // The roster section and both captions are not rendered on the phone (Q1).
      expect(el.textContent, "no roster section").not.toContain("Roster");
      expect(el.textContent, "no roster caption").not.toContain("Configured agents");
      expect(el.textContent, "no view caption").not.toContain("Picking a role is a view");
      expect(rowOf(el, "researcher")!.textContent, "the role sub").toContain("4 msg · 2 calls");
      expect(rowOf(el, "Global")!.textContent, "the Global hint stays").toContain(
        "All agents, one transcript with badges",
      );
    } finally {
      await unmount();
    }
  });

  it("tapping Watch calls onSelect(id); the active row reads 'Watching' (C15)", async () => {
    const onSelect = vi.fn();
    const { el, unmount } = await render(<PhoneAgentsPanel {...base} onSelect={onSelect} phone />);
    try {
      const globalBtn = rowOf(el, "Global")!.querySelector<HTMLButtonElement>("button");
      expect(globalBtn, "the Global Watch pill").not.toBeNull();
      expect(globalBtn!.textContent, "the inactive pill reads Watch").toBe("Watch");
      expect(globalBtn!.getAttribute("aria-pressed")).toBe("false");

      const coderBtn = rowOf(el, "coder")!.querySelector<HTMLButtonElement>("button");
      expect(coderBtn!.textContent, "the active row reads Watching").toBe("Watching");
      expect(coderBtn!.getAttribute("aria-pressed"), "the active row is pressed").toBe("true");

      const dot = (row: HTMLElement): HTMLElement | null =>
        row.querySelector<HTMLElement>("span.rounded-full");
      expect(dot(rowOf(el, "coder")!)!.classList.contains("bg-sky-400"), "active dot is sky").toBe(true);
      expect(dot(rowOf(el, "Global")!)!.classList.contains("bg-zinc-600"), "idle dot is plain").toBe(
        true,
      );

      await act(async () => {
        globalBtn!.click();
      });
      expect(onSelect, "Global's Watch calls onSelect").toHaveBeenCalledWith("global");

      const call2Btn = rowOf(el, "coder · call 2")!.querySelector<HTMLButtonElement>("button");
      await act(async () => {
        call2Btn!.click();
      });
      expect(onSelect, "a per-call row's Watch selects that call").toHaveBeenCalledWith(
        "coder#2",
      );
    } finally {
      await unmount();
    }
  });
});

// ─── BashPanel: phone ─────────────────────────────────────────────────────

describe("BashPanel — phone (phone: true)", () => {
  it("full edition: the 'Bash' + command header and the terminal output (C14)", async () => {
    setBootEdition("full");
    const { el, unmount } = await render(<PhoneBashPanel panel={BASH_PANEL} phone />);
    try {
      expect(el.querySelector("h3")?.textContent, "the phone title").toBe("Bash");
      expect(el.textContent, "the command sub").toContain("cargo test -p auth");
      expect(el.textContent, "the output stays").toContain("running 12 tests");
    } finally {
      await unmount();
    }
  });

  it("local edition: renders nothing (C14, by design)", async () => {
    setBootEdition("local");
    const { el, unmount } = await render(<PhoneBashPanel panel={BASH_PANEL} phone />);
    try {
      expect(el.innerHTML, "the Local edition hides the panel").toBe("");
    } finally {
      await unmount();
    }
  });
});

// ─── desktop locks (no phone, R1) ─────────────────────────────────────────

describe("panels — desktop lock (no phone, R1)", () => {
  it("the desktop TodoPanel keeps the '3/5' count and the status glyphs, no strike-through", async () => {
    const { el, unmount } = await render(<TodoPanel nodes={TODO_NODES} />);
    try {
      expect(el.textContent, "the done/total count").toContain("3/5");
      expect(el.textContent, "the glyphs stay").toContain("✔");
      expect(el.textContent).toContain("◐");
      const doneText = textEl(rowOf(el, "write the plan")!, "write the plan");
      expect(
        doneText!.classList.contains("line-through"),
        "the desktop completed row is not struck",
      ).toBe(false);
    } finally {
      await unmount();
    }
  });

  it("the desktop BgPanel keeps the 'Background' + '🛰 1' header and the '#id command' rows", async () => {
    const { el, unmount } = await render(<BgPanel processes={BG_PROCESSES} />);
    try {
      expect(el.textContent, "the header title").toContain("Background");
      expect(el.textContent, "the running count").toContain("🛰 1");
      expect(el.textContent, "the mono id + command row").toContain("#1 sleep 100");
      expect(el.textContent).toContain("exited (code 0)");
    } finally {
      await unmount();
    }
  });

  it("the desktop StatsPanel keeps the model/calls rows, the 4-decimal cost and the ticked meter", async () => {
    const { el, unmount } = await render(
      <StatsPanel stats={STATS} context={CONTEXT} laneMessages={LANE_MESSAGES} />,
    );
    try {
      expect(el.textContent, "the model row").toContain("sonnet");
      expect(el.textContent, "the calls row").toContain("calls");
      expect(el.textContent, "the 4-decimal cost").toContain("$0.4217");
      expect(el.querySelector('[title="compaction at 80%"]'), "the compaction tick").not.toBeNull();
    } finally {
      await unmount();
    }
  });

  it("the desktop FilesPanel keeps the kind glyph, the rtl mono path and the copy button", async () => {
    const { el, unmount } = await render(<FilesPanel files={FILES} />);
    try {
      expect(el.querySelector('[dir="rtl"]'), "the rtl path stays").not.toBeNull();
      expect(el.textContent, "the kind glyph stays").toContain("✎");
      const copy = [...el.querySelectorAll("button")].find((b) =>
        (b.textContent ?? "").includes("Copy 2 for git add"),
      );
      expect(copy, "the copy button stays").toBeDefined();
    } finally {
      await unmount();
    }
  });

  it("the desktop AgentsPanel keeps the roster, the captions and the emoji rows without Watch pills", async () => {
    const { el, unmount } = await render(
      <AgentsPanel
        boundAgent={BOUND_AGENT}
        active="coder"
        onSelect={() => {}}
        roster={ROSTER}
        laneCalls={LANE_CALLS}
        laneMessages={LANE_MESSAGES}
      />,
    );
    try {
      expect(el.textContent, "the roster section stays").toContain("Roster");
      expect(el.textContent, "the view caption stays").toContain("Picking a role is a view");
      expect(el.textContent, "the emoji rows stay").toContain("🌐");
      expect(
        [...el.querySelectorAll("button")].some((b) => b.textContent?.trim() === "Watch"),
        "no Watch pills on the desktop",
      ).toBe(false);
      expect(el.textContent, "the active role is marked").toContain("watching");
    } finally {
      await unmount();
    }
  });

  it("the desktop BashPanel (full edition) keeps the header row and the pre", async () => {
    setBootEdition("full");
    const { el, unmount } = await render(<BashPanelView panel={BASH_PANEL} />);
    try {
      expect(el.textContent, "the header label").toContain("bash");
      expect(el.textContent).toContain("cargo test -p auth");
      expect(el.textContent, "the running status line").toContain("pid 4242 · 3s");
      expect(el.querySelector("pre"), "the output pre stays").not.toBeNull();
    } finally {
      await unmount();
    }
  });
});
