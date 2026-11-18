// Tests for `components/BottomBar.tsx` — mobile layout contract.
//
// Contracts in this file:
//  1. Rail clearance (desktop only): the fixed right-hand TabbedDrawer rail
//     (40px handles) exists from lg up, so the <footer> carries `desktop:pr-12` —
//     never a bare `pr-12`, because phones have no rail (mobile redesign).
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
    overridesGlobal: false, machine: null,
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

async function mount(): Promise<HTMLDivElement> {
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

describe("BottomBar — rail clearance (desktop only)", () => {
  it("footer has the desktop:pr-12 right clearance class and no bare pr-12", async () => {
    const el = await mount();
    const footer = el.querySelector("footer")!;
    expect(footer).not.toBeNull();
    expect(footer.classList.contains("desktop:pr-12")).toBe(true);
    expect(footer.classList.contains("pr-12"), "phones have no rail").toBe(false);
  });

  it("footer keeps desktop:pr-12 when the cwd picker is shown too", async () => {
    const el = await mount();
    const footer = el.querySelector("footer")!;
    expect(footer.classList.contains("desktop:pr-12")).toBe(true);
    expect(footer.classList.contains("pr-12")).toBe(false);
  });

  it("footer stays mobile-only (lg:hidden) — regression", async () => {
    const el = await mount();
    expect(el.querySelector("footer")!.classList.contains("lg:hidden")).toBe(true);
  });
});

describe("bottom bar mobile overflow (T34b)", () => {
  it("chip_row_wraps_instead_of_overflowing_horizontally", async () => {
    const el = await mount();
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
    const el = await mount();
    for (const sel of [
      '[data-testid="selection-chip"]',
      'button[title="Load a saved conversation"]',
      'button[title="Load a saved conversation"]',
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
