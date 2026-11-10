// Pure liveness decision for the SPA's watchdog: a socket is dead when the
// server's heartbeat frame stopped arriving. No DOM, no React — the hook
// (useAgent.ts) calls these with wall-clock times it owns.
//
// The constants are pinned by Rust (src/ui/web/mod.rs checks the literal
// `HEARTBEAT_STALE_MS = 45_000` line via include_str!) and must stay
// strictly above 2× the server's 20 s beat, so a single dropped beat on a
// healthy link cannot trip the watchdog.

export const HEARTBEAT_STALE_MS = 45_000;  // > 2 × the server's 20 s beat
export const WATCHDOG_TICK_MS   = 5_000;

/** Per socket: null until its first `heartbeat`, then the time of its latest frame. */
export type LastHeard = number | null;

/**
 * Record that a frame of `frameType` arrived at `now`. The first
 * `heartbeat` arms the clock (anything before it is untrusted — a server
 * that never beats must never be declared dead); once armed, ANY frame
 * refreshes it, because any traffic proves the pipe is live.
 */
export function heard(last: LastHeard, frameType: unknown, now: number): LastHeard {
  if (frameType === "heartbeat" || last !== null) return now;
  return null;
}

/** A socket is stale when armed and its latest frame is > 45 s old. */
export function isStale(last: LastHeard, now: number): boolean {
  return last !== null && now - last > HEARTBEAT_STALE_MS;
}

export type Trigger = "tick" | "kick";

/**
 * The two ways the watchdog fires:
 * - `tick` (periodic): reconnect only when the socket is OPEN and stale.
 *   Anything not OPEN is already on its way out (close handler) or was
 *   never a socket — touching it would race the reconnect machinery.
 * - `kick` (resume/online/return to visible): reconnect on any non-OPEN
 *   state, because the event itself says the previous state is suspect
 *   (a suspended browser's socket is usually dead but still "open").
 * Comparing against the literal 1 (OPEN) rather than `WebSocket.OPEN`
 * keeps this usable where test fakes define no static constants.
 */
export function shouldReconnect(
  t: Trigger,
  readyState: number | null,
  last: LastHeard,
  now: number,
): boolean {
  if (t === "kick") return readyState !== 1 || isStale(last, now);
  return readyState === 1 && isStale(last, now);
}
