// @vitest-environment jsdom
//
// Shared-cache shell-independence tests (design: "Cache/delta/refresh code is
// shared by desktop, browser and mobile — no mobile-only cache gates"; "One
// IndexedDB database for ALL conversation cache data; no localStorage
// conversation cache").
// RED before implementation: ./conversationCache does not exist yet.
//
// These guard the two structural promises that no unit of the cache can prove
// on its own:
//   1. the same database is used under every shell marker (write as desktop,
//      read as mobile ⇒ a hit), and
//   2. conversation data never lands in localStorage (the last-chat pointer
//      may stay).

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import {
  createConversationCache,
  type ConversationCache,
} from "./conversationCache";
import type { ConversationSummary } from "./state";

/** A cached row: the client summary plus the cache's per-origin machine tag
 * (Stage 2 adds `machine` to `ConversationSummary`). */
type CacheRow = ConversationSummary & { machine?: string | null };

const DB_NAME = "peakbot-conversation-cache";

type WithShell = { peakbotShell?: unknown };
const g = globalThis as WithShell;

function installFakeIndexedDB(): void {
  Object.defineProperty(globalThis, "indexedDB", {
    value: fakeIndexedDB,
    writable: true,
    configurable: true,
  });
}

async function resetDb(): Promise<void> {
  const req = indexedDB.deleteDatabase(DB_NAME);
  return new Promise<void>((resolve, reject) => {
    req.onsuccess = () => resolve();
    req.onerror = () => reject(req.error);
  });
}

beforeEach(async () => {
  installFakeIndexedDB();
  await resetDb();
});

afterEach(() => {
  delete g.peakbotShell;
});

describe("no shell gate", () => {
  it("data written under the desktop marker is readable under the mobile marker", async () => {
    const sentinel = "shell-shared-sentinel-42";

    g.peakbotShell = "desktop";
    const desktop = await createConversationCache();
    await desktop.saveOriginList("http://remote-a", [
      {
        id: "convo-1",
        name: sentinel,
        updated_at: "2026-01-05T12:00:00Z",
        message_count: 1,
        model: "m",
        active: false,
        running: false,
        profile: "dev",
        machine: "http://remote-a",
      },
    ]);
    await desktop.close();

    // A fresh boot with the OTHER shell marker must share the same database.
    g.peakbotShell = "mobile";
    const mobile = await createConversationCache();
    const rows = await mobile.getOriginList("http://remote-a");
    await mobile.close();

    expect(rows?.map((r: CacheRow) => r.name)).toContain(sentinel);
  });

  it("a browser (no marker) shares the same cache as both shells", async () => {
    const sentinel = "browser-sentinel-7";

    g.peakbotShell = "mobile";
    const mobile = await createConversationCache();
    await mobile.saveOpenedContent("route", "c1", "http://remote-a", {
      chat: [sentinel],
    });
    await mobile.close();

    delete g.peakbotShell; // browser
    const browser = await createConversationCache();
    const hit = await browser.getOpenedContent("route", "c1");
    await browser.close();

    expect(hit?.state.chat).toEqual([sentinel]);
  });
});

describe("no localStorage conversation cache", () => {
  let cache: ConversationCache;
  beforeEach(async () => {
    window.localStorage.clear();
    cache = await createConversationCache();
  });
  afterEach(async () => cache.close());

  it("storing conversation data never writes it to localStorage", async () => {
    const sentinel = "localstorage-forbidden-sentinel-9";
    await cache.saveOriginList("http://remote-a", [
      {
        id: "convo-1",
        name: sentinel,
        updated_at: "2026-01-05T12:00:00Z",
        message_count: 1,
        model: "m",
        active: false,
        running: false,
        profile: "dev",
        machine: "http://remote-a",
      },
    ]);
    await cache.saveOpenedContent("route", "convo-1", "http://remote-a", {
      chat: [sentinel],
    });

    const dump = Array.from({ length: window.localStorage.length }, (_, i) =>
      window.localStorage.getItem(window.localStorage.key(i) ?? ""),
    ).join("\n");
    expect(dump).not.toContain(sentinel);
    expect(dump).not.toContain("convo-1");
  });
});
