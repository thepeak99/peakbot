// ASSUMED MODULE APIS — types-only contract for the M5 RED tests.
//
// The tests in this directory (T20–T28) are written BEFORE the modules exist.
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
import type { CatalogAgent, CatalogProfile, Selection } from "../types";
import type { InboundMessage } from "../state";

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

// ── ./profileOps (T27) ─────────────────────────────────────────────────
// Pure helpers for the profiles map — the panes route ALL ordering/naming
// logic through here (unit-testable, no doc mutation). The profiles map is
// a plain JS object: insertion order = YAML key order, so "move first" =
// rebuild the object with the key first.
//
// makeDefault(profiles: Record<string, unknown>, name: string): Record<string, unknown>
//   A NEW map with `name` moved to the front; relative order of the rest
//   unchanged; the input is not mutated. Unknown `name` (or empty map) →
//   throws Error("no such profile: <name>").
//
// duplicateName(name: string, taken: string[]): string
//   `<name>-copy`; if that is in `taken`, `<name>-copy-2`,
//   `<name>-copy-3`, … the first one not in `taken` (spec §9.3
//   "`<name>-copy[-N]`").
//
// copyToRepoName(name: string, repoProfileNames: string[]): string
//   `name` if the name is free in the repo draft, else `name + "-copy"`
//   (further collisions are the server's validation problem, not this
//   helper's — spec §9.3 "(-copy suffix if the name is taken there)").

// ── ./profiles (T27) ───────────────────────────────────────────────────
// <ProfilesPane/> — D4 "Profiles list" (spec §9.2). Reads the draft via
// useSettingsCtx(); profiles = get(doc, ["profiles"]) (absent = {}). One
// card per profile IN MAP ORDER; the first card carries the DEFAULT
// badge; cards 1..n get a "Make default" button that reorders the map via
// profileOps.makeDefault → edit(["profiles"], newMap).
// data-testids: `profiles-pane`, `profile-card-<i>`, `profile-name-<i>`,
// `profile-badge-default` (inside card 0 only), `profile-make-default-<i>`
// (cards 1..n only — NOT on card 0), `profile-add`.
//
// <ProfilePage profile: string activeProfile?: string | null
//   onCopyToRepo?(name: string, def: Record<string, unknown>): void
//   onOpenProfile?(name: string): void /> — D4 profile page (spec §9.3).
// Reads get(doc, ["profiles", profile]); writes via edit by segment array.
//   `profile-page` — container.
//   `profile-name` — editable name input.
//   `profile-duplicate` — inserts a copy of the profile DIRECTLY AFTER the
//   source, named with profileOps.duplicateName, writes the new profiles
//   map, calls onOpenProfile(newName) (spec: the copy is opened) and
//   toasts `Duplicated as <n>`.
//   `profile-copy-repo` — GLOBAL scope only (absent in repo scope); calls
//   onCopyToRepo(profile, def) with the full profile def — the shell does
//   the cross-scope copy (naming via profileOps.copyToRepoName) + the
//   scope switch.
//   `profile-delete` — refused with the error toast `A config needs at
//   least one profile` when it is the only profile (NO edit); otherwise
//   writes the profiles map without the name and toasts `Deleted <n>`.
//   `profile-tabs` — Interface card: one checkbox per meta.tab_names,
//   `profile-tabs-chip-<name>` (testid on the INPUT, checked = visible).
//   Writes edit(["profiles", p, "ui", "tabs"],
//   serializeFilter({ mode: "disabled", names: hidden })) where
//   hidden = the unchecked tabs IN meta.tab_names (universe) order —
//   all checked → undefined (key omitted, not `enabled: true`).
//   Unchecking `settings` on the profile the current conversation uses
//   (activeProfile === profile) asks first (spec §9.3 Q8): dialog
//   `profile-tabs-confirm` with the exact text `Hide Settings for '<p>'?
//   This removes the ⚙ gear and Ctrl+, for the profile you are using. To
//   get Settings back you will have to edit config.yaml by hand or switch
//   profile.`, buttons `profile-tabs-confirm-hide` ("Hide Settings") and
//   `profile-tabs-confirm-cancel` ("Cancel"). NO edit until "Hide
//   Settings" is clicked; Cancel leaves the checkbox checked and no edit.
//   A non-active profile hides `settings` immediately, no confirm.

