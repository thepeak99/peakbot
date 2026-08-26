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

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { TopBar } from "./TopBar";
import type {
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
} from "../state";
import type { SessionStats } from "../types";
import type { NotifyPermission } from "../useTaskNotifications";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching Transcript.test.tsx's setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

// The real TopBar prop interface as declared today (TopBar.tsx:36-53) has
// no `statusMessage` field. We type `baseProps` as that real interface
// PLUS the not-yet-existing field, so the fixture reads the way the
// post-implementation call site will, while still compiling loosely
// enough for vitest's transform to run the file.
interface TopBarPropsWithStatus {
  stats: SessionStats | null;
  isRunning: boolean;
  statusMessage: string | null;
  connected: boolean;
  pendingInput: number;
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
  lockedReason?: string | null;
}

const baseProps: TopBarPropsWithStatus = {
  connected: true,
  stats: null,
  models: [],
  activeAlias: "gpt",
  hasTranscript: false,
  cwd: null,
  dirListing: null,
  conversations: [],
  pendingInput: 0,
  notifyPermission: "unsupported",
  isRunning: false,
  statusMessage: null,
  send: () => {},
  onSwitchModel: () => {},
  onLoadConversation: () => {},
  notifyEnabled: false,
  onToggleNotify: () => {},
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: TopBarPropsWithStatus): Promise<HTMLDivElement> {
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
