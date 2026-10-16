// Pure `KeyboardEvent` → tab command mapping for the desktop tab bar.

export type TabCommand =
  | { kind: "new" }
  | { kind: "close" }
  | { kind: "cycle"; delta: 1 | -1 }
  | { kind: "jump"; n: number };

export function tabShortcut(
  e: Pick<
    KeyboardEvent,
    "key" | "code" | "ctrlKey" | "metaKey" | "shiftKey" | "altKey"
  >,
): TabCommand | null {
  if (e.altKey) return null;

  // Ctrl only: Cmd+Tab belongs to the OS window switcher.
  if (e.ctrlKey && e.key === "Tab") {
    return { kind: "cycle", delta: e.shiftKey ? -1 : 1 };
  }

  const mod = e.ctrlKey || e.metaKey;
  if (!mod || e.shiftKey) return null;

  const key = e.key.toLowerCase();
  if (key === "t") return { kind: "new" };
  if (key === "w") return { kind: "close" };

  // `code`, not `key`: layout-independent (AZERTY digits need Shift).
  const m = /^Digit([1-9])$/.exec(e.code);
  if (m) return { kind: "jump", n: Number(m[1]) };
  return null;
}
