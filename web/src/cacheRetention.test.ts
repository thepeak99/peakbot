// Pure retention/purge tests for the shared remote conversation cache
// (design "Shared remote conversation cache + delta sync — revised design" §7).
// RED before implementation: ./cacheRetention does not exist yet.
//
// Purge contract:
//   - Purge: explicit authenticated-roster device-id present→absent; explicit
//     revoke; self-revoke/logout (cloud only); self-revoke/logout purges cloud
//     caches but not direct-remote caches.
//   - First roster with no prior authenticated roster must NOT purge merely
//     because a cached device is absent.
//   - Persist last authenticated roster for cross-boot comparison.

import { describe, expect, it } from "vitest";
import {
  determinePurges,
  type RosterEntry,
} from "./cacheRetention";

// ─── roster comparison ──────────────────────────────────────────────────

describe("roster comparison", () => {
  it("no prior roster => first roster never triggers purge", () => {
    const prior: RosterEntry[] | null = null;
    const current: RosterEntry[] = [
      { id: "device-a", name: "Alice's Mac" },
      { id: "device-b", name: "Alice's Phone" },
    ];
    expect(determinePurges(prior, current)).toEqual([]);
  });

  it("device absent from roster => purge only that device", () => {
    const prior: RosterEntry[] = [
      { id: "device-a", name: "Alice's Mac" },
      { id: "device-b", name: "Alice's Phone" },
      { id: "device-c", name: "Bob's Laptop" },
    ];
    const current: RosterEntry[] = [
      { id: "device-a", name: "Alice's Mac" },
      { id: "device-b", name: "Alice's Phone" },
    ];
    expect(determinePurges(prior, current)).toEqual([
      "cloud://device-c",
    ]);
  });

  it("device name change same id => no purge (rename retains)", () => {
    const prior: RosterEntry[] = [
      { id: "device-a", name: "Alice's Old Mac" },
    ];
    const current: RosterEntry[] = [
      { id: "device-a", name: "Alice's New Mac" },
    ];
    expect(determinePurges(prior, current)).toEqual([]);
  });

  it("self-revoke/logout purges cloud caches only", () => {
    const prior: RosterEntry[] = [
      { id: "me", name: "Me" },
      { id: "other", name: "Other" },
    ];
    // Simulate me logging out / self-revoking — current roster is empty
    expect(determinePurges(prior, [])).toEqual([
      "cloud://me",
      "cloud://other",
    ]);
    // Direct remote origins are NOT in the roster — they are purged
    // explicitly, not by roster change.
  });
});

