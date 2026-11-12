// SelectionChip in sheet mode (mobile redesign design §2.5 `sheet?: boolean`,
// §2.7, SC1–SC5; scope C13).
//
// `sheet` defaults to false and every existing selectionChip*.test.tsx stays
// green UNMODIFIED. With `sheet` the trigger is plain subtitle text and the
// existing drill-down renders inside `<Sheet title="Selection">`:
//   * the trigger opens a dialog named "Selection" (portal'd to body);
//   * the panes (profile › agent › team › model) render inside it;
//   * a model pick sends the ONE select frame and CLOSES the sheet (C13) —
//     instead of returning to the Team pane;
//   * a mousedown inside the sheet does NOT close it (the portal would make
//     every in-sheet click "outside" the chip's root);
//   * a locked selection disables the trigger and does not open anything.
//
// Harness: the selectionChip.test.tsx conventions (createRoot/act, matchMedia
// stub, testid helpers).
//
// RED before implementation: `sheet` is not a SelectionChip prop yet, so the
// chip renders its popover inline and no dialog appears.

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

  it("the trigger is plain subtitle text (no border/background chip styling)", () => {
    mountChip();
    const cls = chip().className;
    expect(cls, "no border").not.toMatch(/(^|\s)border(\s|$)/);
    expect(cls, "no background").not.toMatch(/(^|\s)bg-/);
  });

  it("the face still reads profile › agent › model", () => {
    mountChip();
    expect(chip().textContent).toContain("peakbot-dev › team");
  });
});

// ─── SC2: the panes render inside the sheet ──────────────────────────────

describe("SelectionChip sheet — SC2: the drill-down panes live inside the dialog", () => {
  it("the profile and agent rows render inside the Selection dialog", () => {
    mountChip();
    click(chip());
    const dlg = dialog()!;
    expect(dlg.querySelector(tid("selection-profile-peakbot-dev")), "profile rows inside").not.toBeNull();
    expect(dlg.querySelector(tid("selection-profile-writer"))).not.toBeNull();
    expect(dlg.querySelector(tid("selection-agent-team")), "agent rows inside").not.toBeNull();
  });

  it("the agent pick still drills to the Team pane inside the sheet", () => {
    mountChip();
    click(chip());
    click(q("selection-agent-team")); // already bound: only advances the drill
    expect(dialog(), "the sheet stays open").not.toBeNull();
    expect(dialog()!.querySelector(tid("selection-member-orchestrator")), "Team pane").not.toBeNull();
  });
});

// ─── SC3: a model pick sends the frame and closes the sheet (C13) ────────

describe("SelectionChip sheet — SC3: a model pick closes the sheet", () => {
  it("sends the one select frame and the dialog is gone", () => {
    const { send, onToast } = mountChip();
    click(chip());
    click(q("selection-agent-team"));
    click(q("selection-member-orchestrator"));
    click(q("selection-model-opus"));

    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toStrictEqual(select("peakbot-dev", "team", { model: "opus" }));
    expect(onToast).toHaveBeenCalledTimes(1);
    expect(dialog(), "the sheet closes after the model pick").toBeNull();
  });
});

// ─── SC4: in-sheet interaction does not dismiss ───────────────────────────

describe("SelectionChip sheet — SC4: mousedown inside the sheet keeps it open", () => {
  it("a mousedown on a pane control does not close the sheet", () => {
    mountChip();
    click(chip());
    const row = q("selection-profile-writer");
    act(() => {
      row.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
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
