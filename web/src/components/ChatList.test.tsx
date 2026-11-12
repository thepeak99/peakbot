// Tests for `components/ChatList.tsx` — the mobile Chats screen (design
// §4.1, L1–L9). Selectors and aria names are the §4.1 target table, verbatim.
//
// Harness modeled on BottomBar.test.tsx (raw createRoot + act,
// querySelector). The root is kept across renders so the component's internal
// state (search query, selected chip) survives a prop re-render.
//
// RED before implementation: `./ChatList` does not exist yet, so this file
// fails at import — the right reason (feature absent).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ChatList } from "./ChatList";
import type { ConversationSummary, WireMachineStatus } from "../state";
import type { CatalogProfile } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixtures ──────────────────────────────────────────────────────────────

const CATALOG: CatalogProfile[] = [
  { name: "home", origin: "global", overridesGlobal: false, machine: null, agents: [], color: "teal" },
  { name: "dev", origin: "remote", overridesGlobal: false, machine: null, agents: [], color: "orange" },
];

const CONVOS: ConversationSummary[] = [
  {
    id: "c1",
    name: "Debug WS relay",
    updated_at: "2026-11-10T18:00:00.000Z",
    message_count: 3,
    model: "sonnet",
    active: true,
    profile: "home",
  },
  {
    id: "c2",
    name: "Tune WS keepalive",
    updated_at: "2026-11-10T17:00:00.000Z",
    message_count: 5,
    model: "sonnet",
    active: false,
    running: true,
    profile: "dev",
  },
];

const NO_COLOR: ConversationSummary[] = [
  {
    id: "c3",
    name: "Colourless chat",
    updated_at: "2026-11-10T16:00:00.000Z",
    message_count: 1,
    model: "sonnet",
    active: false,
    profile: null,
  },
];

// Mirrors design §4.1 (PR1 props) plus the mobile-redesign `machineStatus` /
// `onNew(profile)` (design §2.5), so overrides type-check. The CATALOG below
// is a SINGLE machine (this device — a remote entry with `machine: null` is
// the legacy shape), so the machine UI stays out of this file's way; the
// machine behaviour lives in ChatList.machines.test.tsx.
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
  machineStatus: [],
};

// ─── harness ───────────────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(over: Partial<ListProps> = {}): Promise<HTMLDivElement> {
  if (!root) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
    root!.render(<ChatList {...baseProps} {...over} />);
  });
  return container!;
}

