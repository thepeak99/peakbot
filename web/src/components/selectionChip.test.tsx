// Selection chip → team models (selection-models plan §5.3 / §9.2, items
// 11–33) plus the pre-existing chip behaviours that survive the rewrite
// (folder marker, browse-only profile click, pin, lock, Esc, outside click).
//
// The chip is now the ONE place that sets up the NEXT conversation:
// Profile › Agent › Team › Model. It renders ONLY from server echoes
// (`selection`, `activeAlias`) and talks to the server through ONE frame:
// `select(profile, agent, {model?, subagentModels?})` — the FULL desired setup.
//
// Harness: plain createRoot/act (project convention, no testing-library);
// jsdom comes from vitest.config.ts for src/components/**. jsdom has no
// layout, so responsive behaviour is pinned by `data-pane` + class names.
//
// Written BEFORE the code (TDD): the current chip has none of the new
// testids, so these fail on assertions (feature absent), not on imports.
// The removed ModelSwitcher / AgentsChip tests are gone from this file on
// purpose (plan §9.2 "REMOVE").

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SelectionChip } from "./SelectionChip";
import { TopBar } from "./TopBar";
import { BottomBar } from "./BottomBar";
import { Composer } from "./Composer";
import { settingsGearEnabled } from "../tabs";
import { select, type InboundMessage, type ModelInfo } from "../state";
import type { CatalogProfile, Selection } from "../types";
import type { SelectionChipProps } from "../settings/contract";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

// ─── fixtures ───────────────────────────────────────────────────────────

const CATALOG: CatalogProfile[] = [
  {
    name: "peakbot-dev",
    origin: "repo",
    overridesGlobal: false,
    agents: [
      {
        name: "team",
        model: "sonnet",
        subagents: [
          { name: "reviewer", model: "sonnet", inherited: true },
          { name: "tester", model: "haiku", inherited: false },
        ],
      },
      { name: "review", model: "sonnet", subagents: [] },
    ],
  },
  {
    name: "writer",
    origin: "global",
    overridesGlobal: false,
    agents: [
      { name: "draft", model: "haiku", subagents: [] },
      {
        name: "editor",
        model: "sonnet",
        subagents: [
          { name: "a", model: "sonnet", inherited: true },
          { name: "b", model: "sonnet", inherited: true },
          { name: "c", model: "sonnet", inherited: true },
        ],
      },
    ],
  },
];

const MODELS: ModelInfo[] = [
  { alias: "haiku", provider_name: "anthropic", model_name: "claude-haiku", context_size: 200000 },
  { alias: "sonnet", provider_name: "anthropic", model_name: "claude-sonnet", context_size: 200000 },
  { alias: "opus", provider_name: "anthropic", model_name: "claude-opus", context_size: 200000 },
];

const SEL: Selection = { profile: "peakbot-dev", agent: "team", subagentModels: {} };
const selOf = (subagentModels: Record<string, string>): Selection => ({
  ...SEL,
  subagentModels,
});

// ─── mount plumbing ─────────────────────────────────────────────────────

let root: Root | null = null;
let host: HTMLElement | null = null;
const originalMatchMedia = window.matchMedia;

beforeEach(() => {
  // useMediaQuery (Composer, ThemeToggle…) calls matchMedia on mount.
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
});

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

function rerender(node: React.ReactNode) {
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

function pressEsc(): KeyboardEvent {
  const ev = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  act(() => {
    document.body.dispatchEvent(ev);
  });
  return ev;
}

function chipProps(over: Partial<SelectionChipProps> = {}) {
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
    ...over,
  };
  return { props, send, onToast };
}

function mountChip(over: Partial<SelectionChipProps> = {}) {
  const r = chipProps(over);
  mount(<SelectionChip {...r.props} />);
  return r;
}

const openChip = () => click(q("selection-chip"));
const pane = () => q("selection-popover").getAttribute("data-pane");

/** Open the popover and land on the Team pane of the BOUND agent (a click on
 * the bound agent sends nothing and only advances the drill). */
function gotoTeam(agent = "team") {
  openChip();
  click(q(`selection-agent-${agent}`));
  expect(pane()).toBe("team");
}

/** Model rows of the Model pane: the per-alias rows only (not the `default`
 * tag or the reset button, which share the testid prefix). */
function modelRows(): HTMLElement[] {
  return Array.from(host!.querySelectorAll<HTMLElement>('[data-testid^="selection-model-"]')).filter(
    (el) => {
      const id = el.getAttribute("data-testid");
      return id !== "selection-model-default-tag" && id !== "selection-model-reset";
    },
  );
}

/** The `selection-member-model` span inside a Team row. */
function memberModel(rowId: string): HTMLElement {
  const el = q(rowId).querySelector<HTMLElement>(tid("selection-member-model"));
  if (!el) throw new Error(`no selection-member-model inside ${rowId}`);
  return el;
}

/** The column that holds `rowId` and the title `titleId` (their smallest
 * common ancestor) — the element that carries the narrow-layout classes. */
