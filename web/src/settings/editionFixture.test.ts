// Cross-side pin for the settings `boot` block (L7 follow-up). The SPA read
// `meta.edition` (top-level) while the server sends `boot.edition`, so on a
// real Local server `edition()` was always "full" — and both sides' own
// tests passed because each modelled its own idea of the shape.
//
// tests/fixtures/settings_boot_local.json is the ONE statement of the shape.
// The Rust side (tests/edition_settings.rs
// `boot_block_shape_matches_the_shared_fixture`) asserts the live server's
// `boot` keys equal the fixture's; this file feeds the fixture through the
// real `getSettings()` and asserts the SPA reads the edition from it.

import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { edition, setBootEdition } from "../desktop";
import { getSettings } from "./api";

const FIXTURE = JSON.parse(
  readFileSync(
    decodeURIComponent(
      new URL("../../../tests/fixtures/settings_boot_local.json", import.meta.url).pathname,
    ),
    "utf8",
  ),
) as { boot: { edition: string } };

function serve(body: unknown) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, json: async () => body })),
  );
}

afterEach(() => {
  setBootEdition(undefined);
  vi.unstubAllGlobals();
});

describe("settings boot-block fixture (shared with tests/edition_settings.rs)", () => {
  it("carries the edition in the boot block, not at the top level", () => {
    expect(FIXTURE.boot.edition).toBe("local");
    expect(Object.keys(FIXTURE)).toEqual(["boot"]);
  });

  it("getSettings() reads boot.edition from the fixture ⇒ edition() is local", async () => {
    serve(FIXTURE);
    await getSettings();
    expect(edition()).toBe("local");
  });

  it("a top-level edition (the old, wrong shape) does NOT make it local", async () => {
    serve({ edition: "local", boot: { bind: "127.0.0.1:1", token_source: "none", tls: false } });
    await getSettings();
    expect(edition()).toBe("full");
  });
});
