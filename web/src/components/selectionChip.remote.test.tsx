// Selection chip — selecting a REMOTE profile that has no local agents.
//
// RED (reported bug): the catalogue emits remote entries with `agents: []`
// (src/config/resolve.rs `Catalog::infos()`, pinned by
// `infos_list_remote_entries_with_origin_and_empty_agents`), but the chip's
// Agent pane renders one row PER AGENT (SelectionPanes.tsx `AgentPane`) —
// so a remote profile shows NO rows and therefore NO way to pick it. The
// remote's agents are the remote's business (design §6.4): the reattach
// target `switchConvo(null, profile?, agent?)` takes an OPTIONAL agent
// (useAgent.ts), so a remote profile with zero local agents must be
// selectable as-is:
//
//   * browsing it offers ONE selectable row in the Agent pane
//     (`selection-agent-remote`) — the profile itself;
//   * picking it reattaches WITHOUT an agent (`onReattach(profile,
//     undefined)` → `switchConvo(null, profile)`) and sends NO `select`
//     frame (a `select` for the remote L would be refused, §6.3 #10);
//   * the decision is pure and local (catalogue only, no network) — a slow
//     or unreachable remote cannot make the profile unselectable (failure
//     is at attach, G7);
//   * a remote profile whose agents ARE known (relay-filled, §6.3
//     `localize_state`) keeps its per-agent rows and shows no bare row.
//
// The existing T9 chip tests (selectionChip.t9.test.tsx) stay green because
// their fixture fabricates agents on the remote profile; this file uses
// the REAL wire shape (`agents: []`).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SelectionChip } from "./SelectionChip";
import { chipAction } from "../state";
import type { InboundMessage, ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";
import type { SelectionChipProps } from "../settings/contract";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

// ─── fixtures: the REAL wire shape (resolve.rs `Catalog::infos()`) ──────

const CATALOG: CatalogProfile[] = [
  {
    name: "dev",
    origin: "global",
    overridesGlobal: false,
    agents: [
      { name: "team", model: "sonnet", subagents: [] },
      { name: "review", model: "sonnet", subagents: [] },
    ],
    color: "teal",
  },
  // The reporter's profile, exactly as `infos()` emits a remote entry:
  // origin "remote", NO agents, resolved colour (ui.color: teal).
  {
    name: "bonsol-dev",
    origin: "remote",
    overridesGlobal: false,
    agents: [],
    color: "teal",
  },
];

const MODELS: ModelInfo[] = [
  { alias: "sonnet", provider_name: "anthropic", model_name: "claude-sonnet", context_size: 200000 },
  { alias: "haiku", provider_name: "anthropic", model_name: "claude-haiku", context_size: 200000 },
];

const SEL: Selection = { profile: "dev", agent: "team", subagentModels: {} };

// ─── mount plumbing (selectionChip.t9.test.tsx conventions) ─────────────

let root: Root | null = null;
let host: HTMLElement | null = null;
const originalMatchMedia = window.matchMedia;

beforeEach(() => {
  window.matchMedia = ((q: string) => ({
    matches: false,
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    onchange: null,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  host?.remove();
  root = null;
  host = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
  vi.restoreAllMocks();
});

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

const tid = (id: string) => `[data-testid="${id}"]`;
const q = <T extends Element = HTMLElement>(id: string): T => {
  const el = host!.querySelector(tid(id));
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
};
const has = (id: string) => host!.querySelector(tid(id)) !== null;

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

interface ChipMount {
  send: ReturnType<typeof vi.fn<(m: InboundMessage) => void>>;
  onReattach: ReturnType<typeof vi.fn<(profile: string, agent?: string) => void>>;
}

function mountChip(over: Partial<SelectionChipProps> = {}): ChipMount {
  const send = vi.fn<(m: InboundMessage) => void>();
  const onReattach = vi.fn<(profile: string, agent?: string) => void>();
  const props: SelectionChipProps = {
    catalog: CATALOG,
    selection: SEL,
    models: MODELS,
    activeAlias: "sonnet",
    locked: false,
    pinned: false,
    send,
    onToast: vi.fn(),
    onReattach,
    ...over,
  };
  mount(<SelectionChip {...props} />);
  return { send, onReattach };
}

const openChip = () => click(q("selection-chip"));

const agentRows = () =>
  Array.from(host!.querySelectorAll<HTMLElement>('button[data-testid^="selection-agent-"]')).map(
    (r) => r.getAttribute("data-testid"),
  );

// ─── the bug: a zero-agent remote profile is selectable ─────────────────

describe("selection chip — a remote profile with zero local agents", () => {
  it("browsing a zero-agent remote profile offers a selectable row in the Agent pane", () => {
    mountChip();
    openChip();
    click(q("selection-profile-bonsol-dev"));

    expect(
      agentRows(),
      "the Agent pane offers exactly one pick: the remote profile itself",
    ).toEqual(["selection-agent-remote"]);
  });

  it("picking the zero-agent remote profile reattaches WITHOUT an agent and sends no select", () => {
    const { send, onReattach } = mountChip();
    openChip();
    click(q("selection-profile-bonsol-dev"));
    click(q("selection-agent-remote"));

    expect(send, "a reattach sends no select frame").not.toHaveBeenCalled();
    expect(onReattach).toHaveBeenCalledTimes(1);
    expect(onReattach).toHaveBeenCalledWith("bonsol-dev", undefined);
  });

  it("browsing a zero-agent remote profile hides the local profile's agent rows", () => {
    mountChip();
    openChip();
    click(q("selection-profile-bonsol-dev"));

    expect(has("selection-agent-team")).toBe(false);
    expect(has("selection-agent-review")).toBe(false);
  });
});

// ─── the decision is pure and local (no network at pick time) ───────────

describe("selection chip — remote pick is a local catalogue decision", () => {
  it("chipAction reattaches for a remote target with no known agent", () => {
    const remoteNoAgent: Selection = { profile: "bonsol-dev", agent: "", subagentModels: {} };
    expect(chipAction(SEL, remoteNoAgent, CATALOG)).toBe("reattach");
  });

  it("chipAction selects within the same remote profile, agent known or not", () => {
    const current: Selection = { profile: "bonsol-dev", agent: "", subagentModels: {} };
    expect(chipAction(current, { profile: "bonsol-dev", agent: "ops", subagentModels: {} }, CATALOG)).toBe(
      "select",
    );
  });
});

// ─── guard: a relay-filled remote profile keeps its agent rows ──────────

describe("selection chip — a remote profile with known agents (relay-filled)", () => {
  it("shows per-agent rows and no bare remote row", () => {
    const filled: CatalogProfile[] = [
      ...CATALOG,
      {
        name: "home-dev",
        origin: "remote",
        overridesGlobal: false,
        agents: [{ name: "ops", model: "sonnet", subagents: [] }],
        color: "pink",
      },
    ];
    mountChip({ catalog: filled });
    openChip();
    click(q("selection-profile-home-dev"));

    expect(has("selection-agent-ops")).toBe(true);
    expect(has("selection-agent-remote"), "no bare row once the agents are known").toBe(false);
  });
});
