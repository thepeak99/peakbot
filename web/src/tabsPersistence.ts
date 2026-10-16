// localStorage boundary for the desktop tab list. `parseStored` validates once
// at load; everything downstream trusts the result.

import type { ConvoTab, TabsState } from "./convoTabs";

export const TABS_STORAGE_KEY = "peakbot.desktop.tabs";
/** A future "restore tabs on launch" setting hooks in here, nowhere else. */
export const RESTORE_ON_LAUNCH = true;

const MAX_TABS = 100;
const MAX_TITLE = 200;

export function parseStored(raw: string | null): TabsState | null {
  if (raw === null) return null;
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const o = data as Record<string, unknown>;
  if (o.v !== 1 || !Array.isArray(o.tabs)) return null;

  const tabs: ConvoTab[] = [];
  const seen = new Set<string>();
  for (const e of o.tabs as unknown[]) {
    if (tabs.length >= MAX_TABS) break;
    if (typeof e !== "object" || e === null) continue;
    const { convo, title } = e as Record<string, unknown>;
    if (typeof title !== "string") continue;
    if (convo !== null && typeof convo !== "string") continue;
    if (convo !== null) {
      if (seen.has(convo)) continue;
      seen.add(convo);
    }
    tabs.push({ convo, title: title.slice(0, MAX_TITLE) });
  }
  if (tabs.length === 0) return null;

  const a = o.active;
  const active =
    typeof a === "number" && Number.isInteger(a) && a >= 0 && a < tabs.length ? a : 0;
  return { tabs, active };
}

export function loadTabs(
  storage: Storage,
  restore: boolean = RESTORE_ON_LAUNCH,
): TabsState | null {
  if (!restore) return null;
  try {
    return parseStored(storage.getItem(TABS_STORAGE_KEY));
  } catch {
    return null;
  }
}

export function saveTabs(storage: Storage, s: TabsState): void {
  try {
    storage.setItem(
      TABS_STORAGE_KEY,
      JSON.stringify({ v: 1, tabs: s.tabs, active: s.active }),
    );
  } catch {
    // quota / disabled storage: the tab list simply isn't remembered
  }
}
