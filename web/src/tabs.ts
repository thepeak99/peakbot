// Tab-visibility helpers (profiles feature). The server computes
// `visible_tabs` (a config/profile decision); the SPA never re-derives that
// rule — it only narrows its own canonical tab list to match, and relocates
// an open drawer tab that just got hidden out from under the user.

/** Narrow `tabs` to the ids listed in `visible`, keeping the CANONICAL order
 * of `tabs` itself (not whatever order `visible` lists ids in — that's a
 * wire array, not a display order). `visible === undefined` means an older
 * server that doesn't send the field yet, so nothing is hidden. */
export function filterTabs<T extends { id: string }>(
  tabs: T[],
  visible: string[] | undefined,
): T[] {
  if (visible === undefined) return tabs;
  const allowed = new Set(visible);
  return tabs.filter((t) => allowed.has(t.id));
}

/** Keep an open drawer pointed at a real tab after `visibleIds` changes.
 * Closed (`null`) stays closed. A still-visible active tab is untouched. A
 * hidden one falls back to the first visible tab, or `null` if none remain. */
export function resolveActiveTab(
  active: string | null,
  visibleIds: string[],
): string | null {
  if (active === null) return null;
  if (visibleIds.includes(active)) return active;
  return visibleIds[0] ?? null;
}
