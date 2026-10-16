// Tests for `tabShortcuts.ts` — the pure `KeyboardEvent` → `TabCommand`
// mapper (design §5):
//
//   export type TabCommand =
//     | { kind: "new" } | { kind: "close" }
//     | { kind: "cycle"; delta: 1 | -1 } | { kind: "jump"; n: number };
//   export function tabShortcut(e: Pick<KeyboardEvent,
//     "key" | "code" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey">): TabCommand | null;
//
// Rules:
//   - altKey → null (never intercept an Alt combo)
//   - mod = ctrlKey || metaKey (Ctrl on Linux/Windows, Cmd on macOS)
//   - ctrlKey && key === "Tab" → cycle (shift ? -1 : +1)
//     *Ctrl only* — Cmd+Tab belongs to the OS window switcher
//   - mod && !shift && key.toLowerCase() === "t" → new; "w" → close
//   - mod && !shift && code ∈ Digit1..Digit9 → jump n
//     *code, not key*: layout-independent (AZERTY "e" has code Digit3)
//
// RED before implementation: `./tabShortcuts` does not exist yet, so this
// file fails at import — the right reason (feature absent).
//
// Runs under `node`: the function takes a plain data object, no DOM needed.

import { describe, expect, it } from "vitest";
import { tabShortcut } from "./tabShortcuts";

type KeyData = Pick<
  KeyboardEvent,
  "key" | "code" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey"
>;

const ev = (over: Partial<KeyData> = {}): KeyData => ({
  key: "",
  code: "",
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  ...over,
});

// ─── new / close (Ctrl or Cmd + T / W) ───────────────────────────────────

describe("tabShortcut — new & close", () => {
  it("maps Ctrl+T to new", () => {
    expect(tabShortcut(ev({ key: "t", code: "KeyT", ctrlKey: true }))).toEqual({ kind: "new" });
  });

  it("maps Cmd+T to new (macOS)", () => {
    expect(tabShortcut(ev({ key: "t", code: "KeyT", metaKey: true }))).toEqual({ kind: "new" });
  });

  it("is case-insensitive on key (the contract lowercases before comparing)", () => {
    // The mapping is a pure function of the event fields: `key.toLowerCase()
    // === "t"` with `!shift` — so a literal "T" without the shift flag maps
    // to new.
    expect(tabShortcut(ev({ key: "T", code: "KeyT", ctrlKey: true }))).toEqual({ kind: "new" });
  });

  it("does NOT map Ctrl+Shift+T (shift blocks; the browser owns history-back)", () => {
    expect(tabShortcut(ev({ key: "T", code: "KeyT", ctrlKey: true, shiftKey: true }))).toBeNull();
  });

  it("maps Ctrl+W to close", () => {
    expect(tabShortcut(ev({ key: "w", code: "KeyW", ctrlKey: true }))).toEqual({ kind: "close" });
  });

  it("maps Cmd+W to close (macOS)", () => {
    expect(tabShortcut(ev({ key: "w", code: "KeyW", metaKey: true }))).toEqual({ kind: "close" });
  });

  it("does NOT map Ctrl+Shift+W (shift blocks)", () => {
    expect(tabShortcut(ev({ key: "W", code: "KeyW", ctrlKey: true, shiftKey: true }))).toBeNull();
  });

  it("ignores bare t / w without a modifier (browser keeps them)", () => {
    expect(tabShortcut(ev({ key: "t", code: "KeyT" }))).toBeNull();
    expect(tabShortcut(ev({ key: "w", code: "KeyW" }))).toBeNull();
  });
});

// ─── Alt never ───────────────────────────────────────────────────────────

describe("tabShortcut — altKey always null", () => {
  it.each([
    ["Alt+Ctrl+T", { key: "t", code: "KeyT", altKey: true, ctrlKey: true }],
    ["Alt+Cmd+W", { key: "w", code: "KeyW", altKey: true, metaKey: true }],
    ["Alt+Ctrl+Tab", { key: "Tab", code: "Tab", altKey: true, ctrlKey: true }],
    ["Alt+Ctrl+1", { key: "1", code: "Digit1", altKey: true, ctrlKey: true }],
  ])("%s is null", (_label, data) => {
    expect(tabShortcut(ev(data))).toBeNull();
  });
});

