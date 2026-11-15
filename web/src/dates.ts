// Pure, Intl-free local-time date helpers. The phone transcript shows day
// separators and times in the user's local timezone; the desktop keeps its
// existing locale clock. These functions avoid Intl APIs so the phone clock
// is deterministic across browsers (spec: "the spec bans Intl").

export const MONTHS: readonly string[] = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DOWS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** Same local calendar day (not UTC: DST and offsets move the UTC day). */
export function sameLocalDay(a: number, b: number): boolean {
  const da = new Date(a);
  const db = new Date(b);
  return (
    da.getFullYear() === db.getFullYear() &&
    da.getMonth() === db.getMonth() &&
    da.getDate() === db.getDate()
  );
}

/** Local midnight-normalised instant one calendar day before `now`. */
export function yesterdayOf(now: number): number {
  const d = new Date(now);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() - 1).getTime();
}

/** Local "HH:MM", 24h, zero-padded. */
export function formatClock(at: number): string {
  const d = new Date(at);
  const h = d.getHours().toString().padStart(2, "0");
  const m = d.getMinutes().toString().padStart(2, "0");
  return `${h}:${m}`;
}

/** Local "Sat, Nov 14 2026, 20:14:03". */
export function formatFull(at: number): string {
  const d = new Date(at);
  const dow = DOWS[d.getDay()];
  const mon = MONTHS[d.getMonth()];
  const day = d.getDate();
  const year = d.getFullYear();
  const h = d.getHours().toString().padStart(2, "0");
  const m = d.getMinutes().toString().padStart(2, "0");
  const s = d.getSeconds().toString().padStart(2, "0");
  return `${dow}, ${mon} ${day} ${year}, ${h}:${m}:${s}`;
}

/** Separator label: "Today" | "Yesterday" | "Thu, Nov 12" | "Wed, Nov 12 2025" (year iff ≠ now's year). */
export function dayLabel(at: number, now: number): string {
  if (sameLocalDay(at, now)) return "Today";
  const y = yesterdayOf(now);
  if (sameLocalDay(at, y)) return "Yesterday";
  const d = new Date(at);
  const dow = DOWS[d.getDay()];
  const mon = MONTHS[d.getMonth()];
  const day = d.getDate();
  if (d.getFullYear() === new Date(now).getFullYear()) {
    return `${dow}, ${mon} ${day}`;
  }
  return `${dow}, ${mon} ${day} ${d.getFullYear()}`;
}

/** Desktop header clock, the locale's short time exactly as `toClock` did. */
export function localeClock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}