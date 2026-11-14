import type { ConversationSummary, WireMachineStatus } from "./state";
import type { CatalogProfile, MachineId } from "./types";
import type { Device } from "./cloud/types";

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

// Two sets are equal iff they hold the same ids — this is what tells the
// caller "nothing changed" apart from a fresh structurally-identical set,
// so unchanged activity keeps the same object references.
const sameSet = (a: ReadonlySet<string>, b: ReadonlySet<string>): boolean =>
  a.size === b.size && [...a].every((id) => b.has(id));

// Fold previous activity into the fresh list: running is the union of listed
// chats currently running and previously-running chats no longer listed
// ("unknown is not finished"); unread is previous unread plus each
// running->idle edge, minus the chat being viewed. Returns `prev` when
// nothing changed so callers can reuse the same references.
export function foldActivity(
  prev: Activity,
  list: readonly ConversationSummary[],
  viewing: string | null,
): Activity {
  const listed = new Set<string>(list.map((c) => c.id));
  const byId = new Map<string, boolean>(list.map((c) => [c.id, c.running === true]));
  const running = new Set<string>(list.filter((c) => c.running === true).map((c) => c.id));
  for (const id of prev.running) if (!listed.has(id)) running.add(id);
  const unread = new Set<string>(prev.unread);
  for (const id of prev.running) if (listed.has(id) && !byId.get(id)) unread.add(id);
  if (viewing !== null) unread.delete(viewing);
  if (sameSet(prev.running, running) && sameSet(prev.unread, unread)) return prev;
  return { running, unread };
}

// ─── Machine derivation + filter (mobile-redesign design §2.4) ────────────
//
// A "machine" is this device (id `null`) or one remote origin. Pure: derived
// from the catalog (profile → machine) and the last conversations_list's
// optional machine statuses. No React, no DOM.

export const LOCAL_MACHINE_NAME = "This device";

export interface Machine {
  readonly id: MachineId;
  readonly name: string;
  readonly online: boolean;
  readonly profiles: readonly [string, ...string[]];
}

export type MachineFilter = { kind: "all" } | { kind: "one"; id: MachineId };

export const ALL_MACHINES: MachineFilter = { kind: "all" };

/** The machine a conversation/profile lives on: the catalog entry's machine;
 * a null or unknown profile is local (legacy rows; remote rows are always
 * retagged with a live local name). */
export function machineOf(
  catalog: readonly CatalogProfile[],
  profile: string | null,
): MachineId {
  if (profile === null) return null;
  return catalog.find((p) => p.name === profile)?.machine ?? null;
}

/** null → "This device"; a classic remote origin → its hostname (display-only,
 * ids and filters keep the full origin). A bare cloud `slug` (no `://`) is
 * looked up in `cloudDevices` and falls back to the slug — it must NEVER hit
 * `new URL` on a bare name (that throws). Total: never throws. */
export function machineName(id: MachineId, cloudDevices: readonly Device[] = []): string {
  if (id === null) return LOCAL_MACHINE_NAME;
  if (!id.includes("://")) return cloudDevices.find((d) => d.slug === id)?.name ?? id;
  try {
    return new URL(id).hostname;
  } catch {
    return id;
  }
}

/** One entry per distinct catalog machine: local first (only if any local
 * profile exists), then remotes in first-catalog-appearance order. Local is
 * always online; a remote follows its status, or is online when it has no
 * status (unknown ≠ offline). A status for an origin the catalog doesn't
 * carry is ignored. */
export function deriveMachines(
  catalog: readonly CatalogProfile[],
  statuses: readonly WireMachineStatus[],
  cloudDevices: readonly Device[] = [],
): Machine[] {
  const localProfiles: string[] = [];
  const remoteOrder: string[] = [];
  const remoteProfiles = new Map<string, string[]>();
  for (const p of catalog) {
    if (p.machine === null) {
      localProfiles.push(p.name);
    } else {
      const existing = remoteProfiles.get(p.machine);
      if (existing) {
        existing.push(p.name);
      } else {
        remoteProfiles.set(p.machine, [p.name]);
        remoteOrder.push(p.machine);
      }
    }
  }
  const statusById = new Map(statuses.map((s) => [s.id, s.online]));
  const out: Machine[] = [];
  if (localProfiles.length > 0) {
    out.push({
      id: null,
      name: LOCAL_MACHINE_NAME,
      online: true,
      profiles: localProfiles as [string, ...string[]],
    });
  }
  for (const id of remoteOrder) {
    out.push({
      id,
      name: machineName(id, cloudDevices),
      online: statusById.get(id) ?? true,
      profiles: remoteProfiles.get(id) as [string, ...string[]],
    });
  }
  return out;
}

/** The effective filter: All when fewer than 2 machines exist, or when a
 * "one" names a machine not in `machines` (stale storage). Storage is not
 * rewritten. */
export function resolveFilter(f: MachineFilter, machines: readonly Machine[]): MachineFilter {
  if (f.kind === "all") return ALL_MACHINES;
  if (machines.length < 2) return ALL_MACHINES;
  if (!machines.some((m) => m.id === f.id)) return ALL_MACHINES;
  return f;
}

/** Rows on the filtered machine (All = unchanged list, in input order). */
export function filterByMachine(
  list: readonly ConversationSummary[],
  catalog: readonly CatalogProfile[],
  f: MachineFilter,
): ConversationSummary[] {
  if (f.kind === "all") return [...list];
  return list.filter((row) => machineOf(catalog, row.profile ?? null) === f.id);
}