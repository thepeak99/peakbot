// Tests for the ConversationsPicker kill-error line.
//
// These lock the target behaviour from the spec:
//   1. When the `error` prop is set and the picker is not loading, a red
//      error line renders at the end of the dropdown panel (role="alert")
//      with the kill-refusal message.
//   2. The error line is hidden while a reopen is loading (opening the
//      picker re-requests the list; the stale error must not flash over
//      the spinner).
//   3. No alert renders when `error` is null.
//
// Harness modeled on CwdPicker.test.tsx (createRoot/act off
// react-dom/client; jsdom is auto-enabled for `src/components/**` by
// vitest.config.ts).
//
// IMPORTANT — expected to be RED before implementation: `error` is not
// declared in ConversationsPicker's props today, so the component ignores
// it and no alert ever renders. We pass props through an
// `as unknown as Parameters<typeof ConversationsPicker>[0]` cast so the
// tests still *run* under vitest (esbuild transform, no type check) and
// fail on the missing behaviour rather than refusing to execute — same
// technique as CwdPicker.test.tsx. `npm run typecheck` would also flag
// the fixture, which is expected and fine.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConversationsPicker } from "./ConversationsPicker";
import type { ConversationSummary } from "../state";
import type { CatalogProfile } from "../types";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching CwdPicker.test.tsx's setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

// The real ConversationsPicker prop interface as declared today
// (ConversationsPicker.tsx:48-64) plus the single new `error` prop the
// feature adds.
interface ConversationsPickerPropsWithError {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  onOpen: () => void;
  onLoad: (id: string) => void;
  onKill: (id: string) => void;
  dropUp?: boolean;
  error: string | null;
  catalog?: CatalogProfile[];
}

const KILL_ERROR = "Can't kill a running conversation — press Stop first.";

const convo = (id: string, active = false): ConversationSummary => ({
  id,
  name: `convo ${id}`,
  updated_at: "2026-10-04T00:00:00Z",
  message_count: 1,
  model: "ollama/llama3",
  active,
});

const baseProps: ConversationsPickerPropsWithError = {
  conversations: [convo("a1", true), convo("b2")],
  hasTranscript: false,
  onOpen: () => {},
  onLoad: () => {},
  onKill: () => {},
  error: null,
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

/** Mount (or re-render, with a fresh `conversations` reference) the picker. */
async function render(props: ConversationsPickerPropsWithError): Promise<HTMLDivElement> {
  if (!root) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
    // `error` is not part of ConversationsPicker's declared props today, so
    // we cast through `unknown` to pass it anyway — the point of these tests
    // is to prove the prop is currently ignored, not to fight the type
    // system. Post-implementation this cast becomes a no-op identity.
    root!.render(
      <ConversationsPicker
        {...(props as unknown as Parameters<typeof ConversationsPicker>[0])}
      />,
    );
  });
  return container!;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
  // Desktop tests install peakbotShell; always start the next test in a
  // browser.
  delete (window as unknown as { peakbotShell?: string }).peakbotShell;
  vi.restoreAllMocks();
});

/** Click the "conversations" chip to open the dropdown (the first button). */
async function openPicker(el: HTMLDivElement): Promise<void> {
  const chip = el.querySelector("button");
  expect(chip, "conversations chip button must render").not.toBeNull();
  await act(async () => {
    chip!.click();
  });
}

