// @vitest-environment jsdom
//
// Tests for `mobileStorage.ts` — the localStorage boundary for the mobile
// shell (design §3.6). `parseLastChat`/`resumeTarget` are pure; `loadLastChat`
// / `saveLastChat`/`bootMobileChats` touch storage and the URL.
//
// RED before implementation: `./mobileStorage` does not exist yet, so this
// file fails at import — the right reason (feature absent).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  LAST_CHAT_KEY,
  bootMobileChats,
  loadLastChat,
  parseLastChat,
  resumeTarget,
  saveLastChat,
} from "./mobileStorage";

// A storage whose accessors throw — stands in for a browser that denies
// localStorage (private mode, quota, an extension).
const throwingStorage = (): Storage =>
  ({
    getItem: () => {
      throw new Error("denied");
    },
    setItem: () => {
      throw new Error("denied");
    },
    removeItem: () => {
      throw new Error("denied");
    },
    clear: () => {
      throw new Error("denied");
    },
    key: () => null,
    get length() {
      return 0;
    },
  }) as unknown as Storage;

let originalStorage: Storage;

beforeEach(() => {
  originalStorage = window.localStorage;
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  Object.defineProperty(window, "localStorage", {
    value: originalStorage,
    configurable: true,
    writable: true,
  });
  window.localStorage.clear();
  window.history.replaceState(null, "", "/");
  vi.restoreAllMocks();
});

describe("LAST_CHAT_KEY", () => {
  it("is the mobile last-chat key", () => {
    expect(LAST_CHAT_KEY).toBe("peakbot.mobile.lastChat");
  });
});

describe("parseLastChat", () => {
  it.each([null, "garbage", "[]", "null"] as const)("returns null for %s", (raw) => {
    expect(parseLastChat(raw)).toBeNull();
  });

  it("parses a valid v1 payload (null and named profile)", () => {
    expect(parseLastChat(JSON.stringify({ v: 1, convo: "c1", profile: null }))).toEqual({
      convo: "c1",
      profile: null,
    });
    expect(parseLastChat(JSON.stringify({ v: 1, convo: "c1", profile: "home" }))).toEqual({
      convo: "c1",
      profile: "home",
    });
  });

  it("a missing or non-string profile becomes null", () => {
    expect(parseLastChat(JSON.stringify({ v: 1, convo: "c1" }))).toEqual({
      convo: "c1",
      profile: null,
    });
    expect(parseLastChat(JSON.stringify({ v: 1, convo: "c1", profile: 5 }))).toEqual({
      convo: "c1",
      profile: null,
    });
  });

  it("an empty or non-string convo is null", () => {
    expect(parseLastChat(JSON.stringify({ v: 1, convo: "", profile: null }))).toBeNull();
    expect(parseLastChat(JSON.stringify({ v: 1, convo: 7, profile: null }))).toBeNull();
  });

  it("a different version is null", () => {
    expect(parseLastChat(JSON.stringify({ v: 2, convo: "c1", profile: null }))).toBeNull();
  });
});

describe("resumeTarget", () => {
  it("resumes the stored chat when the URL has neither param", () => {
    expect(resumeTarget(null, null, { convo: "c9", profile: "home" })).toEqual({
      convo: "c9",
      profile: "home",
    });
  });

  it("a URL convo wins (no resume)", () => {
    expect(resumeTarget("c2", null, { convo: "c9", profile: "home" })).toBeNull();
  });

  it("an explicit URL profile starts fresh there (no resume)", () => {
    expect(resumeTarget(null, "dev", { convo: "c9", profile: "home" })).toBeNull();
  });

  it("first run (nothing stored) is null", () => {
    expect(resumeTarget(null, null, null)).toBeNull();
  });
});

describe("bootMobileChats", () => {
  it("seeds the URL from the stored chat without a new history entry", () => {
    window.localStorage.setItem(
      LAST_CHAT_KEY,
      JSON.stringify({ v: 1, convo: "c9", profile: "home" }),
    );
    window.history.replaceState(null, "", "/");
    const lenBefore = window.history.length;

    bootMobileChats();

    expect(window.location.search).toBe("?convo=c9&profile=home");
    expect(window.history.length).toBe(lenBefore);
  });

  it("leaves a URL that already carries a convo untouched", () => {
    window.localStorage.setItem(
      LAST_CHAT_KEY,
      JSON.stringify({ v: 1, convo: "c9", profile: "home" }),
    );
    window.history.replaceState(null, "", "/?convo=c2");

    bootMobileChats();

    expect(window.location.search).toBe("?convo=c2");
  });

  it("a throwing localStorage leaves the URL untouched and does not throw", () => {
    Object.defineProperty(window, "localStorage", {
      value: throwingStorage(),
      configurable: true,
      writable: true,
    });
    window.history.replaceState(null, "", "/");

    expect(() => bootMobileChats()).not.toThrow();
    expect(window.location.search).toBe("");
  });
});

describe("loadLastChat / saveLastChat", () => {
  it("saveLastChat writes {v:1,convo,profile}", () => {
    saveLastChat(window.localStorage, { convo: "c1", profile: "home" });
    expect(JSON.parse(window.localStorage.getItem(LAST_CHAT_KEY)!)).toEqual({
      v: 1,
      convo: "c1",
      profile: "home",
    });
  });

  it("loadLastChat reads and parses a stored chat", () => {
    window.localStorage.setItem(
      LAST_CHAT_KEY,
      JSON.stringify({ v: 1, convo: "c1", profile: null }),
    );
    expect(loadLastChat(window.localStorage)).toEqual({ convo: "c1", profile: null });
  });

  it("loadLastChat returns null when nothing is stored", () => {
    expect(loadLastChat(window.localStorage)).toBeNull();
  });

  it("loadLastChat returns null on a corrupt payload", () => {
    window.localStorage.setItem(LAST_CHAT_KEY, "garbage");
    expect(loadLastChat(window.localStorage)).toBeNull();
  });

  it("loadLastChat never throws (even on a throwing storage)", () => {
    expect(() => loadLastChat(throwingStorage())).not.toThrow();
    expect(loadLastChat(throwingStorage())).toBeNull();
  });

  it("saveLastChat never throws (even on a throwing storage)", () => {
    expect(() =>
      saveLastChat(throwingStorage(), { convo: "c1", profile: null }),
    ).not.toThrow();
  });
});