// ── ./agent (T28) ──────────────────────────────────────────────────────
// <AgentPage profile: string agent: string /> — D5 agent page (spec
// §9.4). Reads get(doc, ["profiles", profile, "agents", agent]); writes
// via edit by segment array.
// data-testids:
//   `agent-page` — container.
//   `agent-model` — ModelSelect; options built by the page from the
//   DRAFT's providers[].models ({ alias, qualified = provider/name });
//   inner `select`.
//   `agent-persona` — persona textarea (testid on the textarea); while
//   the key is absent its placeholder = the FIRST LINE of
//   meta.builtin_persona (spec: "the first lines of builtin_persona").
//   `agent-persona-chip-builtin` — the "Built-in (Code Crusader)" chip:
//   writes persona: undefined (key omitted) — it NEVER writes a copy of
//   the built-in text (spec §9.4 [changed, Q6]).
//   `agent-persona-chip-neutral` / `agent-persona-chip-terse` — preset
//   chips writing the mockup's preset texts (spec §9.4, mockup lines
//   748–752).
//   `agent-skills` — FilterBlock; universe = meta.skills (never
//   hard-coded); writes serializeFilter(state) to
//   …agents.<a>.skills.
//   `agent-subs-toggle` — Toggle; checked is DERIVED = the subagents map
//   is non-empty. Off → edit(…agents.<a>.subagents, undefined) + the
//   previous map is stashed in the page's own state (editing session) +
//   the `agent-subs-undo` affordance appears (a toast action or a banner
//   button — the fixed testid is what the test drives). Undo →
//   edit(…subagents, <the stashed map, verbatim>) and the stash is
//   consumed (affordance gone). On with no stash → one blank `helper`.
//   `agent-sub-<i>` — i-th subagent card, in map order; inner
//   `agent-sub-name-<i>` (input), `agent-sub-model-<i>` (select — the
//   FIRST option has value "" and text `inherits <agent model>`; choosing
//   it writes model: undefined = key omitted), `agent-sub-prompt-<i>`
//   (textarea), `agent-sub-remove-<i>`.
//   `agent-sub-add` — + Add subagent.
// No persona field on subagents (spec §9.4).

// ── ./profiles — remaining cards (T27b) ────────────────────────────────
// Extra card testids INSIDE <ProfilePage/> (spec §9.3). Every write is by
// segment array under ["profiles", <profile>, …]; the pane re-reads the
// draft doc after each edit (the shell mutates it in place). Additional
// optional prop:
//   globalMcpNames?: string[]   — the universe for the MCP access card when
//     the page is shown in REPO scope (the repo draft has no `mcps`); when
//     omitted the universe = names of get(doc, ["mcps"]).
//
// Tools card
//   `profile-tools`            — the FilterBlock container (inner `mode-all`
//                                 / `mode-only` / `mode-disabled`,
//                                 `chip-<name>`). Universe = meta.builtin_tools
//                                 verbatim (NEVER hard-coded, no `delegate`).
//                                 Writes serializeFilter(state) to
//                                 …<p>.tools (undefined = key omitted).
//   `profile-tools-footnote`   — text contains "delegate is on automatically".
//   `profile-tools-allowpost`  — Toggle (inner `input`) → …<p>.fetch_url
//                                 .allow_post (bool; absent reads false).
// MCP access card
//   `profile-mcps`             — FilterBlock container (same inner testids),
//                                 universe = declared `mcps` names, writes
//                                 …<p>.mcps.
//   `profile-mcps-empty`       — shown INSTEAD of the chips when the universe
//                                 is empty: "No MCP servers declared globally."
// Environment card
//   `profile-env`              — KvRows (inner `add`, `row-<i>-key`,
//                                 `row-<i>-value`, `row-<i>-remove`) →
//                                 …<p>.env as a string map. Rows with a blank
//                                 key are dropped; no rows left = the `env`
//                                 key is REMOVED (undefined, not `{}`).
// Memory card
//   `profile-memory-enabled`   — Toggle → …<p>.memory.enabled (bool). An
//                                 absent memory block reads as enabled=true.
//   `profile-memory-threshold` — number Field → …<p>.memory.threshold_bytes
//                                 (a NUMBER; empty = key removed).
// Knowledge card (vector_db; presence = on)
//   `profile-vdb-toggle`       — Toggle; checked = vector_db block present.
//                                 On  → edit(…<p>.vector_db, <plain object
//                                 holding an `embeddings` object>); Off →
//                                 edit(…<p>.vector_db, undefined) (whole block
//                                 removed).
//   Only while ON (absent from the DOM otherwise):
//   `profile-vdb-dbpath` (hint: global scope contains "working directory",
//   repo scope contains "this repo"), `profile-vdb-baseurl`,
//   `profile-vdb-apikey` (SecretInput — secret text not in the DOM),
//   `profile-vdb-model`, `profile-vdb-dimensions` (number) →
//   …<p>.vector_db.db_path / .embeddings.base_url / .embeddings.api_key /
//   .embeddings.model / .embeddings.dimensions.