// ─── cycle (Ctrl+Tab / Ctrl+Shift+Tab — Ctrl only) ───────────────────────

describe("tabShortcut — cycle", () => {
  it("maps Ctrl+Tab to cycle +1", () => {
    expect(tabShortcut(ev({ key: "Tab", code: "Tab", ctrlKey: true }))).toEqual({
      kind: "cycle",
      delta: 1,
    });
  });

  it("maps Ctrl+Shift+Tab to cycle -1", () => {
    expect(tabShortcut(ev({ key: "Tab", code: "Tab", ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "cycle",
      delta: -1,
    });
  });

  it("does NOT map Cmd+Tab (the OS window switcher owns it)", () => {
    expect(tabShortcut(ev({ key: "Tab", code: "Tab", metaKey: true }))).toBeNull();
    expect(
      tabShortcut(ev({ key: "Tab", code: "Tab", metaKey: true, shiftKey: true })),
    ).toBeNull();
  });

  it("ignores a bare Tab (focus navigation stays)", () => {
    expect(tabShortcut(ev({ key: "Tab", code: "Tab" }))).toBeNull();
  });
});

// ─── jump (mod + Digit1..Digit9, by code not key) ────────────────────────

describe("tabShortcut — jump to tab n", () => {
  it.each([...Array(9).keys()].map((i) => [i + 1, i + 1] as const))(
    "maps Ctrl+Digit%d to jump %d",
    (n) => {
      expect(tabShortcut(ev({ key: String(n), code: `Digit${n}`, ctrlKey: true }))).toEqual({
        kind: "jump",
        n,
      });
    },
  );

  it("maps Cmd+Digit5 to jump 5 (macOS)", () => {
    expect(tabShortcut(ev({ key: "5", code: "Digit5", metaKey: true }))).toEqual({
      kind: "jump",
      n: 5,
    });
  });

  it("uses the physical code, not the key (AZERTY: 'e' has code Digit3)", () => {
    expect(tabShortcut(ev({ key: "e", code: "Digit3", ctrlKey: true }))).toEqual({
      kind: "jump",
      n: 3,
    });
  });

  it("does not jump on a Digit key without the Digit code", () => {
    // A key of "1" from some other physical key is not the numpad-adjacent
    // contract: only codes Digit1..Digit9 count.
    expect(tabShortcut(ev({ key: "1", code: "KeyA", ctrlKey: true }))).toBeNull();
  });

  it("does not jump on numpad codes (only the top-row Digit1..Digit9)", () => {
    expect(tabShortcut(ev({ key: "1", code: "Numpad1", ctrlKey: true }))).toBeNull();
  });

  it("does not jump on Digit0", () => {
    expect(tabShortcut(ev({ key: "0", code: "Digit0", ctrlKey: true }))).toBeNull();
  });

  it("shift blocks the jump (Ctrl+Shift+1 is not in the contract)", () => {
    expect(tabShortcut(ev({ key: "1", code: "Digit1", ctrlKey: true, shiftKey: true }))).toBeNull();
  });

  it("ignores bare digits without a modifier", () => {
    expect(tabShortcut(ev({ key: "3", code: "Digit3" }))).toBeNull();
  });
});

// ─── unclaimed combos stay null ──────────────────────────────────────────

describe("tabShortcut — anything else is null", () => {
  it.each([
    ["a plain key", { key: "t", code: "KeyT" }],
    ["Ctrl alone", { code: "ControlLeft", ctrlKey: true }],
    ["Ctrl+E", { key: "e", code: "KeyE", ctrlKey: true }],
    ["Ctrl+Q", { key: "q", code: "KeyQ", ctrlKey: true }],
    ["Cmd+Q (the app must stay openable)", { key: "q", code: "KeyQ", metaKey: true }],
    ["an empty event", {}],
  ])("%s is null", (_label, data) => {
    expect(tabShortcut(ev(data))).toBeNull();
  });
});
