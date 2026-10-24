// Tests for TUI parity of the "working…" chip: the backend already
// broadcasts `status_message` on the wire snapshot (state.ts:151) during
// memory/context compaction and tool calls, but `TopBar` has no
// `statusMessage` prop yet and never renders it. These tests lock the
// target behaviour described in the design doc:
//
//   - `isRunning && statusMessage` renders `· <statusMessage>` inside a
//     `span[title]` (title == the status text) alongside the existing
//     "working…" text.
//   - No status message (null) leaves today's chip untouched: exactly
//     "working…", no "·", no `span[title]`.
//   - The status text is gated on `isRunning` — a stale/racy
//     `statusMessage` must not render once the run has ended.
//   - No content-based filtering: any non-empty status string (a tool
//     name like "bash", not just the compaction sentences) is rendered
//     verbatim. This is the decision-lock test guarding against a
//     "only show compaction, not tool names" mis-scope.
//
// Harness modeled on `Transcript.test.tsx` (createRoot/act off
// `react-dom/client`; jsdom is auto-enabled for `src/components/**` by
// `vitest.config.ts`). `TopBar` renders `NotifyToggle`, which returns
// `null` for `notifyPermission: "unsupported"`, keeping the mounted tree
// free of extra async permission plumbing.
//
// IMPORTANT — expected to be RED before implementation for TWO reasons:
//   1. Runtime: `statusMessage` is not read anywhere in TopBar.tsx, so
//      T2/T4 (which assert on `[title="..."]`) find nothing and fail.
//   2. Typecheck: `statusMessage` is not declared in TopBar's props
//      interface, so `tsc`/`vue-tsc`-style strict builds will also flag
//      the fixture. We pass props through an `as unknown as Parameters<
//      typeof TopBar>[0]` cast specifically so the test still *runs*
//      under vitest (which uses esbuild/swc, not tsc, for transforms)
//      and fails on the missing rendered output rather than refusing to
//      execute at all. A separate `npm run typecheck` (if run) would
//      also be RED, which is expected and fine per the task brief.
//
// The "focus mode toggle" block below (plan-focus-mode.md §5 case 25) is
// RED for the same runtime reason: TopBar renders no
// `button[aria-label="Focus mode"]` yet and ignores the `focusMode` /
// `onToggleFocus` props, so the queries return null and the click spy is
// never called. The pre-existing chip/sub-agent tests in this file must
// stay GREEN — the extra fixture props are ignored by today's TopBar.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { TopBar } from "./TopBar";
import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { SubAgentRun } from "../types";
import type { NotifyPermission } from "../useTaskNotifications";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching Transcript.test.tsx's setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

// The real TopBar prop interface as declared today (TopBar.tsx:36-53) has
// no `statusMessage`, `focusMode` or `onToggleFocus` fields. We type
// `baseProps` as that real interface PLUS the not-yet-existing fields, so
// the fixture reads the way the post-implementation call site will, while
// still compiling loosely enough for vitest's transform to run the file.
interface TopBarFixtureProps {
  isRunning: boolean;
  statusMessage: string | null;
  subAgent: SubAgentRun | null;
  connected: boolean;
  models: ModelInfo[];
  activeAlias: string;
  hasTranscript: boolean;
  cwd: string | null;
  dirListing: DirListing | null;
  conversations: ConversationSummary[];
  send: (msg: InboundMessage) => void;
  onSwitchModel: (alias: string) => void;
  onLoadConversation: (id: string) => void;
  notifyEnabled: boolean;
  notifyPermission: NotifyPermission;
  onToggleNotify: () => void;
  // Focus mode (plan §6.3): not on TopBar's declared props yet — the
  // fixture carries them so the case-25 tests target the post-
  // implementation call site. `focusMode: true` is the shipped default
  // (D7), mirroring what App.tsx will pass once the hook exists.
  focusMode: boolean;
  onToggleFocus: () => void;
  /** Two-stage stop: a graceful stop is pending. Not on TopBar's declared
   *  props yet — the fixture carries it, like `focusMode` did. */
  stopPending?: boolean;
  lockedReason?: string | null;
}