// ── ./general (T29) ────────────────────────────────────────────────────
// Pure helpers (module ./general):
//   type AccessMode = "local" | "lan"
//   accessModeOf(bind: string | undefined): AccessMode
//     loopback host (127.x.x.x, localhost, [::1]) or absent → "local";
//     anything else (0.0.0.0, [::], 192.168.x.x …) → "lan".
//   bindForAccess(bind: string | undefined, mode: AccessMode): string
//     "local" → `127.0.0.1:<port>`, "lan" → `0.0.0.0:<port>`, KEEPING the
//     port of `bind`; absent / unparseable bind → port 7823.
//   generateToken(): string
//     32 lowercase hex chars from 16 bytes of crypto.getRandomValues.
//   launchCommand(o: { bind: string; tls: boolean; hasToken: boolean }): string
//     "peakbot --bind <bind>" + (" --tls" if tls) + (" --token ••••" if token).
//
// <GeneralPane/> (module ./general, file general.tsx) — D6 (spec §9.7).
// Reads the draft via useSettingsCtx(); writes by segment array under
// ["general", …] (flat: retry / timeouts / http / uploads — NEVER
// general.network). Numbers are written as numbers; empty = key removed.
// Repo scope: read-only (`general-readonly` banner, every control disabled,
// no edits).
// data-testids:
//   `general-pane`; `general-readonly` (repo scope only)
//   Web server: `general-access-local` / `general-access-lan` (radio
//     INPUTS; checked derived from web.bind via accessModeOf; clicking
//     writes web.bind = bindForAccess(bind, mode)), `general-bind` (Field →
//     web.bind), `general-token` (SecretInput → web.token),
//     `general-token-generate` (button → web.token = generateToken()),
//     `general-token-copy` (button → navigator.clipboard.writeText(token)),
//     `general-token-note` (ONLY when meta.boot.token_source is env|cli;
//     text "Currently overridden by PEAKBOT_WEB_TOKEN." / "… by --token."),
//     `general-tls` (Toggle → web.tls bool), `general-ttl`
//     (web.session_ttl_secs), `general-reaper` (web.reaper_tick_secs),
//     `general-launch` (textContent = launchCommand(…)).
//   Restart badges: `general-badge-restart-web|http|uploads|conversation`
//     (and none on Search / Context).
//   Search: `general-search-toggle` (Toggle; checked = block present; on →
//     edit(["general","search"], <plain object>), off → edit(…, undefined)),
//     and only while on: `general-search-baseurl`, `general-search-bearer`
//     (SecretInput), `general-search-timeout`, `general-search-maxresults`
//     → search.base_url / bearer_token / timeout_seconds / max_results.
//   Context: `general-context-enabled`, `general-context-threshold`,
//     `general-context-keep`, `general-context-model` (select; first option
//     value "" = omit) → context.enabled / threshold / keep_recent /
//     compaction_model.
//   Conversations: `general-conv-autosave`, `general-conv-dir` →
//     conversation.auto_save / storage_dir.
//   Network: `general-retry-max` (max_retries), `general-retry-initial`
//     (initial_delay_ms), `general-retry-maxdelay` (max_delay_ms),
//     `general-retry-backoff` (backoff_factor) → general.retry.*;
//     `general-timeout-tool`, `general-timeout-delegate` →
//     general.timeouts.tool_secs / delegate_secs; `general-http-connect`,
//     `general-http-read` → general.http.connect_timeout_secs /
//     read_timeout_secs.
//   Uploads: `general-uploads-maxfile`, `general-uploads-maxfiles` →
//     general.uploads.max_file_mb / max_files.
//   `general-max-turns` → general.agent_max_turns; `general-cost-tracking`
//     (Toggle → general.cost_tracking; absent reads true).
// Only the ones named in general.test.tsx are pinned by a test.