function columnOf(titleId: string, rowId: string): HTMLElement {
  const row = q(rowId);
  let el: HTMLElement | null = q(titleId).parentElement;
  while (el && !el.contains(row)) el = el.parentElement;
  if (!el) throw new Error(`no column holds ${titleId} and ${rowId}`);
  return el;
}

const frameOf = (send: ReturnType<typeof vi.fn<(m: InboundMessage) => void>>, n = 0) =>
  send.mock.calls[n][0];

// ─── SelectionChip: surviving behaviours ────────────────────────────────

describe("selection chip (T32, spec §9.1)", () => {
  it("chip_shows_folder_marker_only_for_repo_profile", () => {
    mountChip();
    expect(text("selection-chip")).toContain("📁");
    expect(text("selection-chip")).toContain("peakbot-dev › team");

    rerender(
      <SelectionChip
        {...chipProps({
          selection: { profile: "writer", agent: "editor", subagentModels: {} },
        }).props}
      />,
    );
    expect(text("selection-chip")).toContain("writer › editor");
    expect(text("selection-chip")).not.toContain("📁");
  });

  it("renders_nothing_without_selection_or_catalog", () => {
    mountChip({ selection: null });
    expect(has("selection-chip")).toBe(false);
    rerender(<SelectionChip {...chipProps({ catalog: [] }).props} />);
    expect(has("selection-chip")).toBe(false);
  });

  it("popover_is_closed_until_chip_clicked_and_lists_profiles_in_catalog_order", () => {
    mountChip();
    expect(has("selection-popover")).toBe(false);
    openChip();
    expect(has("selection-popover")).toBe(true);
    const rows = Array.from(
      host!.querySelectorAll<HTMLElement>('[data-testid^="selection-profile-"]'),
    ).map((r) => r.getAttribute("data-testid"));
    expect(rows).toEqual(["selection-profile-peakbot-dev", "selection-profile-writer"]);
    // 📁 on repo rows only; `default` caption on row 0 only.
    expect(text("selection-profile-peakbot-dev")).toContain("📁");
    expect(text("selection-profile-writer")).not.toContain("📁");
    expect(text("selection-profile-peakbot-dev")).toContain("default");
    expect(text("selection-profile-writer")).not.toContain("default");
  });

  it("popover_opens_browsing_the_selected_profile_with_its_agents", () => {
    mountChip();
    openChip();
    expect(q("selection-profile-peakbot-dev").getAttribute("data-browsed")).toBe("true");
    // The Agent column title no longer carries the profile name.
    expect(text("selection-title-agent").trim().toLowerCase()).toBe("agent");
    expect(text("selection-title-agent").toLowerCase()).not.toContain("peakbot-dev");
    expect(has("selection-agent-team")).toBe(true);
    expect(has("selection-agent-review")).toBe(true);
    expect(has("selection-agent-draft")).toBe(false);
    // captions: "N subagents · default" on the first agent, "single" otherwise.
    expect(text("selection-agent-team")).toContain("2 subagents");
    expect(text("selection-agent-team")).toContain("default");
    expect(text("selection-agent-review")).toContain("single");
  });

  it("profile_click_browses_only", () => {
    const { send, onToast } = mountChip();
    openChip();
    click(q("selection-profile-writer"));

    // The Agent column switched to the browsed profile's agents…
    expect(q("selection-profile-writer").getAttribute("data-browsed")).toBe("true");
    expect(q("selection-profile-peakbot-dev").getAttribute("data-browsed")).not.toBe("true");
    expect(pane()).toBe("agent");
    expect(has("selection-agent-draft")).toBe(true);
    expect(has("selection-agent-editor")).toBe(true);
    expect(has("selection-agent-team")).toBe(false);
    expect(text("selection-agent-editor")).toContain("3 subagents");
    // …but NOTHING is selected or sent, and the popover stays open.
    expect(send).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
    expect(has("selection-popover")).toBe(true);
    expect(text("selection-chip")).toContain("peakbot-dev › team");
  });

  it("agent_click_sends_select", () => {
    const { send, onToast } = mountChip();
    openChip();
    click(q("selection-profile-writer")); // browse…
    click(q("selection-agent-editor")); //   …then pick

    expect(send).toHaveBeenCalledTimes(1);
    // Exactly the 3-key frame — nothing to override on a fresh agent.
    expect(frameOf(send)).toStrictEqual({ type: "select", profile: "writer", agent: "editor" });
    // Same frame the state.ts builder makes (wire parity).
    expect(frameOf(send)).toStrictEqual(select("writer", "editor"));
    expect(onToast).toHaveBeenCalledWith("Next conversation: writer › editor");
    // The popover STAYS open and drills into the new agent's Team.
    expect(has("selection-popover")).toBe(true);
    expect(pane()).toBe("team");
  });

  it("agent_click_in_initially_browsed_profile_sends_that_profile", () => {
    const { send } = mountChip();
    openChip();
    click(q("selection-agent-review"));
    expect(send).toHaveBeenCalledTimes(1);
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "review",
    });
  });

  it("agent_pick_frame_never_carries_the_old_agents_overrides", () => {
    // A new agent starts clean: even with overrides on the bound team, picking
    // another agent sends the bare 3-key frame.
    const { send } = mountChip({ selection: selOf({ tester: "opus" }), activeAlias: "opus" });
    openChip();
    click(q("selection-agent-review"));
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "review",
    });
  });

  it("browsed_profile_resets_to_selection_on_reopen", () => {
    mountChip();
    openChip();
    click(q("selection-profile-writer"));
    click(q("selection-chip")); // close without choosing
    expect(has("selection-popover")).toBe(false);
    openChip();
    expect(q("selection-profile-peakbot-dev").getAttribute("data-browsed")).toBe("true");
    expect(pane(), "a reopen starts at the Profile pane again").toBe("profile");
  });

  it("chip_locked_after_turn", () => {
    const { send } = mountChip({ locked: true });
    const chip = q<HTMLButtonElement>("selection-chip");
    expect(chip.textContent ?? "").toMatch(/^\s*🔒/);
    expect(chip.textContent).toContain("peakbot-dev › team");
    expect(chip.getAttribute("title")).toBe("Locked after the first message");
    expect(chip.disabled).toBe(true);
    expect(chip.textContent).not.toContain("▾");
    click(chip);
    expect(has("selection-popover")).toBe(false);
    expect(send).not.toHaveBeenCalled();
  });

  it("unlocked_chip_has_no_lock_glyph_and_the_switch_tooltip", () => {
    mountChip({ locked: false });
    expect(text("selection-chip")).not.toContain("🔒");
    expect(q("selection-chip").getAttribute("title")).toBe(
      "Switch profile / agent / team model",
    );
    expect(text("selection-chip")).toContain("▾");
  });

  it("locking_while_open_closes_the_popover_read_only", () => {
    const r = chipProps();
    mount(<SelectionChip {...r.props} />);
    openChip();
    expect(has("selection-popover")).toBe(true);
    rerender(<SelectionChip {...r.props} locked />);
    expect(has("selection-popover")).toBe(false);
    expect(r.send).not.toHaveBeenCalled();
  });

  it("pinned_hides_profile_switch_allows_agent", () => {
    const { send } = mountChip({ pinned: true });
    openChip();
    // Only the pinned profile, captioned `pinned`; no way to switch profile.
    expect(has("selection-profile-peakbot-dev")).toBe(true);
    expect(has("selection-profile-writer")).toBe(false);
    expect(text("selection-profile-peakbot-dev")).toContain("pinned");
    click(q("selection-profile-peakbot-dev"));
    expect(send).not.toHaveBeenCalled();
    expect(has("selection-agent-team")).toBe(true);
    // The agent switch still works under a pin.
    click(q("selection-agent-review"));
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "review",
    });
  });

  it("unpinned_popover_has_no_pinned_caption", () => {
    mountChip({ pinned: false });
    openChip();
    expect(text("selection-popover")).not.toContain("pinned");
  });

  it("esc_closes_popover_and_marks_event_handled_so_settings_stays_open", () => {
    mountChip();
    openChip();
    const ev = pressEsc();
    expect(has("selection-popover")).toBe(false);
    expect(ev.defaultPrevented).toBe(true);

    // Closed: Esc is not swallowed (Settings may close on it).
    const ev2 = pressEsc();
    expect(ev2.defaultPrevented).toBe(false);
  });

  it("mousedown_outside_closes_popover", () => {
    mountChip();
    openChip();
    act(() => {
      document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    });
    expect(has("selection-popover")).toBe(false);
  });

  it("mousedown_outside_closes_the_popover_from_the_model_pane_too", () => {
    mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));
    expect(pane()).toBe("model");
    act(() => {
      document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    });
    expect(has("selection-popover")).toBe(false);
  });

  it("mousedown_inside_the_popover_keeps_it_open", () => {
    mountChip();
    openChip();
    act(() => {
      q("selection-profile-writer").dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    });
    expect(has("selection-popover")).toBe(true);
  });

  it("unicode_and_dotted_names_render_and_send_verbatim", () => {
    const catalog: CatalogProfile[] = [
      {
        name: "équipe.v2 ✨",
        origin: "global",
        overridesGlobal: false,
        agents: [
          { name: "agent é", model: "módel.1", subagents: [] },
          { name: "agent ü", model: "módel.1", subagents: [] },
        ],
      },
    ];
    const models: ModelInfo[] = [
      { alias: "módel.1", provider_name: "p", model_name: "m1", context_size: 1 },
      { alias: "módel.2", provider_name: "p", model_name: "m2", context_size: 1 },
    ];
    const { send } = mountChip({
      catalog,
      models,
      activeAlias: "módel.1",
      selection: { profile: "équipe.v2 ✨", agent: "agent é", subagentModels: {} },
    });
    expect(text("selection-chip")).toContain("équipe.v2 ✨ › agent é › módel.1");

    openChip();
    click(q("selection-agent-agent ü"));
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "équipe.v2 ✨",
      agent: "agent ü",
    });

    // The chip renders only from server echoes: simulate the state frame that
    // re-binds the session to `agent ü`, then a unicode model alias must ride
    // the frame verbatim.
    rerender(
      <SelectionChip
        {...chipProps({
          catalog,
          models,
          activeAlias: "módel.1",
          send,
          selection: { profile: "équipe.v2 ✨", agent: "agent ü", subagentModels: {} },
        }).props}
      />,
    );
    click(q("selection-member-orchestrator"));
    click(q("selection-model-módel.2"));
    expect(frameOf(send, 1)).toStrictEqual({
      type: "select",
      profile: "équipe.v2 ✨",
      agent: "agent ü",
      model: "módel.2",
    });
  });
});

