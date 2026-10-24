// The seam between the Settings shell and its section panes (later M5
// tasks). A pane reads the current scope's draft document and edits it by
// SEGMENT ARRAY through `edit` — the shell stores the doc, drops any raw
// text (form-after-raw rule) and bumps the draft version that re-arms the
// save bar's validate debounce.

import { createContext, useContext } from "react";
import type { Doc } from "./yamlDoc";
import type { ConfigErr, Scope, SettingsMeta, Seg } from "./types";

export interface SettingsCtxValue {
  scope: Scope;
  meta: SettingsMeta | null;
  /** The current scope's draft document. */
  doc: Doc;
  /** Set (or, with `undefined`, remove) the value at `path` in the draft. */
  edit(path: Seg[], value: unknown): void;
  /** Validation errors for the current scope (red fields match on `path`). */
  errors: ConfigErr[];
}

export const SettingsCtx = createContext<SettingsCtxValue | null>(null);

export function useSettingsCtx(): SettingsCtxValue {
  const v = useContext(SettingsCtx);
  if (!v) throw new Error("useSettingsCtx must be used inside <Settings>");
  return v;
}
