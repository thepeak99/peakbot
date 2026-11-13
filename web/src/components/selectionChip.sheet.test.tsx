// SelectionChip in sheet mode (mobile redesign design §2.5 `sheet?: boolean`,
// §2.7, SC1–SC6; fidelity plan batch 5, C16/B5; scope C13).
//
// `sheet` defaults to false and every other selectionChip*.test.tsx stays
// green UNMODIFIED (the desktop drill-down keeps its own contract — see the
// desktop lock at the bottom of this file). With `sheet`:
//   * the trigger is the bordered subtitle pill (B5) and the face reads
//     `profile › agent · model` (Z11 reversed, sheet only);
//   * the trigger opens a dialog named "Selection" (portal'd to body);
//   * the sheet shows THREE always-visible labelled groups — Profile /
//     Agent / Model — as pressable pills (button + aria-pressed), no
//     drill-down (C16);
//   * Profile = browse: it sends nothing and stays open;
//   * Agent and Model = bind, toast, then CLOSE the sheet — the agent pick
//     closes as in the mock (Q4);
//   * sub-agent overrides keep the proven drill behind "Team models ›";
//     ‹ Back from the Team drill returns to the segments;
//   * a mousedown inside the sheet does NOT close it (the portal would make
//     every in-sheet click "outside" the chip's root);
//   * a locked selection keeps the 🔒, disables the trigger and opens
//     nothing.
//
// Harness: the selectionChip.test.tsx conventions (createRoot/act, matchMedia
// stub, testid helpers).
//
// RED before the batch-5 build: the sheet still renders the drill-down
// panes, the trigger is plain text and the face reads `profile › agent ›
// model`, so the pill/segment/pill-face assertions fail on the missing
// elements (feature absent) — no import or setup errors.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SelectionChip } from "./SelectionChip";
import { select, type InboundMessage, type ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";
import type { SelectionChipProps } from "../settings/contract";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

// ─── fixtures ─────────────────────────────────────────────────────────────

const CATALOG: CatalogProfile[] = [
  {
    name: "peakbot-dev",
    origin: "repo",
    overridesGlobal: false,
    machine: null,
    agents: [
      { name: "team", model: "sonnet", subagents: [{ name: "tester", model: "haiku", inherited: false }] },
      { name: "review", model: "sonnet", subagents: [] },
    ],
  },
  {
    name: "writer",
    origin: "global",
    overridesGlobal: false,
    machine: null,
    agents: [{ name: "draft", model: "haiku", subagents: [] }],
  },
];

const MODELS: ModelInfo[] = [
  { alias: "haiku", provider_name: "anthropic", model_name: "claude-haiku", context_size: 200000 },
  { alias: "sonnet", provider_name: "anthropic", model_name: "claude-sonnet", context_size: 200000 },
  { alias: "opus", provider_name: "anthropic", model_name: "claude-opus", context_size: 200000 },
];

const SEL: Selection = { profile: "peakbot-dev", agent: "team", subagentModels: {} };

// ─── mount plumbing ───────────────────────────────────────────────────────

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
  window.history.replaceState(null, "", "/");
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
  window.history.replaceState(null, "", "/");
});

function mountChip(over: Partial<SelectionChipProps> = {}) {
  const send = vi.fn<(m: InboundMessage) => void>();
  const onToast = vi.fn<(m: string) => void>();
  const props: SelectionChipProps = {
    catalog: CATALOG,
    selection: SEL,
    models: MODELS,
    activeAlias: "sonnet",
    locked: false,
    pinned: false,
    send,
    onToast,
    sheet: true,
    ...over,
  };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<SelectionChip {...props} />));
  return { send, onToast };
}

const tid = (id: string) => `[data-testid="${id}"]`;
/** Pane controls portal to document.body, so look there. */
const q = <T extends Element = HTMLElement>(id: string): T => {
  const el = document.body.querySelector(tid(id));
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
};
const chip = (): HTMLElement => host!.querySelector<HTMLElement>(tid("selection-chip"))!;
const dialog = (): HTMLElement | null =>
  document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="Selection"]');
const segs = (): HTMLElement | null =>
  document.body.querySelector<HTMLElement>(tid("selection-segments"));

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

// ─── SC1: the trigger opens the Selection dialog ─────────────────────────