const baseProps: TopBarFixtureProps = {
  connected: true,
  models: [],
  activeAlias: "gpt",
  hasTranscript: false,
  cwd: null,
  dirListing: null,
  conversations: [],
  notifyPermission: "unsupported",
  isRunning: false,
  statusMessage: null,
  subAgent: null,
  send: () => {},
  onSwitchModel: () => {},
  onLoadConversation: () => {},
  notifyEnabled: false,
  onToggleNotify: () => {},
  focusMode: true,
  onToggleFocus: () => {},
  stopPending: false,
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: TopBarFixtureProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    // `statusMessage` is not part of TopBar's declared props today, so we
    // cast through `unknown` to pass it anyway — the point of these tests
    // is to prove the prop is currently ignored/absent, not to fight the
    // type system. Post-implementation this cast becomes a no-op identity.
    root.render(
      <TopBar {...(props as unknown as Parameters<typeof TopBar>[0])} />,
    );
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

// ─── tests ──────────────────────────────────────────────────────────────

describe("TopBar — status_message wire parity in the working… chip", () => {
  // T1 — no status, unchanged behaviour.
  it("renders bare 'working…' with no dot separator or title span when statusMessage is null", async () => {
    const el = await mount({ ...baseProps, isRunning: true, statusMessage: null });

    expect(el.textContent).toContain("working…");
    expect(el.textContent).not.toContain("·");
    expect(el.querySelector("span[title]")).toBeNull();
  });

  // T2 — compaction is visible (the actual bug this feature fixes).
  it("renders the compaction status message in a titled span alongside working…", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      statusMessage: "Compacting memory.md...",
    });

    const titled = el.querySelector('[title="Compacting memory.md..."]');
    expect(titled).not.toBeNull();
    expect(titled!.textContent).toBe("· Compacting memory.md...");
    expect(el.textContent).toContain("working…");
  });

  // T3 — status gated on running: a stale statusMessage must not leak
  // once the run has ended.
  it("hides both the spinner text and the status message when isRunning is false", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: false,
      statusMessage: "Compacting memory.md...",
    });

    expect(el.textContent).not.toContain("working…");
    expect(el.textContent).not.toContain("Compacting");
  });

  // T4 — decision lock: no content-based filtering. Tool names (e.g.
  // "bash") must render exactly like compaction sentences — the chip is
  // a dumb passthrough of the wire field, not a compaction-only special
  // case.
  it("renders a bare tool-name status message ('bash') verbatim, with no filtering by content", async () => {
    const el = await mount({ ...baseProps, isRunning: true, statusMessage: "bash" });

    const titled = el.querySelector('[title="bash"]');
    expect(titled).not.toBeNull();
    expect(titled!.textContent).toBe("· bash");
  });
});

// ── Sub-agent chip (pause-subagents feature) ─────────────────────────────
// The chip sits next to "working…" and tracks the wire `sub_agent.pause`
// state: 🧩 <role> while running, the pausing sentence while pausing, and
// the parked sentence while paused. Gated on isRunning like the status
// message, and absent when no sub-agent runs (orchestrator-only turns).

describe("TopBar — sub-agent chip", () => {
  it("renders '🧩 researcher' while the sub-agent is running", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: { role: "researcher", pausable: true, pause: "running" },
    });

    expect(el.textContent).toContain("🧩 researcher");
    expect(el.textContent).toContain("working…");
    expect(el.textContent).not.toContain("paused");
  });

  it("renders the pausing sentence while pause === 'pausing'", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: { role: "researcher", pausable: true, pause: "pausing" },
    });

    expect(el.textContent).toContain(
      "⏸ Pausing researcher after current step…",
    );
  });

  it("renders the parked sentence while pause === 'paused'", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: { role: "researcher", pausable: true, pause: "paused" },
    });

    expect(el.textContent).toContain("⏸ researcher paused");
  });

  it("hides the chip when isRunning is false (stale sub_agent must not leak)", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: false,
      subAgent: { role: "researcher", pausable: true, pause: "paused" },
    });

    expect(el.textContent).not.toContain("researcher");
    expect(el.textContent).not.toContain("🧩");
    expect(el.textContent).not.toContain("⏸");
  });

  it("renders no chip when no sub-agent runs (orchestrator-only turn)", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      subAgent: null,
    });

    expect(el.textContent).toContain("working…");
    expect(el.textContent).not.toContain("🧩");
    expect(el.textContent).not.toContain("⏸");
  });
});

