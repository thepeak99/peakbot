// T32 (RED): D11 chat — selection chip + two-column popover, lock state, pin
// view, Agents chip/roster, composer hint, gear gating. Spec §9.1 / §9.12 /
// §6.5. Written BEFORE the code: ./SelectionChip, ./AgentsChip,
// `settingsGearEnabled` (../tabs) do not exist yet, and TopBar / BottomBar /
// Composer lack the new optional props. Props + data-testids: contract.ts
// ("T32 (D11 chat selection)").
//
// Harness: plain createRoot/act (project convention, no testing-library);
// jsdom comes from vitest.config.ts for src/components/**.
//
// Expected RED: whole-file import error (missing ./SelectionChip) — the
// individual test names below are the bug reports once the modules exist.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { SelectionChip } from "./SelectionChip";
import { AgentsChip } from "./AgentsChip";
import { TopBar } from "./TopBar";
import { BottomBar } from "./BottomBar";
import { Composer } from "./Composer";
import { settingsGearEnabled } from "../tabs";
import { select, type InboundMessage } from "../state";
import type { CatalogAgent, CatalogProfile } from "../types";
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

const SEL = { profile: "peakbot-dev", agent: "team" };

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

function chipProps(over: Partial<SelectionChipProps> = {}) {
  const send = vi.fn<(m: InboundMessage) => void>();
  const onToast = vi.fn<(m: string) => void>();
  const props: SelectionChipProps = {
    catalog: CATALOG,
    selection: SEL,
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

// ─── SelectionChip ──────────────────────────────────────────────────────

describe("selection chip (T32, spec §9.1)", () => {
  it("chip_shows_folder_marker_only_for_repo_profile", () => {
    mountChip();
    expect(text("selection-chip")).toContain("📁");
    expect(text("selection-chip")).toContain("peakbot-dev › team");

    rerender(
      <SelectionChip
        {...chipProps({ selection: { profile: "writer", agent: "editor" } }).props}
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
    expect(text("selection-agents-title").toUpperCase()).toContain("AGENTS · PEAKBOT-DEV");
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

    // Right column switched to the browsed profile's agents…
    expect(q("selection-profile-writer").getAttribute("data-browsed")).toBe("true");
    expect(q("selection-profile-peakbot-dev").getAttribute("data-browsed")).not.toBe("true");
    expect(text("selection-agents-title").toUpperCase()).toContain("AGENTS · WRITER");
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
    expect(send).toHaveBeenCalledWith({ type: "select", profile: "writer", agent: "editor" });
    // Same frame the state.ts builder makes (wire parity).
    expect(send.mock.calls[0][0]).toEqual(select("writer", "editor"));
    expect(onToast).toHaveBeenCalledWith("Next conversation: writer › editor");
    // Closes the popover.
    expect(has("selection-popover")).toBe(false);
  });

  it("agent_click_in_initially_browsed_profile_sends_that_profile", () => {
    const { send } = mountChip();
    openChip();
    click(q("selection-agent-review"));
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({
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
  });

  it("chip_locked_after_turn", () => {
    const { send } = mountChip({ locked: true });
    const chip = q<HTMLButtonElement>("selection-chip");
    expect(chip.textContent ?? "").toMatch(/^\s*🔒/);
    expect(chip.textContent).toContain("peakbot-dev › team");
    expect(chip.getAttribute("title")).toBe("Locked after the first message");
    expect(chip.disabled).toBe(true);
    click(chip);
    expect(has("selection-popover")).toBe(false);
    expect(send).not.toHaveBeenCalled();
  });

  it("unlocked_chip_has_no_lock_glyph_or_lock_tooltip", () => {
    mountChip({ locked: false });
    expect(text("selection-chip")).not.toContain("🔒");
    expect(q("selection-chip").getAttribute("title")).not.toBe("Locked after the first message");
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
    expect(send).toHaveBeenCalledWith({
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
    const ev = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(ev);
    });
    expect(has("selection-popover")).toBe(false);
    expect(ev.defaultPrevented).toBe(true);

    // Closed: Esc is not swallowed (Settings may close on it).
    const ev2 = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(ev2);
    });
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

  it("unicode_and_dotted_names_render_and_send_verbatim", () => {
    const catalog: CatalogProfile[] = [
      {
        name: "équipe.v2 ✨",
        origin: "global",
        overridesGlobal: false,
        agents: [{ name: "agent é", model: "m", subagents: [] }],
      },
    ];
    const { send } = mountChip({
      catalog,
      selection: { profile: "équipe.v2 ✨", agent: "agent é" },
    });
    expect(text("selection-chip")).toContain("équipe.v2 ✨ › agent é");
    openChip();
    click(q(`selection-agent-agent é`));
    expect(send).toHaveBeenCalledWith({
      type: "select",
      profile: "équipe.v2 ✨",
      agent: "agent é",
    });
  });
});

// ─── AgentsChip ─────────────────────────────────────────────────────────

const TEAM: CatalogAgent = CATALOG[0].agents[0];
const SOLO: CatalogAgent = CATALOG[0].agents[1];

describe("agents chip + roster popover (T32, spec §9.1)", () => {
  it("chip_counts_main_plus_subagents", () => {
    mount(<AgentsChip profile="peakbot-dev" agent={TEAM} activeModel="sonnet" />);
    expect(text("agents-chip")).toContain("Agents · 3");
    rerender(<AgentsChip profile="peakbot-dev" agent={SOLO} activeModel="sonnet" />);
    expect(text("agents-chip")).toContain("Agents · 1");
  });

  it("renders_nothing_without_a_bound_agent", () => {
    mount(<AgentsChip profile={null} agent={null} activeModel="" />);
    expect(has("agents-chip")).toBe(false);
  });

  it("roster_lists_main_with_active_model_and_subagents_with_inherits", () => {
    mount(<AgentsChip profile="peakbot-dev" agent={TEAM} activeModel="opus" />);
    expect(has("agents-popover")).toBe(false);
    click(q("agents-chip"));
    expect(text("agents-roster-title")).toContain("Roster · peakbot-dev › team");
    const main = text("agents-roster-main");
    expect(main).toContain("team");
    expect(main).toContain("main");
    expect(main).toContain("opus"); // the ACTIVE model, not the configured one
    expect(text("agents-roster-sub-reviewer")).toContain("↳ reviewer");
    expect(text("agents-roster-sub-reviewer")).toContain("inherits sonnet");
    expect(text("agents-roster-sub-tester")).toContain("↳ tester");
    expect(text("agents-roster-sub-tester")).toContain("haiku");
    expect(text("agents-roster-sub-tester")).not.toContain("inherits");
    expect(has("agents-roster-empty")).toBe(false);
  });

  it("single_agent_roster_says_no_delegate_tool", () => {
    mount(<AgentsChip profile="peakbot-dev" agent={SOLO} activeModel="sonnet" />);
    click(q("agents-chip"));
    expect(text("agents-roster-empty")).toContain("Single agent — no delegate tool.");
  });

  it("esc_closes_roster_and_marks_event_handled", () => {
    mount(<AgentsChip profile="peakbot-dev" agent={TEAM} activeModel="sonnet" />);
    click(q("agents-chip"));
    const ev = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(ev);
    });
    expect(has("agents-popover")).toBe(false);
    expect(ev.defaultPrevented).toBe(true);
  });
});

// ─── TopBar / BottomBar wiring ──────────────────────────────────────────

function barBase() {
  return {
    isRunning: false,
    connected: true,
    statusMessage: null,
    models: [],
    activeAlias: "sonnet",
    hasTranscript: false,
    cwd: null,
    dirListing: null,
    recentDirs: [],
    conversations: [],
    conversationsError: null,
    send: vi.fn<(m: InboundMessage) => void>(),
    onSwitchModel: vi.fn(),
    onLoadConversation: vi.fn(),
    notifyEnabled: false,
    notifyPermission: "default" as const,
    onToggleNotify: vi.fn(),
    focusMode: false,
    onToggleFocus: vi.fn(),
  };
}

// New optional props; spread so the not-yet-existing fields type-check loosely.
const chatChips = {
  catalog: CATALOG,
  selection: SEL,
  profilePinned: false,
  onToast: vi.fn(),
};

describe("TopBar / BottomBar chips (T32)", () => {
  it("topbar_renders_selection_chip_before_agents_chip", () => {
    const base = barBase();
    const props = { ...base, ...chatChips } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    const sel = q("selection-chip");
    const agents = q("agents-chip");
    expect(text("selection-chip")).toContain("peakbot-dev › team");
    expect(text("agents-chip")).toContain("Agents · 3");
    expect(sel.compareDocumentPosition(agents) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("topbar_chip_locks_once_the_chat_has_a_transcript", () => {
    const props = { ...barBase(), ...chatChips, hasTranscript: true } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    expect(text("selection-chip")).toContain("🔒");
    expect(q<HTMLButtonElement>("selection-chip").disabled).toBe(true);
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

  it("topbar_has_no_chips_without_catalog_and_selection", () => {
    mount(<TopBar {...barBase()} />);
    expect(has("selection-chip")).toBe(false);
    expect(has("agents-chip")).toBe(false);
  });

  it("topbar_agents_chip_uses_the_active_model_alias", () => {
    const props = { ...barBase(), ...chatChips, activeAlias: "opus" } as ComponentProps<typeof TopBar>;
    mount(<TopBar {...props} />);
    click(q("agents-chip"));
    expect(text("agents-roster-main")).toContain("opus");
  });

  it("bottombar_renders_selection_and_agents_chips", () => {
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
      onSwitchModel: b.onSwitchModel,
      onLoadConversation: b.onLoadConversation,
      ...chatChips,
    } as ComponentProps<typeof BottomBar>;
    mount(<BottomBar {...props} />);
    expect(text("selection-chip")).toContain("peakbot-dev › team");
    expect(has("agents-chip")).toBe(true);
    click(q("selection-chip"));
    click(q("selection-agent-review"));
    expect(b.send).toHaveBeenCalledWith({
      type: "select",
      profile: "peakbot-dev",
      agent: "review",
    });
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