// ─── the chip face: orchestrator model + overrides badge ────────────────

describe("chip face (selection-models plan §9.2/11–13)", () => {
  it("chip_shows_orchestrator_model_muted_when_default", () => {
    mountChip({ activeAlias: "sonnet" });
    const model = q("selection-chip-model");
    expect(model.textContent).toBe("sonnet");
    expect(model.getAttribute("data-state")).toBe("default");
    expect(text("selection-chip")).toContain("peakbot-dev › team › sonnet");
  });

  it("chip_shows_orchestrator_model_bright_when_overridden", () => {
    mountChip({ activeAlias: "opus" });
    const model = q("selection-chip-model");
    expect(model.textContent).toBe("opus");
    expect(model.getAttribute("data-state")).toBe("overridden");
    expect(text("selection-chip")).toContain("peakbot-dev › team › opus");
  });

  it("chip_falls_back_to_the_agents_model_without_an_active_alias", () => {
    mountChip({ activeAlias: "" });
    expect(q("selection-chip-model").textContent).toBe("sonnet");
    expect(q("selection-chip-model").getAttribute("data-state")).toBe("default");
  });

  it("chip_badge_counts_subagent_overrides", () => {
    // None → no badge.
    mountChip();
    expect(has("selection-chip-overrides")).toBe(false);

    // One override on `team`.
    rerender(<SelectionChip {...chipProps({ selection: selOf({ tester: "opus" }) }).props} />);
    expect(text("selection-chip-overrides")).toBe("+1");
    expect(q("selection-chip-overrides").getAttribute("title")).toBe("1 subagent model changed");

    // Two overrides on `editor` → plural title.
    rerender(
      <SelectionChip
        {...chipProps({
          selection: {
            profile: "writer",
            agent: "editor",
            subagentModels: { a: "opus", b: "haiku" },
          },
        }).props}
      />,
    );
    expect(text("selection-chip-overrides")).toBe("+2");
    expect(q("selection-chip-overrides").getAttribute("title")).toBe("2 subagent models changed");
  });

  it("chip_badge_ignores_the_orchestrator_override", () => {
    // Only SUBAGENT overrides count toward +N.
    mountChip({ activeAlias: "opus" });
    expect(has("selection-chip-overrides")).toBe(false);
  });

  it("chip_badge_is_still_shown_while_locked", () => {
    mountChip({ locked: true, selection: selOf({ tester: "opus" }) });
    expect(text("selection-chip-overrides")).toBe("+1");
    expect(text("selection-chip")).toContain("🔒");
  });
});

