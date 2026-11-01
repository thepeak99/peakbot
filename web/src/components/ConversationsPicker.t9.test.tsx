// Conversations picker — remote rows (remote-profiles T9).
//
// RED until T9. Written against the SPA contract the design locks
// (remote-profiles-design §6.4): "Picker: open = `switchConvo(row.id,
// row.profile)`; kill = `kill_session{convo, profile}`." Today the
// callbacks take only the id, so a remote row would open as a LOCAL
// session and kill the local registry with a remote id.
//
// Also: the picker LISTS remote rows — marked `remote` and coloured from
// the catalogue (the T2 dot stands; the `remote` mark is new).
//
// Harness modeled on ConversationsPicker.test.tsx (createRoot/act; the
// two-arg callbacks are not in the prop types yet, so the new prop shapes
// are passed through `as unknown as` casts — same technique as that file
// used for the then-missing `error` prop).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ConversationsPicker } from "./ConversationsPicker";
import type { ConversationSummary } from "../state";
import type { CatalogProfile } from "../types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

const CATALOG: CatalogProfile[] = [
  { name: "dev", origin: "global", overridesGlobal: false, agents: [], color: "teal" },
  { name: "home-dev", origin: "remote", overridesGlobal: false, agents: [], color: "pink" },
];

const row = (
  id: string,
  over: Partial<ConversationSummary> & { running?: boolean } = {},
): ConversationSummary =>
  ({
    id,
    name: `convo ${id}`,
    updated_at: "2026-10-04T00:00:00Z",
    message_count: 1,
    model: "m",
    active: false,
    ...over,
  } as ConversationSummary);

interface Props {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  onOpen: () => void;
  /** T9: (id, profile) — today only (id). */
  onLoad: (id: string, profile: string | null) => void;
  /** T9: (id, profile) — today only (id). */
  onKill: (id: string, profile: string | null) => void;
  error: string | null;
  catalog?: CatalogProfile[];
}

const baseProps: Props = {
  conversations: [],
  hasTranscript: false,
  onOpen: () => {},
  onLoad: () => {},
  onKill: () => {},
  error: null,
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function render(props: Props): Promise<HTMLDivElement> {
  if (!root) {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container!);
  }
  await act(async () => {
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
  delete (window as unknown as { isTauri?: boolean }).isTauri;
  vi.restoreAllMocks();
});

async function openPicker(el: HTMLDivElement): Promise<void> {
  const chip = el.querySelector("button");
  expect(chip, "conversations chip button must render").not.toBeNull();
  await act(async () => {
    chip!.click();
  });
}

/** The row element (the `group` div) whose load button names `name`. */
function rowEl(el: HTMLDivElement, name: string): HTMLElement | null {
  const btn = [...el.querySelectorAll<HTMLElement>(".absolute button")].find((b) =>
    b.textContent?.includes(name),
  );
  return btn?.closest("div.group") ?? null;
}

// ─── open / kill forward the row's profile ──────────────────────────────

describe("ConversationsPicker — remote rows (remote-profiles T9)", () => {
  it("open forwards the row's profile (remote row)", async () => {
    const onLoad = vi.fn();
    const el = await render({
      ...baseProps,
      catalog: CATALOG,
      onLoad,
      conversations: [row("a1", { profile: "home-dev" }), row("b2")],
    });
    await openPicker(el);
    await render({
      ...baseProps,
      catalog: CATALOG,
      onLoad,
      conversations: [row("a1", { profile: "home-dev" }), row("b2")],
    });

    const r = rowEl(el, "convo a1");
    expect(r, "a row for convo a1 must render").not.toBeNull();
    await act(async () => {
      r!.querySelector("button")!.click(); // the load button
    });

    expect(onLoad).toHaveBeenCalledTimes(1);
    expect(onLoad).toHaveBeenCalledWith("a1", "home-dev");
  });

  it("open forwards a null profile for a profile-less row", async () => {
    const onLoad = vi.fn();
    const el = await render({
      ...baseProps,
      catalog: CATALOG,
      onLoad,
      conversations: [row("b2")],
    });
    await openPicker(el);
    await render({
      ...baseProps,
      catalog: CATALOG,
      onLoad,
      conversations: [row("b2")],
    });

    const r = rowEl(el, "convo b2");
    expect(r, "a row for convo b2 must render").not.toBeNull();
    await act(async () => {
      r!.querySelector("button")!.click();
    });

    expect(onLoad).toHaveBeenCalledTimes(1);
    expect(onLoad).toHaveBeenCalledWith("b2", null);
  });

  it("kill forwards the row's profile (remote row) for kill_session{convo, profile}", async () => {
    const onKill = vi.fn();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const el = await render({
      ...baseProps,
      catalog: CATALOG,
      onKill,
      conversations: [row("a1", { active: true, profile: "home-dev" })],
    });
    await openPicker(el);
    await render({
      ...baseProps,
      catalog: CATALOG,
      onKill,
      conversations: [row("a1", { active: true, profile: "home-dev" })],
    });

    const r = rowEl(el, "convo a1");
    expect(r, "a row for convo a1 must render").not.toBeNull();
    const killBtn = r!.querySelector('button[title="Kill this live session"]');
    expect(killBtn, "the kill button renders on an active row").not.toBeNull();
    await act(async () => {
      (killBtn as HTMLElement).click();
    });

    expect(window.confirm).toHaveBeenCalled();
    expect(onKill).toHaveBeenCalledTimes(1);
    expect(onKill).toHaveBeenCalledWith("a1", "home-dev");
  });

  it("kill forwards a null profile for a local row (kill_session{convo} stays local)", async () => {
    const onKill = vi.fn();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const el = await render({
      ...baseProps,
      catalog: CATALOG,
      onKill,
      conversations: [row("b2", { active: true })],
    });
    await openPicker(el);
    await render({
      ...baseProps,
      catalog: CATALOG,
      onKill,
      conversations: [row("b2", { active: true })],
    });

    const r = rowEl(el, "convo b2");
    expect(r, "a row for convo b2 must render").not.toBeNull();
    await act(async () => {
      r!.querySelector<HTMLElement>('button[title="Kill this live session"]')!.click();
    });

    expect(onKill).toHaveBeenCalledTimes(1);
    expect(onKill).toHaveBeenCalledWith("b2", null);
  });

  // ── remote marking + colour ────────────────────────────────────────────

  it("marks remote rows `remote` and colours them from the catalogue", async () => {
    const el = await render({
      ...baseProps,
      catalog: CATALOG,
      conversations: [
        row("a1", { profile: "home-dev" }),
        row("b2", { profile: "dev" }),
      ],
    });
    await openPicker(el);
    await render({
      ...baseProps,
      catalog: CATALOG,
      conversations: [row("a1", { profile: "home-dev" }), row("b2", { profile: "dev" })],
    });

    const remote = rowEl(el, "convo a1");
    expect(remote, "the remote row renders").not.toBeNull();
    expect(remote!.textContent, "the row is marked remote").toContain("remote");
    const dot = remote!.querySelector('[data-testid="picker-row-color"]');
    expect(dot, "the row carries the catalogue dot").not.toBeNull();
    expect(dot!.getAttribute("style")).toContain("var(--profile-pink)");

    const local = rowEl(el, "convo b2");
    expect(local, "the local row renders").not.toBeNull();
    expect(local!.textContent, "a local row is not marked remote").not.toContain("remote");
  });
});
