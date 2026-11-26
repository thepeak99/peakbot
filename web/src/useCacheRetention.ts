// Retention wiring for the shared conversation cache (design §7).
// Subscribes to the existing cloud state/roster store and applies the pure
// gate-vs-purge rules from `cacheRetention.ts`:
//   - active roster, device present→absent ⇒ purge only that device
//   - rename (same DeviceID) ⇒ retain
//   - first authenticated roster ⇒ never purge by absence
//   - self logout / self revoke ⇒ purge cloud caches only
//   - every other (transient) state ⇒ retain (no pruning)
//
// The "last authenticated roster" for cross-boot diffing is persisted in the
// SAME IndexedDB database as conversation data (via conversationCache), never
// localStorage. This hook removes the legacy localStorage key once (migration)
// and never writes it.

import { useEffect } from "react";
import { store } from "./cloud/store";
import { getSharedConversationCache, type ConversationCache } from "./conversationCache";
import { determinePurges, type RosterEntry } from "./cacheRetention";

/** Legacy key from builds before the roster moved to IndexedDB. Remove once
 * during mount; never write it. */
const LEGACY_ROSTER_KEY = "peakbot.cloudRoster";

/** Apply the retention rules for one cloud snapshot. Exported for clarity;
 * the hook below is the only caller. The cross-boot "last authenticated
 * roster" is read/written through the cache (same IndexedDB database as the
 * conversations, design §7) — never localStorage. */
export async function applyRetention(
  cache: ConversationCache,
  snapshot: { state: string; devices?: { id: string; name: string }[]; trust_error?: string } | null,
): Promise<void> {
  if (!snapshot) return; // no snapshot yet ⇒ retain

  switch (snapshot.state) {
    case "active": {
      // Authenticated roster. First roster (prior === null) never purges.
      const current: RosterEntry[] = (snapshot.devices ?? []).map((d) => ({
        id: d.id,
        name: d.name,
      }));
      const prior = await cache.getAuthenticatedRoster();
      const purges = determinePurges(prior, current);
      for (const origin of purges) {
        await cache.purgeOrigin(origin);
      }
      await cache.saveAuthenticatedRoster(current);
      return;
    }
    case "logged_out":
      // Self logout (or all devices revoked): cloud cache only.
      await cache.purgeCloud();
      return;
    case "trust":
      // A self-revoke surfaces as a trust error; everything else retains.
      if (snapshot.trust_error === "revoked") {
        await cache.purgeCloud();
      }
      return;
    default:
      // disabled / connecting / pending: transient ⇒ retain.
      return;
  }
}

/** Subscribe retention to the existing cloud store. No pruning on transient
 * states; only an authenticated roster diff, self-logout or self-revoke acts. */
export function useCacheRetention(): void {
  useEffect(() => {
    // One-time migration: earlier builds kept the roster in localStorage;
    // the cache must live in IndexedDB only. Remove once; never write it back.
    try {
      window.localStorage.removeItem(LEGACY_ROSTER_KEY);
    } catch {
      /* best-effort */
    }

    let cancelled = false;
    let cache: ConversationCache | null = null;

    void getSharedConversationCache().then((c) => {
      if (cancelled) return;
      cache = c;
      // Apply the current snapshot once (a cold boot into a logged-out state
      // has nothing to purge; a restored active roster is diffed).
      void applyRetention(c, store.getSnapshot().snapshot);
    });

    const unsubscribe = store.subscribe(() => {
      if (cancelled || !cache) return;
      void applyRetention(cache, store.getSnapshot().snapshot);
    });

    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, []);
}