// ─── popover: panes, team rows, model pane ──────────────────────────────

describe("popover drill-down (selection-models plan §9.2/14–17, 28–29)", () => {
  it("popover_titles_profile_agent_team_and_no_model_pane_until_member_click", () => {
    mountChip();
    openChip();
    expect(text("selection-title-profile").trim().toLowerCase()).toBe("profile");
    expect(text("selection-title-agent").trim().toLowerCase()).toBe("agent");
    expect(text("selection-title-team").trim().toLowerCase()).toBe("team");
    expect(has("selection-title-model")).toBe(false);
    expect(pane()).toBe("profile");

    click(q("selection-member-sub-tester"));
    expect(has("selection-title-model")).toBe(true);
  });

  it("team_rows_show_captions_and_effective_models", () => {
    mountChip({ activeAlias: "sonnet" });
    openChip();

    // Orchestrator row: caption + the configured model, muted (default).
    expect(text("selection-member-orchestrator")).toContain("team");
    expect(text("selection-member-orchestrator")).toContain("orchestrator");
    expect(memberModel("selection-member-orchestrator").textContent).toBe("sonnet");
    expect(memberModel("selection-member-orchestrator").getAttribute("data-state")).toBe("default");

    // reviewer is inherited; tester is explicit.
    expect(text("selection-member-sub-reviewer")).toContain("subagent");
    expect(memberModel("selection-member-sub-reviewer").textContent).toBe("inherits sonnet");
    expect(memberModel("selection-member-sub-reviewer").getAttribute("data-state")).toBe(
      "inherited",
    );
    expect(memberModel("selection-member-sub-tester").textContent).toBe("haiku");
    expect(memberModel("selection-member-sub-tester").getAttribute("data-state")).toBe("default");

    // An orchestrator override: reviewer follows, orchestrator turns bright.
    rerender(<SelectionChip {...chipProps({ activeAlias: "opus" }).props} />);
    expect(memberModel("selection-member-sub-reviewer").textContent).toBe("inherits opus");
    expect(memberModel("selection-member-orchestrator").textContent).toBe("opus");
    expect(memberModel("selection-member-orchestrator").getAttribute("data-state")).toBe(
      "overridden",
    );

    // A subagent override.
    rerender(
      <SelectionChip {...chipProps({ selection: selOf({ tester: "opus" }) }).props} />,
    );
    expect(memberModel("selection-member-sub-tester").textContent).toBe("opus");
    expect(memberModel("selection-member-sub-tester").getAttribute("data-state")).toBe(
      "overridden",
    );
  });

  it("team_rows_follow_catalog_order_orchestrator_first", () => {
    mountChip();
    openChip();
    const ids = Array.from(
      host!.querySelectorAll<HTMLElement>('[data-testid^="selection-member-"]'),
    )
      .map((e) => e.getAttribute("data-testid"))
      .filter((id) => id !== "selection-member-model");
    expect(ids).toEqual([
      "selection-member-orchestrator",
      "selection-member-sub-reviewer",
      "selection-member-sub-tester",
    ]);
  });

  it("single_agent_team_row_captioned_main", () => {
    mountChip({ selection: { profile: "peakbot-dev", agent: "review", subagentModels: {} } });
    openChip();
    expect(text("selection-member-orchestrator")).toContain("review");
    expect(text("selection-member-orchestrator")).toContain("main");
    expect(text("selection-member-orchestrator")).not.toContain("orchestrator");
    expect(host!.querySelector('[data-testid^="selection-member-sub-"]')).toBeNull();
  });

  it("member_click_opens_model_pane", () => {
    mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));

    expect(pane()).toBe("model");
    expect(text("selection-title-model").toLowerCase()).toContain("model · tester");

    // One row per model, server order, with the provider · wire-name subline.
    const rows = modelRows();
    expect(rows.map((r) => r.getAttribute("data-testid"))).toEqual([
      "selection-model-haiku",
      "selection-model-sonnet",
      "selection-model-opus",
    ]);
    expect(rows[0].textContent).toContain("anthropic · claude-haiku");
    expect(rows[1].textContent).toContain("anthropic · claude-sonnet");

    // tester's effective AND default model is haiku: current mark + default tag.
    expect(rows[0].getAttribute("data-current")).toBe("true");
    expect(rows[0].textContent).toContain("→");
    expect(rows[1].getAttribute("data-current")).not.toBe("true");
    expect(rows[2].getAttribute("data-current")).not.toBe("true");
    const tags = host!.querySelectorAll(tid("selection-model-default-tag"));
    expect(tags).toHaveLength(1);
    expect(rows[0].contains(tags[0])).toBe(true);
    expect(tags[0].textContent).toBe("default");
  });

  it("model_pane_marks_current_and_default_apart_when_overridden", () => {
    mountChip({ selection: selOf({ tester: "opus" }) });
    gotoTeam();
    click(q("selection-member-sub-tester"));
    const byAlias = (a: string) => q(`selection-model-${a}`);
    // Effective = opus (current), default = haiku (tag).
    expect(byAlias("opus").getAttribute("data-current")).toBe("true");
    expect(byAlias("haiku").getAttribute("data-current")).not.toBe("true");
    expect(byAlias("haiku").querySelector(tid("selection-model-default-tag"))).not.toBeNull();
    expect(byAlias("opus").querySelector(tid("selection-model-default-tag"))).toBeNull();
  });

  it("model_pane_of_an_inherited_member_tags_the_orchestrators_model_as_default", () => {
    // Orchestrator overridden to opus → reviewer (inherited) defaults to opus.
    mountChip({ activeAlias: "opus" });
    gotoTeam();
    click(q("selection-member-sub-reviewer"));
    expect(q("selection-model-opus").getAttribute("data-current")).toBe("true");
    expect(q("selection-model-opus").querySelector(tid("selection-model-default-tag"))).not.toBeNull();
    expect(q("selection-model-sonnet").querySelector(tid("selection-model-default-tag"))).toBeNull();
  });

  it("drill_state_advances_on_clicks", () => {
    mountChip();
    openChip();
    expect(pane()).toBe("profile");
    click(q("selection-profile-peakbot-dev"));
    expect(pane()).toBe("agent");
    click(q("selection-agent-team"));
    expect(pane()).toBe("team");
    click(q("selection-member-sub-tester"));
    expect(pane()).toBe("model");
  });

  it("back_button_steps_up_the_drill", () => {
    mountChip();
    openChip();
    expect(has("selection-back"), "no Back on the first pane").toBe(false);

    click(q("selection-profile-peakbot-dev"));
    click(q("selection-agent-team"));
    click(q("selection-member-sub-tester"));
    expect(pane()).toBe("model");
    expect(text("selection-back")).toContain("‹ Back");

    click(q("selection-back"));
    expect(pane()).toBe("team");
    click(q("selection-back"));
    expect(pane()).toBe("agent");
    click(q("selection-back"));
    expect(pane()).toBe("profile");
    expect(has("selection-back")).toBe(false);
    // Stepping back never sends or closes.
    expect(has("selection-popover")).toBe(true);
  });

  it("esc_steps_back_from_model_then_closes", () => {
    mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));
    expect(pane()).toBe("model");

    const first = pressEsc();
    expect(has("selection-popover"), "still open").toBe(true);
    expect(pane()).toBe("team");
    expect(first.defaultPrevented).toBe(true);

    const second = pressEsc();
    expect(has("selection-popover")).toBe(false);
    expect(second.defaultPrevented).toBe(true);
  });

  it("esc_from_the_team_pane_closes_the_popover", () => {
    mountChip();
    gotoTeam();
    const ev = pressEsc();
    expect(has("selection-popover")).toBe(false);
    expect(ev.defaultPrevented).toBe(true);
  });
});

