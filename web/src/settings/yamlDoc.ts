// Thin wrapper over `yaml` (v2) for the Settings SPA. Spec §9.0: field paths
// are SEGMENT ARRAYS everywhere (the same shape as the server's error `path`,
// §4.1) — nothing splits on `.`. A `yaml` Document is the draft so comments
// and key order survive edits.

import { isNode, parseDocument, type Document } from "yaml";
import type { Seg } from "./types";

/** The draft document: a `yaml` Document so comments/order survive edits. */
export type Doc = Document;

/** `yaml.parseDocument(text)` — the draft is always a Document. */
export function parse(text: string): Doc {
  return parseDocument(text);
}

/** `doc.toString()` — normalized YAML, original comments preserved. */
export function stringify(doc: Doc): string {
  return doc.toString();
}

/**
 * Walk `doc` by segment array and return the value at the end (its `toJS`
 * value). The empty path is the whole document; a missing path is
 * `undefined`.
 */
export function get(doc: Doc, path: Seg[]): unknown {
  if (path.length === 0) return doc.toJS();
  const found = doc.getIn(path, true);
  if (found === undefined || found === null) return undefined;
  return isNode(found) ? found.toJS(doc) : found;
}

/**
 * Set the value at `path` in `doc`, mutating it in place. Missing
 * intermediate containers are created (a map for string keys, a sequence
 * for numeric indices); new map keys are APPENDED, so existing order is
 * preserved. `value === undefined` REMOVES the key (renders nothing — not
 * `null`); removing an absent key is a no-op.
 */
export function set(doc: Doc, path: Seg[], value: unknown): void {
  if (path.length === 0) return;
  if (value === undefined) {
    // deleteIn throws when an intermediate node is absent; removing an
    // absent key is a no-op.
    if (doc.hasIn(path)) doc.deleteIn(path);
    return;
  }
  doc.setIn(path, doc.createNode(value));
}
