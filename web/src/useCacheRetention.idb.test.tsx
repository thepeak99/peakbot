// @vitest-environment jsdom
//
// Integration test: useCacheRetention must persist/read the last
// authenticated roster through the SAME IndexedDB database used for
// conversation cache data, NOT localStorage.
//
// Contract (design §7):
//   - Roster metadata belongs in IndexedDB alongside conversation data.
//   - localStorage must NEVER be used for cache data or roster metadata.
//   - Self logout / self revoke purges only cloud caches (cloud://* origins).
//   - Transient offline states retain all cached data.
//   - Roster device present->absent purges only that device's cloud cache.
//   - Roster rename (same id) retains.
//   - First authenticated roster never purges.

import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import { applyRetention } from "./useCacheRetention";
import { createConversationCache, type ConversationCache } from "./conversationCache";
import type { StateBody, Device } from "./cloud/types";

// ─── IndexedDB setup ───────────────────────────────────────────────────

const DB_NAME = "peakbot-conversation-cache";
let cache: ConversationCache | null = null;

function reqPromise<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

async function openDb(): Promise<IDBDatabase> {
  const req = indexedDB.open(DB_NAME, 1);
  return reqPromise(req);
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(async () => {
  // Recreate DB fresh each test (the hook opens lazily; tests don't use the hook).
  Object.defineProperty(globalThis, "indexedDB", {
    value: fakeIndexedDB,
    writable: true,
    configurable: true,
  });
  const req = indexedDB.deleteDatabase(DB_NAME);
  await reqPromise(req);
  localStorage.clear();
  cache = await createConversationCache();
});

afterEach(async () => {
  if (cache) {
    await cache.close();
    cache = null;
  }
});

// ─── helpers ────────────────────────────────────────────────────────────

function device(id: string, name: string): Device {
  return {
    id,
    name,
    platform: "desktop",
    online: true,
    slug: id,
    self: false,
  };
}

function activeSnapshot(devices: Device[]): StateBody {
  return {
    state: "active",
    devices,
    requests: [],
    last_login_error: null,
  };
}

function loggedOutSnapshot(): StateBody {
  return {
    state: "logged_out",
    reason: "signed_out",
    last_login_error: null,
  };
}

function trustRevokedSnapshot(): StateBody {
  return {
    state: "trust",
    trust_error: "revoked",
    last_login_error: null,
  };
}

function disabledSnapshot(): StateBody {
  return {
    state: "disabled",
  } as StateBody;
}

function connectingSnapshot(): StateBody {
  return {
    state: "connecting",
    attempt: 1,
    last_error: null,
  } as StateBody;
}

async function seedConvo(origin: string, id: string, route: string) {
  await cache!.saveOpenedContent(route, id, origin, {
    chat: ["hello"],
    conversation: { id },
  });
}

/** Scan the shared IndexedDB database for any object store containing the
 * roster value (identified by a known device id + name). Returns true if
 * found anywhere in the database, false otherwise. */
async function rosterInIndexedDB(deviceId: string, deviceName: string): Promise<boolean> {
  try {
    const db = await openDb();
    for (const storeName of db.objectStoreNames) {
      const tx = db.transaction(storeName, "readonly");
      const store = tx.objectStore(storeName);
      const values = await reqPromise(store.getAll());
      for (const v of values) {
        const json = typeof v === "string" ? v : JSON.stringify(v);
        if (json.includes(deviceId) && json.includes(deviceName)) {
          db.close();
          return true;
        }
      }
    }
    db.close();
    return false;
  } catch {
    return false;
  }
}

// ─── roster persistence via IndexedDB ───────────────────────────────────

describe("useCacheRetention roster persistence", () => {
  it("persists roster in IndexedDB, not localStorage", async () => {
    const snapshot = activeSnapshot([device("device-abc", "Alice's Mac")]);
    await applyRetention(cache!, snapshot);

    // Must NOT write roster to localStorage
    expect(localStorage.getItem("peakbot.cloudRoster")).toBeNull();
    // Also check: any key containing the device id or name
    const dump = Array.from({ length: localStorage.length }, (_, i) =>
      localStorage.getItem(localStorage.key(i) ?? "") ?? "",
    ).join("\n");
    expect(dump).not.toContain("device-abc");
    expect(dump).not.toContain("Alice's Mac");

    // Must persist roster in IndexedDB (same database as cache data)
    expect(await rosterInIndexedDB("device-abc", "Alice's Mac")).toBe(true);
  });

  it("roster survives cache close/reopen (cross-boot)", async () => {
    // First session: active roster with two devices
    const snapshot1 = activeSnapshot([
      device("device-abc", "Alice's Mac"),
      device("device-def", "Alice's Phone"),
    ]);
    await applyRetention(cache!, snapshot1);
    await cache!.close();
    cache = null;

    // Simulate reload: reopen cache
    cache = await createConversationCache();

    // Second session: roster shrinks — one device logged out
    const snapshot2 = activeSnapshot([device("device-abc", "Alice's Mac")]);
    // Seed content for the removed device's cloud origin
    await seedConvo("cloud://device-def", "c1", "phone-profile");
    await applyRetention(cache!, snapshot2);

    // Removed device's cloud content must be purged
    expect(await cache!.getOpenedContent("phone-profile", "c1")).toBeNull();
  });

  it("roster present->absent purges only the removed device", async () => {
    // Seed content for both devices
    await seedConvo("cloud://device-abc", "c1", "mac-profile");
    await seedConvo("cloud://device-def", "c2", "phone-profile");

    // First session: both devices active
    const snapshot1 = activeSnapshot([
      device("device-abc", "Alice's Mac"),
      device("device-def", "Alice's Phone"),
    ]);
    await applyRetention(cache!, snapshot1);

    // Second session: phone removed
    const snapshot2 = activeSnapshot([device("device-abc", "Alice's Mac")]);
    await applyRetention(cache!, snapshot2);

    // Phone's cloud content purged
    expect(await cache!.getOpenedContent("phone-profile", "c2")).toBeNull();
    // Mac's cloud content retained
    expect(await cache!.getOpenedContent("mac-profile", "c1")).not.toBeNull();
  });

  it("roster rename retains content", async () => {
    await seedConvo("cloud://device-abc", "c1", "mac-profile");

    const snapshot1 = activeSnapshot([device("device-abc", "Alice's Old Mac")]);
    await applyRetention(cache!, snapshot1);

    // Rename: same id, new name
    const snapshot2 = activeSnapshot([device("device-abc", "Alice's New Mac")]);
    await applyRetention(cache!, snapshot2);

    // Content retained (rename is not removal)
    expect(await cache!.getOpenedContent("mac-profile", "c1")).not.toBeNull();
  });

  it("first roster never purges existing cached content", async () => {
    // Pre-existing content (e.g., from a stale cached snapshot)
    await seedConvo("cloud://device-xyz", "c1", "stale-profile");

    // First authenticated roster arrives — should NOT purge absent devices
    const snapshot = activeSnapshot([device("device-abc", "Alice's Mac")]);
    await applyRetention(cache!, snapshot);

    // Stale content retained
    expect(await cache!.getOpenedContent("stale-profile", "c1")).not.toBeNull();
  });
});

// ─── self logout / self revoke ──────────────────────────────────────────

describe("self logout and revoke purge cloud only", () => {
  it("logout purges cloud caches, not direct-remote", async () => {
    await seedConvo("cloud://device-abc", "c1", "cloud-profile");
    await seedConvo("http://10.0.0.1:7823", "c2", "direct-profile");

    await applyRetention(cache!, loggedOutSnapshot());

    expect(await cache!.getOpenedContent("cloud-profile", "c1")).toBeNull();
    expect(await cache!.getOpenedContent("direct-profile", "c2")).not.toBeNull();
  });

  it("self-revoke (trust_error: revoked) purges cloud caches, not direct-remote", async () => {
    await seedConvo("cloud://device-abc", "c1", "cloud-profile");
    await seedConvo("http://10.0.0.1:7823", "c2", "direct-profile");

    await applyRetention(cache!, trustRevokedSnapshot());

    expect(await cache!.getOpenedContent("cloud-profile", "c1")).toBeNull();
    expect(await cache!.getOpenedContent("direct-profile", "c2")).not.toBeNull();
  });

  it("self-revoke clears roster from IndexedDB", async () => {
    const snapshot = activeSnapshot([device("device-abc", "Alice's Mac")]);
    await applyRetention(cache!, snapshot);
    expect(await rosterInIndexedDB("device-abc", "Alice's Mac")).toBe(true);

    await applyRetention(cache!, trustRevokedSnapshot());
    // Roster should be cleared after self-revoke
    expect(await rosterInIndexedDB("device-abc", "Alice's Mac")).toBe(false);
  });
});

// ─── transient states retain ────────────────────────────────────────────

describe("transient offline states retain all cached data", () => {
  for (const [label, snapshot] of [
    ["disabled", disabledSnapshot()],
    ["connecting", connectingSnapshot()],
  ] as const) {
    it(`retains on ${label}`, async () => {
      await seedConvo("cloud://device-abc", "c1", "cloud-profile");
      await seedConvo("http://10.0.0.1:7823", "c2", "direct-profile");

      await applyRetention(cache!, snapshot);

      expect(await cache!.getOpenedContent("cloud-profile", "c1")).not.toBeNull();
      expect(await cache!.getOpenedContent("direct-profile", "c2")).not.toBeNull();
    });
  }
});

// ─── multiple origins ──────────────────────────────────────────────────

describe("multiple cloud origins", () => {
  it("purge only the specific removed origin, not all cloud", async () => {
    await seedConvo("cloud://device-a", "c1", "profile-a");
    await seedConvo("cloud://device-b", "c2", "profile-b");
    await seedConvo("cloud://device-c", "c3", "profile-c");

    // First session: three devices
    const snapshot1 = activeSnapshot([
      device("device-a", "A"),
      device("device-b", "B"),
      device("device-c", "C"),
    ]);
    await applyRetention(cache!, snapshot1);

    // Second session: only device-b removed
    const snapshot2 = activeSnapshot([
      device("device-a", "A"),
      device("device-c", "C"),
    ]);
    await applyRetention(cache!, snapshot2);

    expect(await cache!.getOpenedContent("profile-a", "c1")).not.toBeNull();
    expect(await cache!.getOpenedContent("profile-b", "c2")).toBeNull();
    expect(await cache!.getOpenedContent("profile-c", "c3")).not.toBeNull();
  });
});