// ─── model picks → the full desired-setup frame ─────────────────────────

describe("model picks (selection-models plan §9.2/18–26)", () => {
  it("model_pick_sends_full_frame_returns_to_team_and_toasts", () => {
    const { send, onToast } = mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));
    click(q("selection-model-opus"));

    expect(send).toHaveBeenCalledTimes(1);
    expect(frameOf(send)).toStrictEqual(
      select("peakbot-dev", "team", { subagentModels: { tester: "opus" } }),
    );
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      subagent_models: { tester: "opus" },
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: tester on opus");
    expect(pane()).toBe("team");
    expect(has("selection-popover")).toBe(true);
  });

  it("model_pick_preserves_other_overrides_and_orchestrator", () => {
    const { send } = mountChip({ activeAlias: "opus", selection: selOf({ tester: "opus" }) });
    gotoTeam();
    click(q("selection-member-sub-reviewer"));
    click(q("selection-model-haiku"));

    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      model: "opus",
      subagent_models: { tester: "opus", reviewer: "haiku" },
    });
  });

  it("model_pick_of_default_clears_that_override", () => {
    const { send } = mountChip({ selection: selOf({ tester: "opus" }) });
    gotoTeam();
    click(q("selection-member-sub-tester"));
    click(q("selection-model-haiku")); // haiku IS tester's default

    // No `subagent_models` key at all — and no `model` (orchestrator unchanged).
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
    });
  });

  it("model_pick_of_current_effective_sends_nothing", () => {
    const { send, onToast } = mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));
    click(q("selection-model-haiku")); // already the effective model

    expect(send).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
    expect(pane(), "still returns to the Team pane").toBe("team");
  });

  it("orchestrator_model_pick_sends_model_field", () => {
    const { send, onToast } = mountChip();
    gotoTeam();
    click(q("selection-member-orchestrator"));
    expect(text("selection-title-model").toLowerCase()).toContain("model · team");
    click(q("selection-model-opus"));

    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      model: "opus",
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: team on opus");
    expect(pane()).toBe("team");
  });

  it("reset_to_default_only_when_overridden", () => {
    const { send, onToast } = mountChip();
    gotoTeam();
    click(q("selection-member-sub-tester"));
    expect(has("selection-model-reset"), "nothing to reset by default").toBe(false);

    // With an override, the member's Model pane offers the reset.
    rerender(
      <SelectionChip {...chipProps({ selection: selOf({ tester: "opus" }), send, onToast }).props} />,
    );
    expect(pane()).toBe("model");
    expect(text("selection-model-reset")).toContain("Reset to default");

    click(q("selection-model-reset"));
    expect(send).toHaveBeenCalledTimes(1);
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: tester on haiku");
    expect(pane()).toBe("team");
  });

  it("reset_of_the_orchestrator_keeps_the_subagent_overrides", () => {
    const { send, onToast } = mountChip({
      activeAlias: "opus",
      selection: selOf({ tester: "opus" }),
    });
    gotoTeam();
    click(q("selection-member-orchestrator"));
    click(q("selection-model-reset"));

    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      subagent_models: { tester: "opus" },
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: team on sonnet");
  });

  it("reset_all_only_when_any_override", () => {
    // Nothing overridden → no button.
    const { send, onToast } = mountChip();
    gotoTeam();
    expect(has("selection-team-reset")).toBe(false);

    // A subagent override → button.
    rerender(
      <SelectionChip {...chipProps({ selection: selOf({ tester: "opus" }), send, onToast }).props} />,
    );
    expect(text("selection-team-reset")).toContain("Reset all to defaults");

    // An orchestrator-only override ALSO counts (anyOverride).
    rerender(<SelectionChip {...chipProps({ activeAlias: "opus", send, onToast }).props} />);
    expect(has("selection-team-reset")).toBe(true);

    click(q("selection-team-reset"));
    expect(send).toHaveBeenCalledTimes(1);
    expect(frameOf(send)).toStrictEqual(select("peakbot-dev", "team"));
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
    });
    expect(onToast).toHaveBeenCalledWith("Overrides reset for team");
    expect(pane(), "stays on the Team pane").toBe("team");
  });

  it("same_agent_click_sends_nothing", () => {
    const { send, onToast } = mountChip({ selection: selOf({ tester: "opus" }) });
    openChip();
    click(q("selection-agent-team")); // already bound

    expect(send).not.toHaveBeenCalled();
    expect(onToast).not.toHaveBeenCalled();
    expect(pane()).toBe("team");
  });

  it("model_pick_in_browsed_profile_targets_its_first_agent", () => {
    const { send, onToast } = mountChip();
    openChip();
    click(q("selection-profile-writer")); // browse only
    expect(send).not.toHaveBeenCalled();

    // The Team column shows the browsed profile's FIRST agent (`draft`, single).
    expect(text("selection-member-orchestrator")).toContain("draft");
    expect(text("selection-member-orchestrator")).toContain("main");

    click(q("selection-member-orchestrator"));
    expect(text("selection-title-model").toLowerCase()).toContain("model · draft");
    click(q("selection-model-opus"));

    expect(send).toHaveBeenCalledTimes(1);
    expect(frameOf(send)).toStrictEqual(select("writer", "draft", { model: "opus" }));
    expect(frameOf(send)).toStrictEqual({
      type: "select",
      profile: "writer",
      agent: "draft",
      model: "opus",
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: draft on opus");
  });
});

