import type { WireChatMessage } from "./state";

/** O(1) seed estimate of a transcript row's pixel height, fed to the
 *  virtualizer's estimateSize. Never scans content — String.length is O(1).
 *  Must stay cheap: the virtualizer calls this for every unmeasured row on
 *  every append in end-anchored mode. */
const CHROME = 46;   // border + padding + header line
const LINE = 22;     // one wrapped line at text-sm/leading-relaxed
const CPL = 90;      // chars per line at max-w-5xl
const MIN = 68;
const MAX = 1200;    // cap: a 4000px tool result must not wreck the initial scrollbar

export function estimateRowHeight(m: WireChatMessage): number {
  const lines = Math.max(1, Math.ceil((m.content?.length ?? 0) / CPL));
  return Math.min(MAX, Math.max(MIN, CHROME + LINE * lines));
}