// ── ./system (T30) + desktop link (D8) ──────────────────────────────────
// <SystemPane/> (module ./system, file system.tsx) — D7 (spec §9.10).
// No props: reads `meta` via useSettingsCtx() (facts, global_path,
// repo_path) and talks to /api/system/* with plain `fetch` (tests stub
// globalThis.fetch; URL relative, JSON bodies carry
// `content-type: application/json` — the server rejects anything else).
//
// facts.install is typed `unknown` in types.ts; the server's shape
// (src/ui/web/settings.rs `install_info`) is
//   { target: string, state: "current"|"other"|"absent",
//     path: {status:"on_path"} | {status:"shadowed", by:string}
//         | {status:"absent", hint:string} }
// and it may be `null`/missing — the pane must not crash on that.
//
// Wire (src/ui/web/system.rs):
//   GET    /api/system/service        → ServiceReport
//   POST   /api/system/service  {op:"install"}   → ServiceReport
//   DELETE /api/system/service        → ServiceReport
//   POST   /api/system/install  {}               → InstallReport
//   ServiceReport = { manager:"systemd-user"|"launchd-agent"|"windows-task"|
//     "unsupported", name, artifact:string|null, installed:boolean,
//     exe:string|null, run_state:"running"|"stopped"|"unknown", tls:boolean,
//     survives_logout:boolean, commands:string[], notes:string[] }
//   InstallReport = { source, target, action:"already_current"|"installed"|
//     "replaced", path:{status:…}, notes:string[] }
//   non-2xx → JSON {error:string} (e.g. 422 / 500); the pane shows it.
// The pane GETs the service status once on mount. After a POST/DELETE it
// shows the ServiceReport returned by that call (no extra GET required).
// data-testids:
//   `system-pane`
//   `system-version`        — textContent contains facts.version
//   `system-path-config`    — contains meta.global_path
//   `system-path-repo`      — contains meta.repo_path
//   `system-path-data`      — contains facts.data_dir
//   `system-path-binary`    — contains facts.exe_path
//   `system-path-install`   — contains facts.install.target (when present)
//   `system-path-pill`      — /on PATH/i for on_path; /not on PATH/i for
//                             absent (and shadowed → contains "shadow")
//   `system-install`        — button "Install / Update binary" →
//                             POST /api/system/install ({}); result lands in
//   `system-install-result` — contains the report's `action` words
//                             (e.g. "installed") and `target`
//   `system-service-kind`   — contains ServiceReport.manager
//   `system-service-installed` — /not installed/i when !installed, else
//                             /installed/i without "not"
//   `system-service-state`  — contains run_state ("running"/"stopped"/…)
//   `system-service-enable` — "Enable at login"  → POST op:"install"
//   `system-service-disable`— "Disable"          → DELETE
//   `system-service-refresh`— "Refresh"          → GET
//   `system-error`          — the `{error}` text of a failed call; absent
//                             otherwise
// NOT built: "Check for updates" (spec: omit in v1) — no such text anywhere.
//
// Desktop link (D8) lives in the Settings shell nav (Settings.tsx), not in a
// pane: testid `nav-desktop-link`, text "Desktop app settings ↗", present
// ONLY when isDesktop() (globalThis.isTauri === true), as the last MACHINE
// item. Click → `window.location.href = "peakbot-desktop:settings"` (a
// click HANDLER assignment — the shell's existing plain <a href> must be
// replaced; tests observe the assignment, jsdom cannot navigate). It does
// NOT change the active pane.
// Nav buttons also get `nav-<section>` testids: nav-providers, nav-mcps,
// nav-profiles, nav-general, nav-system, nav-raw.

