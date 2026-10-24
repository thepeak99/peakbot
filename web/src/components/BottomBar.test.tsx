// T34b regression (QA fix 7): on mobile (≤800 px) the BottomBar's chip row
// (selection · conversations · model · agents · cwd) must not overflow the
// bar horizontally. jsdom cannot measure layout, so the regression is pinned
// on the wrap rule: the bar is a `flex-wrap` row (chips flow to a second
// line) and NOT a `flex-nowrap` one, with every chip still rendered.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { BottomBar } from "./BottomBar";
import type { ConversationSummary, InboundMessage, ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root;
let host: HTMLElement;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(node));
}

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const CATALOG: CatalogProfile[] = [
  {
    name: "dev",
    origin: "global",
    overridesGlobal: false,
    agents: [{ name: "main", model: "sonnet", subagents: [] }],
  },
];
const SELECTION: Selection = { profile: "dev", agent: "main" };
const MODELS: ModelInfo[] = [
  { alias: "sonnet", provider_name: "openrouter", model_name: "anthropic/claude-sonnet-4.5", context_size: 200000 },
];
const CONVERSATIONS: ConversationSummary[] = [
  { id: "c1", name: "chat one", updated_at: "2026-01-01T00:00:00Z", message_count: 3, model: "sonnet", active: true },
];

function mountBar() {
  mount(
    <BottomBar
      conversations={CONVERSATIONS}
      conversationsError={null}
      models={MODELS}
      activeAlias="sonnet"
      hasTranscript={true}
      cwd="/work/repo"
      dirListing={null}
      recentDirs={[]}
      send={vi.fn((m: InboundMessage) => void m)}
      onSwitchModel={vi.fn()}
      onLoadConversation={vi.fn()}
      catalog={CATALOG}
      selection={SELECTION}
      selectionLocked={true}
      profilePinned={false}
      onToast={vi.fn()}
    />,
  );
}

describe("bottom bar mobile overflow (T34b)", () => {
  it("chip_row_wraps_instead_of_overflowing_horizontally", () => {
    mountBar();
    const bar = host.querySelector('[data-testid="bottom-bar"]');
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

  it("all_five_chips_still_render_in_the_writing_row", () => {
    mountBar();
    for (const sel of [
      '[data-testid="selection-chip"]',
      'button[title="Load a saved conversation"]',
      'button[title="Switch model"]',
      '[data-testid="agents-chip"]',
      'button[title="/work/repo"]',
    ]) {
      expect(host.querySelector(sel) !== null, `chip ${sel} rendered`).toBe(true);
    }
  });
});