describe("SelectionChip sheet — SC1: the trigger opens the Selection sheet", () => {
  it("is closed until tapped, then a dialog named Selection is in document.body", () => {
    mountChip();
    expect(dialog(), "closed at first").toBeNull();
    click(chip());
    const dlg = dialog();
    expect(dlg, "the Selection dialog opens").not.toBeNull();
    expect(dlg!.getAttribute("aria-modal")).toBe("true");
    expect(host!.contains(dlg!), "portal'd out of the chip's container").toBe(false);
  });

  it("the trigger is the bordered subtitle pill (B5)", () => {
    mountChip();
    const cls = chip().className;
    expect(cls, "the pill is fully rounded").toContain("rounded-full");
    expect(cls, "the pill carries a border").toMatch(/(^|\s)border(\s|$)/);
  });

  it("the face reads profile › agent · model (Z11 reversed, sheet only)", () => {
    mountChip();
    expect(chip().textContent).toContain("peakbot-dev › team");
    expect(chip().textContent, "the model separator is `·` on the sheet").toContain("· sonnet");
  });
});

// ─── SC2: the three segmented groups render at once (C16) ────────────────

describe("SelectionChip sheet — SC2: the three segmented groups", () => {
  it("Profile, Agent and Model groups render at once inside the dialog", () => {
    mountChip();
    click(chip());
    const dlg = dialog()!;
    const groups = segs();
    expect(groups, "the segments render inside the Selection dialog").not.toBeNull();
    expect(dlg!.contains(groups!), "portal'd with the sheet").toBe(true);

    const labels = Array.from(groups!.querySelectorAll("section h3")).map((h) => h.textContent);
    expect(labels, "three labelled groups, in order").toEqual(["Profile", "Agent", "Model"]);

    const pressed = (id: string) =>
      dlg!.querySelector<HTMLElement>(tid(id))!.getAttribute("aria-pressed");
    // Profile pills: the browsed (bound) profile is pressed.
    expect(pressed("selection-profile-peakbot-dev")).toBe("true");
    expect(pressed("selection-profile-writer")).toBe("false");
    // Agent pills: the bound agent is pressed.
    expect(pressed("selection-agent-team")).toBe("true");
    expect(pressed("selection-agent-review")).toBe("false");
    // Model pills: the orchestrator's effective model is pressed.
    expect(pressed("selection-model-haiku")).toBe("false");
    expect(pressed("selection-model-sonnet")).toBe("true");
    expect(pressed("selection-model-opus")).toBe("false");

    // Every option is a pressable pill, all visible at once (no drill-down).
    const pills = Array.from(groups!.querySelectorAll<HTMLElement>("button[aria-pressed]"));
    expect(pills.length, "2 profiles + 2 agents + 3 models").toBe(7);
    for (const p of pills) expect(p.tagName, "each option is a button").toBe("BUTTON");
  });

  it("'Team models ›' opens the Team drill; ‹ Back returns to the segments", () => {
    mountChip();
    click(chip());
    const teamModels = q("selection-team-models");
    expect(teamModels.textContent, "the drill entry is labelled").toContain("Team models");
    click(teamModels);
    expect(dialog(), "the sheet stays open for the drill").not.toBeNull();
    expect(segs(), "the segments make way for the drill").toBeNull();
    expect(
      dialog()!.querySelector(tid("selection-member-orchestrator")),
      "the Team drill renders",
    ).not.toBeNull();

    click(q("selection-back"));
    expect(segs(), "‹ Back returns to the segments").not.toBeNull();
    expect(
      dialog()!.querySelector(tid("selection-team-models")),
      "the Team models entry is back",
    ).not.toBeNull();
  });
});

// ─── SC3: an Agent or Model pick sends the frame and closes the sheet ────

describe("SelectionChip sheet — SC3: an Agent or Model pick closes the sheet", () => {
  it("a Model pill sends the one select frame and the dialog is gone", () => {
    const { send, onToast } = mountChip();
    click(chip());
    click(q("selection-model-opus"));

    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toStrictEqual(select("peakbot-dev", "team", { model: "opus" }));
    expect(onToast).toHaveBeenCalledTimes(1);
    expect(dialog(), "the sheet closes after the model pick").toBeNull();
  });

  it("an Agent pill sends one select frame and closes the sheet (Q4)", () => {
    const { send, onToast } = mountChip();
    click(chip());
    click(q("selection-agent-review"));

    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toStrictEqual(select("peakbot-dev", "review"));
    expect(onToast).toHaveBeenCalledTimes(1);
    expect(dialog(), "the agent pick closes the sheet, as in the mock").toBeNull();
  });
});

