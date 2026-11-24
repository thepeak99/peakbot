// Locked first-device UX — the copy contract (design §D).
//
// The first device is NOT privileged: the PM's plan forbids language that
// implies a "root"/"primary"/"main" device that outranks the others, and it
// requires two positive statements:
//   • confirm.first.body — this device gets no extra privileges, and any
//     device can approve or revoke another;
//   • banner.genesis_mismatch — the saved device history doesn't match, not
//     "the root device changed".
//
// RED before the copy change: the banned-language guards for "root of trust",
// "root device", "root that approves" and "approves all" hit today's strings,
// and both positive contracts are absent. The "primary device" / "main
// device|computer" guards pass today and exist to keep future copy honest.

import { describe, expect, it } from "vitest";
import { COPY } from "./copy";

type CopyEntry = { path: string; text: string };

/** Every string COPY can produce: literals plus one representative call per
 *  string-returning helper — so functions can't smuggle banned wording either. */
function copyStrings(node: unknown, path = ""): CopyEntry[] {
  if (typeof node === "string") return [{ path, text: node }];
  if (typeof node === "function") {
    const fn = node as (...args: unknown[]) => unknown;
    try {
      const out = fn(1, 1, 1);
      return typeof out === "string" ? [{ path: `${path}()`, text: out }] : [];
    } catch {
      return [];
    }
  }
  if (node && typeof node === "object") {
    return Object.entries(node).flatMap(([key, value]) =>
      copyStrings(value, path ? `${path}.${key}` : key),
    );
  }
  return [];
}

const STRINGS = copyStrings(COPY);

const BANNED_TERMS: Array<{ label: string; pattern: RegExp }> = [
  { label: "root of trust", pattern: /\broot\s+of\s+trust\b/i },
  { label: "root device", pattern: /\broot\s+device\b/i },
  { label: "root that approves", pattern: /\broot\s+that\s+approves\b/i },
  { label: "approves all", pattern: /\bapproves?\s+all\b/i },
  { label: "primary device", pattern: /\bprimary\s+device\b/i },
  { label: "main device/computer", pattern: /\bmain\s+(?:device|computer)\b/i },
];

describe("misleading device-hierarchy language", () => {
  for (const { label, pattern } of BANNED_TERMS) {
    it(`never says "${label}" anywhere in COPY`, () => {
      const hits = STRINGS.filter((entry) => pattern.test(entry.text)).map(
        (entry) => `${entry.path}: ${JSON.stringify(entry.text)}`,
      );
      expect(hits, `COPY says "${label}"`).toEqual([]);
    });
  }
});

describe("positive contracts (locked plan)", () => {
  it("confirm.first.body says the first device gets no extra privileges", () => {
    expect(COPY.confirm.first.body).toMatch(
      /\b(?:no|without)\b[^.]{0,40}\b(?:extra|additional|special)\s+privileges?/i,
    );
  });

  it("confirm.first.body says any device can approve or revoke", () => {
    const body = COPY.confirm.first.body;
    expect(body).toMatch(/\bany\s+(?:of\s+your\s+)?devices?\b/i);
    expect(body).toMatch(/\bapprove\b/i);
    expect(body).toMatch(/\brevoke\b/i);
  });

  it("banner.genesis_mismatch talks about device history, not a root device", () => {
    const message = COPY.banner.genesis_mismatch;
    expect(message).toMatch(/\bdevice\s+history\b/i);
    expect(message).toMatch(
      /mismatch|does\s*n[o']?t\s+match|does\s+not\s+match|no\s+longer\s+match|differ/i,
    );
    expect(message).not.toMatch(/\broot\b/i);
  });
});
