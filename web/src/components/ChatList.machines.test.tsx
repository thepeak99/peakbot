// Tests for `components/ChatList.tsx` — the machine selector (mobile
// redesign design §2.4, §2.7, M1–M14; scope C16–C22 as amended by Z2/Z3).
//
// The machine selector is a first-class concept in the Chats screen:
//   * a centred machine pill (shown only when ≥ 2 machines exist) with an
//     online dot; tapping it opens the Machine sheet (All machines + one
//     row per machine: dot, name, local/remote/offline sub, count, ✓);
//   * the choice persists in localStorage `peakbot.mobile.machine` and
//     survives a remount; a stale id (machine removed) resolves to All;
//   * rows and profile chips are filtered to the selected machine; in All
//     every row carries a machine tag (local rows: "This device"); the row
//     "remote" badge is gone (the tag replaces it);
//   * an offline machine: grey dot, "offline" sub, selecting it shows the
//     empty state "‹name› is offline", the FAB is disabled;
//   * the FAB starts a new chat on the selected machine's first catalog
//     profile; on All it opens the "New chat on…" picker (offline machines
//     disabled); picking one calls onNew(profile) and closes the picker;
//   * the picker and the Machine sheet are OS Back layers: a popstate closes
//     the sheet and the list stays.
//
// The pure helpers (deriveMachines, filterByMachine, resolveFilter, …) are
// unit-tested in chatList.test.ts (P1); this file drives the component
// through its public props and the §2.7 testid/aria contract.
//
// RED before implementation: `machineStatus` / the machine testids do not
// exist yet, so these fail on the missing elements (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatList } from "./ChatList";
import { MACHINE_KEY } from "../mobileStorage";
import type { ConversationSummary, WireMachineStatus } from "../state";
import type { CatalogProfile } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixtures ─────────────────────────────────────────────────────────────

const LOCAL = "dev";
const HS = "https://home-server.local:7823";
const WB = "https://10.0.0.5:7823";

const CATALOG: CatalogProfile[] = [
  { name: LOCAL, origin: "global", overridesGlobal: false, machine: null, agents: [], color: "teal" },
  { name: "home-dev", origin: "remote", overridesGlobal: false, machine: HS, agents: [], color: "purple" },
  { name: "work", origin: "remote", overridesGlobal: false, machine: WB, agents: [], color: "blue" },
];

const CATALOG_ONE_MACHINE: CatalogProfile[] = [
  { name: LOCAL, origin: "global", overridesGlobal: false, machine: null, agents: [], color: "teal" },
  { name: "dev2", origin: "global", overridesGlobal: false, machine: null, agents: [], color: "teal" },
];

const STATUS_UP: WireMachineStatus[] = [
  { id: HS, online: true },
  { id: WB, online: true },
];

const STATUS_WB_DOWN: WireMachineStatus[] = [
  { id: HS, online: true },
  { id: WB, online: false },
];

const row = (id: string, profile: string | null): ConversationSummary =>
  ({
    id,
    name: `convo ${id}`,
    updated_at: "2026-11-10T12:00:00.000Z",
    message_count: 3,
    model: "sonnet",
    active: false,
    profile,
  }) as ConversationSummary;

const CONVOS: ConversationSummary[] = [
  row("c-local-1", LOCAL),
  row("c-local-2", LOCAL),
  row("c-hs", "home-dev"),
  row("c-wb", "work"),
];

// ─── harness ──────────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

interface ListProps {
  conversations: ConversationSummary[];
  catalog: CatalogProfile[];
  loaded: boolean;
  unread: ReadonlySet<string>;
  connected: boolean;
  error: string | null;
  onOpen(convo: string, profile: string | null): void;
  onNew(profile: string | null): void;
  onOpenSettings?: () => void;
  machineStatus: WireMachineStatus[];
}

const baseProps: ListProps = {
  conversations: CONVOS,
  catalog: CATALOG,
  loaded: true,
  unread: new Set<string>(),
  connected: true,
  error: null,
  onOpen: () => {},
  onNew: () => {},
  machineStatus: STATUS_UP,
};

