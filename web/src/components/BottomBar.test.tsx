// Tests for `components/BottomBar.tsx` — mobile layout contract.
//
// Two contracts in one file:
//  1. Rail clearance (mobile): the fixed right-hand TabbedDrawer rail (40px
//     handles) overlaps the bottom bar on phones, so the <footer> carries
//     `pr-12` so its pickers never sit under the rail.
//  2. Overflow (T34b): at ≤800 px the chips must not overflow the bar
//     horizontally — they wrap to a second row instead. The selection chip
//     (profile, agent, team models) replaced the separate model chip and the
//     "Agents · N" chip.
//
// Class names ARE the layout contract (jsdom has no layout engine). Harness
// modeled on Composer.test.tsx.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { BottomBar } from "./BottomBar";
import type { ConversationSummary, InboundMessage, ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// `useMediaQuery` (via BottomBar's chip subtree) calls matchMedia on mount;
// jsdom ships no `window.matchMedia`.
let originalMatchMedia: typeof window.matchMedia | undefined;
beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  window.matchMedia = (media: string): MediaQueryList =>
    ({
      matches: false,
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
});

const CATALOG: CatalogProfile[] = [
  {
    name: "dev",
    origin: "global",
    overridesGlobal: false,
    agents: [{ name: "main", model: "sonnet", subagents: [] }],
  },
];
const SELECTION: Selection = { profile: "dev", agent: "main", subagentModels: {} };
const MODELS: ModelInfo[] = [
  { alias: "sonnet", provider_name: "openrouter", model_name: "anthropic/claude-sonnet-4.5", context_size: 200000 },
];
const CONVERSATIONS: ConversationSummary[] = [
  { id: "c1", name: "chat one", updated_at: "2026-01-01T00:00:00Z", message_count: 3, model: "sonnet", active: true },
];

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(cwd: string | null): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(
      <BottomBar
        conversations={CONVERSATIONS}
        conversationsError={null}
        models={MODELS}
        activeAlias="sonnet"
        hasTranscript={true}
        cwd={cwd}
        dirListing={null}
        recentDirs={[]}
        send={vi.fn((m: InboundMessage) => void m)}
        onLoadConversation={vi.fn()}
        catalog={CATALOG}
        selection={SELECTION}
        selectionLocked={true}
        profilePinned={false}
        onToast={vi.fn()}
      />,
    );
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
});

describe("BottomBar — rail clearance (mobile)", () => {
  it("footer has the pr-12 right clearance class", async () => {
    const el = await mount(null);
    const footer = el.querySelector("footer")!;
    expect(footer).not.toBeNull();
    expect(footer.classList.contains("pr-12")).toBe(true);
  });

  it("footer keeps pr-12 when the cwd picker is shown too", async () => {
    const el = await mount("/tmp/project");
    const footer = el.querySelector("footer")!;
    expect(footer.classList.contains("pr-12")).toBe(true);
  });

  it("footer stays mobile-only (lg:hidden) — regression", async () => {
    const el = await mount(null);
    expect(el.querySelector("footer")!.classList.contains("lg:hidden")).toBe(true);
  });
});

describe("bottom bar mobile overflow (T34b)", () => {
  it("chip_row_wraps_instead_of_overflowing_horizontally", async () => {
    const el = await mount("/work/repo");
    const bar = el.querySelector('[data-testid="bottom-bar"]');
    expect(bar).not.toBeNull();
    const cls = bar!.className;
    // The wrap rule — chips drop to a second row at narrow widths.
    expect(cls).toContain("flex-wrap");
    expect(cls).not.toContain("flex-nowrap");
    // …and it is a flex row (wrap only means something there).
    expect(cls).toContain("flex");
    // Horizontal scroll is NOT the chosen mechanism: no overflow-x on the bar.
    expect(cls).not.toMatch(/overflow-x-(auto|scroll)/);
  });

  it("selection_conversations_and_cwd_chips_render", async () => {
    const el = await mount("/work/repo");
    for (const sel of [
      '[data-testid="selection-chip"]',
      'button[title="Load a saved conversation"]',
      'button[title="/work/repo"]',
    ]) {
      expect(el.querySelector(sel) !== null, `chip ${sel} rendered`).toBe(true);
    }
    // The model chip and the Agents chip are folded into the selection chip.
    for (const sel of [
      'button[title="Switch model"]',
      '[data-testid="agents-chip"]',
    ]) {
      expect(el.querySelector(sel), `chip ${sel} must be gone`).toBeNull();
    }
  });
});
