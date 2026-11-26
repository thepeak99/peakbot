// Pure merge logic for the shared remote conversation cache
// (design "Shared remote conversation cache + delta sync — revised design" §2-4).
// No IndexedDB, no React — just the pure functions.

import type { ConversationSummary } from "./state";

/** A cached row: the client summary plus the cache's per-origin machine tag. */
export type CacheRow = ConversationSummary & { machine?: string | null };

export interface MergeOptions {
  live: CacheRow[];
  cachedByOrigin: Record<string, CacheRow[]>;
  replacedOrigins: string[];
  limit: number;
}

const LIMIT = 100;

function ts(row: CacheRow): number {
  return Date.parse(row.updated_at) || -Infinity;
}

function sortRows(rows: CacheRow[]): CacheRow[] {
  rows.sort((a, b) => {
    const ta = ts(a);
    const tb = ts(b);
    if (ta !== tb) return tb - ta;
    if (a.id < b.id) return -1;
    if (a.id > b.id) return 1;
    return 0;
  });
  return rows;
}

/** Merge a list response with cached snapshots. */
export function mergeListing(opts: MergeOptions): CacheRow[] {
  const { live, cachedByOrigin, replacedOrigins, limit = LIMIT } = opts;
  const replacedSet = new Set(replacedOrigins);

  // Collect live rows
  const rows = [...live];

  // Add cached rows for origins that are NOT in replacedOrigins
  // (they failed/offline, so we retain their cached snapshot)
  for (const [origin, cached] of Object.entries(cachedByOrigin)) {
    if (!replacedSet.has(origin)) {
      rows.push(...cached);
    }
  }

  // Dedupe by id (live wins over cache)
  const byId = new Map<string, CacheRow>();
  for (const row of rows) {
    if (!byId.has(row.id)) {
      byId.set(row.id, row);
    }
  }

  const merged = sortRows(Array.from(byId.values()));
  return merged.slice(0, limit);
}

/** Strip live-ness fields for caching (running/command/stop/pending/status). */
export function quiesceAppStateForCache(state: Record<string, unknown>): Record<string, unknown> {
  if (!state || typeof state !== "object") return {};
  const s = { ...state };
  s.is_running = false;
  s.is_command = false;
  s.command_running = false;
  s.stop_requested = false;
  s.pending = false;
  s.status = null;
  s.status_message = null;
  return s;
}

/** Should this state be cached? Settled only. */
export function shouldCacheState(state: Record<string, unknown>): boolean {
  if (!state || typeof state !== "object") return false;
  if (state.is_running) return false;
  if (state.is_command) return false;
  if (state.command_running) return false;
  return true;
}

/** Content cache key: JSON.stringify([profileRoute, convoId]). The route is
 * `string | null` so a read-side probe for an uncached/local route matches the
 * (always-string) write key without inventing a sentinel. */
export function makeContentCacheKey(profileRoute: string | null, convoId: string): string {
  return JSON.stringify([profileRoute, convoId]);
}