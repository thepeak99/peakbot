// T10 (RED): the pairing-link split (remote-profiles design §8.4).
//
// The `./pairingLink` module does not exist yet — this file must fail with
// a missing-module import error.
//
// Contract pinned here (simplest reading of §8.4: "The SPA splits the URL
// with the URL API into the form fields `url` (origin), `token`, `ca`,
// `profile`, and a name defaulting to `<profile> on <host>` sanitised to
// the name rule. … A link without `ca` leaves the field empty, and the
// form requires a choice (pin or `system`): there is no silent system
// trust." / "The TS split only fills a form and never judges."):
//
//   export interface PairingFields {
//     url: string;         // the ORIGIN only: scheme://host[:port]
//     token: string;       // the `?token=` value
//     ca: string | null;   // the `ca=` value, or null when absent (the
//                          // form then requires a pin/system choice)
//     profile: string | null; // the `profile=` value, or null when absent
//   }
//   export function parsePairingLink(text: string): PairingFields
//     — splits with the URL API. https only. Throws an Error with the
//     locked message below when the text is not a URL at all or carries
//     no `token` param. A missing `ca` / `profile` is NOT an error.
//   export function defaultRemoteName(profile: string | null, host: string): string
//     — `<profile> on <host>` (or just `<host>` without a profile),
//     sanitised to the config name rule (`[A-Za-z0-9_ .-]{1,64}`):
//     disallowed chars removed, trimmed, at most 64 chars.
//
// The link format is the server's entry link plus `&ca=sha256:…`
// (§8.4/§9 T10): `https://10.2.0.48:7823/?token=pito&ca=sha256:3f9a…`,
// optionally with `&profile=dev` appended by the panel's dropdown.
// `Config::parse` is the validator — the split never judges beyond
// "is this a pairing link at all".

import { describe, expect, it } from "vitest";
import { defaultRemoteName, parsePairingLink } from "./pairingLink";

const CA = `sha256:${"0123456789abcdef".repeat(4)}`;
const FULL_LINK = `https://10.2.0.48:7823/?token=pito&ca=${CA}&profile=dev`;

/// The locked error for a text that is not a pairing link at all.
const NOT_A_LINK = "Not a pairing link — expected https://host:port/?token=…";

describe("parsePairingLink (T10, design §8.4)", () => {
  it("fills_url_token_ca_profile_from_the_doc_link_format", () => {
    const f = parsePairingLink(FULL_LINK);
    expect(f.url).toBe("https://10.2.0.48:7823"); // the origin — no query
    expect(f.token).toBe("pito");
    expect(f.ca).toBe(CA);
    expect(f.profile).toBe("dev");
  });

  it("fills_token_and_profile_without_ca_or_url_port", () => {
    // The default port (443) is dropped from the origin.
    const f = parsePairingLink(`https://bot.example.ts.net/?token=abc123&profile=web`);
    expect(f.url).toBe("https://bot.example.ts.net");
    expect(f.token).toBe("abc123");
    expect(f.ca).toBeNull();
    expect(f.profile).toBe("web");
  });

  it("a_link_without_ca_is_not_an_error", () => {
    // §8.4: "A link without `ca` leaves the field empty, and the form
    // requires a choice (pin or system)" — the split reports null, it
    // does not judge.
    const f = parsePairingLink(`https://10.2.0.48:7823/?token=pito`);
    expect(f.ca).toBeNull();
    expect(f.profile).toBeNull();
  });

  it("rejects_a_link_without_a_token_param", () => {
    expect(() => parsePairingLink(`https://10.2.0.48:7823/?ca=${CA}`)).toThrow(NOT_A_LINK);
    expect(() => parsePairingLink("https://10.2.0.48:7823/")).toThrow(NOT_A_LINK);
  });

  it("rejects_non_urls_and_non_https_urls", () => {
    for (const bad of ["", "hello world", "ftp://10.2.0.48:7823/?token=pito", "http://127.0.0.1:7823/?token=pito"]) {
      expect(() => parsePairingLink(bad)).toThrow(NOT_A_LINK);
    }
  });
});

describe("defaultRemoteName (T10, design §8.4)", () => {
  it("defaults_to_profile_on_host", () => {
    expect(defaultRemoteName("dev", "10.2.0.48")).toBe("dev on 10.2.0.48");
    expect(defaultRemoteName("web", "bot.example.ts.net")).toBe("web on bot.example.ts.net");
  });

  it("falls_back_to_the_host_without_a_profile", () => {
    expect(defaultRemoteName(null, "10.2.0.48")).toBe("10.2.0.48");
  });

  it("sanitises_to_the_name_rule", () => {
    // The name rule allows letters, digits, space, '_', '.', '-' (1–64).
    // A disallowed char is removed; the result is trimmed.
    expect(defaultRemoteName("dev!", "10.2.0.48")).toBe("dev on 10.2.0.48");
    // 64-char cap.
    const long = defaultRemoteName("a".repeat(80), "host");
    expect(long.length).toBeLessThanOrEqual(64);
  });
});
