// RED (T23 D1d): ./diff — thin wrapper over `diff` (jsdiff).
// Spec §9.6 "Diff modal": a line diff of saved vs current text, computed by
// the `diff` package (`structuredPatch`, `context: 2`) — no hand-written LCS.
// Render hunks with `@@ … @@` gap markers and `--- a/<path>` / `+++ b/<path>`
// headers. No textual change → the exact message below.
//
// Assumed API (see contract.ts):
//   configDiff(oldText, newText, path): Patch
//     = structuredPatch("a/" + path, "b/" + path, oldText, newText, "", "",
//                       { context: 2 })
//   diffIsEmpty(patch): boolean          — patch.hunks.length === 0
//   hunkHeader(hunk): string             — "@@ -<oldStart>,<oldLines>
//                                          +<newStart>,<newLines> @@"
//   NO_TEXTUAL_CHANGE_MESSAGE: string (const)
//
// The expected hunk values below are pinned against the real `diff` package
// (installed dependency), so these tests fail only because `./diff` is
// missing, never because the expected data is wrong.
//
// Expected RED: `./diff` does not exist yet → import error.

import { describe, expect, it } from "vitest";
import { NO_TEXTUAL_CHANGE_MESSAGE, configDiff, diffDisplayName, diffIsEmpty, hunkHeader } from "./diff";

describe("diff", () => {
  it("diff_headers_and_hunks", () => {
    // Change at line 6 of a 10-line file: with context: 2 the hunk must span
    // exactly lines 4–8 (two context lines before and after the change) —
    // this catches a wrapper that defaults to 3 lines of context.
    const oldText = ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"].join("\n");
    const newText = ["L1", "L2", "L3", "L4", "L5", "X", "L7", "L8", "L9", "L10"].join("\n");

    const patch = configDiff(oldText, newText, "config.yaml");

    // --- a/<path> / +++ b/<path> file headers.
    expect(patch.oldFileName).toBe("a/config.yaml");
    expect(patch.newFileName).toBe("b/config.yaml");

    expect(patch.hunks).toHaveLength(1);
    const hunk = patch.hunks[0];
    expect(hunk.oldStart).toBe(4);
    expect(hunk.oldLines).toBe(5);
    expect(hunk.newStart).toBe(4);
    expect(hunk.newLines).toBe(5);
    // @@ … @@ gap marker for the hunk.
    expect(hunkHeader(hunk)).toBe("@@ -4,5 +4,5 @@");

    // Exactly: 2 context lines, the removal, the addition, 2 context lines.
    expect(hunk.lines).toEqual([" L4", " L5", "-L6", "+X", " L7", " L8"]);

    expect(diffIsEmpty(patch)).toBe(false);

    // Repo file path flows through too.
    const repo = configDiff("a: 1", "a: 2", ".peakbot/config.yaml");
    expect(repo.oldFileName).toBe("a/.peakbot/config.yaml");
    expect(repo.newFileName).toBe("b/.peakbot/config.yaml");
  });

  // T34b (QA): an ABSOLUTE config path used to render `a//abs/path` in the
  // modal header (double slash). The header shows the file name; the full
  // path stays in the modal's separate path tag.
  it("absolute_paths_get_a_single_slash_file_name_header", () => {
    const abs = configDiff("a: 1", "a: 2", "/home/u/.config/peakbot/config.yaml");
    expect(abs.oldFileName).toBe("a/config.yaml");
    expect(abs.newFileName).toBe("b/config.yaml");
    expect(abs.oldFileName).not.toContain("//");
    expect(abs.newFileName).not.toContain("//");

    // A relative path is unchanged (covered above, pinned explicitly).
    expect(diffDisplayName(".peakbot/config.yaml")).toBe(".peakbot/config.yaml");
    expect(diffDisplayName("config.yaml")).toBe("config.yaml");
    // Windows-style backslash paths are not absolute-unix: untouched.
    expect(diffDisplayName("C:\\peakbot\\config.yaml")).toBe("C:\\peakbot\\config.yaml");
  });

  it("no_textual_change_message", () => {
    const text = "providers:\n  - name: patchnotes\nprofiles:\n  dev: {}\n";
    const patch = configDiff(text, text, "config.yaml");
    expect(patch.hunks).toHaveLength(0);
    expect(diffIsEmpty(patch)).toBe(true);

    // The exact modal copy from spec §9.6.
    expect(NO_TEXTUAL_CHANGE_MESSAGE).toBe(
      "(no textual changes in the generated YAML — e.g. UI-only state)",
    );
  });
});
