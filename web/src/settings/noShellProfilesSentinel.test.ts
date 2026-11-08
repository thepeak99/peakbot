// The mobile shell no longer owns a profile list, so the SPA must not link
// to the shell's profiles sentinel anywhere. Test files are excluded: they
// are the spec carriers (and this file names the sentinel on purpose).
//
// Pure-logic file: runs in the node environment (a real file URL, unlike the
// jsdom component tests), same technique as editionFixture.test.ts.

import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";

// Built from parts so this file never contains the literal itself.
const SENTINEL = "peakbot-mobile:" + "profiles";

function walk(dir: string, out: string[]): void {
  for (const name of readdirSync(dir)) {
    if (name.includes(".test.")) continue;
    const path = dir + name;
    const st = statSync(path);
    if (st.isDirectory()) walk(path + "/", out);
    else if (st.isFile() && /\.(ts|tsx|js|jsx|css|html)$/.test(name)) out.push(path);
  }
}

describe("shell profiles sentinel", () => {
  it("nothing_in_web_src_references_the_shell_profiles_sentinel", () => {
    const srcDir = decodeURIComponent(new URL("./../", import.meta.url).pathname);
    const files: string[] = [];
    walk(srcDir, files);
    expect(files.length, "the walk must actually find source files").toBeGreaterThan(20);

    const offenders = files.filter((f) => readFileSync(f, "utf8").includes(SENTINEL));
    expect(offenders, `files still referencing the sentinel: ${offenders.join(", ")}`).toEqual([]);
  });
});
