// NameFilter tri-state ⇄ YAML serialization (spec §9.3 Tools/MCP cards;
// Rust `NameFilter`: `{enabled, disabled[], only[]}`, never both lists).
//
// The UI tri-state: "all" (no filter) | "only" (allowlist) | "disabled"
// (blocklist). `names` is the checked set. `serializeFilter` returns the
// VALUE for the key — `undefined` = omit the key entirely (renders nothing,
// not `null`), matching yamlDoc.set.

export type FilterMode = "all" | "only" | "disabled";

export interface FilterState {
  mode: FilterMode;
  names: string[];
}

/**
 * Read a NameFilter value (or its absence) out of the draft.
 *   undefined / null / non-object → "all"
 *   { only: [n…] }                → allowlist (empty list = the
 *                                   allowlist-empty state, not "all")
 *   { disabled: [n…] }            → blocklist
 *   { enabled: false }            → allowlist with zero checked (§9.3)
 * `only`/`disabled` win over a redundant `enabled: false`.
 */
export function parseFilter(value: unknown): FilterState {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { mode: "all", names: [] };
  }
  const o = value as Record<string, unknown>;
  const only = stringList(o.only);
  if (only !== null) return { mode: "only", names: only };
  const disabled = stringList(o.disabled);
  if (disabled !== null) return { mode: "disabled", names: disabled };
  if (o.enabled === false) return { mode: "only", names: [] };
  return { mode: "all", names: [] };
}

/**
 * The YAML value for a NameFilter key.
 *   "all"                          → undefined (key omitted)
 *   disabled, n ≥ 1                → { disabled: [n…] }
 *   disabled, n = 0                → undefined (blocklist with 0)
 *   only, n ≥ 1                    → { only: [n…] }
 *   only, n = 0                    → { enabled: false }
 * The output carries exactly the listed keys — no `enabled: true`, no empty
 * sibling list; the order of `names` is preserved.
 */
export function serializeFilter(state: FilterState): unknown {
  switch (state.mode) {
    case "all":
      return undefined;
    case "disabled":
      return state.names.length === 0 ? undefined : { disabled: [...state.names] };
    case "only":
      return state.names.length === 0 ? { enabled: false } : { only: [...state.names] };
  }
}

/**
 * Switch the tri-state. A mode change CLEARS the checked set (spec §9.3
 * [fix vs mockup] — keeping the checks would silently invert meaning);
 * the same mode is a no-op.
 */
export function setMode(state: FilterState, mode: FilterMode): FilterState {
  if (state.mode === mode) return state;
  return { mode, names: [] };
}

function stringList(v: unknown): string[] | null {
  if (!Array.isArray(v)) return null;
  return v.filter((x): x is string => typeof x === "string");
}
