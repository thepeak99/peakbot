// T29 (RED): D6 "General" pane (spec §9.7, plan T29).
// The ./general module does not exist yet — this file must fail with a
// missing-module import error. Panes read the draft via useSettingsCtx()
// and write through `edit(path, value)` by segment array; network keys stay
// FLAT (general.retry / timeouts / http / uploads — never general.network).
// Props/testids/helpers: contract.ts "./general (T29)".
//
// Harness mimics the shell: `edit` mutates the draft doc IN PLACE (yamlDoc
// `set`) and re-renders, so assertions read the resulting doc via `get` and,
// where the exact written value matters, the recorded edits.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  GeneralPane,
  accessModeOf,
  bindForAccess,
  generateToken,
  launchCommand,
} from "./general";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { get, parse, set, type Doc } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { Scope, Seg, SettingsMeta } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | undefined;
let host: HTMLElement;

function unmount() {
  if (!root) return;
  act(() => root!.unmount());
  host.remove();
  root = undefined;
  vi.unstubAllGlobals();
}

afterEach(unmount);

interface Edit {
  path: Seg[];
  value: unknown;
}

function meta(boot: SettingsMeta["boot"]): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "",
    mcp_status: [],
    boot,
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

const BASE = `
providers:
  - name: openrouter
    type: openrouter
    models:
      - name: anthropic/claude-sonnet-4.5
        alias: sonnet
`;

function Harness({
  doc,
  edits,
  scope,
  boot,
}: {
  doc: Doc;
  edits: Edit[];
  scope: Scope;
  boot: SettingsMeta["boot"];
}) {
  const [, bump] = useState(0);
  const ctx: SettingsCtxValue = {
    scope,
    meta: meta(boot),
    doc,
    edit: (path, value) => {
      set(doc, path, value);
      edits.push({ path, value });
      bump((n) => n + 1);
    },
    errors: [],
  };
  return (
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <GeneralPane />
      </SettingsCtx.Provider>
    </ToastProvider>
  );
}

interface Mounted {
  doc: Doc;
  edits: Edit[];
}

function mountPane(
  yaml: string,
  opts: { scope?: Scope; boot?: SettingsMeta["boot"] } = {},
): Mounted {
  unmount();
  const doc = parse(yaml);
  const edits: Edit[] = [];
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <Harness
        doc={doc}
        edits={edits}
        scope={opts.scope ?? "global"}
        boot={opts.boot ?? { bind: "127.0.0.1:7823", token_source: "none", tls: false }}
      />,
    ),
  );
  return { doc, edits };
}

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function has(sel: string): boolean {
  return host.querySelector(sel) !== null;
}

