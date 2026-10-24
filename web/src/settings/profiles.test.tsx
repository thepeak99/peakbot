// T27 (RED): D4 "Profiles list + profile page" (spec §9.2/§9.3, plan T27).
// The ./profiles and ./profileOps modules do not exist yet — this file must
// fail with a missing-module import error. Panes read the draft via
// useSettingsCtx() and write through context.ts `edit(path, value)` by
// segment array. Ordering/naming logic is pinned to the pure profileOps
// helpers (unit-testable, doc-free). Props/testids: contract.ts
// ("./profileOps (T27)", "./profiles (T27)").
//
// YAML maps: "move first" = rebuilding the profiles map with the key
// first — one test applies the written value to a real doc and asserts
// the stringified key order via yamlDoc.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilePage, ProfilesPane } from "./profiles";
import { copyToRepoName, duplicateName, makeDefault } from "./profileOps";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { get, parse, set, stringify } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { Scope, Seg, SettingsMeta } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root;
let host: HTMLElement;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(<div>{node}</div>));
}

function unmount() {
  act(() => root.unmount());
  host.remove();
}

afterEach(unmount);

interface Edit {
  path: Seg[];
  value: unknown;
}

// TAB_NAMES from spec §3.3 (`ui.tabs` names) — meta.tab_names in tests.
const TAB_NAMES = ["session", "todo", "files", "tasks", "bash", "agents", "settings"];

function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: TAB_NAMES,
    skills: [],
    builtin_persona: "",
    mcp_status: [],
    boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
    facts: {
      os: "linux",
      arch: "amd64",
      exe_path: "/usr/local/bin/peakbot",
      data_dir: "/tmp/peakbot-data",
      version: "0.24.0",
      install: null,
    },
  };
}

// ── fixture docs ────────────────────────────────────────────────────────

const LIST_DOC = `
providers:
  - name: openrouter
    type: openrouter
    models:
      - name: anthropic/claude-sonnet-4.5
        alias: sonnet
profiles:
  solo:
    agents:
      main:
        model: sonnet
  team:
    agents:
      lead:
        model: sonnet
  dev:
    agents:
      main:
        model: sonnet
`;

const PAGE_DOC = `
profiles:
  dev:
    agents:
      main:
        model: sonnet
  ops:
    agents:
      main:
        model: sonnet
`;

const SINGLE_DOC = `
profiles:
  solo:
    agents:
      main:
        model: sonnet
`;

const DUP_TAKEN_DOC = `
profiles:
  dev:
    agents:
      main:
        model: sonnet
  dev-copy:
    agents:
      main:
        model: sonnet
  ops:
    agents:
      main:
        model: sonnet
`;

function mountList(yaml: string): Edit[] {
  const edits: Edit[] = [];
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: meta(),
    doc: parse(yaml),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: [],
  };
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <ProfilesPane />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
  return edits;
}

interface PageOpts {
  scope?: Scope;
  activeProfile?: string | null;
  onCopyToRepo?: (name: string, def: Record<string, unknown>) => void;
  onOpenProfile?: (name: string) => void;
}

