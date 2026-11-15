// Tests for the pure, Intl-free date helpers in dates.ts (design §6).
// Every row of the design's example table is covered. Local-day tests
// construct times directly with the Date constructor so they're robust
// to the machine's timezone without needing process.env.TZ.
//
// RED reason: "Cannot find module './dates'" — the feature is absent.

import { describe, it, expect } from "vitest";
import {
  formatClock,
  formatFull,
  dayLabel,
  sameLocalDay,
  yesterdayOf,
  localeClock,
} from "./dates";

// Build a local Date from components (y, m-1, d, h, mi, s) → epoch ms.
const L = (y: number, m: number, d: number, h: number, mi: number, s: number): number =>
  new Date(y, m - 1, d, h, mi, s).getTime();

describe("formatClock — local HH:MM, 24h, zero-padded", () => {
  it("formatClock_evening", () => {
    expect(formatClock(L(2026, 11, 14, 20, 14, 3))).toBe("20:14");
  });
  it("formatClock_morning_single_digit", () => {
    expect(formatClock(L(2026, 1, 5, 9, 5, 0))).toBe("09:05");
  });
  it("formatClock_midnight", () => {
    expect(formatClock(L(2026, 1, 5, 0, 0, 0))).toBe("00:00");
  });
});

describe("formatFull — local Sat, Nov 14 2026, 20:14:03", () => {
  it("formatFull_november", () => {
    expect(formatFull(L(2026, 11, 14, 20, 14, 3))).toBe("Sat, Nov 14 2026, 20:14:03");
  });
  it("formatFull_january_single_digit_day", () => {
    expect(formatFull(L(2026, 1, 5, 7, 3, 9))).toBe("Mon, Jan 5 2026, 07:03:09");
  });
});

describe("dayLabel — separator labels", () => {
  it("dayLabel_today_at_local_midnight", () => {
    expect(dayLabel(L(2026, 11, 15, 0, 0, 0), L(2026, 11, 15, 0, 0, 30))).toBe("Today");
  });
  it("dayLabel_yesterday_2359", () => {
    expect(dayLabel(L(2026, 11, 14, 23, 59, 59), L(2026, 11, 15, 0, 0, 30))).toBe("Yesterday");
  });
  it("dayLabel_three_days_ago", () => {
    expect(dayLabel(L(2026, 11, 12, 10, 0, 0), L(2026, 11, 15, 9, 0, 0))).toBe("Thu, Nov 12");
  });
  it("dayLabel_yesterday_at_year_boundary_no_year", () => {
    expect(dayLabel(L(2026, 12, 31, 23, 0, 0), L(2027, 1, 1, 10, 0, 0))).toBe("Yesterday");
  });
  it("dayLabel_two_days_ago_at_year_boundary_includes_year", () => {
    expect(dayLabel(L(2026, 12, 31, 23, 0, 0), L(2027, 1, 2, 10, 0, 0))).toBe("Thu, Dec 31 2026");
  });
  it("dayLabel_last_year_includes_year", () => {
    expect(dayLabel(L(2025, 11, 12, 8, 0, 0), L(2026, 11, 15, 9, 0, 0))).toBe("Wed, Nov 12 2025");
  });
  it("dayLabel_leap_day_yesterday", () => {
    expect(dayLabel(L(2028, 2, 29, 12, 0, 0), L(2028, 3, 1, 9, 0, 0))).toBe("Yesterday");
  });
  it("dayLabel_future_no_special_case", () => {
    expect(dayLabel(L(2026, 11, 16, 9, 0, 0), L(2026, 11, 15, 9, 0, 0))).toBe("Mon, Nov 16");
  });
});

describe("dayLabel — local day boundaries", () => {
  it("dayLabel_same_local_day", () => {
    // Two times on the same local day (Nov 14) at 15:00 and 17:00.
    // In TZs with large UTC offsets these span a UTC day boundary,
    // but they're clearly the same local day.
    const a = new Date(2026, 10, 14, 15, 0).getTime();
    const b = new Date(2026, 10, 14, 17, 0).getTime();
    expect(dayLabel(a, b)).toBe("Today");
  });
});

describe("sameLocalDay and yesterdayOf", () => {
  it("sameLocalDay_true_same_day", () => {
    expect(sameLocalDay(L(2026, 11, 1, 0, 30, 0), L(2026, 11, 1, 23, 30, 0))).toBe(true);
  });
  it("sameLocalDay_false_consecutive_days", () => {
    expect(sameLocalDay(L(2026, 11, 1, 23, 59, 59), L(2026, 11, 2, 0, 0, 0))).toBe(false);
  });
  it("yesterdayOf_returns_local_midnight_of_previous_day", () => {
    const now = L(2026, 11, 15, 9, 0, 0);
    const y = yesterdayOf(now);
    const d = new Date(y);
    expect(d.getFullYear()).toBe(2026);
    expect(d.getMonth()).toBe(10); // Nov = 10
    expect(d.getDate()).toBe(14);
    expect(d.getHours()).toBe(0);
    expect(d.getMinutes()).toBe(0);
  });
});

describe("localeClock — desktop header clock", () => {
  it("localeClock_matches_toLocaleTimeString", () => {
    const at = L(2026, 11, 14, 20, 14, 3);
    const expected = new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    expect(localeClock(at)).toBe(expected);
  });
});