async function render(over: Partial<ListProps> = {}): Promise<HTMLDivElement> {
  if (container === null) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
    root!.render(<ChatList {...baseProps} {...over} />);
  });
  return container!;
}

beforeEach(() => {
  window.localStorage.clear();
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  window.localStorage.clear();
  vi.restoreAllMocks();
  vi.useRealTimers();
  window.history.replaceState(null, "", "/");
});

// ─── helpers ──────────────────────────────────────────────────────────────

const pill = (el: HTMLElement): HTMLElement | null =>
  el.querySelector<HTMLElement>('[data-testid="machine-pill"]');

const machineDialog = (): HTMLElement | null =>
  document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="Machine"]');

const option = (scope: ParentNode, machine: string): HTMLElement => {
  const opt = scope.querySelector<HTMLElement>(
    `[data-testid="machine-option"][data-machine="${machine}"]`,
  );
  expect(opt, `machine option ${machine} must render`).not.toBeNull();
  return opt!;
};

async function openMachineSheet(el: HTMLElement): Promise<void> {
  const p = pill(el);
  expect(p, "the machine pill renders").not.toBeNull();
  await act(async () => {
    p!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

async function pressMachine(scope: ParentNode, machine: string): Promise<void> {
  await act(async () => {
    option(scope, machine).dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

const tag = (el: HTMLElement, id: string): HTMLElement | null =>
  el.querySelector<HTMLElement>(
    `[data-testid="chat-row"][data-convo="${id}"] [data-testid="chat-machine-tag"]`,
  );

const rowEl = (el: HTMLElement, id: string): HTMLElement => {
  const r = el.querySelector<HTMLElement>(`[data-testid="chat-row"][data-convo="${id}"]`);
  expect(r, `row ${id} must render`).not.toBeNull();
  return r!;
};

// ─── M1: single-machine installs see nothing new ─────────────────────────

describe("ChatList machines — M1: one machine ⇒ no pill, no tags", () => {
  it("no pill, no row tags, unchanged header, and the FAB calls onNew(null)", async () => {
    const onNew = vi.fn();
    const el = await render({
      catalog: CATALOG_ONE_MACHINE,
      machineStatus: [],
      conversations: [row("c1", LOCAL), row("c2", "dev2")],
      onNew,
    });
    expect(pill(el), "no pill with one machine").toBeNull();
    expect(tag(el, "c1"), "no tags in the single-machine path").toBeNull();
    expect(el.querySelector("h1")!.textContent, "the header title is unchanged").toBe("Chats");
    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onNew).toHaveBeenCalledTimes(1);
    // Boolean compare: a forwarded click event must not be pretty-printed
    // (its DOM/fiber graph OOMs the worker).
    expect(onNew.mock.calls[0]?.[0] === null, "onNew(null) on the single-machine path").toBe(true);
  });
});

// ─── M2: the pill and the 3-column header ─────────────────────────────────

describe("ChatList machines — M2: the centred pill", () => {
  it("three machines: a 'All machines' pill in the middle of a 1fr_auto_1fr grid", async () => {
    const el = await render();
    const p = pill(el);
    expect(p, "the pill renders with ≥ 2 machines").not.toBeNull();
    expect(p!.getAttribute("aria-label")).toBe("Choose machine");
    expect(p!.textContent).toContain("All machines");
    const grid = p!.parentElement!;
    expect(grid.className, "the header row is a 3-column grid").toContain("grid-cols-[1fr_auto_1fr]");
    expect(Array.from(grid.children).indexOf(p!), "the pill is the middle cell").toBe(1);
  });

  it("the pill dot is green while every machine is online", async () => {
    const el = await render();
    const dot = pill(el)!.querySelector<HTMLElement>('[data-testid="machine-pill-dot"]');
    expect(dot, "the dot renders").not.toBeNull();
    expect(dot!.getAttribute("data-online")).toBe("true");
  });
});

// ─── M3: the Machine sheet ────────────────────────────────────────────────

describe("ChatList machines — M3: the Machine sheet", () => {
  it("opens a dialog titled Machine with All + one option per machine", async () => {
    const el = await render();
    await openMachineSheet(el);
    const dlg = machineDialog();
    expect(dlg, "the Machine dialog opens").not.toBeNull();

    // All machines: total count, pressed, sub "local + remote".
    const all = option(dlg!, "all");
    expect(all.getAttribute("aria-pressed")).toBe("true");
    expect(all.querySelector<HTMLElement>('[data-testid="machine-option-count"]')!.textContent).toBe("4");
    expect(all.querySelector<HTMLElement>('[data-testid="machine-option-sub"]')!.textContent).toBe(
      "local + remote",
    );

    // Local: "This device", sub "local", its 2 rows.
    const local = option(dlg!, "local");
    expect(local.textContent).toContain("This device");
    expect(local.getAttribute("aria-pressed")).toBe("false");
    expect(local.querySelector<HTMLElement>('[data-testid="machine-option-sub"]')!.textContent).toBe("local");
    expect(local.querySelector<HTMLElement>('[data-testid="machine-option-count"]')!.textContent).toBe("2");

    // Each remote: its host name (2.20: without the port), sub "remote",
    // its rows.
    const hs = option(dlg!, HS);
    expect(hs.textContent).toContain("home-server.local");
    expect(hs.textContent).not.toContain(":7823");
    expect(hs.querySelector<HTMLElement>('[data-testid="machine-option-sub"]')!.textContent).toBe("remote");
    expect(hs.querySelector<HTMLElement>('[data-testid="machine-option-count"]')!.textContent).toBe("1");
    const wb = option(dlg!, WB);
    expect(wb.textContent).toContain("10.0.0.5");
    expect(wb.textContent).not.toContain(":7823");
    expect(wb.querySelector<HTMLElement>('[data-testid="machine-option-count"]')!.textContent).toBe("1");
  });
});

// ─── M4: selecting a machine ──────────────────────────────────────────────

describe("ChatList machines — M4: selecting a machine", () => {
  it("filters rows and chips, drops the tags, renames the pill and persists", async () => {
    const el = await render();
    await openMachineSheet(el);
    await pressMachine(machineDialog()!, HS);

    // Rows filtered to the machine…
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c-hs"]'), "its row stays").not.toBeNull();
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c-local-1"]')).toBeNull();
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c-wb"]')).toBeNull();
    // …and the tags are gone in single-machine mode.
    expect(tag(el, "c-hs"), "no tags when one machine is selected").toBeNull();

    // Chips filtered to the machine's profiles.
    const chips = Array.from(el.querySelectorAll('button[data-testid="chat-filter"]')).map((c) =>
      c.getAttribute("data-profile"),
    );
    expect(chips, "chips are All + the machine's profiles").toEqual(["", "home-dev"]);

    // The pill shows the machine name (2.20: host without the port).
    expect(pill(el)!.textContent).toContain("home-server.local");
    expect(pill(el)!.textContent).not.toContain(":7823");

    // The choice is persisted (the v1 shape from design §2.3).
    expect(JSON.parse(window.localStorage.getItem(MACHINE_KEY) ?? "null")).toEqual({
      v: 1,
      kind: "one",
      id: HS,
    });
  });
});

// ─── M5: persistence survives a remount (C18) ─────────────────────────────

describe("ChatList machines — M5: the choice survives a remount", () => {
  it("a fresh mount reads the stored machine", async () => {
    window.localStorage.setItem(MACHINE_KEY, JSON.stringify({ v: 1, kind: "one", id: WB }));
    const el = await render();
    expect(pill(el)!.textContent).toContain("10.0.0.5");
    expect(pill(el)!.textContent).not.toContain(":7823");
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c-wb"]'), "its row shows").not.toBeNull();
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c-local-1"]')).toBeNull();
  });
});

// ─── M6: a chip on another machine resets to All (C19) ────────────────────

describe("ChatList machines — M6: a chip on another machine resets to All", () => {
  it("a selected chip the machine no longer offers falls back to All (C19)", async () => {
    const el = await render();
    // In All mode, pick the `home-dev` chip (it is on the HS machine).
    const homeChip = el.querySelector<HTMLElement>(
      'button[data-testid="chat-filter"][data-profile="home-dev"]',
    )!;
    await act(async () => {
      homeChip.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(homeChip.getAttribute("aria-pressed")).toBe("true");

    // Now select the WB machine: the `home-dev` chip is no longer on it.
    await openMachineSheet(el);
    await pressMachine(machineDialog()!, WB);

    expect(
      el.querySelector<HTMLElement>('button[data-testid="chat-filter"][data-profile="home-dev"]'),
      "the foreign chip is filtered out",
    ).toBeNull();
    const allChip = el.querySelector<HTMLElement>('button[data-testid="chat-filter"][data-profile=""]');
    expect(allChip!.getAttribute("aria-pressed"), "the chip filter resets to All").toBe("true");
    expect(pill(el)!.textContent, "the machine selection is kept").toContain("10.0.0.5");
    expect(pill(el)!.textContent).not.toContain(":7823");
  });
});

// ─── M7: All tags every row (C20) ─────────────────────────────────────────

describe("ChatList machines — M7: All tags every row", () => {
  it("every row carries a chat-machine-tag; local rows read 'This device'", async () => {
    const el = await render();
    for (const id of ["c-local-1", "c-local-2", "c-hs", "c-wb"]) {
      const t = tag(el, id);
      expect(t, `row ${id} has a tag in All mode`).not.toBeNull();
    }
    // Decision 4 (A38): the local machine's label is "This device".
    expect(tag(el, "c-local-1")!.textContent).toBe("This device");
    // 2.20: remote tags are the host without the port.
    expect(tag(el, "c-hs")!.textContent).toBe("home-server.local");
    expect(tag(el, "c-wb")!.textContent).toBe("10.0.0.5");
  });
});

// ─── M8: an offline machine (C21 as amended) ──────────────────────────────

describe("ChatList machines — M8: an offline machine", () => {
  it("grey dot, 'offline' sub, the offline empty state, and a disabled FAB", async () => {
    const el = await render({ machineStatus: STATUS_WB_DOWN });
    await openMachineSheet(el);
    const wb = option(machineDialog()!, WB);
    expect(wb.querySelector<HTMLElement>('[data-testid="machine-option-sub"]')!.textContent).toBe("offline");

    await pressMachine(machineDialog()!, WB);
    expect(pill(el)!.textContent).toContain("10.0.0.5");
    expect(pill(el)!.textContent).not.toContain(":7823");
    expect(
      pill(el)!.querySelector<HTMLElement>('[data-testid="machine-pill-dot"]')!.getAttribute("data-online"),
      "the pill dot greys for the selected offline machine",
    ).toBe("false");

    // Its rows are already absent (the server drops them): the empty state.
    const empty = el.querySelector<HTMLElement>('[data-testid="chat-empty"]');
    expect(empty, "the offline empty state renders").not.toBeNull();
    expect(empty!.textContent).toBe("10.0.0.5 is offline");

    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    expect(fab.hasAttribute("disabled"), "the FAB is disabled").toBe(true);
  });
});

// ─── M9: the FAB on a selected machine (C22) ──────────────────────────────

describe("ChatList machines — M9: the FAB on a selected machine", () => {
  it("starts a new chat on the machine's first catalog profile", async () => {
    const onNew = vi.fn();
    const el = await render({ onNew });
    await openMachineSheet(el);
    await pressMachine(machineDialog()!, WB);
    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onNew).toHaveBeenCalledTimes(1);
    // String compare (never pretty-print a forwarded click event: OOM).
    expect(String(onNew.mock.calls[0]?.[0]), "first catalog profile on WB").toBe("work");
  });
});

// ─── M10: the FAB on All opens the picker ─────────────────────────────────

describe("ChatList machines — M10: the FAB on All", () => {
  it("opens 'New chat on…' with one option per machine; offline is disabled", async () => {
    const el = await render({ machineStatus: STATUS_WB_DOWN });
    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    const dlg = document.body.querySelector<HTMLElement>(
      '[role="dialog"][aria-label="New chat on…"]',
    );
    expect(dlg, "the New chat on… picker opens").not.toBeNull();
    const local = dlg!.querySelector<HTMLElement>(
      '[data-testid="newchat-option"][data-machine="local"]',
    );
    expect(local, "the local option renders").not.toBeNull();
    expect(local!.hasAttribute("disabled")).toBe(false);
    const wb = dlg!.querySelector<HTMLElement>(`[data-testid="newchat-option"][data-machine="${WB}"]`);
    expect(wb, "the offline remote option renders").not.toBeNull();
    expect(wb!.hasAttribute("disabled"), "the offline machine is disabled").toBe(true);
  });
});

// ─── M11: a picker pick ───────────────────────────────────────────────────

describe("ChatList machines — M11: a picker pick", () => {
  it("calls onNew(profile) and closes the dialog", async () => {
    const onNew = vi.fn();
    const el = await render({ onNew });
    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    const dlg = document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="New chat on…"]')!;
    const hs = dlg.querySelector<HTMLElement>(`[data-testid="newchat-option"][data-machine="${HS}"]`)!;
    await act(async () => {
      hs.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    expect(onNew).toHaveBeenCalledTimes(1);
    expect(String(onNew.mock.calls[0]?.[0]), "first catalog profile on HS").toBe("home-dev");
    expect(
      document.body.querySelector('[role="dialog"][aria-label="New chat on…"]'),
      "the picker is gone",
    ).toBeNull();
  });
});

// ─── M12: a stale stored machine resolves to All ──────────────────────────

describe("ChatList machines — M12: a stale stored machine", () => {
  it("a stored id no longer in the catalog is treated as All", async () => {
    window.localStorage.setItem(
      MACHINE_KEY,
      JSON.stringify({ v: 1, kind: "one", id: "https://gone.example:7823" }),
    );
    const el = await render();
    expect(pill(el)!.textContent, "the pill reads All").toContain("All machines");
    expect(tag(el, "c-local-1"), "tags come back (All mode)").not.toBeNull();
  });
});

// ─── M13: OS Back on the Machine sheet ────────────────────────────────────

describe("ChatList machines — M13: OS Back closes the sheet", () => {
  it("a popstate closes the Machine sheet and the list stays", async () => {
    const el = await render();
    await openMachineSheet(el);
    expect(machineDialog(), "the sheet is open").not.toBeNull();

    const popped = new Promise<void>((resolve) => {
      window.addEventListener("popstate", () => resolve(), { once: true });
    });
    act(() => {
      window.history.back();
    });
    await act(async () => {
      await popped;
    });
    expect(machineDialog(), "the sheet is closed").toBeNull();
    expect(el.querySelector('[data-testid="chat-list"]'), "the list stays").not.toBeNull();
  });
});

// ─── M14: the row 'remote' badge is gone ──────────────────────────────────

describe("ChatList machines — M14: no row 'remote' badge", () => {
  it("rows no longer show a 'remote' badge (the machine tag replaces it)", async () => {
    const el = await render();
    for (const id of ["c-local-1", "c-local-2", "c-hs", "c-wb"]) {
      const rowEl = el.querySelector<HTMLElement>(`[data-testid="chat-row"][data-convo="${id}"]`)!;
      const badges = Array.from(rowEl.querySelectorAll("span")).filter(
        (s) => (s.textContent ?? "").trim() === "remote",
      );
      expect(badges, `row ${id} has no remote badge`).toEqual([]);
    }
  });
});

// ─── B2: the batch-2 (fidelity) contracts ──────────────────────────────────

describe("ChatList machines — B2: the row's second line (A20/A21)", () => {
  it("the second line reads profile, then the machine tag, then the time", async () => {
    // Pin the clock: the row's updated_at is 7h before ⇒ "7h ago" in any TZ.
    const now = new Date(2026, 10, 10, 19, 0, 0);
    vi.useFakeTimers();
    vi.setSystemTime(now);
    const convo = {
      ...row("c-hs", "home-dev"),
      updated_at: new Date(now.getTime() - 7 * 3_600_000).toISOString(),
    };
    const el = await render({ conversations: [convo] });
    const meta = rowEl(el, "c-hs").querySelector("button")!.lastElementChild as HTMLElement;
    const parts = [...meta.children].map((c) => c.textContent);
    // profile · [machine tag] · time — the tag moves into the second line
    // (A20), the line is kept (A21). 2.20: the tag is the bare host.
    expect(parts).toEqual(["home-dev", "home-server.local", "\u00A0·\u00A07h ago"]);
    // The old "N msgs · model" meta is dropped (§9 Q2).
    expect(meta.textContent).not.toContain("msgs");
    expect(meta.textContent).not.toContain("sonnet");
  });
});

describe("ChatList machines — B2: the All machines pill dot (A8)", () => {
  it("the All machines pill dot is online while a machine is offline", async () => {
    const el = await render({ machineStatus: STATUS_WB_DOWN });
    const dot = pill(el)!.querySelector<HTMLElement>('[data-testid="machine-pill-dot"]');
    expect(dot, "the pill dot renders").not.toBeNull();
    expect(dot!.getAttribute("data-online"), "All stays emerald (proto:1140)").toBe("true");
  });
});

describe("ChatList machines — B2: the online empty state (A37)", () => {
  it("an online machine with no chats shows 'No chats on ‹name› yet'", async () => {
    const el = await render({ conversations: [row("c-local-1", LOCAL)] });
    await openMachineSheet(el);
    await pressMachine(machineDialog()!, HS);
    const empty = el.querySelector<HTMLElement>('[data-testid="chat-empty"]');
    expect(empty, "the empty state renders").not.toBeNull();
    expect(empty!.textContent).toBe("No chats on home-server.local yet");
  });
});

// ─── B5: the batch-5 (fidelity) contracts ──────────────────────────────────

describe("ChatList machines — B5: the Machine sheet option (A35)", () => {
  it("an offline machine option in the Machine sheet is disabled", async () => {
    const el = await render({ machineStatus: STATUS_WB_DOWN });
    await openMachineSheet(el);
    const dlg = machineDialog()!;
    expect(option(dlg, WB).hasAttribute("disabled"), "the offline machine is untappable").toBe(true);
    expect(option(dlg, "all").hasAttribute("disabled"), "the All option is always enabled").toBe(false);
    expect(option(dlg, "local").hasAttribute("disabled"), "the local option stays enabled").toBe(false);
  });
});

describe("ChatList machines — B5: the picker rows (A34)", () => {
  it("picker rows show the sub under the name", async () => {
    const el = await render();
    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
    const dlg = document.body.querySelector<HTMLElement>(
      '[role="dialog"][aria-label="New chat on…"]',
    );
    expect(dlg, "the picker opens").not.toBeNull();
    for (const [machine, name, sub] of [
      ["local", "This device", "local"],
      [HS, "home-server.local", "remote"],
    ] as const) {
      const opt = dlg!.querySelector<HTMLElement>(
        `[data-testid="newchat-option"][data-machine="${machine}"]`,
      )!;
      // Name and sub are the option's only text-bearing leaf spans.
      const leaves = [...opt.querySelectorAll("span")].filter(
        (s) => s.textContent?.trim() && !s.querySelector("span"),
      );
      const nameEl = leaves.find((s) => s.textContent?.trim() === name);
      const subEl = leaves.find((s) => s.textContent?.trim() === sub);
      expect(nameEl, `the ${machine} option shows its name`).not.toBeUndefined();
      expect(subEl, `the ${machine} option shows its sub`).not.toBeUndefined();
      expect(
        nameEl!.compareDocumentPosition(subEl!) & Node.DOCUMENT_POSITION_FOLLOWING,
        `the ${machine} sub follows the name in the row`,
      ).toBeTruthy();
      expect(
        subEl!.parentElement === opt,
        `the ${machine} sub is not a trailing sibling of the row — it sits in the name column`,
      ).toBe(false);
      expect(
        nameEl!.parentElement === subEl!.parentElement,
        `the ${machine} sub sits in the same column as the name, under it`,
      ).toBe(true);
    }
  });
});

describe("ChatList machines — B5: the light online dot (A42)", () => {
  it("the online machine dot carries the light-theme green", async () => {
    const el = await render();
    const dot = pill(el)!.querySelector<HTMLElement>('[data-testid="machine-pill-dot"]')!;
    expect(dot.className, "the dark dot").toContain("bg-emerald-400");
    expect(dot.className, "the light theme paints the dot #16a34a").toContain("light:bg-[#16a34a]");
  });
});