function mountPage(yaml: string, profile: string, opts: PageOpts = {}): Edit[] {
  const edits: Edit[] = [];
  const ctx: SettingsCtxValue = {
    scope: opts.scope ?? "global",
    meta: meta(),
    doc: parse(yaml),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: [],
  };
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <ProfilePage
          profile={profile}
          activeProfile={opts.activeProfile}
          onCopyToRepo={opts.onCopyToRepo}
          onOpenProfile={opts.onOpenProfile}
        />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
  return edits;
}

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function eqPath(a: Seg[], b: Seg[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

function lastWrite(edits: Edit[], path: Seg[]): Edit | undefined {
  for (let i = edits.length - 1; i >= 0; i--) {
    if (eqPath(edits[i].path, path)) return edits[i];
  }
  return undefined;
}

describe("profiles list + profile page (T27)", () => {
  // §9.2: "Make default" moves the card to the top. YAML map = key
  // reorder; the pane writes the new map to ["profiles"].
  it("make_default_moves_first", () => {
    // Pure helper: rebuild with the key first, rest in order, no mutation.
    const src: Record<string, unknown> = { solo: 1, team: 2, dev: 3 };
    const moved = makeDefault(src, "team");
    expect(Object.keys(moved)).toEqual(["team", "solo", "dev"]);
    expect(Object.keys(src)).toEqual(["solo", "team", "dev"]); // untouched
    // Already first → same order.
    expect(Object.keys(makeDefault(moved, "team"))).toEqual([
      "team",
      "solo",
      "dev",
    ]);
    // Unknown name → throws, names the profile.
    expect(() => makeDefault(moved, "ghost")).toThrow(/no such profile: ghost/);

    // Pane: click on the SECOND card reorders the YAML map.
    const edits = mountList(LIST_DOC);
    // Cards render in map order; the DEFAULT badge and the Make-default
    // button belong to the first card only (badge) / the rest (button).
    expect(q("[data-testid='profile-card-0']").textContent).toContain("solo");
    expect(q("[data-testid='profile-card-1']").textContent).toContain("team");
    expect(q("[data-testid='profile-card-2']").textContent).toContain("dev");
    expect(
      q("[data-testid='profile-card-0']").querySelector(
        "[data-testid='profile-badge-default']",
      ),
    ).not.toBeNull();
    expect(
      q("[data-testid='profile-card-1']").querySelector(
        "[data-testid='profile-badge-default']",
      ),
    ).toBeNull();
    expect(
      host.querySelector("[data-testid='profile-make-default-0']"),
      "no Make-default button on the first card",
    ).toBeNull();

    click(q("[data-testid='profile-make-default-1']"));
    const write = lastWrite(edits, ["profiles"]);
    expect(write, "profiles map rewritten").toBeDefined();
    expect(Object.keys(write!.value as object)).toEqual(["team", "solo", "dev"]);

    // Stringified order through yamlDoc: team: precedes solo: precedes dev:.
    const doc = parse(LIST_DOC);
    set(doc, ["profiles"], write!.value);
    const s = stringify(doc);
    expect(s.indexOf("team:")).toBeLessThan(s.indexOf("solo:"));
    expect(s.indexOf("solo:")).toBeLessThan(s.indexOf("dev:"));
  });

  // §9.3: "Copy to this repo" — `-copy` suffix only if the name is taken
  // in the repo draft; the pane hands the def to the shell's callback.
  it("copy_to_repo_suffix", () => {
    expect(copyToRepoName("dev", [])).toBe("dev");
    expect(copyToRepoName("dev", ["ops"])).toBe("dev");
    expect(copyToRepoName("dev", ["dev"])).toBe("dev-copy");
    expect(copyToRepoName("dev", ["ops", "dev"])).toBe("dev-copy");

    // Global scope: the button calls onCopyToRepo(name, full def).
    let called: [string, Record<string, unknown>] | null = null;
    mountPage(PAGE_DOC, "dev", {
      onCopyToRepo: (name, def) => {
        called = [name, def];
      },
    });
    click(q("[data-testid='profile-copy-repo']"));
    expect(called, "onCopyToRepo called").not.toBeNull();
    expect(called![0]).toBe("dev");
    expect(called![1]).toEqual(get(parse(PAGE_DOC), ["profiles", "dev"]));

    // Repo scope: the button is absent (global scope only).
    unmount();
    mountPage(PAGE_DOC, "dev", { scope: "repo" });
    expect(
      host.querySelector("[data-testid='profile-copy-repo']"),
      "no Copy-to-repo in repo scope",
    ).toBeNull();
  });

  // §9.2: deleting the last profile is refused — toast, no edit.
  it("delete_last_profile_refused", () => {
    const edits = mountPage(SINGLE_DOC, "solo");
    click(q("[data-testid='profile-delete']"));
    expect(edits, "no edit for a refused delete").toHaveLength(0);
    expect(host.textContent).toContain("A config needs at least one profile");

    // With a second profile, Delete removes the name from the map.
    unmount();
    const edits2 = mountPage(PAGE_DOC, "dev");
    click(q("[data-testid='profile-delete']"));
    expect(host.textContent).toContain("Deleted dev");
    const w = lastWrite(edits2, ["profiles"]);
    expect(w, "profiles map rewritten").toBeDefined();
    expect(Object.keys(w!.value as object)).toEqual(["ops"]);
  });

  // §9.3: Duplicate inserts `<name>-copy[-N]` directly AFTER the source,
  // opens it, toasts `Duplicated as <n>`.
  it("duplicate_profile_appends_copy", () => {
    expect(duplicateName("dev", [])).toBe("dev-copy");
    expect(duplicateName("dev", ["dev-copy"])).toBe("dev-copy-2");
    expect(duplicateName("dev", ["dev-copy", "dev-copy-2"])).toBe("dev-copy-3");

    let opened: string | null = null;
    const edits = mountPage(PAGE_DOC, "dev", {
      onOpenProfile: (n) => {
        opened = n;
      },
    });
    click(q("[data-testid='profile-duplicate']"));
    expect(host.textContent).toContain("Duplicated as dev-copy");
    expect(opened, "the copy is opened").toBe("dev-copy");
    const w = lastWrite(edits, ["profiles"]);
    expect(w, "profiles map rewritten").toBeDefined();
    const map = w!.value as Record<string, unknown>;
    // Inserted directly after the source: dev, dev-copy, ops.
    expect(Object.keys(map)).toEqual(["dev", "dev-copy", "ops"]);
    expect(map["dev-copy"]).toEqual(map["dev"]); // full copy of the source

    // Name already taken → the -copy-2 suffix.
    unmount();
    const edits2 = mountPage(DUP_TAKEN_DOC, "dev");
    click(q("[data-testid='profile-duplicate']"));
    expect(host.textContent).toContain("Duplicated as dev-copy-2");
    const w2 = lastWrite(edits2, ["profiles"]);
    expect(Object.keys(w2!.value as object)).toEqual([
      "dev",
      "dev-copy",
      "dev-copy-2",
      "ops",
    ]);
  });

  // §9.3 Interface card: checked = visible; serialize as
  // ui: { tabs: { disabled: [unchecked…] } }, omit when all checked.
  it("tabs_serialize_disabled", () => {
    const edits = mountPage(PAGE_DOC, "dev");
    const tabs: Seg[] = ["profiles", "dev", "ui", "tabs"];

    // No ui.tabs in the doc → every tab checkbox starts checked.
    for (const t of TAB_NAMES) {
      const cb = q<HTMLInputElement>(`[data-testid='profile-tabs-chip-${t}']`);
      expect(cb.checked, `tab ${t} checked by default`).toBe(true);
    }

    click(q("[data-testid='profile-tabs-chip-bash']"));
    let e = lastWrite(edits, tabs);
    expect(e, "ui.tabs written").toBeDefined();
    expect(e!.value).toEqual({ disabled: ["bash"] });

    // A second uncheck: the `disabled` list is in meta.tab_names order.
    click(q("[data-testid='profile-tabs-chip-files']"));
    e = lastWrite(edits, tabs);
    expect(e!.value).toEqual({ disabled: ["files", "bash"] });

    // All visible again → the key is omitted (undefined), not
    // `enabled: true`.
    click(q("[data-testid='profile-tabs-chip-bash']"));
    click(q("[data-testid='profile-tabs-chip-files']"));
    e = lastWrite(edits, tabs);
    expect(e, "ui.tabs rewrite for all-checked").toBeDefined();
    expect(e!.value, "all checked omits the key").toBeUndefined();
  });

  // §9.3 (Q8): unchecking `settings` on the ACTIVE profile asks first —
  // exact copy, Hide Settings / Cancel; no edit until confirmed.
  it("confirm_before_hiding_settings_tab", () => {
    const edits = mountPage(PAGE_DOC, "dev", { activeProfile: "dev" });
    const tabs: Seg[] = ["profiles", "dev", "ui", "tabs"];

    click(q("[data-testid='profile-tabs-chip-settings']"));
    const dlg = host.querySelector("[data-testid='profile-tabs-confirm']");
    expect(dlg, "confirm dialog appears").not.toBeNull();
    expect(dlg!.textContent).toContain("Hide Settings for 'dev'?");
    expect(dlg!.textContent).toContain("edit config.yaml by hand");
    expect(edits, "no edit while the dialog is open").toHaveLength(0);

    // Cancel: dialog gone, the checkbox is still checked, still no edit.
    click(q("[data-testid='profile-tabs-confirm-cancel']"));
    expect(host.querySelector("[data-testid='profile-tabs-confirm']")).toBeNull();
    expect(
      q<HTMLInputElement>("[data-testid='profile-tabs-chip-settings']").checked,
      "Cancel leaves the tab visible",
    ).toBe(true);
    expect(edits, "no edit after cancel").toHaveLength(0);

    // Re-open and confirm → the write happens.
    click(q("[data-testid='profile-tabs-chip-settings']"));
    click(q("[data-testid='profile-tabs-confirm-hide']"));
    expect(host.querySelector("[data-testid='profile-tabs-confirm']")).toBeNull();
    const e = lastWrite(edits, tabs);
    expect(e, "ui.tabs written on confirm").toBeDefined();
    expect(e!.value).toEqual({ disabled: ["settings"] });

    // A NON-active profile hides `settings` immediately — no confirm.
    unmount();
    const edits2 = mountPage(PAGE_DOC, "dev", { activeProfile: "other" });
    click(q("[data-testid='profile-tabs-chip-settings']"));
    expect(
      host.querySelector("[data-testid='profile-tabs-confirm']"),
      "no confirm for a non-active profile",
    ).toBeNull();
    const e2 = lastWrite(edits2, tabs);
    expect(e2, "ui.tabs written").toBeDefined();
    expect(e2!.value).toEqual({ disabled: ["settings"] });
  });
});