// ─── layout contract (jsdom cannot measure; pin the classes) ────────────

describe("layout classes (selection-models plan §9.2/30–31)", () => {
  it("popover_opens_upward_with_dropUp", () => {
    mountChip({ dropUp: true });
    openChip();
    expect(q("selection-popover").className).toContain("bottom-full");
    expect(q("selection-popover").className).not.toContain("top-full");

    // Re-mount the other way round.
    act(() => root!.unmount());
    host!.remove();
    mountChip();
    openChip();
    expect(q("selection-popover").className).toContain("top-full");
    expect(q("selection-popover").className).not.toContain("bottom-full");
  });

  it("narrow_layout_classes_present", () => {
    mountChip();
    openChip();
    const cls = q("selection-popover").className;
    expect(cls).toContain("max-[800px]:w-[calc(100vw-1.5rem)]");
    expect(cls).toContain("max-[800px]:max-h-[70vh]");
    expect(cls).toContain("max-[800px]:flex-col");

    const HIDDEN = "max-[800px]:hidden";
    const profileCol = () => columnOf("selection-title-profile", "selection-profile-peakbot-dev");
    const agentCol = () => columnOf("selection-title-agent", "selection-agent-team");
    const teamCol = () => columnOf("selection-title-team", "selection-member-orchestrator");

    // Profile pane current: only the profile column is visible on narrow.
    expect(profileCol().className).not.toContain(HIDDEN);
    expect(agentCol().className).toContain(HIDDEN);
    expect(teamCol().className).toContain(HIDDEN);

    // Agent pane.
    click(q("selection-profile-peakbot-dev"));
    expect(agentCol().className).not.toContain(HIDDEN);
    expect(profileCol().className).toContain(HIDDEN);
    expect(teamCol().className).toContain(HIDDEN);

    // Team pane.
    click(q("selection-agent-team"));
    expect(teamCol().className).not.toContain(HIDDEN);
    expect(agentCol().className).toContain(HIDDEN);

    // Model pane: its column is the visible one.
    click(q("selection-member-sub-tester"));
    const modelCol = columnOf("selection-title-model", "selection-model-haiku");
    expect(modelCol.className).not.toContain(HIDDEN);
    expect(teamCol().className).toContain(HIDDEN);
    expect(profileCol().className).toContain(HIDDEN);
  });
});