/** The kill-error line, once it exists: role="alert" inside the panel. */
function findAlert(el: HTMLDivElement): HTMLElement | null {
  return el.querySelector('[role="alert"]');
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("ConversationsPicker — kill error line", () => {
  // 1. The refused-kill message renders as an alert at the end of the panel.
  it("shows red error line under the list when error is set", async () => {
    const el = await render({ ...baseProps, error: KILL_ERROR });

    await openPicker(el);

    // Opening starts a reload; the alert only shows once a fresh list
    // (new `conversations` reference) lands and loading clears.
    await render({
      ...baseProps,
      error: KILL_ERROR,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });

    const alert = findAlert(el);
    expect(alert, "an alert element must render when error is set").not.toBeNull();
    expect(alert!.textContent).toContain("Can't kill a running conversation");
    expect(alert!.textContent).toContain("press Stop first");
  });

  // 2. The error is hidden while a reopen is loading: opening the picker
  //    re-requests the list (onOpen), and the stale error must not flash
  //    over the spinner. Once the fresh list lands (a new `conversations`
  //    reference) the error shows again; reopening hides it once more.
  it("hides error while a reopen is loading", async () => {
    const el = await render({ ...baseProps, error: KILL_ERROR });

    // Open: the reopen is in flight (onOpen is a no-op, so no list lands) →
    // the error line must be hidden behind the loading state.
    await openPicker(el);
    expect(el.textContent).toContain("Loading conversations…");
    expect(
      findAlert(el),
      "no alert while the reopen is loading",
    ).toBeNull();

    // The fresh list lands (new reference) → loading clears, error shows.
    await render({
      ...baseProps,
      error: KILL_ERROR,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });
    expect(findAlert(el), "alert returns once the list lands").not.toBeNull();

    // Reopen → loading again → the stale error hides.
    await openPicker(el);
    await openPicker(el); // close
    await openPicker(el); // reopen
    expect(
      findAlert(el),
      "no alert while the second reopen is loading",
    ).toBeNull();
  });

  // 3. No alert when there is no error.
  it("renders no alert when error is null", async () => {
    const el = await render({ ...baseProps, error: null });

    await openPicker(el);

    expect(findAlert(el)).toBeNull();
  });
});

// ─── desktop tab bar (issue #373): picker behaviour split ────────────────
//
// Design §5 (App wiring): `ConversationsPicker` — if `isDesktop()`
// (`window.isTauri === true`), DON'T render the "+" button and SKIP the
// `hasTranscript` confirm in `load` (in desktop, loading opens/switches a
// tab via the wired `onLoad` = `tabs.open`, so nothing is replaced). In a
// browser the existing behaviour must stay UNCHANGED: the "+" opens a new
// browser tab via `window.open(pathname, "_blank", "noopener")`, and a
// non-empty transcript is confirmed before loading.
//
// The first two tests here are GREEN-today guard tests (browser behaviour
// unchanged); the desktop tests are RED until `isDesktop()` lands in the
// component.

describe("ConversationsPicker — desktop vs browser (issue #373)", () => {
  function setDesktop(on: boolean): void {
    if (on) {
      (window as { peakbotShell?: string }).peakbotShell = "desktop";
    } else {
      delete (window as { peakbotShell?: string }).peakbotShell;
    }
  }

  const NEW_TAB_BUTTON = 'button[aria-label="Open a new conversation in a new tab"]';

  // ── browser: behaviour unchanged ───────────────────────────────────────

  it("browser: renders the + button and opens a new browser tab (window.open)", async () => {
    const openSpy = vi.spyOn(window, "open").mockImplementation(() => null);
    const el = await render({ ...baseProps, error: null });

    const plus = el.querySelector<HTMLElement>(NEW_TAB_BUTTON);
    expect(plus, "the browser header + must render").not.toBeNull();

    await act(async () => {
      plus!.click();
    });

    expect(openSpy).toHaveBeenCalledTimes(1);
    expect(openSpy).toHaveBeenCalledWith(
      window.location.pathname,
      "_blank",
      "noopener",
    );
  });

  it("browser: loading with a transcript confirms first (unchanged)", async () => {
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    const onLoad = vi.fn();
    const el = await render({
      ...baseProps,
      error: null,
      hasTranscript: true,
      onLoad,
    });

    await openPicker(el);
    await render({
      ...baseProps,
      error: null,
      hasTranscript: true,
      onLoad,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });

    // Click the second row's load button (the first button is the chip).
    const rowButtons = el.querySelectorAll<HTMLElement>(".absolute button");
    expect(rowButtons.length, "dropdown rows must render").toBeGreaterThan(0);
    const target = [...rowButtons].find(
      (b) => b.textContent?.includes("convo b2"),
    );
    expect(target, "a row for convo b2 must render").not.toBeNull();

    await act(async () => {
      target!.click();
    });

    expect(confirmSpy, "the browser must keep the replace-confirm").toHaveBeenCalled();
    expect(onLoad).toHaveBeenCalledWith("b2", null);
  });

  // ── desktop: no +, no confirm, load opens a tab ────────────────────────

  it("desktop: the + button is not rendered", async () => {
    setDesktop(true);
    const el = await render({ ...baseProps, error: null });

    expect(
      el.querySelector(NEW_TAB_BUTTON),
      "desktop: the header + must be hidden (the tab bar has its own +)",
    ).toBeNull();
  });

  it("desktop: loading with a transcript skips the confirm and loads (opens a tab)", async () => {
    setDesktop(true);
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const onLoad = vi.fn();
    const el = await render({
      ...baseProps,
      error: null,
      hasTranscript: true,
      onLoad,
    });

    await openPicker(el);
    await render({
      ...baseProps,
      error: null,
      hasTranscript: true,
      onLoad,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });

    const target = [...el.querySelectorAll<HTMLElement>(".absolute button")].find(
      (b) => b.textContent?.includes("convo b2"),
    );
    expect(target, "a row for convo b2 must render").not.toBeNull();

    await act(async () => {
      target!.click();
    });

    expect(
      confirmSpy,
      "desktop: no replace-confirm — loading opens a tab, nothing is replaced",
    ).not.toHaveBeenCalled();
    expect(onLoad, "the picker must still hand the id to the tab store").toHaveBeenCalledWith("b2", null);
  });

  it("desktop: loading without a transcript loads directly", async () => {
    setDesktop(true);
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    const onLoad = vi.fn();
    const el = await render({
      ...baseProps,
      error: null,
      hasTranscript: false,
      onLoad,
    });

    await openPicker(el);
    await render({
      ...baseProps,
      error: null,
      hasTranscript: false,
      onLoad,
      conversations: [convo("a1", true), convo("b2"), convo("c3")],
    });

    const target = [...el.querySelectorAll<HTMLElement>(".absolute button")].find(
      (b) => b.textContent?.includes("convo b2"),
    );
    expect(target, "a row for convo b2 must render").not.toBeNull();

    await act(async () => {
      target!.click();
    });

    expect(confirmSpy).not.toHaveBeenCalled();
    expect(onLoad).toHaveBeenCalledWith("b2", null);
  });
});

// ─── per-profile row colour dot (remote-profiles design T2) ──────────────
//
// A row whose `profile` matches a coloured catalogue entry shows a dot in
// the profile's colour (`var(--profile-<c>)`); a profile-less or unknown
// row shows none. The catalogue is the only colour source (RP8).

describe("ConversationsPicker — per-profile row colour dot (remote-profiles T2)", () => {
  const CATALOG: CatalogProfile[] = [
    { name: "dev", origin: "global", overridesGlobal: false, agents: [], color: "teal" },
    { name: "ops", origin: "global", overridesGlobal: false, agents: [], color: "pink" },
  ];
  const convoWith = (id: string, profile: string | null): ConversationSummary => ({
    ...convo(id),
    profile,
  });
  const rows = [convoWith("a1", "dev"), convoWith("b2", "ops")];

  it("shows a dot in the catalogue colour for each coloured row", async () => {
    const el = await render({ ...baseProps, error: null, catalog: CATALOG, conversations: rows });
    await openPicker(el);
    // A fresh array reference clears the loading state (the list "landed").
    await render({ ...baseProps, error: null, catalog: CATALOG, conversations: [...rows] });

    const dots = el.querySelectorAll<HTMLElement>('[data-testid="picker-row-color"]');
    expect(dots, "one dot per coloured row").toHaveLength(2);
    expect(dots[0].getAttribute("style")).toContain("var(--profile-teal)");
    expect(dots[1].getAttribute("style")).toContain("var(--profile-pink)");
  });

  it("shows no dot for profile-less or unknown-profile rows", async () => {
    const rows2 = [convoWith("a1", null), convoWith("b2", "ghost")];
    const el = await render({ ...baseProps, error: null, catalog: CATALOG, conversations: rows2 });
    await openPicker(el);
    await render({ ...baseProps, error: null, catalog: CATALOG, conversations: [...rows2] });

    expect(el.querySelectorAll('[data-testid="picker-row-color"]')).toHaveLength(0);
  });
});

// ─── conversation-row states (consistent with the mobile ChatList) ─────────
//
// The approved row-state contract:
//   • RUNNING rows (`running === true`; a running chat is also active) get
//     the AMBER language: wrapper `bg-amber-950/40 hover:bg-amber-900/40`,
//     title `font-semibold text-amber-200`, and the amber spinner marker
//     (`data-testid="picker-status-running"`, aria-label "running", the same
//     classes as the mobile `chat-status-running` spinner).
//   • LIVE rows (`active && !running`) keep the emerald tint
//     (`bg-emerald-950/40 hover:bg-emerald-900/40`) and the
//     `font-semibold text-emerald-200` title, but the VISIBLE green dot is
//     removed: no `bg-emerald-400` element, and a visually hidden (sr-only)
//     "Live session" label stays for accessibility. The " · live" meta text
//     and the kill ✕ are kept.
//   • Plain rows: no tint, `font-medium` title, no markers.
//   • The keyboard-highlighted (selected) row keeps its precedence over the
//     active tint (selected wins) — that is today's behaviour and must stay.
//
// The picker has NO unread concept today (no unread prop, no sky dot), so —
// per the spec — no unread assertions are invented here.
//
// RED before implementation: the component knows only `active` — there is no
// `running` branch, no `picker-status-running` marker, no amber classes and
// no sr-only label, while the visible `bg-emerald-400` dot still renders.

describe("ConversationsPicker — row states (running amber / live emerald / plain)", () => {
  const rowConvo = (over: Partial<ConversationSummary>): ConversationSummary => ({
    id: "x1",
    name: "row convo",
    updated_at: "2026-10-04T00:00:00Z",
    message_count: 2,
    model: "sonnet",
    active: false,
    ...over,
  });

  // The dropdown panel is the `.absolute` div; rows live in it. A row's load
  // button is the button whose text carries the conversation name, and the
  // row itself is that button's parent.
  const rowButton = (el: HTMLElement, name: string): HTMLElement | undefined =>
    [...el.querySelectorAll<HTMLElement>(".absolute button")].find((b) =>
      b.textContent?.includes(name),
    );

  const rowDiv = (el: HTMLElement, name: string): HTMLElement => {
    const btn = rowButton(el, name);
    expect(btn, `a row button for "${name}" must render`).not.toBeUndefined();
    return btn!.parentElement!;
  };

  // The row's title span (the only `truncate` span inside the row button).
  const rowTitle = (rowEl: HTMLElement): HTMLElement => {
    const t = rowEl.querySelector<HTMLElement>("span.truncate");
    expect(t, "a title span must render in the row").not.toBeNull();
    return t!;
  };

  // The hidden "Live session" label: by aria-label or text, per the
  // text/aria-label contract.
  const liveLabel = (rowEl: HTMLElement): HTMLElement | null =>
    [
      ...rowEl.querySelectorAll<HTMLElement>('[aria-label="Live session"]'),
    ].find(Boolean) ??
    [...rowEl.querySelectorAll<HTMLElement>("*")].find(
      (n) => n.textContent === "Live session",
    ) ??
    null;

  // Open the picker, then land a fresh list (new reference) so the loading
  // state clears — the established pattern in this file.
  async function openWith(convs: ConversationSummary[]): Promise<HTMLElement> {
    const el = await render({ ...baseProps, error: null, conversations: convs });
    await openPicker(el);
    await render({ ...baseProps, error: null, conversations: [...convs] });
    return el;
  }

  const SPINNER_CLASSES = [
    "h-3.5",
    "w-3.5",
    "animate-spin",
    "rounded-full",
    "border-2",
    "border-amber-400",
    "border-t-transparent",
  ];

  it("running rows get the amber tint, an amber title and the amber spinner marker", async () => {
    const convs = [
      rowConvo({ id: "r1", name: "Running convo", active: true, running: true }),
      rowConvo({ id: "p1", name: "Plain convo" }),
    ];
    const el = await openWith(convs);
    const d = rowDiv(el, "Running convo");

    // Amber tint on the wrapper (desktop adds the hover variant).
    expect(d.classList.contains("bg-amber-950/40"), "the running row tint").toBe(true);
    expect(d.classList.contains("hover:bg-amber-900/40"), "the running row hover tint").toBe(true);
    // Amber semibold title.
    const t = rowTitle(d);
    expect(t.classList.contains("font-semibold")).toBe(true);
    expect(t.classList.contains("text-amber-200")).toBe(true);
    // A running row is not emerald, even though a running chat is also active.
    expect(d.classList.contains("bg-emerald-950/40")).toBe(false);
    expect(t.classList.contains("text-emerald-200")).toBe(false);

    // The amber spinner is the running marker, matching the mobile classes.
    const spin = d.querySelector<HTMLElement>('[data-testid="picker-status-running"]');
    expect(spin, "the running spinner marker").not.toBeNull();
    expect(spin!.getAttribute("aria-label")).toBe("running");
    for (const cls of SPINNER_CLASSES) {
      expect(spin!.classList.contains(cls), `spinner class ${cls}`).toBe(true);
    }

    // Running is not live: no live label, no " · live" text.
    expect(liveLabel(d), "a running row is not live").toBeNull();
    expect(d.textContent).not.toContain("· live");
  });

  it("live rows keep the emerald tint and title; the visible green dot is gone", async () => {
    // The live row sits at index 1 so the initial keyboard highlight (row 0)
    // does not shadow its tint.
    const convs = [
      rowConvo({ id: "p1", name: "Plain convo" }),
      rowConvo({ id: "l1", name: "Live convo", active: true }),
    ];
    const el = await openWith(convs);
    const d = rowDiv(el, "Live convo");

    expect(d.classList.contains("bg-emerald-950/40"), "the live row tint").toBe(true);
    expect(d.classList.contains("hover:bg-emerald-900/40"), "the live row hover tint").toBe(true);
    const t = rowTitle(d);
    expect(t.classList.contains("font-semibold")).toBe(true);
    expect(t.classList.contains("text-emerald-200")).toBe(true);

    // No visible green dot anywhere in the row.
    expect(d.querySelector(".bg-emerald-400"), "no visible green dot").toBeNull();
    // The hidden "Live session" label stays, visually hidden.
    const label = liveLabel(d);
    expect(label, "the hidden Live session label").not.toBeNull();
    expect(label!.classList.contains("sr-only"), "the label must be sr-only").toBe(true);
    expect(label!.classList.contains("bg-emerald-400"), "the label is not a dot").toBe(false);
    // The " · live" meta text and the kill ✕ are kept.
    expect(d.textContent).toContain("· live");
    expect(d.querySelector('button[title="Kill this live session"]')).not.toBeNull();
    // No running marker.
    expect(d.querySelector('[data-testid="picker-status-running"]')).toBeNull();
  });

  it("plain rows get no tint, a medium title and no status markers", async () => {
    const convs = [rowConvo({ id: "p1", name: "Plain convo" })];
    const el = await openWith(convs);
    const d = rowDiv(el, "Plain convo");

    expect(d.classList.contains("bg-amber-950/40")).toBe(false);
    expect(d.classList.contains("bg-emerald-950/40")).toBe(false);
    const t = rowTitle(d);
    expect(t.classList.contains("font-medium")).toBe(true);
    expect(t.classList.contains("font-semibold")).toBe(false);
    expect(d.querySelector('[data-testid="picker-status-running"]')).toBeNull();
    expect(liveLabel(d)).toBeNull();
    expect(d.querySelector(".bg-emerald-400")).toBeNull();
    expect(d.querySelector('button[title="Kill this live session"]')).toBeNull();
    expect(d.textContent).not.toContain("· live");
  });

  it("the keyboard-highlighted (selected) row wins over the active tint", async () => {
    // highlightedIndex starts at 0 on open, so the first (active) row is the
    // highlighted one: its `bg-zinc-800` highlight must beat the emerald tint.
    const convs = [
      rowConvo({ id: "l1", name: "Live convo", active: true }),
      rowConvo({ id: "p1", name: "Plain convo" }),
    ];
    const el = await openWith(convs);
    const d = rowDiv(el, "Live convo");
    expect(d.classList.contains("bg-zinc-800"), "the highlighted row keeps its highlight").toBe(true);
    expect(d.classList.contains("bg-emerald-950/40"), "selected wins over the active tint").toBe(false);
  });
});
