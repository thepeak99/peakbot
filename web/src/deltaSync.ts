// Pure helpers for the shared cache's delta sync + moving anchor.
//
// The server is authoritative and one-way: every `state` frame is absolute
// against the socket anchor. A suffix frame carries `messages_from` — the
// length of the prefix it is relative to — plus only the rows after it.
// Timestamps are compared as raw strings, never parsed: identical instants
// can serialise differently (nanoseconds/offset), so `Date.parse` would
// accept a cursor the server will reject.

import type { Anchor, AppState } from "./state";

/** Prefix cursor sent on attach and on a periodic `sync_anchor`. */
export type { Anchor };

/** How many newly applied authoritative rows move the anchor. */
export const ANCHOR_EVERY = 10;

const isCount = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value > 0;

const isEpoch = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

/** Lenient wire parse: any malformed shape is null, never a throw. */
export function parseAnchor(raw: unknown): Anchor | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const object = raw as Record<string, unknown>;
  const { n, timestamp, epoch } = object;
  if (!isCount(n) || !isEpoch(epoch) || typeof timestamp !== "string") {
    return null;
  }
  return { n, timestamp, epoch };
}

/** Accept `a` iff it is exactly a prefix of `msgs` for `epoch`. */
export function validateAnchor(
  a: Anchor,
  epoch: number,
  msgs: readonly { timestamp: string }[],
): Anchor | null {
  if (!isCount(a.n) || a.n > msgs.length || a.epoch !== epoch) return null;
  return msgs[a.n - 1].timestamp === a.timestamp ? a : null;
}

/** Cursor for the current base, or null when no prefix can be formed. */
export function anchorFor(
  msgs: readonly { timestamp: string }[],
  epoch: number,
): Anchor | null {
  if (msgs.length === 0 || !isEpoch(epoch)) return null;
  const timestamp = msgs[msgs.length - 1].timestamp;
  if (typeof timestamp !== "string") return null;
  return { n: msgs.length, timestamp, epoch };
}

/**
 * Rebuild a full state from a cached prefix and an absolute suffix.
 *
 * Uses `current.messages[0..messages_from]` as the prefix rather than a
 * separately retained snapshot: rows before `messages_from` are identical in
 * both (the server only appends between lineage breaks), and this is what
 * keeps an old-base frame in flight across a rebase decodable. Returns null —
 * never a bare tail — when the base cannot be proven.
 */
export function mergeSuffix(
  current: AppState | null,
  frame: AppState,
  messages_from: number | undefined,
): AppState | null {
  if (messages_from === undefined || current === null) return null;
  if (!isCount(messages_from)) return null;
  const prefix = current.chat?.messages;
  const suffix = frame.chat?.messages;
  if (!Array.isArray(prefix) || !Array.isArray(suffix)) return null;
  if (messages_from > prefix.length) return null;
  if (current.history_epoch !== frame.history_epoch) return null;

  return {
    ...frame,
    chat: {
      ...frame.chat,
      messages: [...prefix.slice(0, messages_from), ...suffix],
    },
  };
}

/** Fire on every non-zero multiple of `every` applied rows. */
export function shouldSyncAnchor(
  applied: number,
  every: number,
  deltaSupported: boolean,
): boolean {
  return (
    deltaSupported &&
    every > 0 &&
    Number.isInteger(applied) &&
    applied > 0 &&
    applied % every === 0
  );
}

/** The additive inbound frame that moves the socket's base. */
export function syncAnchorFrame(a: Anchor): {
  type: "sync_anchor";
  since: Anchor;
} {
  return { type: "sync_anchor", since: a };
}