// ─── TopBar / BottomBar wiring ──────────────────────────────────────────

function barBase() {
  return {
    isRunning: false,
    connected: true,
    statusMessage: null,
    models: MODELS,
    activeAlias: "sonnet",
    hasTranscript: false,
    cwd: null,
    dirListing: null,
    recentDirs: [],
    conversations: [],
    conversationsError: null,
    send: vi.fn<(m: InboundMessage) => void>(),
    onLoadConversation: vi.fn(),
    notifyEnabled: false,
    notifyPermission: "default" as const,
    onToggleNotify: vi.fn(),
    focusMode: false,
    onToggleFocus: vi.fn(),
  };
}

const chatChips = {
  catalog: CATALOG,
  selection: SEL,
  profilePinned: false,
  onToast: vi.fn(),
};

describe("TopBar chips (T32 + selection-models plan §9.2/32–33)", () => {
  it("topbar_has_no_model_switcher_or_agents_chip", () => {
    const props = { ...barBase(), ...chatChips } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    expect(has("selection-chip")).toBe(true);
    expect(host!.querySelector('button[title="Switch model"]')).toBeNull();
    expect(has("agents-chip")).toBe(false);
  });

  it("topbar_passes_models_and_active_alias_to_chip", () => {
    const props = { ...barBase(), ...chatChips, activeAlias: "opus" } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    expect(text("selection-chip-model")).toBe("opus");
    expect(q("selection-chip-model").getAttribute("data-state")).toBe("overridden");

    // …and the Model pane lists the models it was given.
    click(q("selection-chip"));
    click(q("selection-agent-team"));
    click(q("selection-member-orchestrator"));
    expect(modelRows().map((r) => r.getAttribute("data-testid"))).toEqual([
      "selection-model-haiku",
      "selection-model-sonnet",
      "selection-model-opus",
    ]);
  });

  it("topbar_chip_locks_once_the_conversation_has_turns", () => {
    const props = { ...barBase(), ...chatChips, selectionLocked: true } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    expect(text("selection-chip")).toContain("🔒");
    expect(q<HTMLButtonElement>("selection-chip").disabled).toBe(true);
  });

  // T34 regression: picking an agent posts a `Profile: …` system message, so
  // the transcript is non-empty with zero turns — the chip must stay usable.
  it("topbar_chip_stays_unlocked_with_only_system_messages", () => {
    const props = {
      ...barBase(),
      ...chatChips,
      hasTranscript: true,
      selectionLocked: false,
    } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    expect(text("selection-chip")).not.toContain("🔒");
    expect(q<HTMLButtonElement>("selection-chip").disabled).toBe(false);
  });

  it("topbar_agent_click_sends_select_through_send", () => {
    const base = barBase();
    const props = { ...base, ...chatChips } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    click(q("selection-chip"));
    click(q("selection-agent-review"));
    expect(base.send).toHaveBeenCalledWith({
      type: "select",
      profile: "peakbot-dev",
      agent: "review",
    });
  });

  it("topbar_model_pick_sends_the_full_frame_through_send_and_toasts", () => {
    const base = barBase();
    const onToast = vi.fn();
    const props = { ...base, ...chatChips, onToast } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    click(q("selection-chip"));
    click(q("selection-agent-team"));
    click(q("selection-member-sub-tester"));
    click(q("selection-model-opus"));
    expect(frameOf(base.send)).toStrictEqual({
      type: "select",
      profile: "peakbot-dev",
      agent: "team",
      subagent_models: { tester: "opus" },
    });
    expect(onToast).toHaveBeenCalledWith("Next conversation: tester on opus");
  });

  it("topbar_has_no_chips_without_catalog_and_selection", () => {
    mount(<TopBar {...barBase()} />);
    expect(has("selection-chip")).toBe(false);
    expect(has("agents-chip")).toBe(false);
  });

  it("bottombar_chip_opens_upward_and_has_no_model_chip", () => {
    const b = barBase();
    const props = {
      conversations: b.conversations,
      conversationsError: null,
      models: b.models,
      activeAlias: "sonnet",
      hasTranscript: false,
      cwd: null,
      dirListing: null,
      recentDirs: [],
      send: b.send,
      onLoadConversation: b.onLoadConversation,
      ...chatChips,
    } as ComponentProps<typeof BottomBar>;
    mount(<BottomBar {...props} />);
    click(q("selection-chip"));
    expect(q("selection-popover").className).toContain("bottom-full");
    expect(host!.querySelector('button[title="Switch model"]')).toBeNull();
    expect(has("agents-chip")).toBe(false);
  });
});