// ── Focus mode toggle (plan-focus-mode.md §5 case 25, §6.2/§6.3) ──────────
// TopBar gains `focusMode: boolean` + `onToggleFocus: () => void` and
// renders a <FocusToggle> immediately before the <ThemeToggle>:
// a button with aria-label="Focus mode" whose aria-pressed reflects
// focusMode, invoking onToggleFocus on click.
//
// RED today: TopBar renders no such button and ignores both props, so
// `button[aria-label="Focus mode"]` resolves to null in every test below.
// (ThemeToggle's aria-label is "Switch to light theme" / "Switch to dark
// theme" — selected via the "Switch to" prefix, theme-agnostic.)

describe("TopBar — focus mode toggle", () => {
  it("renders a 'Focus mode' button with aria-pressed=true when focusMode is on (the default)", async () => {
    const el = await mount({ ...baseProps, focusMode: true });

    const focusBtn = el.querySelector('button[aria-label="Focus mode"]');
    expect(focusBtn, "TopBar must render the focus toggle button").not.toBeNull();
    expect(focusBtn!.getAttribute("aria-pressed")).toBe("true");
  });

  it("reflects focusMode=false as aria-pressed=false", async () => {
    const el = await mount({ ...baseProps, focusMode: false });

    const focusBtn = el.querySelector('button[aria-label="Focus mode"]');
    expect(focusBtn).not.toBeNull();
    expect(focusBtn!.getAttribute("aria-pressed")).toBe("false");
  });

  it("calls onToggleFocus exactly once when the focus button is clicked", async () => {
    const onToggleFocus = vi.fn();
    const el = await mount({ ...baseProps, focusMode: true, onToggleFocus });

    const focusBtn = el.querySelector(
      'button[aria-label="Focus mode"]',
    ) as HTMLButtonElement | null;
    expect(focusBtn).not.toBeNull();
    await act(async () => {
      focusBtn!.click();
    });
    expect(onToggleFocus).toHaveBeenCalledTimes(1);
  });

  it("sits immediately before the theme toggle in the bar", async () => {
    const el = await mount({ ...baseProps, focusMode: true });

    const focusBtn = el.querySelector('button[aria-label="Focus mode"]');
    const themeBtn = el.querySelector('button[aria-label^="Switch to"]');
    expect(focusBtn).not.toBeNull();
    expect(themeBtn).not.toBeNull();
    // DOCUMENT_POSITION_FOLLOWING (0x04) set ⇒ theme button comes after
    // the focus button in document order.
    expect(
      focusBtn!.compareDocumentPosition(themeBtn!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });
});

// ── Two-stage stop: pending sentence in the working… chip ─────────────────
// While `stopPending`, the TopBar shows
// "Stopping after current tool call…" INSTEAD of the statusMessage
// (the graceful stage is the status the user needs to see). Gated on
// isRunning like everything else in the chip.
//
// RED today: TopBar has no `stopPending` prop — the sentence never
// renders and the statusMessage shows as it does today.

describe("TopBar — two-stage stop pending sentence", () => {
  it("stopPending=true: shows 'Stopping after current tool call…' and suppresses the statusMessage", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      stopPending: true,
      statusMessage: "bash",
    });

    expect(el.textContent).toContain("Stopping after current tool call…");
    expect(el.textContent).not.toContain("· bash");
    expect(el.textContent).toContain("working…");
  });

  it("stopPending=true with no statusMessage: the pending sentence is the status", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      stopPending: true,
      statusMessage: null,
    });

    expect(el.textContent).toContain("Stopping after current tool call…");
    expect(el.textContent).not.toContain("· null");
  });

  it("stopPending=false: the statusMessage renders as today (no pending sentence)", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: true,
      stopPending: false,
      statusMessage: "bash",
    });

    expect(el.textContent).not.toContain("Stopping after current tool call…");
    expect(el.textContent).toContain("· bash");
  });

  it("stopPending is gated on isRunning: a stale flag must not leak when idle", async () => {
    const el = await mount({
      ...baseProps,
      isRunning: false,
      stopPending: true,
    });

    expect(el.textContent).not.toContain("Stopping after current tool call…");
  });
});
