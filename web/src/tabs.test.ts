// Tests for the Layer-E tab-visibility contract (profiles feature).
//
// Backend now computes `visible_tabs: string[]` on the AppState snapshot —
// the server-side subset of the canonical drawer-tab order
// ["session","todo","files","tasks","bash","agents","profile"]. The SPA
// must narrow App.tsx's `tabs` array to that subset (in CANONICAL order,
// not the order the server happened to list them in) and, separately,
// relocate an open drawer tab that just got hidden.
//
// Two pure functions carry this logic (no component/DOM needed — plain
// `node` environment, matching adapt.test.ts/transcriptRows.test.ts):
//
//   filterTabs<T extends {id: string}>(tabs: T[], visible: string[] | undefined): T[]
//     - `visible === undefined` (older server, pre-feature) → ALL tabs,
//       unchanged order. This is the "legacy tabs still show" defaulting
//       the spec calls out for `visible_tabs` absence.
//     - otherwise → only tabs whose `id` is in `visible`, in the ORIGINAL
//       `tabs` array's order (App's canonical order), never `visible`'s
//       order — a deliberate decision lock (see the "preserves CANONICAL
//       order" test below).
//
//   resolveActiveTab(active: string | null, visibleIds: string[]): string | null
//     - `active === null` (drawer closed) → stays null, always.
//     - `active` still visible → unchanged.
//     - `active` hidden → falls back to the first entry of `visibleIds`.
//     - nothing visible at all → null (drawer effectively closes).
//
// Neither module exists yet (`web/src/tabs.ts` is new production code, not
// written by this task). Expected RED reason: "Cannot find module './tabs'
// or its corresponding type declarations" — i.e. the feature is simply
// absent, not a typo in this test file.

import { describe, it, expect } from "vitest";
import { filterTabs, resolveActiveTab } from "./tabs";

interface Tab {
  id: string;
  label: string;
}

// Mirrors App.tsx's canonical tab order after the Profile tab is added
// (E5): session, todo, files, tasks, bash, agents, profile.
const CANONICAL: Tab[] = [
  { id: "session", label: "Session" },
  { id: "todo", label: "Todo" },
  { id: "files", label: "Files" },
  { id: "tasks", label: "Tasks" },
  { id: "bash", label: "Bash" },
  { id: "agents", label: "Agents" },
  { id: "profile", label: "Profile" },
];

describe("filterTabs", () => {
  it("returns every tab unchanged, in canonical order, when visible is undefined (legacy server)", () => {
    expect(filterTabs(CANONICAL, undefined)).toEqual(CANONICAL);
  });

  it("keeps only tabs whose id is listed in visible", () => {
    const result = filterTabs(CANONICAL, ["session", "agents"]);
    expect(result.map((t) => t.id)).toEqual(["session", "agents"]);
  });

  it("preserves CANONICAL order, not the order given in `visible` (decision lock)", () => {
    // `visible` deliberately lists profile before session — the output must
    // still put session first, because App's own tab order governs display,
    // not whatever order the wire array happens to list ids in.
    const result = filterTabs(CANONICAL, ["profile", "session"]);
    expect(result.map((t) => t.id)).toEqual(["session", "profile"]);
  });

  it("returns an empty array when visible is an empty list", () => {
    expect(filterTabs(CANONICAL, [])).toEqual([]);
  });

  it("ignores unknown ids in visible without fabricating tabs", () => {
    const result = filterTabs(CANONICAL, ["session", "made_up_tab"]);
    expect(result.map((t) => t.id)).toEqual(["session"]);
  });

  it("tolerates duplicate ids inside visible without duplicating output", () => {
    const result = filterTabs(CANONICAL, ["session", "session", "session"]);
    expect(result.map((t) => t.id)).toEqual(["session"]);
  });

  it("returns an empty array for an empty tabs input regardless of visible", () => {
    expect(filterTabs([], ["session"])).toEqual([]);
    expect(filterTabs([], undefined)).toEqual([]);
  });

  it("handles unicode tab ids opaquely", () => {
    const tabs: Tab[] = [
      { id: "セッション", label: "Session (JP)" },
      { id: "todo", label: "Todo" },
    ];
    expect(filterTabs(tabs, ["セッション"]).map((t) => t.id)).toEqual([
      "セッション",
    ]);
  });

  it("keeps every matching entry when the tabs array itself has duplicate ids (pathological but must not crash)", () => {
    const dup: Tab[] = [
      { id: "session", label: "Session A" },
      { id: "session", label: "Session B" },
      { id: "todo", label: "Todo" },
    ];
    expect(filterTabs(dup, ["session"])).toEqual([
      { id: "session", label: "Session A" },
      { id: "session", label: "Session B" },
    ]);
  });
});

describe("resolveActiveTab", () => {
  const visible = ["session", "todo", "agents"];

  it("keeps the drawer closed (null) regardless of the visible set", () => {
    expect(resolveActiveTab(null, visible)).toBeNull();
    expect(resolveActiveTab(null, [])).toBeNull();
  });

  it("leaves an active tab unchanged when it is still visible", () => {
    expect(resolveActiveTab("todo", visible)).toBe("todo");
  });

  it("falls back to the first visible tab when the active one was hidden", () => {
    expect(resolveActiveTab("bash", visible)).toBe("session");
  });

  it("respects the ORDER of visibleIds for the fallback, not alphabetical order", () => {
    expect(resolveActiveTab("bash", ["agents", "todo", "session"])).toBe(
      "agents",
    );
  });

  it("returns null when nothing is visible at all", () => {
    expect(resolveActiveTab("todo", [])).toBeNull();
  });

  it("treats an empty-string active id like any other (non-null) id", () => {
    expect(resolveActiveTab("", visible)).toBe("session");
    expect(resolveActiveTab("", [])).toBeNull();
  });

  it("leaves the sole visible tab unchanged when it is exactly the active one", () => {
    expect(resolveActiveTab("agents", ["agents"])).toBe("agents");
  });

  it("integration: filterTabs + resolveActiveTab relocate an open drawer off a tab the server just hid", () => {
    // Mirrors the intended App.tsx wiring: canonical tabs → filterTabs
    // (visible_tabs) → ids → resolveActiveTab(drawerTab, ids). A profile
    // pin (or no configured profiles) hides "profile"; a user who had it
    // open lands on the first still-visible tab instead of a blank panel.
    const visibleTabs = ["session", "todo", "agents"]; // files/tasks/bash/profile hidden
    const shown = filterTabs(CANONICAL, visibleTabs);
    expect(resolveActiveTab("profile", shown.map((t) => t.id))).toBe(
      "session",
    );
  });
});
