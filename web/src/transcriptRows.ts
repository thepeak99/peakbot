import type { WireChatMessage } from "./state";

/** O(1) seed estimate of a transcript row's pixel height, fed to the
 *  virtualizer's estimateSize. Never scans content — String.length is O(1).
 *  Must stay cheap: the virtualizer calls this for every unmeasured row on
 *  every append in end-anchored mode.
 *
 *  Thinking blocks (Anthropic extended thinking, only present on the wire when
 *  the server-side `display_reasoning` gate is open) are folded into the line
 *  count so an expanded `<details>` row doesn't punch a hole in the
 *  end-anchored scroll: the collapsed chrome is ~28px, but expanded thinking
 *  can run hundreds of lines, and the virtualizer only self-corrects after the
 *  next `measureElement` pass. Adding the sum of `thinking[].text.length`
 *  keeps the estimate honest for the "fully expanded" case without scanning
 *  the content. Absent `thinking` is a zero contribution. */
const CHROME = 46;   // border + padding + header line
const THINKING_CHROME = 28; // <details> collapsed: summary line + padding + border
const LINE = 22;     // one wrapped line at text-sm/leading-relaxed
const CPL = 90;      // chars per line at max-w-5xl
const MIN = 68;
const MAX = 1200;    // cap: a 4000px tool result must not wreck the initial scrollbar

export function estimateRowHeight(m: WireChatMessage): number {
  const contentChars = m.content?.length ?? 0;
  // Thinking contributes both its collapsed chrome (~28px) and the wrapped
  // lines of its content. An empty array on the wire (display_reasoning off)
  // is the common case — it must stay O(1) and must not throw when no field
  // is present at all. Defensive on `text` too: the wire serializer only
  // emits `{ text: string }`, but a custom client could send anything.
  let thinkingChars = 0;
  if (m.thinking && m.thinking.length > 0) {
    for (const t of m.thinking) thinkingChars += t.text?.length ?? 0;
  }
  const chars = contentChars + thinkingChars;
  const lines = Math.max(1, Math.ceil(chars / CPL));
  const base = CHROME + (thinkingChars > 0 ? THINKING_CHROME : 0) + LINE * lines;
  return Math.min(MAX, Math.max(MIN, base));
}
