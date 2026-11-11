// The last-seen cache (convo-state-cache design §3): up to 5 conversations'
// last live snapshots, keyed by (profile, conversation id). Snapshots leave
// only through `recall`, which quiesces them, so a stale snapshot can never
// claim a live turn.

import type { AppState } from "./state";

/** How many conversations' last snapshots are kept (the current one included). */
export const LAST_SEEN_MAX = 5;

export interface LastSeen {
  /** Record a live snapshot under (profile, its own conversation id).
   *  A snapshot without `conversation.id` is ignored. Re-remembering a
   *  conversation makes it the most recent; past LAST_SEEN_MAX the least
   *  recent is dropped. */
  remember(profile: string | null, state: AppState): void;
  /** The conversation's last snapshot with every in-flight-turn field
   *  quiesced, or null (never seen, evicted, or `convo === null`). Never
   *  returns the stored object itself. */
  recall(profile: string | null, convo: string | null): AppState | null;
}

// JSON pair, not a `${profile}:${convo}` join: profile names may hold any
// character, and a join can collide.
function key(profile: string | null, convo: string): string {
  return JSON.stringify([profile, convo]);
}

// The single exit for stored snapshots: strip every live-turn claim (the
// controls and "busy now" fields) so staleness can't leak as liveness.
function quiesce(s: AppState): AppState {
  return {
    ...s,
    is_running: false,
    command_running: false,
    stop_pending: false,
    pending_input_count: 0,
    status_message: null,
    sub_agent: null,
  };
}

export function createLastSeen(): LastSeen {
  // Insertion order = recency. `recall` never touches it: the live frame
  // that follows a recall re-remembers the conversation anyway.
  const entries = new Map<string, AppState>();

  return {
    remember(profile, state) {
      const convo = state?.conversation?.id;
      if (convo === undefined || convo === null) return;
      const k = key(profile, convo);
      // Delete-then-set moves the entry to the tail (most recent).
      entries.delete(k);
      entries.set(k, state);
      if (entries.size > LAST_SEEN_MAX) {
        const oldest = entries.keys().next().value;
        if (oldest !== undefined) entries.delete(oldest);
      }
    },
    recall(profile, convo) {
      if (convo === null) return null;
      const held = entries.get(key(profile, convo));
      return held === undefined ? null : quiesce(held);
    },
  };
}
