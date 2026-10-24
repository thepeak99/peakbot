// ASSUMED MODULE APIS — types-only contract for the M5 RED tests.
//
// The tests in this directory (T20–T26) are written BEFORE the modules exist.
// Each test file's header names the module it imports; the signatures, props
// and data-testids below are what the builders implement against. Do not
// re-shape these to fit an implementation — change the tests only with a
// reason noted in the test's header.
//
// Sources: config-v2-plan.md M5 (T20–T23), spec make-config-great-again.md
// §9.0 (state model, entry/exit, nav), §9.6 (save bar, restart, diff,
// errors), §8.2 (endpoints).
//
// This file is TYPES ONLY (no runtime exports) so it type-checks green while
// every other module in this directory is still missing.

import type { Document } from "yaml";
import type { StructuredPatch, StructuredPatchHunk } from "diff";
import type { Scope, Seg } from "./types";

/** The draft document: a `yaml` Document so comments/order survive edits. */
export type Doc = Document;

/** A jsdiff structured patch (context: 2) of saved → current text. */
export type DiffPatch = StructuredPatch;
/** One hunk of a DiffPatch (oldStart/oldLines/newStart/newLines/lines). */
export type DiffHunk = StructuredPatchHunk;

// ── ./yamlDoc ────────────────────────────────────────────────────────────
// parse(text: string): Doc                — yaml.parseDocument(text)
// stringify(doc: Doc): string             — doc.toString()
// get(doc: Doc, path: Seg[]): unknown     — segment-array walk; [] = whole doc
//                                          (its toJS value); missing = undefined
// set(doc: Doc, path: Seg[], value: unknown): void
//                                         — mutates in place; appends new keys
//                                         at the end (order preserved);
//                                         value === undefined REMOVES the key
//                                         (renders nothing — not `null`)

// ── ./store ──────────────────────────────────────────────────────────────
// One store, one state slot per scope (spec §9.0 "State model").
//
// createStore(): Store
export interface ScopeState {
  savedText: string;
  sha256: string | null;
  draftDoc: Doc;
  rawText: string | null;
  validate: ValidateResLike | null;
}
// Minimal structural stand-in so this file stays types-only; the real type
// is ValidateRes from ./types (same shape, discriminant on `ok`).
type ValidateResLike = { ok: true; restart_required: string[]; warnings: string[] } | {
  error: string;
  errors: { file: Scope; path: (string | number)[]; message: string }[];
};

export interface Store {
  current: Scope;
  setScope(scope: Scope): void;
  state(): ScopeState; // state of `current`
  // Loads a fetched file: savedText/sha256 + draftDoc = parse(text),
  // rawText = null, validate = null.
  setSaved(scope: Scope, text: string, sha256: string | null): void;
  // Form edit: stores the doc and DROPS rawText (form-after-raw rule).
  setDraft(scope: Scope, doc: Doc): void;
  // Raw YAML edit (null = raw cleared).
  setRaw(scope: Scope, text: string | null): void;
  setValidate(scope: Scope, res: ValidateResLike): void;
  // discard: draftDoc = parse(savedText), rawText = null, validate = null.
  discard(scope: Scope): void;
  // Dirty = stringify(draftDoc) !== stringify(parse(savedText))
  //         || (rawText !== null && rawText !== savedText).
  isDirty(scope: Scope): boolean;
}

// ── ./api ────────────────────────────────────────────────────────────────
// Typed fetchers for the §8.2 endpoints. Plain `fetch` underneath, so tests
// stub globalThis.fetch.
//
// class ApiError extends Error {
//   readonly status: number;
//   readonly body: { error?: string; errors?: ConfigErr[] };
// }
//   — every non-2xx response rejects with ApiError (403 pin/tenant, 409
//     stale, 422 validation envelope, 400/500).
//
// getSettings(convo?: string): Promise<SettingsMeta>
//   GET /api/settings  (…?convo=<uuid> when given)
// getConfig(scope: Scope, convo?: string): Promise<ConfigText>
//   GET /api/config?scope=…&convo=…
// validateConfig(scope: Scope, text: string, convo?: string): Promise<ValidateRes>
//   POST /api/config/validate  body { scope, text, convo? } (JSON)
// putConfig(scope: Scope, text: string, baseSha256: string | null, convo?: string): Promise<PutOk>
//   PUT /api/config  body { scope, text, base_sha256, convo? } (JSON)

// ── ./Settings ───────────────────────────────────────────────────────────
// Shell state hook (Settings is an in-app view in App.tsx, not a route).
//
// useSettingsToggle(): { open: boolean; toggle(): void; close(): void }
//   window keydown while mounted:
//     - key "," with ctrlKey OR metaKey (no shift/alt) → preventDefault +
//       toggle (Ctrl+, / ⌘+,)
//     - key "Escape" while open → close, UNLESS the event was already
//       handled (defaultPrevented) — modals and popovers close first
//       (spec: Esc order modal → popover → Settings)
//   The TopBar gear (tooltip `Settings  Ctrl ,`) calls the same toggle.

