// IndexedDB shared conversation cache (design §2, §4, §7).
// ONE database for ALL conversation cache data. Per-origin list snapshots,
// per-conversation opened content, origin metadata for revocation cleanup.
// Best-effort: cache unavailable/quota/corrupt → cache miss, never crash.

import type { ConversationSummary } from "./state";
import { makeContentCacheKey } from "./conversationMerge";

export interface CacheRow extends ConversationSummary {
  machine?: string | null;
}

export interface CacheLimits {
  maxEntries: number;
  maxTotalBytes: number;
  maxEntryBytes: number;
}

export const DEFAULT_CACHE_LIMITS: CacheLimits = {
  maxEntries: 20,
  maxTotalBytes: 24 * 1024 * 1024,
  maxEntryBytes: 8 * 1024 * 1024,
};

export interface CachedContent {
  state: Record<string, unknown>;
  origin: string | null;
}

const DB_NAME = "peakbot-conversation-cache";
const DB_VERSION = 1;
const ORIGIN_LIST_STORE = "origin_lists";
const CONTENT_STORE = "content";
// Roster metadata (the last authenticated cloud device list) lives in the SAME
// database as conversation data — never localStorage (design §7). It gets its
// own store so list reads (`listOrigins`) and content eviction never see it.
const META_STORE = "meta";
const ROSTER_KEY = "authenticated_roster";

/** Last authenticated cloud roster entry — stable device id + display name.
 * Metadata only; never conversation rows. */
export interface AuthenticatedRosterEntry {
  id: string;
  name: string;
}

interface ContentRow {
  key: string;
  state: unknown;
  origin: string | null;
  ts: number;
}

export interface ConversationCache {
  saveOriginList(origin: string, rows: CacheRow[]): Promise<void>;
  getOriginList(origin: string): Promise<CacheRow[] | null>;
  listOrigins(): Promise<string[]>;
  clearOrigin(origin: string): Promise<void>;
  saveAuthenticatedRoster(roster: AuthenticatedRosterEntry[]): Promise<void>;
  getAuthenticatedRoster(): Promise<AuthenticatedRosterEntry[] | null>;
  saveOpenedContent(
    profileRoute: string,
    convoId: string,
    origin: string | null,
    state: unknown,
    options?: { now?: number },
  ): Promise<void>;
  /** Read a conversation snapshot. `profileRoute` is the key component, so a
   * `null` (no profile route) probe is allowed on the read side — writes are
   * still restricted to `string`, so local content is never cached. */
  getOpenedContent(
    profileRoute: string | null,
    convoId: string,
  ): Promise<CachedContent | null>;
  clearOriginContent(origin: string): Promise<void>;
  purgeOrigin(origin: string): Promise<void>;
  purgeCloud(): Promise<void>;
  close(): Promise<void>;
}

function reqPromise<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function txPromise(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve) => {
    tx.oncomplete = () => resolve();
    tx.onerror = () => resolve();
  });
}

