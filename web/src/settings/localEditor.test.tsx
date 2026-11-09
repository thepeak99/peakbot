// The Local edition's profile/agent editor: it must never offer host-only
// tools (`bash` and friends) or an `mcps` field.
//
// The editor renders the tool universe from `meta.builtin_tools` — the
// server's list for THIS edition — so the Local edition (whose build has no
// file/code tools at all) simply never sees `bash`. This file locks that
// end-to-end: a Local `meta` (the Local build's five built-ins, as the
// server sends them) fed through the REAL ProfilePage and AgentPage must
// render no `bash` chip and no MCP field. The full-edition guard proves the
// test is watching the universe (with a full `meta`, `bash` IS offered).
//
// Status: GREEN lock — the panes already derive the universe from
// `meta.builtin_tools` and the MCP card from the global MCP names, and a
// remote entry's form carries no local capabilities at all.
//
// Harness: ./profiles.t10.test.tsx conventions (ctx provider + raw DOM).

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilePage } from "./profiles";
import { AgentPage } from "./agent";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { SettingsMeta } from "./types";

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

// ─── fixtures ───────────────────────────────────────────────────────────

/// The Local build's built-ins (src/config/mod.rs, non-host): no tool that
/// touches the device's files or runs code exists in this edition.
const LOCAL_TOOLS = ["fetch_page", "fetch_url", "think", "todo", "web_search"];
/// The full (host) build's list — includes the host-only tools.
const FULL_TOOLS = [
  "bash",
  "bash_bg",
  "doc_index",
  "doc_search",
  "fetch_page",
  "fetch_url",
  "file_create",
  "file_insert",
  "file_read",
  "file_str_replace",
  "list_directory",
  "pdf_read",
  "powershell",
  "powershell_bg",
  "think",
  "todo",
  "view_image",
  "web_search",
];

function meta(builtinTools: string[]): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: builtinTools,
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "You are a coding agent.",
    mcp_status: [],
    boot: { bind: "127.0.0.1:1", token_source: "none", tls: false },
    facts: {
      os: "linux",
      arch: "arm64",
      exe_path: "/apex/peakbot",
      data_dir: "/tmp/peakbot-data",
      version: "0.27.0",
      install: null,
    },
  };
}

const DOC = `
providers:
  - name: ollama
    type: ollama
    base_url: http://127.0.0.1:11434
    models:
      - name: llama3.1:8b
        alias: llama
        max_tokens: 8192
profiles:
  On this phone:
    agents:
      main:
        model: llama
  dev on host:
    remote:
      url: https://10.2.0.48:7899
      token: pito
      ca: sha256:aa
      profile: dev
`;

function ctxFor(builtinTools: string[]): SettingsCtxValue {
  return {
    scope: "global",
    meta: meta(builtinTools),
    doc: parse(DOC),
    globalDoc: parse(DOC),
    edit: () => {},
    errors: [],
    switchToGlobal: () => {},
  };
}

function mountProfilePage(builtinTools: string[], profile: string, globalMcpNames: string[]) {
  const ctx = ctxFor(builtinTools);
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <ProfilePage profile={profile} globalMcpNames={globalMcpNames} />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
}

function mountAgentPage(builtinTools: string[]) {
  const ctx = ctxFor(builtinTools);
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <AgentPage profile="On this phone" agent="main" />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
}

// ─── profile page ────────────────────────────────────────────────────────

describe("Local profile editor — tools and MCP access", () => {
  it("the_local_tool_universe_offers_no_bash_or_host_tools", () => {
    mountProfilePage(LOCAL_TOOLS, "On this phone", []);
    const tools = host.querySelector('[data-testid="profile-tools"]');
    expect(tools, "the Tools card renders").not.toBeNull();

    for (const t of LOCAL_TOOLS) {
      expect(tools!.textContent, `local tool ${t} is offered`).toContain(t);
    }
    for (const hostTool of ["bash", "bash_bg", "file_read", "file_create", "list_directory", "powershell", "pdf_read"]) {
      expect(tools!.textContent, `host tool ${hostTool} must NOT be offered`).not.toContain(hostTool);
    }
    // No chip carries a host-only tool.
    for (const chip of tools!.querySelectorAll('[data-testid^="chip-"]')) {
      expect(LOCAL_TOOLS, `chip ${chip.textContent} is a local tool`).toContain(chip.textContent ?? "");
    }
  });

  it("the_local_editor_offers_no_mcps_field", () => {
    // The Local edition declares no global MCP servers (the Settings → MCP
    // pane is hidden), so the MCP access card offers no field — only the
    // empty state.
    mountProfilePage(LOCAL_TOOLS, "On this phone", []);
    const mcps = host.querySelector('[data-testid="profile-mcps"]');
    expect(mcps, "the MCP access card slot renders").not.toBeNull();
    expect(
      mcps!.querySelector('[data-testid^="chip-"]'),
      "no MCP chips when the universe is empty",
    ).toBeNull();
    expect(
      mcps!.querySelector("input,select,textarea,button"),
      "no interactive MCP field is offered",
    ).toBeNull();
    expect(mcps!.textContent).toContain("No MCP servers declared globally.");
  });

  it("the_full_edition_still_offers_bash_and_the_mcps_field", () => {
    // Guard: the test is watching the universe — with the full tool list and
    // global MCP names, both ARE offered.
    mountProfilePage(FULL_TOOLS, "On this phone", ["github", "linear"]);
    const tools = host.querySelector('[data-testid="profile-tools"]');
    expect(tools!.textContent).toContain("bash");
    const mcps = host.querySelector('[data-testid="profile-mcps"]');
    expect(
      mcps!.querySelector('[data-testid^="chip-"]'),
      "the full edition offers the MCP field",
    ).not.toBeNull();
  });

  it("a_remote_profile_on_local_carries_no_local_capabilities", () => {
    mountProfilePage(LOCAL_TOOLS, "dev on host", []);
    expect(host.querySelector('[data-testid="remote-form"]'), "the remote form renders").not.toBeNull();
    expect(host.querySelector('[data-testid="profile-tools-card"]')).toBeNull();
    expect(host.querySelector('[data-testid="profile-mcps-card"]')).toBeNull();
    expect(host.textContent).not.toContain("bash");
  });
});

// ─── agent page ──────────────────────────────────────────────────────────

describe("Local agent editor — no host tools, no mcps", () => {
  it("the_local_agent_page_offers_neither_bash_nor_mcps", () => {
    mountAgentPage(LOCAL_TOOLS);
    // The agent page edits model/persona/prompt/skills/subagents — the
    // tool and MCP capabilities live on the profile page, so the agent
    // editor must not surface them at all.
    expect(host.textContent).not.toContain("bash");
    expect(host.querySelector('[data-testid^="chip-"]')).toBeNull();
    expect(host.textContent).not.toMatch(/MCP access/i);
  });

  it("the_full_agent_page_still_has_no_tool_chips_of_its_own", () => {
    // Guard: even on full, the AGENT page is not where tools are picked —
    // the universe check above (profile page) is the real gate.
    mountAgentPage(FULL_TOOLS);
    expect(host.querySelector('[data-testid="agent-page"]')).not.toBeNull();
    expect(host.querySelector('[data-testid^="chip-"]')).toBeNull();
  });
});