// ── ./SaveBar ────────────────────────────────────────────────────────────
// <SaveBar …/> — rendered only while the current scope is dirty.
export interface SaveBarProps {
  scope: Scope;
  dirty: boolean;
  /** Increments on every draft edit; re-arms the 400 ms validate debounce. */
  draftVersion: number;
  /** Server-computed from the last validate; the client keeps no boot-only list. */
  restartRequired: string[];
  saving: boolean;
  /** 409 from the last PUT. */
  stale: boolean;
  staleMessage: string | null;
  /** Debounced (400 ms, trailing) by SaveBar while dirty; also on
      Save / View diff / Raw blur (the shell passes those through here). */
  onValidate(): void;
  onSave(): void;
  onDiscard(): void;
  onReload(): void; // stale banner Reload
  onViewDiff(): void;
}
// data-testids:
//   savebar               — bar container (absent when !dirty)
//   savebar-unsaved       — "Unsaved changes"
//   savebar-chip-nextnew  — "applies on next /new"  (restartRequired empty)
//   savebar-chip-restart  — "restart required"      (restartRequired non-empty)
//   savebar-discard       — "Discard"  → onDiscard
//   savebar-diff          — "View diff" → onViewDiff
//   savebar-save          — "Save"     → onSave  (element must stay stable
//                         across re-renders with unchanged props)
//   savebar-stale         — stale banner (staleMessage text) when stale
//   savebar-stale-reload  — "Reload"   → onReload

// <RestartBanner …/> — bottom banner after a save with restart_required.
export interface RestartBannerProps {
  keys: string[]; // the restart_required entries
  desktop: boolean; // isDesktop()
  onLater(): void;
  onRestartNow(): void; // caller navigates to "peakbot-desktop:restart"
}
// data-testids:
//   restart-banner          — "…restart-required (<keys>) — they take effect
//                             after a restart."
//   restart-banner-later    — "Later" → onLater (both desktops and browser)
//   restart-banner-restart  — "Restart now" → onRestartNow, ONLY when
//                             desktop; browser shows "Restart PeakBot to apply."
//                             text instead (no button)

// ── ./paths ──────────────────────────────────────────────────────────────
// Error-path (segment array) → screen routing (spec §9.6 "Fix" rules).
//
// route(path: Seg[]): RouteTarget
export type Section = "providers" | "mcps" | "profiles" | "general" | "system" | "raw";
export interface RouteTarget {
  section: Section;
  profile?: string; // ["profiles", p, …]
  agent?: string;   // … "agents", a …
  subagent?: string;// … "subagents", s …
  field: Seg[]; // remaining segments after profile/agent/subagent
}
//   ["providers", …] → providers;  ["mcps", …] → mcps;
//   ["profiles", p] → profile page; ["profiles", p, "agents", a, …] → agent
//   page;  ["general", …] → general;  [] (root) → raw.
//   Names may contain dots and spaces — the array is never split on ".".

// ── ./diff ───────────────────────────────────────────────────────────────
// configDiff(oldText: string, newText: string, path: string): DiffPatch
//   structuredPatch("a/" + path, "b/" + path, oldText, newText, "", "",
//                   { context: 2 }) — no hand-written LCS.
// diffIsEmpty(patch: DiffPatch): boolean   — patch.hunks.length === 0
// hunkHeader(hunk: Hunk): string
//   "@@ -<oldStart>,<oldLines> +<newStart>,<newLines> @@"
// NO_TEXTUAL_CHANGE_MESSAGE: string (const)
//   "(no textual changes in the generated YAML — e.g. UI-only state)"

