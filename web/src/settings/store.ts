// Per-scope draft store (spec §9.0 "State model"): one state slot per scope
// (`global`, `repo`) holding savedText/sha256 (from GET /api/config), the
// draftDoc (a `yaml` Document), rawText (non-null only after a Raw YAML
// edit), and the last validate response.
//
// Dirty = stringify(draftDoc) !== stringify(parse(savedText))
//         || (rawText !== null && rawText !== savedText).
// There is no change count — the save bar just says "Unsaved changes".
//
// The store is a plain object (not a hook) so it is unit-testable on its
// own; the shell keeps it in a `useState` and bumps a version counter after
// every mutation.

import { visit } from "yaml";
import { parse, stringify, type Doc } from "./yamlDoc";
import type { Scope, ValidateRes } from "./types";
import type { RawError } from "./contract";

/** The state of one scope (contract.ts `ScopeState`, real types). */
export interface ScopeState {
  savedText: string;
  sha256: string | null;
  draftDoc: Doc;
  rawText: string | null;
  /** Last Raw edit failed to parse (draftDoc is then the last good parse). */
  rawError: RawError | null;
  /**
   * The comment-loss banner flag (spec §9.0): set by a form edit after a raw
   * edit, or by the first form edit on a file whose source has comments.
   * Cleared by setSaved / discard only.
   */
  rewriteWarning: boolean;
  validate: ValidateRes | null;
}

export interface Store {
  current: Scope;
  setScope(scope: Scope): void;
  /** State of `current`. */
  state(): ScopeState;
  /**
   * Load a fetched file: savedText/sha256 + draftDoc = parse(text),
   * rawText = null, validate = null.
   */
  setSaved(scope: Scope, text: string, sha256: string | null): void;
  /** Form edit: stores the doc, DROPS rawText + rawError (form-after-raw rule). */
  setDraft(scope: Scope, doc: Doc): void;
  /**
   * Raw YAML edit (null = raw cleared). Parses back: valid text becomes the
   * draft; a syntax error sets rawError and keeps the last good draft.
   */
  setRaw(scope: Scope, text: string | null): void;
  setValidate(scope: Scope, res: ValidateRes): void;
  /** discard: draftDoc = parse(savedText), raw/rawError/rewriteWarning/validate reset. */
  discard(scope: Scope): void;
  isDirty(scope: Scope): boolean;
}

function freshSlot(): ScopeState {
  return {
    savedText: "",
    sha256: null,
    draftDoc: parse(""),
    rawText: null,
    rawError: null,
    rewriteWarning: false,
    validate: null,
  };
}

/** True when the YAML source has any comment (a `#` inside a quoted value is not one). */
function hasComments(text: string): boolean {
  if (!text.includes("#")) return false;
  const doc = parse(text);
  if (doc.commentBefore || doc.comment) return true;
  let found = false;
  visit(doc, (_key, node) => {
    const n = node as { comment?: string | null; commentBefore?: string | null };
    if (n.comment || n.commentBefore) {
      found = true;
      return visit.BREAK;
    }
    return undefined;
  });
  return found;
}

/** The first syntax error of a parsed document, in the `line L, col C` shape. */
function firstError(doc: Doc): RawError | null {
  const e = doc.errors[0];
  if (!e) return null;
  const pos = e.linePos?.[0];
  // yaml appends " at line N, column M:" and a source excerpt to `message`.
  const message = e.message.split("\n")[0].replace(/\s+at line \d+, column \d+:?$/, "");
  return { line: pos?.line ?? 1, col: pos?.col ?? 1, message };
}

export function createStore(): Store {
  const slots: Record<Scope, ScopeState> = { global: freshSlot(), repo: freshSlot() };
  let current: Scope = "global";

  return {
    get current() {
      return current;
    },
    setScope(scope: Scope) {
      current = scope;
    },
    state(): ScopeState {
      return slots[current];
    },
    setSaved(scope, text, sha256) {
      slots[scope] = {
        savedText: text,
        sha256,
        draftDoc: parse(text),
        rawText: null,
        rawError: null,
        rewriteWarning: false,
        validate: null,
      };
    },
    setDraft(scope, doc) {
      const s = slots[scope];
      if (!s.rewriteWarning && (s.rawText !== null || hasComments(s.savedText))) {
        s.rewriteWarning = true;
      }
      s.draftDoc = doc;
      s.rawText = null;
      s.rawError = null;
    },
    setRaw(scope, text) {
      const s = slots[scope];
      s.rawText = text;
      s.rawError = null;
      if (text === null) return;
      const doc = parse(text);
      const err = firstError(doc);
      if (err) s.rawError = err;
      else s.draftDoc = doc;
    },
    setValidate(scope, res) {
      slots[scope].validate = res;
    },
    discard(scope) {
      slots[scope].draftDoc = parse(slots[scope].savedText);
      slots[scope].rawText = null;
      slots[scope].rawError = null;
      slots[scope].rewriteWarning = false;
      slots[scope].validate = null;
    },
    isDirty(scope) {
      const s = slots[scope];
      if (s.rawText !== null && s.rawText !== s.savedText) return true;
      return stringify(s.draftDoc) !== stringify(parse(s.savedText));
    },
  };
}