// ─── SC4: in-sheet interaction does not dismiss ───────────────────────────

describe("SelectionChip sheet — SC4: mousedown inside the sheet keeps it open", () => {
  it("a mousedown on a segment pill does not close the sheet", () => {
    mountChip();
    click(chip());
    const pill = q("selection-profile-writer");
    act(() => {
      pill.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
    });
    expect(dialog(), "an in-sheet mousedown is not 'outside'").not.toBeNull();
  });

  it("Esc closes the whole sheet (the Sheet owns dismissal)", () => {
    mountChip();
    click(chip());
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(dialog(), "Esc closes the Selection sheet").toBeNull();
  });

  it("Done closes the sheet", () => {
    mountChip();
    click(chip());
    click(q("sheet-done"));
    expect(dialog()).toBeNull();
  });
});

// ─── SC5: locked ──────────────────────────────────────────────────────────

describe("SelectionChip sheet — SC5: a locked selection does not open", () => {
  it("the trigger is disabled and a tap opens nothing", () => {
    mountChip({ locked: true });
    expect(chip().hasAttribute("disabled"), "the trigger is disabled").toBe(true);
    click(chip());
    expect(dialog(), "a locked chip opens no sheet").toBeNull();
  });

  it("locking while the sheet is open closes it", () => {
    const props = {
      catalog: CATALOG,
      selection: SEL,
      models: MODELS,
      activeAlias: "sonnet",
      pinned: false,
      send: vi.fn(),
      onToast: vi.fn(),
      sheet: true,
    } satisfies Omit<SelectionChipProps, "locked">;
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() => root!.render(<SelectionChip {...props} locked={false} />));
    click(chip());
    expect(dialog(), "open before the lock").not.toBeNull();
    act(() => root!.render(<SelectionChip {...props} locked={true} />));
    expect(dialog(), "the lock closes the open sheet").toBeNull();
  });
});

// ─── SC6: the segments' behaviour ─────────────────────────────────────────

describe("SelectionChip sheet — SC6: the segments' behaviour", () => {
  it("a Profile pill browses: no frame is sent and the Agent group switches", () => {
    const { send } = mountChip();
    click(chip());
    click(q("selection-profile-writer"));

    expect(send, "browsing sends nothing").not.toHaveBeenCalled();
    expect(dialog(), "the sheet stays open while browsing").not.toBeNull();
    const dlg = dialog()!;
    expect(
      dlg.querySelector(tid("selection-profile-writer"))!.getAttribute("aria-pressed"),
      "the browsed profile's pill is pressed",
    ).toBe("true");
    expect(
      dlg.querySelector(tid("selection-agent-draft")),
      "the Agent group shows writer's agents",
    ).not.toBeNull();
    expect(dlg.querySelector(tid("selection-agent-team")), "the old agents are gone").toBeNull();
    expect(
      dlg.querySelector(tid("selection-team-models")),
      "no subagents ⇒ no Team models entry",
    ).toBeNull();
  });

  it("the locked chip keeps the 🔒 and does not open (keep-R)", () => {
    mountChip({ locked: true });
    expect(chip().textContent, "the lock glyph is kept").toContain("🔒");
    expect(chip().hasAttribute("disabled"), "the trigger is disabled").toBe(true);
    click(chip());
    expect(dialog(), "a locked chip opens no sheet").toBeNull();
  });
});

// ─── desktop lock (R1): the !sheet drill-down is untouched ────────────────

describe("SelectionChip — desktop lock: without sheet the drill-down stays", () => {
  it("the trigger keeps the bordered chip face and the drill renders inline, not in a dialog", () => {
    mountChip({ sheet: false });
    expect(chip().className, "the desktop chip keeps its border").toMatch(/(^|\s)border(\s|$)/);
    click(chip());
    expect(dialog(), "no dialog on the desktop path").toBeNull();
    const pop = host!.querySelector<HTMLElement>(tid("selection-popover"));
    expect(pop, "the popover renders inline in the chip's container").not.toBeNull();
    expect(host!.contains(pop!), "not portal'd").toBe(true);
    expect(pop!.querySelector(tid("selection-title-profile")), "the profile column").not.toBeNull();
    expect(pop!.querySelector(tid("selection-title-agent")), "the agent column").not.toBeNull();
  });
});