// ── ./filter (T24) ───────────────────────────────────────────────────────
// NameFilter tri-state ⇄ YAML serialization (spec §9.3 Tools/MCP cards;
// Rust NameFilter: {enabled, disabled[], only[]}, never both lists).
//
// The UI tri-state: "all" (no filter) | "only" (allowlist) | "disabled"
// (blocklist). `names` is the checked set.
export type FilterMode = "all" | "only" | "disabled";
export interface FilterState {
  mode: FilterMode;
  names: string[];
}
// parseFilter(value: unknown): FilterState
//   undefined / null / non-object → { mode: "all", names: [] }
//   { only: [n…] }                → { mode: "only", names: [n…] }
//   { disabled: [n…] }            → { mode: "disabled", names: [n…] }
//   { enabled: false }            → { mode: "only", names: [] }
//                                   (allowlist-empty is enabled:false, §9.3)
//   `only`/`disabled` win over a redundant `enabled: false`.
// serializeFilter(state: FilterState): unknown  — the VALUE for the key
//   (undefined = omit the key entirely — renders nothing, not `null`)
//   { mode: "all", … }                        → undefined
//   { mode: "disabled", names: [n…] }         → { disabled: [n…] }
//   { mode: "disabled", names: [] }           → undefined (blocklist w/ 0)
//   { mode: "only", names: [n…] } non-empty   → { only: [n…] }
//   { mode: "only", names: [] }               → { enabled: false }
//   Output carries exactly the listed keys — no `enabled: true`, no empty
//   sibling list.
// setMode(state: FilterState, mode: FilterMode): FilterState
//   Switching mode CLEARS the checked set (spec §9.3 [fix vs mockup] —
//   keeping the checks would silently invert meaning). Same mode = no-op.

// ── ./widgets (T24) ──────────────────────────────────────────────────────
// Form building blocks. All are DUMB: they take value + onChange and render
// red on `invalid` (the pane matches ConfigErr paths, spec §9.6). Every
// composite stamps `testid` on its container; inner elements use the fixed
// relative testids noted below. Number fields write a NUMBER (never a
// string); an empty number field calls onChange(undefined) = key removed.
//
export interface FieldProps {
  testid: string;
  label: string;
  value: string | number | undefined; // undefined = key absent
  onChange(value: string | number | undefined): void;
  type?: "text" | "number"; // default "text"
  hint?: string;
  disabled?: boolean;
  invalid?: string; // matched error message → red field + message below
}
// inner testids: `input`, `hint`, `error` (only when invalid)
//
export interface ToggleProps {
  testid: string;
  label: string;
  checked: boolean;
  onChange(checked: boolean): void;
  hint?: string;
  invalid?: string;
}
// inner testids: `input` (checkbox)
//
export interface KvRow {
  key: string;
  value: string;
}
export interface KvRowsProps {
  testid: string;
  label: string;
  rows: KvRow[];
  onChange(rows: KvRow[]): void; // full new array on every edit
}
// inner testids: `row-<i>`, `row-<i>-key`, `row-<i>-value`,
// `row-<i>-remove`, `add`
//
export interface ChipRowProps {
  testid: string;
  label: string;
  items: string[];
  onChange(items: string[]): void; // full new array on every edit
  placeholder?: string;
}
// inner testids: `chip-<name>` (with `chip-<name>-remove` ✕), `add-input`,
// `add`. Enter in `add-input` or clicking `add` appends — trimmed, empty
// ignored, duplicates ignored.
//
export type VisionTri = "auto" | "on" | "off"; // → vision: omit / true / false
export interface TriProps {
  testid: string;
  label: string;
  value: VisionTri;
  onChange(value: VisionTri): void;
}
// inner testids: `auto`, `on`, `off` (radios)
//
export interface FilterBlockProps {
  testid: string;
  label: string;
  universe: string[]; // names from SettingsMeta (builtin_tools / mcps names /
  //                    tab_names / skills) — NEVER hard-coded
  value: FilterState; // from ./filter
  onChange(value: FilterState): void;
  emptyHint?: string; // shown for an allowlist with zero checked
}
// inner testids: `mode-all`, `mode-only`, `mode-disabled`, `chip-<name>`.
// Clicking a mode calls setMode (checked set cleared) and emits the new
// state; clicking a chip toggles its name in the checked set (same mode).
//
export interface SecretInputProps {
  testid: string;
  label: string;
  value: string | undefined;
  onChange(value: string | undefined): void; // undefined = key removed
  hint?: string;
  invalid?: string;
}
// Masked by default: `value` renders dots (the secret text is NOT in the
// DOM). Clicking `reveal` swaps in a text input holding the secret; clicking
// again re-masks. inner testids: `reveal`, `value`.
//
export interface ModelSelectProps {
  testid: string;
  label: string;
  options: { alias: string; qualified: string }[];
  value: string;
  onChange(alias: string): void;
}
// inner testids: `select`
//
export interface CrumbsProps {
  testid: string;
  items: string[]; // e.g. ["Profiles", "dev"] — "Profiles › dev"
  onNavigate(index: number): void;
}
// inner testids: `crumb-<i>`
//
export interface BadgeProps {
  testid?: string;
  tone?: "green" | "amber" | "red" | "neutral";
  children: React.ReactNode;
}

