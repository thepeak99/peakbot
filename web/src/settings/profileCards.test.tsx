// T27b (RED): the remaining profile-page cards — Tools, MCP access,
// Environment, Memory, Knowledge (vector_db) — spec §9.3 / §3.3.
// The cards are NOT built yet inside ./profiles `ProfilePage`; ProfilePage
// itself exists, so these tests fail at runtime on missing testids
// (`missing element: …`), which is the intended RED. Props/testids:
// contract.ts "./profiles — remaining cards (T27b)".
//
// Harness mimics the shell: `edit` mutates the draft doc IN PLACE (yamlDoc
// `set`) and re-renders, so assertions read the resulting doc via `get`
// (implementation-agnostic) and, where the exact written value matters,
// the recorded edits.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilePage } from "./profiles";
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
}

afterEach(unmount);

interface Edit {
  path: Seg[];
  value: unknown;
}

function meta(builtin: string[]): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: builtin,
    tab_names: ["session", "settings"],
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

const BASE = `
providers:
  - name: openrouter
    type: openrouter
    models:
      - name: anthropic/claude-sonnet-4.5
        alias: sonnet
`;

function profileDoc(profileExtra = "", top = ""): string {
  return `${BASE}${top}
profiles:
  dev:
    agents:
      main:
        model: sonnet
${profileExtra}`;
}

interface Opts {
  scope?: Scope;
  tools?: string[];
  globalMcpNames?: string[];
}

interface Mounted {
  doc: Doc;
  edits: Edit[];
}

function Harness({
  doc,
  edits,
  opts,
}: {
  doc: Doc;
  edits: Edit[];
  opts: Opts;
}) {
  const [, bump] = useState(0);
  const ctx: SettingsCtxValue = {
    scope: opts.scope ?? "global",
    meta: meta(opts.tools ?? ["bash", "read_file", "fetch_url"]),
    doc,
    globalDoc: doc,
    edit: (path, value) => {
      set(doc, path, value);
      edits.push({ path, value });
      bump((n) => n + 1);
    },
    errors: [],
    switchToGlobal: () => {},
  };
  return (
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <ProfilePage profile="dev" globalMcpNames={opts.globalMcpNames} />
      </SettingsCtx.Provider>
    </ToastProvider>
  );
}

function mountPage(yaml: string, opts: Opts = {}): Mounted {
  unmount();
  const doc = parse(yaml);
  const edits: Edit[] = [];
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<Harness doc={doc} edits={edits} opts={opts} />));
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

function chipIds(containerTestid: string): string[] {
  return Array.from(
    q(tid(containerTestid)).querySelectorAll("[data-testid^='chip-']"),
  )
    .map((e) => e.getAttribute("data-testid")!)
    .filter((t) => !t.endsWith("-remove"));
}

function lastWrite(edits: Edit[], path: Seg[]): Edit | undefined {
  for (let i = edits.length - 1; i >= 0; i--) {
    const p = edits[i].path;
    if (p.length === path.length && p.every((x, j) => x === path[j])) return edits[i];
  }
  return undefined;
}

const P = (...rest: Seg[]): Seg[] => ["profiles", "dev", ...rest];

