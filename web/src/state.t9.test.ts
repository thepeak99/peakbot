// @vitest-environment jsdom
//
// RED (remote-profiles T9): `chipAction`, `apiBase` and the URL `profile`
// helper do not exist yet. Written against the SPA contract the design
// locks (remote-profiles-design §6.4):
//
//   * `chipAction(current, target, catalog) → "select" | "reattach"` —
//     "reattach" iff `target.profile ≠ current.profile` and either side is
//     remote (origin "remote"). Reattach = `switchConvo(null, target.profile,
//     target.agent)`; a profile switch within the same profile (any kind)
//     stays a `select`.
//   * `apiBase(profile, catalog)` = `"/r/" + encodeURIComponent(profile)`
//     when the entry's origin is "remote", else `""`. Derived at the point
//     of use, never stored.
//   * URL `?convo=<id>&profile=<name>`: `setConvoInUrl(convo, profile)` /
//     `profileFromUrl()`. `attach` always carries the known profile.

import { afterEach, describe, expect, it } from "vitest";
import { apiBase, chipAction, profileFromUrl, setConvoInUrl } from "./state";
import type { CatalogProfile, Selection } from "./types";

const CATALOG: CatalogProfile[] = [
  {
    name: "dev",
    origin: "global",
    overridesGlobal: false,
    agents: [
      { name: "team", model: "sonnet", subagents: [] },
      { name: "review", model: "sonnet", subagents: [] },
    ],
    color: "teal",
  },
  {
    name: "home-dev",
    origin: "remote",
    overridesGlobal: false,
    agents: [{ name: "ops", model: "sonnet", subagents: [] }],
    color: "pink",
  },
];

const sel = (profile: string, agent: string): Selection => ({
  profile,
  agent,
  subagentModels: {},
});

afterEach(() => {
  window.history.replaceState(null, "", window.location.pathname);
});

describe("chipAction — the chip's select-vs-reattach decision (design §6.4)", () => {
  it("selects within the same local profile", () => {
    expect(chipAction(sel("dev", "team"), sel("dev", "review"), CATALOG)).toBe("select");
  });

  it("selects within the same REMOTE profile (the relay rewrites to the target)", () => {
    expect(chipAction(sel("home-dev", "ops"), sel("home-dev", "ops"), CATALOG)).toBe("select");
  });

  it("selects for a switch between two local profiles", () => {
    expect(chipAction(sel("dev", "team"), sel("writer", "draft"), CATALOG)).toBe("select");
  });

  it("reattaches from local to remote", () => {
    expect(chipAction(sel("dev", "team"), sel("home-dev", "ops"), CATALOG)).toBe("reattach");
  });

  it("reattaches from remote back to local", () => {
    expect(chipAction(sel("home-dev", "ops"), sel("dev", "team"), CATALOG)).toBe("reattach");
  });

  it("reattaches for a switch between two different remote profiles", () => {
    const other: CatalogProfile[] = [
      ...CATALOG,
      { name: "home-vps", origin: "remote", overridesGlobal: false, agents: [{ name: "a", model: "m", subagents: [] }], color: "blue" },
    ];
    expect(chipAction(sel("home-dev", "ops"), sel("home-vps", "a"), other)).toBe("reattach");
  });

  it("treats a target profile missing from the catalogue as local (select)", () => {
    expect(chipAction(sel("dev", "team"), sel("ghost", "a"), CATALOG)).toBe("select");
  });

  it("reattaches when the CURRENT side is the remote one, even if it is no longer listed", () => {
    expect(chipAction(sel("home-dev", "ops"), sel("dev", "team"), CATALOG)).toBe("reattach");
  });
});

describe("apiBase — the /r/<L>/ prefix (design §6.4)", () => {
  it("is /r/<name> for a remote entry, URL-encoded", () => {
    expect(apiBase("home-dev", CATALOG)).toBe("/r/home-dev");
  });

  it("encodes special characters in the name", () => {
    const cat: CatalogProfile[] = [
      { name: "home dev", origin: "remote", overridesGlobal: false, agents: [], color: "pink" },
    ];
    expect(apiBase("home dev", cat)).toBe("/r/home%20dev");
  });

  it("is empty for a local entry", () => {
    expect(apiBase("dev", CATALOG)).toBe("");
  });

  it("is empty for a null, empty or unknown profile", () => {
    expect(apiBase(null, CATALOG)).toBe("");
    expect(apiBase("", CATALOG)).toBe("");
    expect(apiBase("ghost", CATALOG)).toBe("");
  });
});

describe("URL ?convo & ?profile (design §6.1, §6.4)", () => {
  it("profileFromUrl reads ?profile= next to ?convo=", () => {
    window.history.replaceState(null, "", "/?convo=abc&profile=home-dev");
    expect(profileFromUrl()).toBe("home-dev");
  });

  it("profileFromUrl is null when absent", () => {
    window.history.replaceState(null, "", "/?convo=abc");
    expect(profileFromUrl()).toBeNull();
    window.history.replaceState(null, "", "/");
    expect(profileFromUrl()).toBeNull();
  });

  it("setConvoInUrl(convo, profile) round-trips both params without navigating", () => {
    window.history.replaceState(null, "", "/");
    setConvoInUrl("abc", "home-dev");
    const url = new URL(window.location.href);
    expect(url.searchParams.get("convo")).toBe("abc");
    expect(url.searchParams.get("profile")).toBe("home-dev");
    expect(profileFromUrl()).toBe("home-dev");
  });

  it("setConvoInUrl(null, null) removes both params", () => {
    window.history.replaceState(null, "", "/?convo=abc&profile=home-dev");
    setConvoInUrl(null, null);
    const url = new URL(window.location.href);
    expect(url.searchParams.has("convo")).toBe(false);
    expect(url.searchParams.has("profile")).toBe(false);
    expect(profileFromUrl()).toBeNull();
  });

  it("setConvoInUrl updates only the param that changed (idempotent)", () => {
    window.history.replaceState(null, "", "/?convo=abc&profile=home-dev");
    setConvoInUrl("abc", "home-dev"); // no-op
    setConvoInUrl("def", "home-dev"); // convo only
    const url = new URL(window.location.href);
    expect(url.searchParams.get("convo")).toBe("def");
    expect(url.searchParams.get("profile")).toBe("home-dev");
    setConvoInUrl("def", "ops"); // profile only
    const url2 = new URL(window.location.href);
    expect(url2.searchParams.get("convo")).toBe("def");
    expect(url2.searchParams.get("profile")).toBe("ops");
  });
});
