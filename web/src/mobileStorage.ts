// Mobile-shell localStorage boundary (design §3.6): the last chat, parsed
// once per boot, and the machine filter (mobile-redesign §2.3). Storage
// access is best-effort — a denied/quota-full localStorage degrades to
// "first run" / "All" instead of throwing.
import { setConvoInUrl } from "./state";
import type { MachineFilter } from "./chatList";
import { ALL_MACHINES } from "./chatList";

export const LAST_CHAT_KEY = "peakbot.mobile.lastChat";
export const MACHINE_KEY = "peakbot.mobile.machine";

export interface LastChat {
  readonly convo: string;
  readonly profile: string | null;
}

/** Strictly accept one stored shape; anything else (corrupt, older version,
 * wrong types) reads as "nothing stored" so a bad payload can't misroute boot. */
export function parseLastChat(raw: string | null): LastChat | null {
  if (raw === null) return null;
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const obj = data as Record<string, unknown>;
  if (obj.v !== 1) return null;
  if (typeof obj.convo !== "string" || obj.convo === "") return null;
  return { convo: obj.convo, profile: typeof obj.profile === "string" ? obj.profile : null };
}

/** Read the stored chat. `getItem` itself can throw (denied storage), so the
 * catch is here, not at call sites. */
export function loadLastChat(storage: Storage): LastChat | null {
  try {
    return parseLastChat(storage.getItem(LAST_CHAT_KEY));
  } catch {
    return null;
  }
}

/** Best-effort: losing the save only costs the next boot's resume, so a
 * denied storage must not surface as an app error. */
export function saveLastChat(storage: Storage, chat: LastChat): void {
  try {
    storage.setItem(LAST_CHAT_KEY, JSON.stringify({ v: 1, convo: chat.convo, profile: chat.profile }));
  } catch {
    // storage refused; resume falls back to "first run" next boot.
  }
}

/** Strictly accept one stored machine-filter shape; anything else (corrupt,
 * other version, unknown kind, wrong-typed id) reads as All, so a bad payload
 * can't misroute the list. */
export function parseMachineFilter(raw: string | null): MachineFilter {
  if (raw === null) return ALL_MACHINES;
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return ALL_MACHINES;
  }
  if (typeof data !== "object" || data === null || Array.isArray(data)) return ALL_MACHINES;
  const obj = data as Record<string, unknown>;
  if (obj.v !== 1) return ALL_MACHINES;
  if (obj.kind === "all") return ALL_MACHINES;
  if (obj.kind === "one") {
    if (obj.id === null) return { kind: "one", id: null };
    if (typeof obj.id === "string") return { kind: "one", id: obj.id };
  }
  return ALL_MACHINES;
}

/** Read the stored machine filter. `getItem` itself can throw (denied
 * storage), so the catch is here, not at call sites. */
export function loadMachineFilter(storage: Storage): MachineFilter {
  try {
    return parseMachineFilter(storage.getItem(MACHINE_KEY));
  } catch {
    return ALL_MACHINES;
  }
}

/** Best-effort: losing the save only costs the next boot's filter, so a
 * denied storage must not surface as an app error. */
export function saveMachineFilter(storage: Storage, f: MachineFilter): void {
  try {
    const payload = f.kind === "all" ? { v: 1, kind: "all" } : { v: 1, kind: "one", id: f.id };
    storage.setItem(MACHINE_KEY, JSON.stringify(payload));
  } catch {
    // storage refused; the filter falls back to All next boot.
  }
}

/** The URL is the user's explicit choice (deep link, fresh chat in a profile);
 * only a URL with neither param defers to the stored chat. */
export function resumeTarget(
  urlConvo: string | null,
  urlProfile: string | null,
  stored: LastChat | null,
): LastChat | null {
  if (urlConvo !== null || urlProfile !== null) return null;
  return stored;
}

/** Runs once in `main.tsx` before first render (same seam as
 * `bootDesktopTabs`), so `useAgent` reads the resumed binding from the URL.
 * `replaceState` keeps the history length unchanged. */
export function bootMobileChats(): void {
  const params = new URLSearchParams(window.location.search);
  const target = resumeTarget(params.get("convo"), params.get("profile"), loadLastChat(window.localStorage));
  if (target !== null) setConvoInUrl(target.convo, target.profile);
}
