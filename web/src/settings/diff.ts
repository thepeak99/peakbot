// Thin wrapper over `diff` (jsdiff) for the diff modal (spec §9.6). The
// modal diffs `stringify(parse(savedText))` vs `rawText ??
// stringify(draftDoc)` — no hand-written LCS.

import { structuredPatch, type StructuredPatch, type StructuredPatchHunk } from "diff";

/** A jsdiff structured patch (context: 2) of saved → current text. */
export type DiffPatch = StructuredPatch;
/** One hunk of a DiffPatch (oldStart/oldLines/newStart/newLines/lines). */
export type DiffHunk = StructuredPatchHunk;

/** The exact modal copy for an empty diff (spec §9.6). */
export const NO_TEXTUAL_CHANGE_MESSAGE =
  "(no textual changes in the generated YAML — e.g. UI-only state)";

/**
 * structuredPatch("a/" + path, "b/" + path, oldText, newText, "", "",
 * { context: 2 }).
 */
export function configDiff(oldText: string, newText: string, path: string): DiffPatch {
  return structuredPatch("a/" + path, "b/" + path, oldText, newText, "", "", { context: 2 });
}

export function diffIsEmpty(patch: DiffPatch): boolean {
  return patch.hunks.length === 0;
}

/** `@@ -<oldStart>,<oldLines> +<newStart>,<newLines> @@` */
export function hunkHeader(hunk: DiffHunk): string {
  return `@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`;
}
