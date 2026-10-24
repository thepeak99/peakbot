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

import { parse, stringify, type Doc } from "./yamlDoc";
import type { Scope, ValidateRes } from "./types";

/** The state of one scope (contract.ts `ScopeState`, real types). */
export interface ScopeState {
  savedText: string;
  sha256: string | null;
  draftDoc: Doc;
  rawText: string | null;
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
  /** Form edit: stores the doc and DROPS rawText (form-after-raw rule). */
  setDraft(scope: Scope, doc: Doc): void;
  /** Raw YAML edit (null = raw cleared). */
  setRaw(scope: Scope, text: string | null): void;
  setValidate(scope: Scope, res: ValidateRes): void;
  /** discard: draftDoc = parse(savedText), rawText = null, validate = null. */
  discard(scope: Scope): void;
  isDirty(scope: Scope): boolean;
}

function freshSlot(): ScopeState {
  return {
    savedText: "",
    sha256: null,
    draftDoc: parse(""),
    rawText: null,
    validate: null,
  };
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
        validate: null,
      };
    },
    setDraft(scope, doc) {
      slots[scope].draftDoc = doc;
      slots[scope].rawText = null;
    },
    setRaw(scope, text) {
      slots[scope].rawText = text;
    },
    setValidate(scope, res) {
      slots[scope].validate = res;
    },
    discard(scope) {
      slots[scope].draftDoc = parse(slots[scope].savedText);
      slots[scope].rawText = null;
      slots[scope].validate = null;
    },
    isDirty(scope) {
      const s = slots[scope];
      if (s.rawText !== null && s.rawText !== s.savedText) return true;
      return stringify(s.draftDoc) !== stringify(parse(s.savedText));
    },
  };
}