describe("profile page cards (T27b)", () => {
  // §9.3 Tools: the grid is meta.builtin_tools — never hard-coded; delegate
  // is not in the list (the server omits it from builtin_tools).
  it("tools_filter_universe_is_builtin_tools", () => {
    // Deliberately unusual names prove the universe is NOT hard-coded.
    const { doc, edits } = mountPage(profileDoc(), {
      tools: ["alpha_tool", "beta_tool"],
    });
    expect(chipIds("profile-tools")).toEqual(["chip-alpha_tool", "chip-beta_tool"]);
    expect(has(tid("profile-tools") + " " + tid("chip-delegate"))).toBe(false);
    expect(q(tid("profile-tools-footnote")).textContent).toContain(
      "delegate is on automatically",
    );

    // Every tool (default) → key omitted.
    expect(get(doc, P("tools"))).toBeUndefined();

    // Blocklist + one checked → { disabled: [...] }.
    click(q(tid("profile-tools") + " " + tid("mode-disabled")));
    click(q(tid("profile-tools") + " " + tid("chip-beta_tool")));
    expect(get(doc, P("tools"))).toEqual({ disabled: ["beta_tool"] });

    // Switching mode clears the checked set (spec: no silent inversion);
    // an allowlist with nothing checked is { enabled: false }.
    click(q(tid("profile-tools") + " " + tid("mode-only")));
    expect(get(doc, P("tools"))).toEqual({ enabled: false });
    click(q(tid("profile-tools") + " " + tid("chip-alpha_tool")));
    expect(get(doc, P("tools"))).toEqual({ only: ["alpha_tool"] });

    // Back to "every tool" → the key is omitted (not `enabled: true`).
    click(q(tid("profile-tools") + " " + tid("mode-all")));
    expect(lastWrite(edits, P("tools"))!.value).toBeUndefined();
    expect(get(doc, P("tools"))).toBeUndefined();
  });

  it("tools_card_fetch_url_allow_post_toggle_writes_bool", () => {
    const { doc } = mountPage(profileDoc());
    const box = q<HTMLInputElement>(tid("profile-tools-allowpost") + " " + tid("input"));
    expect(box.checked).toBe(false); // absent = false
    click(box);
    expect(get(doc, P("fetch_url", "allow_post"))).toBe(true);
  });

  // §9.3 MCP access: chips come from the declared `mcps` names only.
  it("mcp_access_filter_over_declared_mcps", () => {
    const top = `
mcps:
  - name: playwright
    type: stdio
    command: npx
  - name: linear
    type: streamable-http
    url: https://mcp.linear.app/mcp
`;
    const { doc } = mountPage(
      profileDoc("    mcps:\n      only: [linear]\n", top),
    );
    expect(chipIds("profile-mcps")).toEqual(["chip-playwright", "chip-linear"]);
    expect(has(tid("profile-mcps-empty"))).toBe(false);

    // Existing allowlist is parsed: checking another extends it.
    click(q(tid("profile-mcps") + " " + tid("chip-playwright")));
    expect(get(doc, P("mcps"))).toEqual({ only: ["linear", "playwright"] });

    // Mode switch clears the checks; blocklist writes { disabled }.
    click(q(tid("profile-mcps") + " " + tid("mode-disabled")));
    expect(get(doc, P("mcps"))).toBeUndefined(); // blocklist w/ 0 checked
    click(q(tid("profile-mcps") + " " + tid("chip-playwright")));
    expect(get(doc, P("mcps"))).toEqual({ disabled: ["playwright"] });

    // Allowlist with none checked → { enabled: false }.
    click(q(tid("profile-mcps") + " " + tid("mode-only")));
    expect(get(doc, P("mcps"))).toEqual({ enabled: false });
  });

  it("mcp_access_shows_empty_note_when_no_servers_declared", () => {
    mountPage(profileDoc());
    expect(q(tid("profile-mcps-empty")).textContent).toContain(
      "No MCP servers declared globally.",
    );
    expect(chipIds("profile-mcps")).toEqual([]);
  });

  it("mcp_access_universe_comes_from_globalMcpNames_in_repo_scope", () => {
    mountPage(profileDoc(), { scope: "repo", globalMcpNames: ["gh", "docs"] });
    expect(chipIds("profile-mcps")).toEqual(["chip-gh", "chip-docs"]);
    expect(has(tid("profile-mcps-empty"))).toBe(false);
  });

  // §9.3 Environment: string map; blank keys dropped; empty = key removed.
  it("env_rows_write_map", () => {
    const { doc } = mountPage(profileDoc("    env:\n      FOO: bar\n"));
    expect(q<HTMLInputElement>(tid("profile-env") + " " + tid("row-0-key")).value).toBe("FOO");
    expect(q<HTMLInputElement>(tid("profile-env") + " " + tid("row-0-value")).value).toBe("bar");

    typeInput(q(tid("profile-env") + " " + tid("row-0-value")), "baz");
    expect(get(doc, P("env"))).toEqual({ FOO: "baz" });

    // + add row → type key then value.
    click(q(tid("profile-env") + " " + tid("add")));
    expect(get(doc, P("env"))).toEqual({ FOO: "baz" }); // blank-key row dropped
    typeInput(q(tid("profile-env") + " " + tid("row-1-key")), "TOKEN");
    typeInput(q(tid("profile-env") + " " + tid("row-1-value")), "1");
    expect(get(doc, P("env"))).toEqual({ FOO: "baz", TOKEN: "1" });
    // Values stay STRINGS even when they look numeric.
    expect((get(doc, P("env")) as Record<string, unknown>).TOKEN).toBe("1");

    // ✕ removes the row; removing the last one removes the key.
    click(q(tid("profile-env") + " " + tid("row-0-remove")));
    expect(get(doc, P("env"))).toEqual({ TOKEN: "1" });
    click(q(tid("profile-env") + " " + tid("row-0-remove")));
    expect(get(doc, P("env"))).toBeUndefined();
  });

  it("env_card_copy_says_bash_only_not_mcp", () => {
    mountPage(profileDoc());
    // Spec §9.3 [changed copy]: the profile env does not reach MCP servers.
    expect(q(tid("profile-env")).closest("section,div")!.textContent).toBeDefined();
    const card = q(tid("profile-env")).parentElement!.textContent ?? "";
    expect(card).toContain("exported to bash for the agent and its subagents");
    expect(card).not.toContain("MCP-launched");
  });

  // §9.3 Memory.
  it("memory_toggle_and_threshold_number", () => {
    // Absent block → enabled reads true (default), threshold empty.
    let m = mountPage(profileDoc());
    expect(q<HTMLInputElement>(tid("profile-memory-enabled") + " " + tid("input")).checked).toBe(true);
    expect(q<HTMLInputElement>(tid("profile-memory-threshold") + " " + tid("input")).value).toBe("");

    click(q(tid("profile-memory-enabled") + " " + tid("input")));
    expect(get(m.doc, P("memory", "enabled"))).toBe(false);
    click(q(tid("profile-memory-enabled") + " " + tid("input")));
    expect(get(m.doc, P("memory", "enabled"))).toBe(true);

    // Threshold is written as a NUMBER, never a string; empty removes it.
    typeInput(q(tid("profile-memory-threshold") + " " + tid("input")), "102400");
    expect(get(m.doc, P("memory", "threshold_bytes"))).toBe(102400);
    typeInput(q(tid("profile-memory-threshold") + " " + tid("input")), "");
    expect(get(m.doc, P("memory", "threshold_bytes"))).toBeUndefined();

    // Existing values are shown.
    m = mountPage(
      profileDoc("    memory:\n      enabled: false\n      threshold_bytes: 2048\n"),
    );
    expect(q<HTMLInputElement>(tid("profile-memory-enabled") + " " + tid("input")).checked).toBe(false);
    expect(q<HTMLInputElement>(tid("profile-memory-threshold") + " " + tid("input")).value).toBe("2048");
  });

  // §9.3 Knowledge: presence = on; off removes the WHOLE block.
  it("knowledge_card_writes_vector_db_block_and_removes_it", () => {
    const { doc, edits } = mountPage(profileDoc());
    const toggle = () => q<HTMLInputElement>(tid("profile-vdb-toggle") + " " + tid("input"));
    expect(toggle().checked).toBe(false);
    for (const f of ["dbpath", "baseurl", "apikey", "model", "dimensions"]) {
      expect(has(tid(`profile-vdb-${f}`)), `${f} hidden while off`).toBe(false);
    }

    click(toggle());
    const created = lastWrite(edits, P("vector_db"))!.value as Record<string, unknown>;
    expect(created).toBeTypeOf("object");
    expect(Array.isArray(created)).toBe(false);
    expect(created.embeddings).toBeTypeOf("object");
    expect(toggle().checked).toBe(true);
    for (const f of ["dbpath", "baseurl", "apikey", "model", "dimensions"]) {
      expect(has(tid(`profile-vdb-${f}`)), `${f} shown while on`).toBe(true);
    }

    typeInput(q(tid("profile-vdb-dbpath") + " " + tid("input")), ".peakbot/vec.db");
    typeInput(q(tid("profile-vdb-baseurl") + " " + tid("input")), "http://127.0.0.1:11434/v1");
    typeInput(q(tid("profile-vdb-model") + " " + tid("input")), "bge-m3");
    typeInput(q(tid("profile-vdb-dimensions") + " " + tid("input")), "1024");
    const block = get(doc, P("vector_db")) as {
      db_path: string;
      embeddings: Record<string, unknown>;
    };
    expect(block.db_path).toBe(".peakbot/vec.db");
    expect(block.embeddings.base_url).toBe("http://127.0.0.1:11434/v1");
    expect(block.embeddings.model).toBe("bge-m3");
    expect(block.embeddings.dimensions).toBe(1024); // number, not "1024"

    // Off → the whole block is gone (no `enabled: false`).
    click(toggle());
    expect(lastWrite(edits, P("vector_db"))!.value).toBeUndefined();
    expect(get(doc, P("vector_db"))).toBeUndefined();
    expect(has(tid("profile-vdb-dbpath"))).toBe(false);
  });

  it("knowledge_card_shows_existing_block_and_masks_api_key", () => {
    const yaml = profileDoc(
      [
        "    vector_db:",
        "      db_path: vec.db",
        "      embeddings:",
        "        base_url: https://api.openai.com/v1",
        "        api_key: sk-super-secret-123",
        "        model: text-embedding-3-small",
        "        dimensions: 1536",
        "",
      ].join("\n"),
    );
    const { doc } = mountPage(yaml);
    expect(q<HTMLInputElement>(tid("profile-vdb-toggle") + " " + tid("input")).checked).toBe(true);
    expect(q<HTMLInputElement>(tid("profile-vdb-dbpath") + " " + tid("input")).value).toBe("vec.db");
    expect(q<HTMLInputElement>(tid("profile-vdb-dimensions") + " " + tid("input")).value).toBe("1536");
    // Secret is masked: its text is not in the DOM.
    expect(host.innerHTML).not.toContain("sk-super-secret-123");
    // Clearing the dimensions removes the key (never 0 / "").
    typeInput(q(tid("profile-vdb-dimensions") + " " + tid("input")), "");
    expect(
      (get(doc, P("vector_db", "embeddings")) as Record<string, unknown>).dimensions,
    ).toBeUndefined();
  });

  it("knowledge_db_path_hint_depends_on_scope", () => {
    const yaml = profileDoc("    vector_db:\n      db_path: v.db\n      embeddings:\n        model: m\n        dimensions: 3\n");
    mountPage(yaml, { scope: "global" });
    expect(q(tid("profile-vdb-dbpath") + " " + tid("hint")).textContent).toContain(
      "working directory",
    );
    mountPage(yaml, { scope: "repo" });
    expect(q(tid("profile-vdb-dbpath") + " " + tid("hint")).textContent).toContain(
      "this repo",
    );
  });
});
