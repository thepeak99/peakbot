// G.6 — the cloud store (design §B): ONE EventSource for the whole app,
// reconnect, and a re-entrancy-guarded `act`.
//
// RED before implementation: `store` is a skeleton whose methods throw
// "not implemented", so every case fails for the right reason (feature absent).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { store } from "./store";
import {
  active,
  disabled,
  FakeEventSource,
  installFakeEventSource,
  json,
  mockFetch,
  pendingWithSas,
  uninstallFakeEventSource,
} from "./testkit";

// The store is a module singleton; every test tears it down first.
afterEach(() => {
  try {
    store.reset();
  } catch {
    /* skeleton: reset throws — the whole suite is RED on purpose */
  }
  uninstallFakeEventSource();
});

beforeEach(() => {
  installFakeEventSource();
  FakeEventSource.reset();
  vi.restoreAllMocks();
});

describe("availability", () => {
  it("a 404 on GET /state → available=false and NO EventSource is opened", () => {
    mockFetch((url) =>
      url.endsWith("/api/cloud/state") ? json(404, { error: "cloud_disabled", message: "" }) : json(200, {}),
    );
    store.reset();
    store.subscribe(() => {});
    expect(store.getSnapshot().available).toBe(false);
    expect(FakeEventSource.instances).toHaveLength(0);
  });

  it("a 200 on GET /state → available=true and exactly ONE EventSource for N subscribers", () => {
    mockFetch((url) =>
      url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {}),
    );
    store.reset();
    const un1 = store.subscribe(() => {});
    const un2 = store.subscribe(() => {});
    const un3 = store.subscribe(() => {});
    expect(store.getSnapshot().available).toBe(true);
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.last()?.url).toBe("/api/cloud/events");
    un1();
    un2();
    un3();
  });

  it("the single EventSource is closed when the LAST subscriber leaves", () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {})));
    store.reset();
    const un1 = store.subscribe(() => {});
    const un2 = store.subscribe(() => {});
    const es = FakeEventSource.last();
    expect(es).toBeDefined();
    un1();
    expect(es!.closed, "still open while one subscriber remains").toBe(false);
    un2();
    expect(es!.closed, "closed when the last subscriber leaves").toBe(true);
  });
});

describe("the snapshot is the single source of truth", () => {
  it("emit replaces the snapshot", () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {})));
    store.reset();
    store.subscribe(() => {});
    const es = FakeEventSource.last()!;
    es.emit("state", pendingWithSas());
    expect(store.getSnapshot().snapshot?.state).toBe("pending");
  });

  it("a stream error KEEPS the last snapshot and sets stale=true", () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {})));
    store.reset();
    store.subscribe(() => {});
    const es = FakeEventSource.last()!;
    es.emit("state", active());
    es.fail();
    const s = store.getSnapshot();
    expect(s.stale).toBe(true);
    expect(s.snapshot?.state, "the last snapshot survives the drop").toBe("active");
  });

  it("reconnect: open() + a new emit → stale=false and the snapshot is replaced", () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {})));
    store.reset();
    store.subscribe(() => {});
    const es = FakeEventSource.last()!;
    es.emit("state", active());
    es.fail();
    expect(store.getSnapshot().stale).toBe(true);
    es.open();
    es.emit("state", pendingWithSas());
    const s = store.getSnapshot();
    expect(s.stale).toBe(false);
    expect(s.snapshot?.state).toBe("pending");
  });

  it("malformed SSE data is ignored and the previous snapshot is kept", () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, disabled()) : json(200, {})));
    store.reset();
    store.subscribe(() => {});
    const es = FakeEventSource.last()!;
    es.emit("state", active());
    es.emit("state", "not-json{{{");
    expect(store.getSnapshot().snapshot?.state).toBe("active");
  });
});

describe("act — the single, re-entrancy-guarded POST", () => {
  it("act never mutates the snapshot", async () => {
    mockFetch((url, init) => {
      if (url.endsWith("/api/cloud/state")) return json(200, disabled());
      if (url.endsWith("/api/cloud/enroll/retry")) return json(200, {});
      void init;
      return json(200, {});
    });
    store.reset();
    store.subscribe(() => {});
    FakeEventSource.last()!.emit("state", disabled());
    const before = store.getSnapshot().snapshot;
    await store.act("/api/cloud/enroll/retry");
    expect(store.getSnapshot().snapshot, "act must not patch the snapshot").toBe(before);
  });

  it("a second act while the first is in flight returns WITHOUT a second fetch", async () => {
    let release: ((r: Response) => void) | undefined;
    const gate = new Promise<Response>((resolve) => {
      release = resolve;
    });
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return json(200, disabled());
      if (url.endsWith("/api/cloud/enroll/retry")) return gate;
      return json(200, {});
    });
    store.reset();
    store.subscribe(() => {});
    const first = store.act("/api/cloud/enroll/retry");
    const second = store.act("/api/cloud/enroll/retry");
    release!(json(200, {}));
    await first;
    await second;
    const fetchMock = vi.mocked(globalThis.fetch);
    const posts = fetchMock.mock.calls.filter(([u]) => String(u).endsWith("/api/cloud/enroll/retry"));
    expect(posts, "the re-entrancy guard sends exactly one request").toHaveLength(1);
  });

  it("a non-2xx stores the server `message` in `error` (snapshot untouched)", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return json(200, disabled());
      if (url.endsWith("/api/cloud/devices/x/revoke")) return json(409, { error: "last_device", message: "You can't revoke the last device; sign out instead." });
      return json(200, {});
    });
    store.reset();
    store.subscribe(() => {});
    await expect(store.act("/api/cloud/devices/x/revoke")).rejects.toBeDefined();
    expect(store.getSnapshot().error).toContain("last device");
  });
});
