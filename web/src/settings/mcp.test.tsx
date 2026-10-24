// T26 (RED): D3 "MCP servers" pane (spec §9.9, plan T26). The ./mcp module
// does not exist yet — this file must fail with a missing-module import
// error. The pane reads the draft via useSettingsCtx() (doc/edit/meta) and
// writes through context.ts `edit(path, value)` by segment array.
// Props/testids: contract.ts ("./mcp (T26)").

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { McpsPane } from "./mcp";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import type { Seg, SettingsMeta } from "./types";

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

const DOC = `
mcps:
  - name: playwright
    type: stdio
    command: npx
    args: ["@playwright/mcp@latest"]
  - name: linear
    type: streamable-http
    url: https://mcp.linear.app/mcp
  - name: legacy
    type: streamable-http
    url: https://x.example/mcp
    auth_token: tok-legacy
`;

function metaWith(
  status: SettingsMeta["mcp_status"],
): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: [],
    skills: [],
    builtin_persona: "",
    mcp_status: status,
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

function mountPane(): Edit[] {
  const edits: Edit[] = [];
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: metaWith([
      { name: "playwright", state: "connected" },
      { name: "linear", state: "failed" },
      // `legacy` has no status entry (not in the boot config) → no pill.
    ]),
    doc: parse(DOC),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: [],
  };
  mount(
    <SettingsCtx.Provider value={ctx}>
      <McpsPane />
    </SettingsCtx.Provider>,
  );
  return edits;
}

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function typeInput(el: HTMLInputElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  set.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function select(el: HTMLSelectElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(
    HTMLSelectElement.prototype,
    "value",
  )!.set!;
  set.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
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

describe("mcp pane (T26)", () => {
  // §9.9: a status pill per server from GET /api/settings.mcp_status
  // (connected / failed / disabled); a server with no status entry shows no
  // pill.
  it("mcp_status_pill", () => {
    mountPane();
    expect(host.querySelectorAll('[data-testid^="mcp-card-"]').length).toBe(3);
    expect(q('[data-testid="mcp-status-0"]').textContent).toBe("connected");
    expect(q('[data-testid="mcp-status-1"]').textContent).toBe("failed");
    expect(host.querySelector('[data-testid="mcp-status-2"]')).toBeNull();
  });

  // §9.9: the real `auth` is a TAGGED map — bearer writes
  // { type: "bearer", token }, never the legacy `auth_token` key.
  it("mcp_auth_bearer_shape", () => {
    const edits = mountPane();
    select(q<HTMLSelectElement>('[data-testid="mcp-auth-1"]'), "bearer");
    // Reveal the token secret, then type.
    click(q('[data-testid="mcp-auth-token-1"] [data-testid="reveal"]'));
    typeInput(
      q<HTMLInputElement>(
        '[data-testid="mcp-auth-token-1"] [data-testid="value"]',
      ),
      "tok-123",
    );
    const write = lastWrite(edits, ["mcps", 1, "auth"]);
    expect(write, "auth written").toBeDefined();
    expect(write!.value).toEqual({ type: "bearer", token: "tok-123" });
    expect(
      edits.some((e) => eqPath(e.path, ["mcps", 1, "auth_token"])),
      "legacy auth_token never written",
    ).toBe(false);
  });

  // §9.9: oauth writes { type: "oauth", client_id?, client_secret?,
  // scopes: [n…] } — scopes is a comma/chip list → YAML list; empty
  // client_id/secret are omitted.
  it("mcp_oauth_scopes_list", () => {
    const edits = mountPane();
    select(q<HTMLSelectElement>('[data-testid="mcp-auth-1"]'), "oauth");
    const addInput = q<HTMLInputElement>(
      '[data-testid="mcp-auth-scopes-1"] [data-testid="add-input"]',
    );
    typeInput(addInput, "read");
    act(() => {
      addInput.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      );
    });
    typeInput(addInput, "write");
    act(() => {
      addInput.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      );
    });
    const write = lastWrite(edits, ["mcps", 1, "auth"]);
    expect(write, "auth written").toBeDefined();
    expect(write!.value).toEqual({ type: "oauth", scopes: ["read", "write"] });
  });

  // §9.9: the deprecated `auth_token:` is shown read-only with
  // "Deprecated — use auth: bearer".
  it("legacy_auth_token_readonly", () => {
    const edits = mountPane();
    const el = q('[data-testid="mcp-authtoken-2"]');
    expect(el.textContent).toContain("tok-legacy");
    expect(el.textContent).toMatch(/Deprecated/);
    expect(el.textContent).toMatch(/use auth: bearer/);
    // Read-only: no editable input inside.
    expect(el.querySelector("input:not([readonly])")).toBeNull();
    // Merely displaying it must not write the key.
    expect(
      edits.some((e) => eqPath(e.path, ["mcps", 2, "auth_token"])),
    ).toBe(false);
  });

  it("mcp_type_select_writes_type", () => {
    const edits = mountPane();
    select(q<HTMLSelectElement>('[data-testid="mcp-type-0"]'), "streamable-http");
    expect(lastWrite(edits, ["mcps", 0, "type"])?.value).toBe(
      "streamable-http",
    );
  });

  it("mcp_enabled_toggle_writes_bool", () => {
    const edits = mountPane();
    // `enabled` is absent in the doc — the toggle reads it as true and
    // writes false on the first click.
    const toggleInput = q<HTMLInputElement>(
      '[data-testid="mcp-enabled-0"] [data-testid="input"]',
    );
    expect(toggleInput.checked).toBe(true);
    click(toggleInput);
    expect(lastWrite(edits, ["mcps", 0, "enabled"])?.value).toBe(false);
  });

  it("mcp_add_appends_entry", () => {
    const edits = mountPane();
    click(q('[data-testid="mcp-add"]'));
    const write = lastWrite(edits, ["mcps"]);
    expect(write, "+ Add MCP server writes the mcps list").toBeDefined();
    const list = write!.value as { name: string; type: string }[];
    expect(Array.isArray(list)).toBe(true);
    expect(list.length).toBe(4);
  });
});
