// Selection chip — remote profiles (remote-profiles T9).
//
// RED until T9. Written against the SPA contract the design locks
// (remote-profiles-design §6.4, §9 T9; the catalogue entries land in the
// SAME commit as this routing):
//
//   * The chip's Profile pane LISTS the remote catalogue entries — marked
//     `remote` and coloured (the catalogue dot, `var(--profile-<c>)`),
//     next to the local ones.
//   * `chipAction(current, target, catalog)` (pure, state.ts) decides the
//     pick: "reattach" iff the target profile differs and either side is
//     remote. The chip gets a reattach path (an `onReattach(profile, agent)`
//     prop wired to `switchConvo(null, profile, agent)`); a reattach pick
//     sends NO `select` frame (a `select` for the remote L would be
//     refused server-side, §5.10/§6.3 #10).
//   * Same-profile picks — including agent switches WITHIN a remote
//     profile — stay `select` (the relay rewrites `profile: L` → `P`,
//     §6.3 #10).
//
// Harness modeled on selectionChip.test.tsx (createRoot/act; the
// `onReattach` prop is not in `SelectionChipProps` yet, so it is passed
// through an `as unknown as` cast, like ConversationsPicker.test.tsx does
// for props that do not exist yet).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SelectionChip } from "./SelectionChip";
import type { InboundMessage, ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";
import type { SelectionChipProps } from "../settings/contract";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

// ─── fixtures ───────────────────────────────────────────────────────────

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
  {
    name: "home-dev",
    origin: "remote",
    overridesGlobal: false,
    agents: [
      { name: "ops", model: "sonnet", subagents: [] },
      { name: "watch", model: "sonnet", subagents: [] },
    ],
    color: "pink",
  },
];

const MODELS: ModelInfo[] = [
  { alias: "sonnet", provider_name: "anthropic", model_name: "claude-sonnet", context_size: 200000 },
  { alias: "haiku", provider_name: "anthropic", model_name: "claude-haiku", context_size: 200000 },
];

const SEL: Selection = { profile: "dev", agent: "team", subagentModels: {} };

// ─── mount plumbing (selectionChip.test.tsx conventions) ────────────────

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
const text = (id: string) => q(id).textContent ?? "";

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

interface ChipMount {
  send: ReturnType<typeof vi.fn<(m: InboundMessage) => void>>;
  onReattach: ReturnType<typeof vi.fn<(profile: string, agent: string) => void>>;
}

/** Mount the chip; `over` may include the not-yet-existing `onReattach`
 * prop (cast through `unknown`, the ConversationsPicker.test.tsx pattern). */
function mountChip(over: Partial<SelectionChipProps> & { onReattach?: ChipMount["onReattach"] } = {}): ChipMount {
  const send = vi.fn<(m: InboundMessage) => void>();
  const onReattach = vi.fn<(profile: string, agent: string) => void>();
  const props: SelectionChipProps = {
    catalog: CATALOG,
    selection: SEL,
    models: MODELS,
    activeAlias: "sonnet",
    locked: false,
    pinned: false,
    send,
    onToast: vi.fn(),
    ...over,
  } as SelectionChipProps;
  mount(<SelectionChip {...(props as unknown as SelectionChipProps)} onReattach={onReattach as never} />);
  return { send, onReattach };
}

const openChip = () => click(q("selection-chip"));

// ─── the Profile pane lists remote entries ──────────────────────────────

describe("selection chip — remote entries in the Profile pane (T9)", () => {
  it("lists remote catalogue entries marked remote and coloured, next to the locals", () => {
    mountChip();
    openChip();

    // Both kinds are listed, in catalogue order.
    // Rows are the buttons: the colour dot (`selection-profile-color`) also
    // starts with the prefix but is not a row.
    const rows = Array.from(
      host!.querySelectorAll<HTMLElement>('button[data-testid^="selection-profile-"]'),
    ).map((r) => r.getAttribute("data-testid"));
    expect(rows).toEqual(["selection-profile-dev", "selection-profile-home-dev"]);

    // The remote row is marked `remote` and carries the catalogue dot.
    const remote = q("selection-profile-home-dev");
    expect(text("selection-profile-home-dev")).toContain("remote");
    const dot = remote.querySelector<HTMLElement>(tid("selection-profile-color"));
    expect(dot, "the remote row has a catalogue colour dot").not.toBeNull();
    expect(dot!.getAttribute("style")).toContain("var(--profile-pink)");

    // The local row has neither the mark nor a dot in this fixture.
    expect(text("selection-profile-dev")).not.toContain("remote");
    expect(q("selection-profile-dev").querySelector(tid("selection-profile-color"))).toBeNull();
  });

  it("browsing a remote row shows its agents", () => {
    mountChip();
    openChip();
    click(q("selection-profile-home-dev"));
    expect(has("selection-agent-ops")).toBe(true);
    expect(has("selection-agent-watch")).toBe(true);
    expect(has("selection-agent-team")).toBe(false);
  });
});

// ─── picks: reattach across kinds, select within one ────────────────────

describe("selection chip — remote picks reattach instead of selecting (T9)", () => {
  it("picking a REMOTE profile's agent reattaches (no select frame)", () => {
    const { send, onReattach } = mountChip();
    openChip();
    click(q("selection-profile-home-dev")); // browse…
    click(q("selection-agent-ops")); //   …then pick

    expect(send, "a reattach sends no select frame").not.toHaveBeenCalled();
    expect(onReattach).toHaveBeenCalledTimes(1);
    expect(onReattach).toHaveBeenCalledWith("home-dev", "ops");
  });

  it("picking a LOCAL profile's agent still sends select (unchanged)", () => {
    const { send, onReattach } = mountChip();
    openChip();
    click(q("selection-agent-review"));

    expect(onReattach).not.toHaveBeenCalled();
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toStrictEqual({
      type: "select",
      profile: "dev",
      agent: "review",
    });
  });

  it("an agent switch WITHIN the remote profile stays a select (the relay rewrites it)", () => {
    const { send, onReattach } = mountChip({
      selection: { profile: "home-dev", agent: "ops", subagentModels: {} },
    });
    openChip();
    click(q("selection-agent-watch"));

    expect(onReattach).not.toHaveBeenCalled();
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toStrictEqual({
      type: "select",
      profile: "home-dev",
      agent: "watch",
    });
  });

  it("picking a LOCAL profile's agent from a REMOTE tab reattaches", () => {
    const { send, onReattach } = mountChip({
      selection: { profile: "home-dev", agent: "ops", subagentModels: {} },
    });
    openChip();
    click(q("selection-profile-dev")); // browse the local profile…
    click(q("selection-agent-review")); //   …then pick

    expect(send).not.toHaveBeenCalled();
    expect(onReattach).toHaveBeenCalledTimes(1);
    expect(onReattach).toHaveBeenCalledWith("dev", "review");
  });
});