// ── ./raw (T31) + store additions (D9) ──────────────────────────────────
// Store (store.ts) additions, per scope (ScopeState gains two fields):
export interface RawError {
  /** 1-based, from `yaml`'s error linePos. */
  line: number;
  col: number;
  message: string;
}
export interface ScopeStateRawExt {
  /** Last Raw edit failed to parse; null when it parsed (or no raw edit). */
  rawError: RawError | null;
  /**
   * The comment-loss banner flag. Set by setDraft(scope, …) when the
   * previous rawText was non-null (form edit AFTER a raw edit) OR the
   * savedText of the scope contains YAML comments (first form edit on a
   * commented file). Cleared by setSaved and discard. NOT cleared by
   * further form edits. A raw edit alone never sets it.
   */
  rewriteWarning: boolean;
}
// store.setRaw(scope, text: string | null) now ALSO parses back:
//   - text parses  → draftDoc = parse(text), rawText = text, rawError = null
//   - syntax error → rawText = text, rawError = {line,col,message} (first
//     error of the yaml Document), draftDoc UNCHANGED (last good parse)
//   - null         → rawText = null, rawError = null (draftDoc kept)
// store.setDraft clears rawText AND rawError. setSaved/discard reset
// rawError = null, rewriteWarning = false. isDirty formula is unchanged.
//
// <RawPane/> (module ./raw, file raw.tsx) — controlled, props only:
export interface RawPaneProps {
  scope: Scope;
  /** What the textarea shows. The shell computes it: rawText ?? (dirty ?
   *  stringify(draftDoc) : savedText). Verbatim — no trimming. */
  text: string;
  error: RawError | null;
  /** Every keystroke (textarea input event) → shell calls store.setRaw. */
  onChange(text: string): void;
  /** Textarea blur → shell validates (spec §9.6 "Raw YAML blur"). */
  onBlur?(): void;
}
// data-testids:
//   `raw-pane`
//   `raw-banner`  — "Advanced: unknown keys are rejected at boot…" (repo
//                   scope text additionally contains "only contain profiles:")
//   `raw-text`    — the <textarea>; .value === props.text
//   `raw-error`   — ONLY while error != null; text is exactly
//                   `line ${line}, col ${col}: ${message}`
// Shell (Settings.tsx) wiring pinned by raw.test.tsx:
//   - nav `nav-raw` shows RawPane with the scope's text (see RawPaneProps.text).
//   - rawError != null ⇒ the SaveBar Save button is DISABLED
//     (`saveBlocked` prop below), clicking it never PUTs, and the shell does
//     not call validate with the broken text.
//   - rewriteWarning ⇒ a `rewrite-banner` element, shown above every FORM
//     pane (not on Raw / System), with EXACTLY (spec §9.0):
//       "Form edits rewrite the file in normalized YAML — comments and anchors are lost."
//     (NB: the task brief paraphrased this as "form edits rewrite the file
//     and may drop comments"; the spec text above is the contract.)
// SaveBar gains: saveBlocked?: boolean — when true `savebar-save` has the
// `disabled` attribute and onSave is never called.

// ═════════════════════════════════════════════════════════════════════════
// T32 (D11 chat selection) + T33 (D10/D12 first run)  — RED contracts
// Tests: src/components/selectionChip.test.tsx, src/settings/firstRun.test.tsx
// ═════════════════════════════════════════════════════════════════════════


// ── ../components/SelectionChip (T32, spec §9.1) ────────────────────────
// export function SelectionChip(props: SelectionChipProps)
// Renders NOTHING (null) when `selection` is null or `catalog` is empty
// (old server / no wire fields).
export interface SelectionChipProps {
  catalog: CatalogProfile[];
  /** The (profile, agent) this conversation is bound to (adaptSelection). */
  selection: Selection | null;
  /** hasTranscript — after the first message the chip is locked (§6.5). */
  locked: boolean;
  /** AppState.profile_pinned — `--profile` pin: only the pinned profile is
   *  listed (caption `pinned`); agent switching still works. */
  pinned: boolean;
  /** The only way the chip talks to the server: send(select(profile, agent)). */
  send(msg: InboundMessage): void;
  /** Called with `Next conversation: <p> › <a>` after an agent click. */
  onToast?(message: string): void;
  dropUp?: boolean;
}
// data-testids (the chip owns all of them):
//   `selection-chip`      — the <button>. textContent = `[📁 ]<profile> › <agent> ▾`
//                           (📁 only when the SELECTED profile is origin
//                           "repo"). Locked: textContent starts `🔒`, no ▾,
//                           `title` === "Locked after the first message",
//                           `disabled`, click never opens the popover.
//   `selection-popover`   — present only while open (never while locked).
//   `selection-profile-<name>` — one row per profile, catalog order (repo
//                           first). Repo rows show 📁; row 0 shows caption
//                           `default`. Attribute data-browsed="true" on the
//                           BROWSED row (initially the selected profile).
//                           Click = BROWSE ONLY: right column switches,
//                           nothing selected, nothing sent.
//   `selection-agents-title` — `AGENTS · <PROFILE>` (browsed profile; compared
//                           case-insensitively).
//   `selection-agent-<name>` — rows of the BROWSED profile only. Caption
//                           `N subagents` (+ ` · default` on the first agent)
//                           or `single`. Click = send(select(browsedProfile,
//                           agent)) + close popover + onToast(...).
// Esc while open closes the popover AND calls event.preventDefault() (so the
// Settings view's Esc handler, which skips defaultPrevented events, stays
// open: Esc order modal → popover → Settings). Esc while closed does NOT
// preventDefault. mousedown outside closes.
// Pinned (`pinned`): only the row for selection.profile is rendered, with
// caption `pinned`; no other `selection-profile-*`.

