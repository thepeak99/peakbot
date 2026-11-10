// Mobile-shell localStorage boundary (design §3.6): the last chat, parsed
// once per boot. Storage access is best-effort — a denied/quota-full
// localStorage degrades to "first run" instead of throwing.
import { setConvoInUrl } from "./state";

export const LAST_CHAT_KEY = "peakbot.mobile.lastChat";

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
