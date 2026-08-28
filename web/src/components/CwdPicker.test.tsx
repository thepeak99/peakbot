// Tests for the CwdPicker "Recent" section (recent-dirs feature).
//
// These lock the target behaviour from the spec:
//   E. Opening the picker sends BOTH `list_dir` and `request_recent_dirs`.
//   F. Non-empty `recentDirs` renders a "Recent" section with one row per
//      directory (section label + row text asserted).
//   G. Empty `recentDirs` renders no "Recent" section at all.
//   H. Clicking a recent row with no transcript sends `switch_cwd` for that
//      path (rows reuse the existing `commit`).
//   I. Clicking a recent row WITH a transcript triggers the confirm guard
//      (mocked `window.confirm`): no `switch_cwd` when declined, sent when
//      confirmed.
//
// Harness modeled on TopBar.test.tsx (createRoot/act off react-dom/client;
// jsdom is auto-enabled for `src/components/**` by vitest.config.ts).
//
// IMPORTANT — expected to be RED before implementation for TWO reasons:
//   1. Runtime: `recentDirs` is not read anywhere in CwdPicker.tsx today, so
//      F (missing "Recent" rows), H/I (no recent row to click → the row
//      lookup throws) and the `request_recent_dirs` half of E fail.
//   2. Typecheck: `recentDirs` is not declared in CwdPicker's props
//      interface, so `tsc --noEmit` flags the fixture. We pass props through
//      an `as unknown as Parameters<typeof CwdPicker>[0]` cast specifically
//      so the test still *runs* under vitest (esbuild transform, no type
//      check) and fails on the missing behaviour rather than refusing to
//      execute — same technique as TopBar.test.tsx. A separate
//      `npm run typecheck` would also be RED, which is expected and fine.
//   3. `request_recent_dirs` is not in the `InboundMessage` union (state.ts)
//      yet; the `toContainEqual` comparisons are runtime deep-equals, so
//      they run and fail on the missing frame rather than on types.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { CwdPicker } from "./CwdPicker";
import type { DirListing, InboundMessage } from "../state";

// React 19's `flushSync` checks `IS_REACT_ACT_ENVIRONMENT`; set once so the
// console stays clean, matching TopBar.test.tsx's setup.
beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

// The real CwdPicker prop interface as declared today (CwdPicker.tsx:38-43)
// plus the single new `recentDirs` prop the feature adds.
interface CwdPickerPropsWithRecent {
  cwd: string;
  hasTranscript: boolean;
  dirListing: DirListing | null;
  send: (msg: InboundMessage) => void;
  dropUp?: boolean;
  recentDirs: string[];
}

const baseProps: CwdPickerPropsWithRecent = {
  cwd: "/project",
  hasTranscript: false,
  dirListing: null,
  send: () => {},
  recentDirs: [],
};

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: CwdPickerPropsWithRecent): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    // `recentDirs` is not part of CwdPicker's declared props today, so we
    // cast through `unknown` to pass it anyway — the point of these tests is
    // to prove the prop is currently ignored/absent, not to fight the type
    // system. Post-implementation this cast becomes a no-op identity.
    root.render(
      <CwdPicker {...(props as unknown as Parameters<typeof CwdPicker>[0])} />,
    );
  });
  return container;
}

afterEach(() => {
  vi.restoreAllMocks();
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
});

/** Click the cwd chip to open the picker (the chip is the first button). */
async function openPicker(el: HTMLDivElement): Promise<void> {
  const chip = el.querySelector("button");
  expect(chip, "cwd chip button must render").not.toBeNull();
  await act(async () => {
    chip!.click();
  });
}

/** Find a rendered row button by its text content (the Recent rows). */
function findRow(el: HTMLDivElement, text: string): HTMLButtonElement {
  const row = Array.from(el.querySelectorAll("button")).find((b) =>
    b.textContent?.includes(text),
  );
  if (!row) {
    throw new Error(`no button containing "${text}" in:\n${el.innerHTML}`);
  }
  return row;
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("CwdPicker — recent dirs", () => {
  // E. Opening the picker requests the recent list alongside the browse.
  it("sends both list_dir and request_recent_dirs when the picker opens", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount({ ...baseProps, send: (m) => sent.push(m) });

    await openPicker(el);

    expect(sent).toContainEqual({ type: "list_dir", path: "/project" });
    expect(sent).toContainEqual({ type: "request_recent_dirs" });
  });

  // F. Non-empty recentDirs renders a "Recent" section with one row per dir.
  it("renders a Recent section with one row per directory when recentDirs is non-empty", async () => {
    const el = await mount({
      ...baseProps,
      recentDirs: ["/alpha/proj", "/beta/proj"],
    });

    await openPicker(el);

    expect(el.textContent).toContain("Recent");
    expect(el.textContent).toContain("/alpha/proj");
    expect(el.textContent).toContain("/beta/proj");
  });

  // G. Empty recentDirs hides the section entirely.
  it("renders no Recent section when recentDirs is empty", async () => {
    const el = await mount({ ...baseProps, recentDirs: [] });

    await openPicker(el);

    expect(el.textContent).not.toContain("Recent");
  });

  // H. Clicking a recent row with no transcript commits it: switch_cwd.
  it("clicking a recent row with no transcript sends switch_cwd for that path", async () => {
    const sent: InboundMessage[] = [];
    const el = await mount({
      ...baseProps,
      hasTranscript: false,
      recentDirs: ["/recent/one"],
      send: (m) => sent.push(m),
    });

    await openPicker(el);
    const row = findRow(el, "/recent/one");
    await act(async () => {
      row.click();
    });

    expect(sent).toContainEqual({ type: "switch_cwd", path: "/recent/one" });
  });

  // I. Clicking a recent row WITH a transcript hits the confirm guard and
  // declines → no switch_cwd.
  it("clicking a recent row with a transcript triggers the confirm guard and stays put when declined", async () => {
    const sent: InboundMessage[] = [];
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const el = await mount({
      ...baseProps,
      hasTranscript: true,
      recentDirs: ["/recent/two"],
      send: (m) => sent.push(m),
    });

    await openPicker(el);
    const row = findRow(el, "/recent/two");
    await act(async () => {
      row.click();
    });

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(confirmSpy).toHaveBeenCalledWith(
      expect.stringContaining("/recent/two"),
    );
    expect(sent).not.toContainEqual({ type: "switch_cwd", path: "/recent/two" });
  });

  // I (proceed). ...and sends switch_cwd when the guard is confirmed.
  it("clicking a recent row with a transcript sends switch_cwd when confirmed", async () => {
    const sent: InboundMessage[] = [];
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    const el = await mount({
      ...baseProps,
      hasTranscript: true,
      recentDirs: ["/recent/three"],
      send: (m) => sent.push(m),
    });

    await openPicker(el);
    const row = findRow(el, "/recent/three");
    await act(async () => {
      row.click();
    });

    expect(confirmSpy).toHaveBeenCalledTimes(1);
    expect(sent).toContainEqual({ type: "switch_cwd", path: "/recent/three" });
  });
});
