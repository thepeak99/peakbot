// ASSUMED MODULE APIS — types-only contract for the M5 foundation RED tests.
//
// The tests in this directory (T20–T23) are written BEFORE the modules exist.
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
