import type { ConversationSummary } from "./state";

export type Range = readonly [start: number, end: number];

export interface ChatRow {
  readonly chat: ConversationSummary;
  readonly titleMatches: readonly Range[];
}

export type ChatSection = {
  kind: "running" | "live" | "today" | "yesterday" | "earlier" | "matches";
  label: string;
  rows: ChatRow[];
};

export interface Activity {
  readonly running: ReadonlySet<string>;
  readonly unread: ReadonlySet<string>;
}

export const NO_ACTIVITY: Activity = { running: new Set(), unread: new Set() };

// Month names from a fixed English array — the spec bans Intl, so a literal
// table is the only stable source for relative-time labels.
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// Compare local calendar fields, not UTC: a DST shift changes the UTC day while
// the user's clock day stays the same.
const dayKey = (d: Date): [number, number, number] => [d.getFullYear(), d.getMonth(), d.getDate()];
const sameDay = (a: Date, b: Date): boolean => {
  const [ay, am, ad] = dayKey(a);
  const [by, bm, bd] = dayKey(b);
  return ay === by && am === bm && ad === bd;
};
// The instant "one day before now" in the local calendar; the Date constructor
// normalises month boundaries so 1 Jan -> 31 Dec needs no special cases.
const yesterdayOf = (now: Date): Date => new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);

// Timestamp in ms, or undefined when the field is blank/unparseable. A blank
// string is the old-server shape, so we treat it as unparseable on purpose.
const when = (s: string): number | undefined => {
  if (s === "") return undefined;
  const t = new Date(s).getTime();
  return Number.isNaN(t) ? undefined : t;
};

// Labels in strict first-match order; minutes beat days and days beat years.
// "just now" also covers future timestamps, matching the fixture table.
export function relativeTime(updatedAt: string, now: Date): string {
  const t = when(updatedAt);
  if (t === undefined) return "";
  const diff = now.getTime() - t;
  if (diff < 60_000) return "just now";
  const min = Math.floor(diff / 60_000);
  if (min < 60) return `${min}m ago`;
  if (sameDay(new Date(t), now)) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (sameDay(new Date(t), yesterdayOf(now))) return "Yesterday";
  const d = new Date(t);
  if (d.getFullYear() === now.getFullYear()) return `${d.getDate()} ${MONTHS[d.getMonth()]}`;
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
}

// Join the count/model/relative segments with " · ", dropping any that are
// empty (0 messages, no model, unparseable timestamp) rather than printing
// them blank.
export function metaLine(chat: ConversationSummary, now: Date): string {
  const parts: string[] = [];
  const n = chat.message_count;
  if (n > 0) parts.push(`${n} ${n === 1 ? "msg" : "msgs"}`);
  if (chat.model) parts.push(chat.model);
  const rel = relativeTime(chat.updated_at, now);
  if (rel) parts.push(rel);
  return parts.join(" · ");
}

export function filterChats(
  list: readonly ConversationSummary[],
  query: string,
  profile: string | null,
): ChatRow[] {
  const q = query.trim().toLowerCase();
  const rows: ChatRow[] = [];
  for (const chat of list) {
    // Profile first (the "All" tab is null); legacy rows have profile null
    // so a named profile never includes them.
    if (profile !== null && (chat.profile ?? null) !== profile) continue;
    if (q === "") {
      rows.push({ chat, titleMatches: [] });
      continue;
    }
    const lower = chat.name.toLowerCase();
    if (!lower.includes(q)) continue;
    // Non-overlapping occurrences, left to right; positions are only valid
    // when lowercasing is 1:1 (some Unicode chars change length), otherwise
    // the row still matches but simply cannot be highlighted safely.
    const matches: Range[] = [];
    if (chat.name.length === lower.length) {
      let from = 0;
      let idx = lower.indexOf(q, from);
      while (idx !== -1) {
        matches.push([idx, idx + q.length]);
        from = idx + q.length;
        idx = lower.indexOf(q, from);
      }
    }
    rows.push({ chat, titleMatches: matches });
  }
  return rows;
}

// Section kind from the running/live flags first, then the local calendar day:
// same day OR strictly after now -> "today"; the previous calendar day ->
// "yesterday"; older or unparseable -> "earlier".
type SectionKind = "running" | "live" | "today" | "yesterday" | "earlier";
const kindOf = (c: ConversationSummary, now: Date): SectionKind => {
  if (c.running === true) return "running";
  if (c.active) return "live";
  const t = when(c.updated_at);
  if (t === undefined) return "earlier";
  if (sameDay(new Date(t), now) || t > now.getTime()) return "today";
  if (sameDay(new Date(t), yesterdayOf(now))) return "yesterday";
  return "earlier";
};
// Section labels, literal per the design table.
const LABELS: Record<SectionKind, string> = {
  running: "RUNNING NOW",
  live: "LIVE",
  today: "TODAY",
  yesterday: "YESTERDAY",
  earlier: "EARLIER",
};

// In-section ordering: updated_at descending (unparseable counts as the
// oldest), ties broken by id ascending.
const ts = (c: ConversationSummary): number => when(c.updated_at) ?? -Infinity;
const sortRows = (rows: readonly ChatRow[]): ChatRow[] =>
  rows.slice().sort((a, b) => {
    const ta = ts(a.chat);
    const tb = ts(b.chat);
    if (ta !== tb) return tb - ta;
    if (a.chat.id < b.chat.id) return -1;
    if (a.chat.id > b.chat.id) return 1;
    return 0;
  });

export function groupChats(rows: readonly ChatRow[], now: Date, searching: boolean): ChatSection[] {
  // One pass buckets rows by section kind; the fixed order below keeps
  // running/live/today/yesterday/earlier precedence, and `searching`
  // collapses the same buckets into running -> live -> combined rest.
  const buckets: Record<SectionKind, ChatRow[]> = {
    running: [],
    live: [],
    today: [],
    yesterday: [],
    earlier: [],
  };
  for (const r of rows) buckets[kindOf(r.chat, now)].push(r);
  if (searching) {
    // The matches section is the same buckets re-ordered: running, then live,
    // then every other section joined and re-sorted by recency.
    const rest = [...buckets.today, ...buckets.yesterday, ...buckets.earlier];
    const all = [sortRows(buckets.running), sortRows(buckets.live), sortRows(rest)].flat();
    if (all.length === 0) return [];
    return [{ kind: "matches", label: all.length === 1 ? "1 MATCH" : `${all.length} MATCHES`, rows: all }];
  }
  const out: ChatSection[] = [];
  for (const k of (["running", "live", "today", "yesterday", "earlier"] as const)) {
    if (buckets[k].length === 0) continue;
    out.push({ kind: k, label: LABELS[k], rows: sortRows(buckets[k]) });
  }
  return out;
}