function tid(id: string): string {
  return `[data-testid='${id}']`;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function typeInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

// A real user click: React 19 listens to `click` for checkboxes and ignores a
// bare dispatched `change` after `el.checked` was flipped by hand (its value
// tracker has already seen the new value), so the original helper never
// reached onChange. `el.click()` flips the box AND fires the click React uses.
function toggle(el: HTMLInputElement) {
  act(() => {
    el.click();
  });
}

function lastWrite(edits: Edit[], path: Seg[]): Edit | undefined {
  for (let i = edits.length - 1; i >= 0; i--) {
    const p = edits[i].path;
    if (p.length === path.length && p.every((x, j) => x === path[j])) return edits[i];
  }
  return undefined;
}

describe("general helpers (T29)", () => {
  it("accessModeOf_derives_from_loopback", () => {
    expect(accessModeOf("127.0.0.1:7823")).toBe("local");
    expect(accessModeOf("127.0.5.9:1")).toBe("local");
    expect(accessModeOf("localhost:7823")).toBe("local");
    expect(accessModeOf("[::1]:7823")).toBe("local");
    expect(accessModeOf("0.0.0.0:7823")).toBe("lan");
    expect(accessModeOf("[::]:9000")).toBe("lan");
    expect(accessModeOf("192.168.1.20:7823")).toBe("lan");
    expect(accessModeOf(undefined)).toBe("local");
  });

  it("bindForAccess_rewrites_host_keeps_port", () => {
    expect(bindForAccess("0.0.0.0:9123", "local")).toBe("127.0.0.1:9123");
    expect(bindForAccess("127.0.0.1:9123", "lan")).toBe("0.0.0.0:9123");
    // Idempotent.
    expect(bindForAccess("127.0.0.1:7823", "local")).toBe("127.0.0.1:7823");
    expect(bindForAccess("0.0.0.0:7823", "lan")).toBe("0.0.0.0:7823");
    // No bind → the default port 7823.
    expect(bindForAccess(undefined, "local")).toBe("127.0.0.1:7823");
    expect(bindForAccess(undefined, "lan")).toBe("0.0.0.0:7823");
  });

  it("generateToken_is_32_lower_hex", () => {
    for (let i = 0; i < 8; i++) {
      const t = generateToken();
      expect(t).toMatch(/^[0-9a-f]{32}$/);
    }
    // Random: repeated draws are not identical.
    expect(new Set(Array.from({ length: 8 }, () => generateToken())).size).toBeGreaterThan(1);
  });

  it("launchCommand_previews_flags", () => {
    expect(launchCommand({ bind: "127.0.0.1:7823", tls: false, hasToken: false })).toBe(
      "peakbot --bind 127.0.0.1:7823",
    );
    expect(launchCommand({ bind: "0.0.0.0:9000", tls: true, hasToken: true })).toBe(
      "peakbot --bind 0.0.0.0:9000 --tls --token ••••",
    );
  });
});

describe("general pane (T29)", () => {
  // §9.7: the radio is derived from web.bind and rewrites it, keeping the port.
  it("access_radio_rewrites_bind", () => {
    const { doc, edits } = mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`);

    const local = q<HTMLInputElement>(tid("general-access-local"));
    const lan = q<HTMLInputElement>(tid("general-access-lan"));
    expect(local.checked).toBe(true);
    expect(lan.checked).toBe(false);
    expect(q<HTMLInputElement>(tid("general-bind") + " " + tid("input")).value).toBe(
      "127.0.0.1:7823",
    );

    click(lan);
    expect(get(doc, ["general", "web", "bind"])).toBe("0.0.0.0:7823");
    expect(lan.checked).toBe(true);
    expect(local.checked).toBe(false);

    // Back to local: the port is kept.
    click(local);
    expect(lastWrite(edits, ["general", "web", "bind"])!.value).toBe("127.0.0.1:7823");
    expect(get(doc, ["general", "web", "bind"])).toBe("127.0.0.1:7823");

    // A non-default port survives the round trip.
    click(lan);
    typeInput(q(tid("general-bind") + " " + tid("input")), "0.0.0.0:9123");
    click(local);
    expect(get(doc, ["general", "web", "bind"])).toBe("127.0.0.1:9123");
  });

  it("generate_token_32_hex", () => {
    const copy = vi.fn();
    vi.stubGlobal("navigator", { clipboard: { writeText: copy } });

    const { doc, edits } = mountPane(`${BASE}general:\n  web:\n    bind: 0.0.0.0:7823\n`);
    const field = q(tid("general-token"));
    // Masked: nothing secret in the DOM.
    expect(field.textContent).not.toContain("•••• (none");

    click(q(tid("general-token-generate")));
    const token = get(doc, ["general", "web", "token"]) as string;
    expect(token).toMatch(/^[0-9a-f]{32}$/);
    expect(lastWrite(edits, ["general", "web", "token"])!.value).toBe(token);

    click(q(tid("general-token-copy")));
    expect(copy).toHaveBeenCalledWith(token);
  });

  it("token_source_note_when_env_overrides", () => {
    // Boot overridden by the environment → the note names the variable.
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`, {
      boot: { bind: "127.0.0.1:7823", token_source: "env", tls: false },
    });
    expect(q(tid("general-token-note")).textContent).toContain("PEAKBOT_WEB_TOKEN");

    // Boot overridden by the CLI flag → the note names --token.
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`, {
      boot: { bind: "127.0.0.1:7823", token_source: "cli", tls: false },
    });
    expect(q(tid("general-token-note")).textContent).toContain("--token");

    // Token from the config file (or none) → no note.
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`, {
      boot: { bind: "127.0.0.1:7823", token_source: "config", tls: false },
    });
    expect(has(tid("general-token-note"))).toBe(false);
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`, {
      boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
    });
    expect(has(tid("general-token-note"))).toBe(false);
  });

  // §9.7: presence = on — the toggle adds / removes the WHOLE search block.
  it("search_toggle_removes_block", () => {
    const { doc, edits } = mountPane(
      `${BASE}general:\n  search:\n    base_url: http://127.0.0.1:8080\n    max_results: 10\n`,
    );
    const on = () => q<HTMLInputElement>(tid("general-search-toggle") + " " + tid("input"));
    expect(on().checked).toBe(true);
    expect(
      q<HTMLInputElement>(tid("general-search-baseurl") + " " + tid("input")).value,
    ).toBe(
      "http://127.0.0.1:8080",
    );

    // Editing a field while on writes into the block.
    typeInput(q(tid("general-search-baseurl") + " " + tid("input")), "http://search:8888");
    expect(get(doc, ["general", "search", "base_url"])).toBe("http://search:8888");
    typeInput(q(tid("general-search-maxresults") + " " + tid("input")), "5");
    expect(get(doc, ["general", "search", "max_results"])).toBe(5);

    // Off → the whole block is gone (not `enabled: false`).
    toggle(on());
    expect(lastWrite(edits, ["general", "search"])!.value).toBeUndefined();
    expect(get(doc, ["general", "search"])).toBeUndefined();
    expect(on().checked).toBe(false);
    expect(has(tid("general-search-baseurl"))).toBe(false);

    // On again → a new plain object.
    toggle(on());
    const created = lastWrite(edits, ["general", "search"])!.value as Record<string, unknown>;
    expect(created).toBeTypeOf("object");
    expect(Array.isArray(created)).toBe(false);
    expect(get(doc, ["general", "search"])).toBeTypeOf("object");
  });

  it("number_fields_write_numbers", () => {
    const { doc, edits } = mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`);
    const cases: [string, Seg[]][] = [
      ["general-timeout-tool", ["general", "timeouts", "tool_secs"]],
      ["general-timeout-delegate", ["general", "timeouts", "delegate_secs"]],
      ["general-http-connect", ["general", "http", "connect_timeout_secs"]],
      ["general-http-read", ["general", "http", "read_timeout_secs"]],
      ["general-uploads-maxfile", ["general", "uploads", "max_file_mb"]],
      ["general-uploads-maxfiles", ["general", "uploads", "max_files"]],
    ];
    for (const [id, path] of cases) {
      typeInput(q(tid(id) + " " + tid("input")), "42");
      const v = get(doc, path);
      expect(v, `${id} writes a number`).toBe(42);
      expect(lastWrite(edits, path)!.value, `${id} exact value`).toBe(42);
    }

    // Empty = key removed (the key disappears from the block, not 0/null).
    typeInput(q(tid("general-http-read") + " " + tid("input")), "");
    expect(get(doc, ["general", "http", "read_timeout_secs"])).toBeUndefined();
    expect(get(doc, ["general", "http", "connect_timeout_secs"])).toBe(42); // siblings kept

    // Pre-filled values are shown.
    mountPane(
      `${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n  timeouts:\n    tool_secs: 120\n  uploads:\n    max_files: 7\n`,
    );
    expect(
      q<HTMLInputElement>(tid("general-timeout-tool") + " " + tid("input")).value,
    ).toBe("120");
    expect(
      q<HTMLInputElement>(tid("general-uploads-maxfiles") + " " + tid("input")).value,
    ).toBe("7");
  });

  it("tls_toggle_writes_bool", () => {
    const { doc, edits } = mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`);
    const box = () => q<HTMLInputElement>(tid("general-tls") + " " + tid("input"));
    expect(box().checked).toBe(false); // absent = false

    toggle(box());
    expect(get(doc, ["general", "web", "tls"])).toBe(true);
    expect(lastWrite(edits, ["general", "web", "tls"])!.value).toBe(true);
    // The launch-command preview picks up the flag (masked token aside).
    expect(q(tid("general-launch")).textContent).toContain("--tls");

    toggle(box());
    expect(get(doc, ["general", "web", "tls"])).toBe(false);

    // Pre-filled tls: true renders checked.
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n    tls: true\n`);
    expect(
      q<HTMLInputElement>(tid("general-tls") + " " + tid("input")).checked,
    ).toBe(true);
  });

  it("restart_badges_mark_web_http_uploads_conversation", () => {
    mountPane(`${BASE}general:\n  web:\n    bind: 127.0.0.1:7823\n`);
    for (const k of ["web", "http", "uploads", "conversation"]) {
      expect(has(tid(`general-badge-restart-${k}`)), `${k} badge`).toBe(true);
    }
    // Search / Context are reload-safe.
    expect(has(tid("general-badge-restart-search"))).toBe(false);
    expect(has(tid("general-badge-restart-context"))).toBe(false);
  });
});