// ── ../components/AgentsChip (T32, spec §9.1 "Agents chip popover") ─────
// export function AgentsChip(props: AgentsChipProps)  — null when `agent` is null.
export interface AgentsChipProps {
  profile: string | null;
  /** The bound catalog agent (App resolves selection → catalog). */
  agent: CatalogAgent | null;
  /** The model alias currently active (after /model it can differ from agent.model). */
  activeModel: string;
  dropUp?: boolean;
}
// data-testids:
//   `agents-chip`          — textContent `Agents · N ▾`, N = 1 + subagents.
//   `agents-popover`       — only while open (click toggles; Esc closes+preventDefault).
//   `agents-roster-title`  — `Roster · <profile> › <agent>`.
//   `agents-roster-main`   — contains agent.name, tag `main`, activeModel.
//   `agents-roster-sub-<name>` — contains `↳ <name>` and the sub's model, or
//                           `inherits <agent.model>` when sub.inherited.
//   `agents-roster-empty`  — "Single agent — no delegate tool." (no subagents).

// ── TopBar / BottomBar / Composer additions (T32) ───────────────────────
// TopBar & BottomBar gain OPTIONAL props (all absent ⇒ no chips, as today):
export interface ChatChipProps {
  catalog?: CatalogProfile[];
  selection?: Selection | null;
  profilePinned?: boolean;
  /** Toast sink for `Next conversation: …`. */
  onToast?(message: string): void;
}
// They render <SelectionChip locked={hasTranscript} send={send} …/> then (after
// the model chip) <AgentsChip …/>. TopBar: selection-chip precedes agents-chip.
// BottomBar renders both too (dropUp).
// Composer gains `settingsEnabled?: boolean` (default true): a hint element
// `composer-settings-hint` with text `Ctrl+, settings`, placed AFTER the
// `/ for commands` hint; not rendered when settingsEnabled === false.
// ../tabs.ts gains the pure gate App/TopBar use for the gear (and Ctrl+,):
//   settingsGearEnabled(settingsAvailable: boolean | undefined,
//                       visibleTabs: string[] | undefined): boolean
//   = (settingsAvailable ?? true) && (visibleTabs === undefined || visibleTabs.includes("settings"))