// ── ./providers (T25) ────────────────────────────────────────────────────
// <ProvidersPane/> — D2 "Providers & Models" (spec §9.8). Reads the draft
// via useSettingsCtx() (`doc`, `edit`, `errors`, `meta`); every change goes
// through `edit(path, value)` by segment array. Providers = get(doc,
// ["providers"]) (absent = []). Renders the REAL ModelEntry fields —
// `context_size`, NOT the mockup's `num_ctx`.
//
// Provider card fields (per provider i):
//   type select — openrouter | openai | anthropic | llamacpp | ollama.
//     Changing type writes `type` AND auto-fills `base_url` from the preset:
//       openai       https://api.openai.com/v1
//       anthropic    https://api.anthropic.com
//       openrouter   https://openrouter.ai/api/v1  (input disabled — fixed)
//       ollama       http://127.0.0.1:11434
//       llamacpp     http://127.0.0.1:8080
//   name, base_url (disabled for openrouter), api_key (SecretInput, hint
//   "Optional for local servers." for ollama/llamacpp), ✕ remove.
//   data-testids: `provider-card-<i>`, `provider-type-<i>` (select),
//   `provider-name-<i>`, `provider-baseurl-<i>`, `provider-apikey-<i>`,
//   `provider-remove-<i>`, `provider-add` (appends a new entry via
//   edit(["providers"], […])).
//
// Models table (per model j of provider i): name · alias · max_tok · temp ·
// vision pill; expand → the real ModelEntry fields:
//   name, alias (hint "Globally unique across providers."), max_tokens
//   (number), temperature (number), context_size (number, label `context`,
//   hint "Empty = auto-detect."), vision Tri (auto/on/off → omit/true/false),
//   anthropic-only prompt_caching select, non-anthropic/ollama extra_params
//   (a JSON textarea that MUST parse as a JSON object — invalid input shows
//   an inline error and writes nothing; a valid object is written as a
//   parsed object, not a string), Advanced disclosure with
//   preserve_reasoning / display_reasoning toggles, Remove model.
//   `+ Add model` with the type's preset chips.
//   data-testids: `model-row-<i>-<j>`, `model-expand-<i>-<j>`,
//   `model-name-<i>-<j>`, `model-alias-<i>-<j>`, `model-maxtokens-<i>-<j>`,
//   `model-temp-<i>-<j>`, `model-context-<i>-<j>`, `model-vision-<i>-<j>`,
//   `model-caching-<i>-<j>`, `model-extra-<i>-<j>` (textarea),
//   `model-extra-<i>-<j>-error` (only while invalid),
//   `model-remove-<i>-<j>`, `model-add-<i>`, `model-preset-<i>-<k>`.

// ── ./mcp (T26) ──────────────────────────────────────────────────────────
// <McpsPane/> — D3 "MCP servers" (spec §9.9). Reads the draft via
// useSettingsCtx(); servers = get(doc, ["mcps"]) (absent = []). Top banner:
// "⚠ MCP server changes are restart-required — they take effect after a
// restart, not on /new."
//
// Per server i:
//   name (Field), a transport tag, a status pill from
//   meta.mcp_status (name match; absent entry → no pill) with tone
//   connected=green / failed=red / disabled=neutral; enabled Toggle
//   (absent `enabled` reads as true); ✕ remove;
//   type select stdio | streamable-http | sse (the real key is `type`,
//   NOT the mockup's `transport`).
//   stdio:  command (Field), args (ChipRow → `args` list), env (KvRows →
//           `env` map).
//   remote: url (Field), auth select none | bearer | oauth (writes the
//           TAGGED map `auth` — never the scalar), headers (KvRows →
//           `headers` map).
//     bearer → token (SecretInput) → auth = { type: "bearer", token };
//              the legacy `auth_token` key is never written.
//     oauth  → client_id (Field), client_secret (SecretInput),
//              scopes (ChipRow, comma list → YAML list) →
//              auth = { type: "oauth", client_id?, client_secret?,
//                        scopes: [n…] } — empty client_id/secret omitted,
//              scopes always a list (possibly empty).
//   Legacy `auth_token` (when present with no `auth`) renders read-only
//   with "Deprecated — use auth: bearer".
//   data-testids: `mcp-banner`, `mcp-card-<i>`, `mcp-name-<i>`,
//   `mcp-status-<i>`, `mcp-type-<i>` (select), `mcp-enabled-<i>`,
//   `mcp-remove-<i>`, `mcp-add` (appends via edit(["mcps"], […]), entry has
//   a `name` and `type: "stdio"`), `mcp-command-<i>`, `mcp-args-<i>`,
//   `mcp-env-<i>`, `mcp-url-<i>`, `mcp-auth-<i>` (select),
//   `mcp-auth-token-<i>`, `mcp-auth-clientid-<i>`, `mcp-auth-secret-<i>`,
//   `mcp-auth-scopes-<i>`, `mcp-headers-<i>`, `mcp-authtoken-<i>` (read-only
//   legacy display).