// ─── gear gating (§9.12 + settings_available) ───────────────────────────

describe("settings gear gating (T32)", () => {
  it("gear_hidden_without_settings_tab", () => {
    expect(settingsGearEnabled(true, ["session", "todo"])).toBe(false);
    expect(settingsGearEnabled(true, [])).toBe(false);
  });

  it("gear_hidden_when_settings_unavailable", () => {
    // --profile pin ⇒ settings_available:false, even if the tab is listed.
    expect(settingsGearEnabled(false, ["session", "settings"])).toBe(false);
    expect(settingsGearEnabled(false, undefined)).toBe(false);
  });

  it("gear_shown_when_available_and_tab_listed", () => {
    expect(settingsGearEnabled(true, ["session", "settings"])).toBe(true);
  });

  it("gear_shown_on_old_wire_without_either_field", () => {
    expect(settingsGearEnabled(undefined, undefined)).toBe(true);
    expect(settingsGearEnabled(undefined, ["settings"])).toBe(true);
    expect(settingsGearEnabled(true, undefined)).toBe(true);
  });

  it("topbar_gear_renders_only_when_onOpenSettings_given", () => {
    const gear = () => host!.querySelector('button[aria-label="Settings"]');
    mount(<TopBar {...barBase()} />);
    expect(gear()).toBeNull();
    const open = vi.fn();
    rerender(<TopBar {...barBase()} onOpenSettings={open} />);
    expect(gear()).not.toBeNull();
    click(gear()!);
    expect(open).toHaveBeenCalledTimes(1);
  });
});

// ─── composer hint ──────────────────────────────────────────────────────

describe("composer settings hint (T32)", () => {
  const composerProps = () => ({
    isRunning: false,
    connected: true,
    commands: [],
    onSend: vi.fn(),
    onStop: vi.fn(),
    subAgent: null,
    onPause: vi.fn(),
    onResume: vi.fn(),
    watchingRole: null,
    onClearWatch: () => {},
    pendingInput: 0,
  });

  it("hint_shows_ctrl_comma_settings_after_slash_for_commands", () => {
    mount(<Composer {...composerProps()} />);
    expect(text("composer-settings-hint")).toContain("Ctrl+,");
    expect(text("composer-settings-hint")).toContain("settings");
    const hint = q("composer-settings-hint");
    const slash = Array.from(host!.querySelectorAll("span")).find((s) =>
      (s.textContent ?? "").includes("for commands"),
    )!;
    expect(slash).toBeTruthy();
    expect(slash.compareDocumentPosition(hint) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("hint_hidden_when_settings_not_enabled", () => {
    const props = { ...composerProps(), settingsEnabled: false } as ComponentProps<typeof Composer>;
    mount(<Composer {...props} />);
    expect(has("composer-settings-hint")).toBe(false);
  });
});