// ── ./firstRun (T33, spec §9.13 / D10) ──────────────────────────────────
export type FirstRunProviderType = "openrouter" | "openai" | "anthropic" | "llamacpp" | "ollama";
export interface FirstRunModel {
  name: string;
  alias: string;
  max_tokens: number;
}
export interface FirstRunAnswers {
  provider: {
    type: FirstRunProviderType;
    name: string;
    /** Omitted from the YAML for openrouter (fixed) and when empty. */
    base_url?: string;
    /** Omitted from the YAML when empty/undefined. */
    api_key?: string;
    models: FirstRunModel[];
  };
  agent: {
    /** An alias from provider.models. */
    model: string;
    /** null = Built-in (Code Crusader) chip = OMIT `persona`. */
    persona: string | null;
    /** true ⇒ subagents.helper.prompt "Take focused sub-tasks and report back." */
    subagents: boolean;
  };
  web: {
    access: "local" | "lan";
    /** Written as general.web.token only for access === "lan". */
    token: string | null;
  };
}
// buildFirstRunYaml(a: FirstRunAnswers): string   — PURE. Top-level key order
//   providers, mcps (always `[]`), profiles, general. Profile `default`,
//   agent `main` (MAPS, never `- name:` lists). general.web.bind =
//   "127.0.0.1:7823" (local) | "0.0.0.0:7823" (+ token). Ends with "\n".
// deriveSetupPhase(meta: {needs_setup:boolean; legacy_layout:boolean},
//                  globalExists: boolean): "ready" | "guided" | "restart_pending"
//   !needs_setup → "ready"; needs_setup && globalExists && !legacy_layout →
//   "restart_pending"; otherwise "guided" (§6.9 — derived, never stored).
export type SetupPhase = "ready" | "guided" | "restart_pending";
//
// <FirstRun meta={SettingsMeta} global={ConfigText} onClose?()/> (file
// firstRun.tsx). Self-contained: uses ./api putConfig only. If
// deriveSetupPhase(meta, global.exists) is "restart_pending" it renders the
// restart screen straight away; otherwise the wizard.
// Wizard testids — `fr-*` controls are either the element itself or a wrapper
// holding a descendant [data-testid="input"] (tests accept both):
//   `firstrun` root;  rail: `firstrun-rail-welcome|provider|agent|web|review`
//   (buttons; DISABLED until the step has been visited);  step panes:
//   `firstrun-step-welcome|provider|agent|web|review` (exactly one present).
//   Footer: `firstrun-back`, `firstrun-skip` ("Skip to review"),
//   `firstrun-next` (disabled while the step has errors), `firstrun-finish`
//   (Review only).  `firstrun-step-errors` lists the step's error messages.
//   Welcome: `firstrun-facts` (OS · Binary · Config · Data dir · Version),
//     `firstrun-start-fresh` (the ONLY action; no Migrate button),
//     `firstrun-banner-migrate` ONLY when meta.legacy_layout: contains
//     "PeakBot couldn't migrate your old config", the verbatim
//     meta.migrate_error, and "Your config.yaml was not changed.".
//   Provider: `fr-provider-type` (select), `fr-provider-name`,
//     `fr-provider-baseurl`, `fr-provider-apikey`, `fr-model-add` (button),
//     per row k: `fr-model-name-k`, `fr-model-alias-k`, `fr-model-maxtokens-k`.
//     Errors: `An API key is required for <type>.` (hosted types only:
//     openrouter/openai/anthropic) and `Add at least one model.`
//     Defaults: type openrouter, name "openrouter", zero model rows.
//   Agent: `firstrun-agent-banner` ("Generated: profile default › agent main
//     using model <alias>."), `fr-agent-model` (select of aliases),
//     `fr-persona-builtin|neutral|terse` (chips; builtin default),
//     `fr-agent-subagents` (toggle).
//   Web: `fr-web-local`, `fr-web-lan` (radio inputs; local default),
//     `fr-web-token`, `fr-web-token-generate`. LAN without token error:
//     "A LAN bind requires a token — generate one, or switch back to local only."
//   Review: `firstrun-yaml` (preview; secrets masked VISUALLY only),
//     `firstrun-review-path` (contains meta.global_path),
//     `firstrun-review-note` (desktop: "Finishing restarts PeakBot with this
//     config." / browser: "After Finish, restart PeakBot to start chatting."),
//     `firstrun-review-legacy-note` ONLY when legacy_layout ("old file kept
//     as config.yaml.pre-0.24"), `firstrun-error` (a failed PUT's message).
//   Finish → PUT /api/config {scope:"global", text: buildFirstRunYaml(answers),
//     base_sha256: global.sha256}. Desktop (isDesktop()): location.href =
//     "peakbot-desktop:restart", NO success screen. Browser: `firstrun-restart`
//     (also shown for the derived restart_pending phase) containing
//     "Config written", "Restart PeakBot to start chatting."; after Finish
//     it also contains "PeakBot wrote your config. A backup was kept.", and
//     `firstrun-restart-backup` ONLY when PutOk.backup !== null. No "Open chat".
//
// ── ../Boot (T33, spec §9.13 trigger + D12) ─────────────────────────────
// export function Boot({ children }: { children: ReactNode })
//   main.tsx wraps <App/> in it. On mount: getSettings(); when
//   needs_setup also getConfig("global"). `children` (App → useAgent → /ws)
//   is NOT mounted while needs_setup (the socket would 503); getSettings
//   failure (404 old server, 403, network) ⇒ render children (never block
//   the chat). `/setup` is an alias: with needs_setup it lands on the same
//   guided FirstRun; with !needs_setup it renders children.
//   testids: `boot-loading` (while fetching), `setup-strip` (after Esc closes
//   FirstRun: "PeakBot isn't configured yet — no provider or model is set
//   up." or, when legacy_layout, "PeakBot couldn't migrate your old config"),
//   `setup-strip-finish` ("Finish setup" → reopens FirstRun).