/** Set a controlled input's value the way React's tracker expects. */
async function type(el: HTMLElement, selector: string, value: string): Promise<void> {
  const input = el.querySelector<HTMLInputElement>(selector)!;
  const setNative = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  await act(async () => {
    setNative.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.restoreAllMocks();
});

const kinds = (el: HTMLElement): (string | null)[] =>
  [...el.querySelectorAll('[data-testid="chat-section"]')].map((s) => s.getAttribute("data-kind"));

const row = (el: HTMLElement, id: string): HTMLElement =>
  el.querySelector<HTMLElement>(`[data-testid="chat-row"][data-convo="${id}"]`)!;

// The row's title span: the span whose full text is the chat name AND which
// is the title itself (carries the structural `min-w-0` class) — not the
// outer flex span that wraps title + badges + markers with the same text.
const titleOf = (rowEl: HTMLElement, name: string): HTMLElement => {
  const span = [...rowEl.querySelectorAll("span")].find(
    (s) => s.textContent === name && s.classList.contains("min-w-0"),
  );
  expect(span, `a title span for "${name}" must render`).not.toBeUndefined();
  return span!;
};

// ─── root / header / title / logo ──────────────────────────────────────────

describe("ChatList — root and header", () => {
  it("renders the chat-list section with the Chats title and logo", async () => {
    const el = await render();
    // Design §4.1 lists both on one row; read as one <section> carrying both.
    const list = el.querySelector('section[aria-label="Chats"][data-testid="chat-list"]');
    expect(list, "the root must render").not.toBeNull();
    expect(el.querySelector("h1")!.textContent).toBe("Chats");
    expect(el.querySelector('img[src="/logo_shifu.png"]')).not.toBeNull();
  });

  it("with a single machine the header is unchanged: no machine pill", async () => {
    const el = await render();
    expect(el.querySelector('[data-testid="machine-pill"]')).toBeNull();
  });
});

// ─── L1–L9 ─────────────────────────────────────────────────────────────────

describe("ChatList — L1: sections and rows", () => {
  it("rows render under sections in §3.1 order with title, meta line and colour bar", async () => {
    const el = await render();
    expect(kinds(el)).toEqual(["running", "live"]);

    const c2 = row(el, "c2");
    expect(c2.textContent).toContain("Tune WS keepalive");
    expect(c2.textContent).toContain("5 msgs");
    expect(c2.textContent).toContain("sonnet");
    const c2bar = c2.querySelector('[data-testid="chat-bar"]');
    expect(c2bar, "a coloured profile gets a bar").not.toBeNull();
    expect(c2bar!.getAttribute("style")).toContain("var(--profile-orange)");

    const c1 = row(el, "c1");
    expect(c1.textContent).toContain("Debug WS relay");
    expect(c1.textContent).toContain("3 msgs");
    const c1bar = c1.querySelector('[data-testid="chat-bar"]');
    expect(c1bar!.getAttribute("style")).toContain("var(--profile-teal)");
  });

  it("rows carry no 'remote' badge (Z8: the machine tag replaces it); the chip keeps its own", async () => {
    const el = await render();
    for (const id of ["c1", "c2"]) {
      const badges = [...row(el, id).querySelectorAll("span")].filter(
        (s) => (s.textContent ?? "").trim() === "remote",
      );
      expect(badges, `row ${id} has no remote badge`).toEqual([]);
    }
    const devChip = el.querySelector('button[data-testid="chat-filter"][data-profile="dev"]')!;
    expect(devChip.textContent, "chips keep the remote badge").toContain("remote");
  });


  it("a row whose profile has no catalog colour has no bar", async () => {
    const el = await render({ conversations: NO_COLOR });
    expect(row(el, "c3").querySelector('[data-testid="chat-bar"]')).toBeNull();
  });
});

describe("ChatList — L2: status markers", () => {
  it("running rows show only the running marker; live rows show live", async () => {
    const el = await render({ unread: new Set<string>() });
    const c2 = row(el, "c2");
    const running = c2.querySelector('[data-testid="chat-status-running"]');
    expect(running).not.toBeNull();
    expect(running!.getAttribute("aria-label")).toBe("running");
    // The amber spinner is the running marker (kept, same mobile classes).
    for (const cls of [
      "h-3.5",
      "w-3.5",
      "animate-spin",
      "rounded-full",
      "border-2",
      "border-amber-400",
      "border-t-transparent",
    ]) {
      expect(running!.classList.contains(cls), `spinner class ${cls}`).toBe(true);
    }
    expect(c2.querySelector('[data-testid="chat-status-live"]'), "running is not also live").toBeNull();

    const c1 = row(el, "c1");
    const live = c1.querySelector('[data-testid="chat-status-live"]');
    expect(live).not.toBeNull();
    expect(live!.getAttribute("aria-label")).toBe("live session");
    // The live marker is visually hidden, not a visible dot.
    expect(live!.classList.contains("sr-only"), "the live marker must be sr-only").toBe(true);
    expect(live!.classList.contains("bg-emerald-400"), "no visible green dot").toBe(false);
    expect(c1.querySelector('[data-testid="chat-status-running"]')).toBeNull();
  });

  it("an unread id shows the unread marker, coexisting with live", async () => {
    const el = await render({ unread: new Set(["c1"]) });
    const c1 = row(el, "c1");
    const live = c1.querySelector('[data-testid="chat-status-live"]');
    expect(live).not.toBeNull();
    expect(live!.classList.contains("sr-only"), "the live marker must be sr-only").toBe(true);
    const unread = c1.querySelector('[data-testid="chat-status-unread"]');
    expect(unread).not.toBeNull();
    expect(unread!.getAttribute("aria-label")).toBe("unread");
    // The sky dot stays the only visible dot in the row.
    expect(unread!.classList.contains("bg-sky-400")).toBe(true);
    expect(c1.querySelector(".bg-emerald-400"), "no visible green dot").toBeNull();
  });

  it("running rows get the amber tint on the wrapper and an amber semibold title", async () => {
    const el = await render();
    const c2 = row(el, "c2");
    expect(c2.classList.contains("bg-amber-950/40"), "the running wrapper tint").toBe(true);
    const title = titleOf(c2, "Tune WS keepalive");
    expect(title.classList.contains("font-semibold")).toBe(true);
    expect(title.classList.contains("text-amber-200")).toBe(true);
    // A running row is not emerald, even though a running chat is also active.
    expect(c2.classList.contains("bg-emerald-950/40")).toBe(false);
    expect(title.classList.contains("text-emerald-200")).toBe(false);
  });

  it("live rows keep the emerald tint and title, with the green dot removed", async () => {
    const el = await render();
    const c1 = row(el, "c1");
    expect(c1.classList.contains("bg-emerald-950/40"), "the live wrapper tint").toBe(true);
    const title = titleOf(c1, "Debug WS relay");
    expect(title.classList.contains("font-semibold")).toBe(true);
    expect(title.classList.contains("text-emerald-200")).toBe(true);
    // No visible green dot: no element in the row carries bg-emerald-400.
    expect(c1.querySelector(".bg-emerald-400"), "no visible green dot").toBeNull();
  });

  it("plain rows have no tint, a medium title and no markers", async () => {
    const el = await render({ conversations: NO_COLOR });
    const c3 = row(el, "c3");
    expect(c3.className).not.toContain("bg-amber-950/40");
    expect(c3.className).not.toContain("bg-emerald-950/40");
    const title = titleOf(c3, "Colourless chat");
    expect(title.classList.contains("font-medium")).toBe(true);
    expect(c3.querySelector('[data-testid="chat-status-running"]')).toBeNull();
    expect(c3.querySelector('[data-testid="chat-status-live"]')).toBeNull();
    expect(c3.querySelector('[data-testid="chat-status-unread"]')).toBeNull();
  });
});

describe("ChatList — L3: profile chips", () => {
  it("chips are All + each catalog profile in order; All is pressed initially", async () => {
    const el = await render();
    const chips = el.querySelectorAll('button[data-testid="chat-filter"]');
    expect([...chips].map((c) => c.getAttribute("data-profile"))).toEqual(["", "home", "dev"]);
    expect(chips[0].getAttribute("aria-pressed")).toBe("true");
    expect(chips[1].getAttribute("aria-pressed")).toBe("false");
    expect(chips[2].textContent).toContain("remote");
  });

  it("pressing a chip filters the rows and carries the emerald border", async () => {
    const el = await render();
    const devChip = el.querySelector<HTMLElement>('button[data-testid="chat-filter"][data-profile="dev"]')!;
    await act(async () => {
      devChip.click();
    });
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c2"]')).not.toBeNull();
    expect(el.querySelector('[data-testid="chat-row"][data-convo="c1"]'), "home row filtered out").toBeNull();
    expect(devChip.classList.contains("border-emerald-700")).toBe(true);
  });

  it("a selected profile that leaves the catalog falls back to All", async () => {
    const el = await render();
    const devChip = el.querySelector<HTMLElement>('button[data-testid="chat-filter"][data-profile="dev"]')!;
    await act(async () => {
      devChip.click();
    });
    // Re-render (same instance) with a catalog that no longer has dev.
    await render({ catalog: CATALOG.filter((p) => p.name !== "dev") });
    const allChip = el.querySelector('button[data-testid="chat-filter"][data-profile=""]');
    expect(allChip!.getAttribute("aria-pressed")).toBe("true");
  });
});

describe("ChatList — L4: search", () => {
  it("typing collapses to a matches section with highlights; Clear restores", async () => {
    const el = await render();
    expect(el.querySelector('button[aria-label="Clear search"]'), "no clear button when empty").toBeNull();

    await type(el, 'input[aria-label="Search chats"]', "ws");

    expect(kinds(el)).toEqual(["matches"]);
    const section = el.querySelector('[data-testid="chat-section"]')!;
    expect(section.textContent).toContain("2 MATCHES");
    expect(el.querySelector("mark"), "matches are highlighted").not.toBeNull();
    expect(el.querySelector('button[aria-label="Clear search"]')).not.toBeNull();

    await act(async () => {
      el.querySelector<HTMLElement>('button[aria-label="Clear search"]')!.click();
    });
    expect(kinds(el)).toEqual(["running", "live"]);
    expect(el.querySelector("mark")).toBeNull();
    expect(el.querySelector('button[aria-label="Clear search"]')).toBeNull();
  });
});

describe("ChatList — L5: open and new", () => {
  it("a row's open button calls onOpen(id, profile ?? null); the FAB calls onNew(null)", async () => {
    const onOpen = vi.fn();
    const onNew = vi.fn();
    const el = await render({ onOpen, onNew });

    const open = row(el, "c1").querySelector<HTMLElement>('button[data-testid="chat-open"]')!;
    await act(async () => {
      open.click();
    });
    expect(onOpen).toHaveBeenCalledTimes(1);
    expect(onOpen).toHaveBeenCalledWith("c1", "home");

    const fab = el.querySelector<HTMLElement>('button[aria-label="New chat"]')!;
    await act(async () => {
      fab.click();
    });
    expect(onNew).toHaveBeenCalledTimes(1);
    // Compare as a boolean: if the FAB still forwards the click event, a
    // `toHaveBeenCalledWith(null)` diff would pretty-print the event's DOM
    // and fiber graph and OOM the worker.
    expect(onNew.mock.calls[0]?.[0] === null, "one machine ⇒ onNew(null): the server-default path").toBe(true);
  });

  it("the FAB is enabled and labelled 'New chat' with a single machine", async () => {
    const el = await render();
    const fab = el.querySelector<HTMLButtonElement>('button[aria-label="New chat"]')!;
    expect(fab.disabled).toBe(false);
  });
});

describe("ChatList — L6: loading / empty states", () => {
  it("loaded=false shows the loading state and no empty state", async () => {
    const el = await render({ loaded: false });
    const loading = el.querySelector('[data-testid="chat-loading"]');
    expect(loading).not.toBeNull();
    expect(loading!.textContent).toContain("Loading chats…");
    expect(el.querySelector('[data-testid="chat-empty"]')).toBeNull();
  });

  it("loaded with no conversations shows 'No chats yet'", async () => {
    const el = await render({ loaded: true, conversations: [] });
    const empty = el.querySelector('[data-testid="chat-empty"]');
    expect(empty).not.toBeNull();
    expect(empty!.textContent).toContain("No chats yet");
  });

  it("a query with no match shows the no-match empty state", async () => {
    const el = await render();
    await type(el, 'input[aria-label="Search chats"]', "zzz");
    const empty = el.querySelector('[data-testid="chat-empty"]');
    expect(empty).not.toBeNull();
    expect(empty!.textContent).toContain("No chats match “zzz”");
  });
});

describe("ChatList — L7: connection and error", () => {
  it("the connection dot reflects connected; an error shows in the alert", async () => {
    let el = await render({ connected: true });
    expect(el.querySelector('[data-testid="chats-conn"]')!.getAttribute("aria-label")).toBe("connected");

    el = await render({ connected: false });
    expect(el.querySelector('[data-testid="chats-conn"]')!.getAttribute("aria-label")).toBe("offline");

    el = await render({ error: "boom" });
    const err = el.querySelector('[data-testid="chats-error"][role="alert"]');
    expect(err).not.toBeNull();
    expect(err!.textContent).toContain("boom");
  });
});

describe("ChatList — L8: the gear", () => {
  it("the gear renders iff onOpenSettings is given, and calls it once", async () => {
    let el = await render();
    expect(el.querySelector('button[aria-label="Settings"]'), "no gear without the prop").toBeNull();

    const onOpenSettings = vi.fn();
    el = await render({ onOpenSettings });
    const gear = el.querySelector<HTMLElement>('button[aria-label="Settings"]')!;
    await act(async () => {
      gear.click();
    });
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

describe("ChatList — L9: safe-area insets", () => {
  it("the header carries the top inset and the FAB the bottom inset", async () => {
    const el = await render();
    const header = el.querySelector("header");
    expect(header, "a header element must render").not.toBeNull();
    expect(header!.className).toContain("pt-[env(safe-area-inset-top)]");
    const fab = el.querySelector('button[aria-label="New chat"]')!;
    expect(fab.className).toContain("env(safe-area-inset-bottom)");
  });
});