export async function createConversationCache(
  limits: Partial<CacheLimits> = {},
): Promise<ConversationCache> {
  const effectiveLimits: CacheLimits = {
    ...DEFAULT_CACHE_LIMITS,
    ...limits,
  };

  let db: IDBDatabase | null = null;
  let dbAvailable = false;
  let dbFailed = false;

  async function openDb(): Promise<void> {
    if (dbFailed || db) return;
    try {
      const indexedDB = globalThis.indexedDB;
      if (!indexedDB) {
        dbFailed = true;
        return;
      }
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        const d = req.result;
        if (!d.objectStoreNames.contains(ORIGIN_LIST_STORE)) {
          d.createObjectStore(ORIGIN_LIST_STORE);
        }
        if (!d.objectStoreNames.contains(CONTENT_STORE)) {
          const store = d.createObjectStore(CONTENT_STORE, { keyPath: "key" });
          store.createIndex("origin", "origin", { unique: false });
        }
        if (!d.objectStoreNames.contains(META_STORE)) {
          d.createObjectStore(META_STORE);
        }
      };
      db = await reqPromise(req);
      dbAvailable = true;
    } catch {
      dbFailed = true;
    }
  }

  async function saveOriginList(origin: string, rows: CacheRow[]): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;
    try {
      const tx = db.transaction(ORIGIN_LIST_STORE, "readwrite");
      tx.objectStore(ORIGIN_LIST_STORE).put(JSON.stringify(rows), origin);
      await txPromise(tx);
    } catch {
      // quota failure, best-effort
    }
  }

  async function getOriginList(origin: string): Promise<CacheRow[] | null> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return null;
    try {
      const tx = db.transaction(ORIGIN_LIST_STORE, "readonly");
      const req = tx.objectStore(ORIGIN_LIST_STORE).get(origin);
      const result = await reqPromise(req);
      if (result === undefined) return null;
      return JSON.parse(result);
    } catch {
      return null;
    }
  }

  async function listOrigins(): Promise<string[]> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return [];
    try {
      const tx = db.transaction(ORIGIN_LIST_STORE, "readonly");
      const keys = await reqPromise(tx.objectStore(ORIGIN_LIST_STORE).getAllKeys());
      return keys.filter((k): k is string => typeof k === "string");
    } catch {
      return [];
    }
  }

  async function clearOrigin(origin: string): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;
    try {
      const tx = db.transaction(ORIGIN_LIST_STORE, "readwrite");
      tx.objectStore(ORIGIN_LIST_STORE).delete(origin);
      await txPromise(tx);
    } catch {
      // best-effort
    }
  }

  // ─── authenticated roster (cloud device metadata, persisted in IDB) ──

  async function saveAuthenticatedRoster(roster: AuthenticatedRosterEntry[]): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;
    try {
      const tx = db.transaction(META_STORE, "readwrite");
      tx.objectStore(META_STORE).put(JSON.stringify(roster), ROSTER_KEY);
      await txPromise(tx);
    } catch {
      // best-effort
    }
  }

  async function getAuthenticatedRoster(): Promise<AuthenticatedRosterEntry[] | null> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return null;
    try {
      const tx = db.transaction(META_STORE, "readonly");
      const raw = await reqPromise(tx.objectStore(META_STORE).get(ROSTER_KEY));
      if (typeof raw !== "string") return null;
      const parsed = JSON.parse(raw) as unknown;
      if (!Array.isArray(parsed)) return null;
      const entries: AuthenticatedRosterEntry[] = [];
      for (const item of parsed) {
        if (
          item &&
          typeof item === "object" &&
          typeof item.id === "string" &&
          typeof item.name === "string"
        ) {
          entries.push({ id: item.id, name: item.name });
        }
      }
      return entries;
    } catch {
      return null;
    }
  }

  async function clearAuthenticatedRoster(): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;
    try {
      const tx = db.transaction(META_STORE, "readwrite");
      tx.objectStore(META_STORE).delete(ROSTER_KEY);
      await txPromise(tx);
    } catch {
      // best-effort
    }
  }

  // ─── per-conversation opened content ──────────────────────────────────

  async function saveOpenedContent(
    profileRoute: string,
    convoId: string,
    origin: string | null,
    state: unknown,
    options?: { now?: number },
  ): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;

    const key = makeContentCacheKey(profileRoute, convoId);
    const serialized = JSON.stringify(state);
    const byteCount = serialized.length; // approximation

    if (byteCount > effectiveLimits.maxEntryBytes) return;

    const now = options?.now ?? Date.now();

    try {
      // Read existing entries
      let allEntries: ContentRow[] = [];
      {
        const tx = db.transaction(CONTENT_STORE, "readonly");
        const req = tx.objectStore(CONTENT_STORE).getAll();
        allEntries = await new Promise((resolve) => {
          req.onsuccess = () => resolve(req.result || []);
        });
      }

      // Remove existing entry for this key
      const toKeep = allEntries.filter((e) => e.key !== key);

      // Evict oldest to stay under entry limit
      toKeep.sort((a, b) => a.ts - b.ts);
      while (toKeep.length >= effectiveLimits.maxEntries) {
        toKeep.shift();
      }

      // Calculate total bytes and evict if over budget
      let totalBytes = 0;
      for (const entry of toKeep) {
        totalBytes += JSON.stringify(entry.state).length;
      }
      if (totalBytes + byteCount > effectiveLimits.maxTotalBytes) {
        while (toKeep.length > 0) {
          const oldest = toKeep.shift();
          if (oldest) {
            totalBytes -= JSON.stringify(oldest.state).length;
          }
          if (totalBytes + byteCount <= effectiveLimits.maxTotalBytes) break;
        }
      }

      // Write in a new transaction: clear all, then write back only what we keep
      const tx = db.transaction(CONTENT_STORE, "readwrite");
      const store = tx.objectStore(CONTENT_STORE);
      store.clear();
      for (const entry of toKeep) {
        store.put(entry);
      }
      store.put({ key, state, origin, ts: now });
    } catch {
      // quota failure, best-effort
    }
  }

  async function getOpenedContent(
    profileRoute: string | null,
    convoId: string,
  ): Promise<CachedContent | null> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return null;

    const key = makeContentCacheKey(profileRoute, convoId);
    try {
      const tx = db.transaction(CONTENT_STORE, "readonly");
      const req = tx.objectStore(CONTENT_STORE).get(key);
      const result = await reqPromise(req);
      if (result === undefined) return null;
      return { state: result.state, origin: result.origin };
    } catch {
      return null;
    }
  }

  async function clearOriginContent(origin: string): Promise<void> {
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;

    try {
      const tx = db.transaction(CONTENT_STORE, "readwrite");
      const store = tx.objectStore(CONTENT_STORE);
      const index = store.index("origin");

      // Read all entries for this origin
      const req = index.getAll(origin);
      const entries = await reqPromise(req);
      for (const entry of entries) {
        store.delete(entry.key);
      }
      await txPromise(tx);
    } catch {
      // best-effort
    }
  }

  async function purgeOrigin(origin: string): Promise<void> {
    await clearOrigin(origin);
    await clearOriginContent(origin);
  }

  /** Clear every cloud-origin cache (self logout / self revoke). Direct
   * remote origins are untouched — they are purged explicitly, not by roster.
   * Also clears the authenticated roster so a fresh auth can't diff against
   * stale prior devices (the roster *is* cloud metadata, not direct cache). */
  async function purgeCloud(): Promise<void> {
    await clearAuthenticatedRoster();
    if (!dbAvailable && !dbFailed) await openDb();
    if (!db || !dbAvailable) return;
    const isCloud = (o: unknown): o is string =>
      typeof o === "string" && o.startsWith("cloud://");

    try {
      const originsTx = db.transaction(ORIGIN_LIST_STORE, "readwrite");
      const originStore = originsTx.objectStore(ORIGIN_LIST_STORE);
      const keys = await reqPromise(originStore.getAllKeys());
      for (const key of keys) {
        if (isCloud(key)) originStore.delete(key);
      }
    } catch {
      // best-effort
    }

    try {
      const contentTx = db.transaction(CONTENT_STORE, "readwrite");
      const contentStore = contentTx.objectStore(CONTENT_STORE);
      const entries = await reqPromise(contentStore.getAll());
      for (const entry of entries as ContentRow[]) {
        if (isCloud(entry.origin)) contentStore.delete(entry.key);
      }
    } catch {
      // best-effort
    }
  }

  async function close(): Promise<void> {
    if (db) {
      db.close();
      db = null;
      dbAvailable = false;
    }
  }

  // Open on creation
  await openDb();

  return {
    saveOriginList,
    getOriginList,
    listOrigins,
    clearOrigin,
    saveAuthenticatedRoster,
    getAuthenticatedRoster,
    saveOpenedContent,
    getOpenedContent,
    clearOriginContent,
    purgeOrigin,
    purgeCloud,
    close,
  };
}

// One shared cache per page: desktop, browser and mobile all use this single
// IndexedDB database (no shell gate). Lazily created; best-effort.
let sharedCache: Promise<ConversationCache> | null = null;

export function getSharedConversationCache(): Promise<ConversationCache> {
  if (!sharedCache) sharedCache = createConversationCache();
  return sharedCache;
}