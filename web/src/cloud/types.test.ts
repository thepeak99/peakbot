// G.1 — `parseStateBody`: the ONE boundary parser (design §B).
//
// RED before implementation: `parseStateBody` throws "not implemented", so
// every case below fails for the right reason (feature absent).
//
// Contract: each of the 6 `state` tags round-trips; `sas` with ≠6 words →
// rejected (`null`); an unknown tag → `null`; a `pending` body missing
// `rejected` → rejected (strict — the server always sends it).

import { describe, expect, it } from "vitest";
import { parseStateBody } from "./types";

const six = ["apple", "river", "ocean", "tiger", "maple", "spark"];

describe("parseStateBody — the six tags round-trip", () => {
  it("disabled", () => {
    expect(parseStateBody({ state: "disabled", last_login_error: null })).toEqual({
      state: "disabled",
      last_login_error: null,
    });
  });

  it("logged_out (all three reasons)", () => {
    for (const reason of ["never", "expired", "signed_out"] as const) {
      expect(parseStateBody({ state: "logged_out", reason, last_login_error: null }))
        .toEqual({ state: "logged_out", reason, last_login_error: null });
    }
  });

  it("connecting carries attempt + last_error", () => {
    expect(parseStateBody({ state: "connecting", attempt: 3, last_error: "x", last_login_error: null }))
      .toEqual({ state: "connecting", attempt: 3, last_error: "x", last_login_error: null });
  });

  it("pending (empty account, no SAS, not rejected)", () => {
    expect(parseStateBody({ state: "pending", empty_account: true, sas: null, rejected: false, last_login_error: null }))
      .toEqual({ state: "pending", empty_account: true, sas: null, rejected: false, last_login_error: null });
  });

  it("active carries devices + requests", () => {
    const d = { id: "aa", name: "Laptop", platform: "linux", online: true, slug: "laptop", self: true };
    const r = { letter_id: "bb", device_id: "cc", name: "Phone", platform: "ios", sas: six, expires_at: 1 };
    expect(parseStateBody({ state: "active", devices: [d], requests: [r], last_login_error: null }))
      .toEqual({ state: "active", devices: [d], requests: [r], last_login_error: null });
  });

  it("trust (all four errors)", () => {
    for (const trust_error of ["genesis_mismatch", "fork", "revoked", "chain_invalid", "seed_store_error"] as const) {
      expect(parseStateBody({ state: "trust", trust_error, last_login_error: null }))
        .toEqual({ state: "trust", trust_error, last_login_error: null });
    }
  });

  it("keeps a non-null last_login_error", () => {
    expect(parseStateBody({ state: "connecting", attempt: 1, last_error: null, last_login_error: "boom" })
      ?.last_login_error).toBe("boom");
  });
});

describe("parseStateBody — malformed input → null (never throws)", () => {
  it("a pending body with 5 SAS words is rejected", () => {
    expect(parseStateBody({ state: "pending", empty_account: false, sas: six.slice(0, 5), rejected: false, last_login_error: null }))
      .toBeNull();
  });

  it("a pending body with 7 SAS words is rejected", () => {
    expect(parseStateBody({ state: "pending", empty_account: false, sas: [...six, "extra"], rejected: false, last_login_error: null }))
      .toBeNull();
  });

  it("an active request with ≠6 SAS words is rejected", () => {
    const r = { letter_id: "bb", device_id: "cc", name: "Phone", platform: "ios", sas: six.slice(0, 4), expires_at: 1 };
    expect(parseStateBody({ state: "active", devices: [], requests: [r], last_login_error: null }))
      .toBeNull();
  });

  it("an unknown state tag is rejected", () => {
    expect(parseStateBody({ state: "bogus", last_login_error: null })).toBeNull();
  });

  it("a missing state tag is rejected", () => {
    expect(parseStateBody({ last_login_error: null })).toBeNull();
  });

  it("a pending body missing `rejected` is rejected (strict)", () => {
    expect(parseStateBody({ state: "pending", empty_account: false, sas: six, last_login_error: null }))
      .toBeNull();
  });

  it("a pending body with a non-boolean `rejected` is rejected", () => {
    expect(parseStateBody({ state: "pending", empty_account: false, sas: null, rejected: "yes", last_login_error: null }))
      .toBeNull();
  });

  it("garbage (non-object) input is rejected", () => {
    expect(parseStateBody(null)).toBeNull();
    expect(parseStateBody("nope")).toBeNull();
  });
});
