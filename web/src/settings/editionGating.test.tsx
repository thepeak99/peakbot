// SPA per-edition gating — written
// BEFORE the code, RED.
//
// The edition signal: `boot.edition: "full" | "local"` on `GET
// /api/settings` (the Rust side is locked in tests/edition_settings.rs).
// Missing ⇒ `"full"` (old servers keep today's behaviour).
//
// **The locked contract (implemented exactly this way):**
//
// 1. `web/src/settings/types.ts` — `SettingsMeta.boot` gains
//    `edition?: "full" | "local"` (OPTIONAL — old servers omit it). The
//    edition lives in the `boot` block on the wire, exactly as the server
//    sends it (`src/ui/web/settings.rs`); the cross-side pin is
//    `tests/fixtures/settings_boot_local.json`.
//
// 2. `web/src/desktop.ts` (the shell/edition-detection seam, beside
//    `shell()`):
//       export type Edition = "full" | "local";
//       export function edition(): Edition;
//       export function setBootEdition(e: Edition | undefined): void;
//    `edition()` reports what THIS server can do: `"full"` until the boot
//    read, then the `edition` of the last successful `GET /api/settings`;
//    `setBootEdition(undefined)` resets to `"full"` (missing ⇒ full).
//
// 3. Wiring: `getSettings()` in `web/src/settings/api.ts` calls
//    `setBootEdition(meta.boot?.edition)` on every successful
//    `/api/settings` read (the single read point — Boot, Settings and the
//    wizard all go through it).
//
// 4. Gating when `edition() === "local"`:
//    - Settings hides the MCP, System and Raw YAML panes (nav buttons AND
//      the collapsed `<select>); the Profiles pane stays VISIBLE — the
//      phone manages its profiles (incl. remote ones) in the SPA; and
//      General hides its "Web server" section;
//    - the first-run wizard has no `web` step and no PATH fact (driven by
//      the `meta` prop, which Boot passes from the same boot read);
//    - the TopBar/BottomBar CwdPicker (cwd line) is hidden and the Bash
//      panel renders nothing — EXCEPT the picker when the active
//      selection is a catalog entry with `origin: "remote"`: the Local
//      server refuses `list_dir` for itself, but the relay forwards
//      `list_dir` / `request_recent_dirs` / `switch_cwd` to the full
//      remote server, so a remote profile keeps its working-directory
//      picker (see the CwdPicker describe block);
//    - the composer attach input gets `accept="image/*"`;
//    - with `shell() === "mobile"` AND local, the "Restart now" control is
//      an anchor with href EXACTLY `peakbot-mobile:restart` (the only
//      SPA→shell restart message; the desktop `peakbot-desktop:restart`
//      button stays desktop-only).
//
// 5. `full` (and missing edition): everything unchanged — the `full`
//    tests below are guards that must stay green before and after.
//
// GREEN/RED split: the `edition` / `setBootEdition` seam and every
// local-edition gate above exist (GREEN); the CwdPicker remote-profile
// cases (`local + origin "remote"` ⇒ shown in BOTH bars) are RED — the
// bars gate on `edition() !== "local"` alone, hiding the picker for all
// profiles. Fixtures use the real server's wire shape.
//
// Harness: same style as ./mobileGating.test.tsx — plain createRoot/act +
// raw DOM queries (no testing-library), marker globals set/deleted per
// test, stubbed `globalThis.fetch`. jsdom via vitest.config.ts
// (src/settings/**/*.tsx).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { isDesktop, edition, setBootEdition } from "../desktop";
import { getSettings } from "./api";
import { parse } from "./yamlDoc";
import type { ConfigText, SettingsMeta } from "./types";
import { Settings } from "./Settings";
import { GeneralPane } from "./general";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { RestartBanner } from "./SaveBar";
import { FirstRun } from "./firstRun";
import { Composer } from "../components/Composer";
import { TopBar } from "../components/TopBar";
import { BottomBar } from "../components/BottomBar";
import { BashPanel } from "../components/BashPanel";
import type { BashPanel as BashPanelView, CatalogProfile, Selection } from "../types";

type ShellGlobal = {
  peakbotShell?: unknown;
  isTauri?: unknown;
};
const g = globalThis as ShellGlobal;

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

/** What the mounted node rendered, ignoring mount()'s own wrapper div. */
function rendered(): string {
  return host.firstElementChild?.innerHTML ?? "";
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

/** "desktop" / "mobile" set the peakbotShell global AND isTauri (both real
 *  Tauri shells set isTauri); "browser" leaves both absent. */
function setShell(marker: "desktop" | "mobile" | "browser") {
  delete g.peakbotShell;
  delete g.isTauri;
  if (marker !== "browser") {
    g.peakbotShell = marker;
    g.isTauri = true;
  }
}

// ─── fixtures ─────────────────────────────────────────────────────────────

const GLOBAL_PATH = "/home/u/.config/peakbot/config.yaml";

// The REAL wire shape (src/ui/web/settings.rs `get_settings`): `edition`
// lives in the `boot` block, never top-level. The cross-side pin is
// editionFixture.test.ts (reads tests/fixtures/settings_boot_local.json,
// which tests/edition_settings.rs also asserts against), so the two sides
// can't drift again.
const BASE_BOOT: SettingsMeta["boot"] = { bind: "127.0.0.1:7823", token_source: "none", tls: false };
const LOCAL_BOOT: SettingsMeta["boot"] = { ...BASE_BOOT, edition: "local" };
const FULL_BOOT: SettingsMeta["boot"] = { ...BASE_BOOT, edition: "full" };

function meta(over: Partial<SettingsMeta> = {}): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: GLOBAL_PATH,
    repo_path: "/work/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "You are Code Crusader.",
    mcp_status: [],
    boot: BASE_BOOT,
    facts: {
      os: "linux",
      arch: "amd64",
      exe_path: "/opt/peakbot/bin/peakbot",
      data_dir: "/home/u/.local/share/peakbot",
      version: "0.27.0",
      install: null,
    },
    ...over,
  };
}

const MISSING_GLOBAL: ConfigText = {
  scope: "global",
  path: GLOBAL_PATH,
  exists: false,
  text: "",
  sha256: null,
};

/** Stub /api/settings (+ /api/config) for the Settings-shell tests. */
function stubSettingsFetch(bootMeta: SettingsMeta) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      const reply = (status: number, json: unknown) => ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => json,
        text: async () => JSON.stringify(json),
      });
      if (url.startsWith("/api/settings")) return reply(200, bootMeta);
      if (url.startsWith("/api/config"))
        return reply(200, {
          scope: "global",
          path: GLOBAL_PATH,
          exists: false,
          text: "",
          sha256: null,
        });
      return reply(404, { error: "unrouted " + url });
    }),
  );
}

// ─── edition() seam ───────────────────────────────────────────────────────

describe("edition() seam", () => {
  it("defaults to full before any boot read", () => {
    expect(edition()).toBe("full");
  });

  it("setBootEdition reports local, full, and undefined resets to full", () => {
    setBootEdition("local");
    expect(edition()).toBe("local");
    setBootEdition("full");
    expect(edition()).toBe("full");
    setBootEdition(undefined);
    expect(edition()).toBe("full");
  });

  it("a boot read with edition local updates edition()", async () => {
    stubSettingsFetch(meta({ boot: LOCAL_BOOT }));
    await getSettings();
    expect(edition()).toBe("local");
  });

  it("a boot read WITHOUT edition stays full (old servers)", async () => {
    setBootEdition("local"); // a stale local must not survive a full read
    stubSettingsFetch(meta());
    await getSettings();
    expect(edition()).toBe("full");
  });
});

// ─── Settings panes per edition ───────────────────────────────────────────

const HIDDEN_PANES = ["mcps", "system", "raw"] as const;
const HIDDEN_LABELS = ["MCP servers", "System", "Raw YAML"];

describe("Settings panes per edition", () => {
  it("local: hides the MCP, System and Raw YAML panes and KEEPS Profiles", async () => {
    stubSettingsFetch(meta({ boot: LOCAL_BOOT }));
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    for (const id of HIDDEN_PANES) {
      expect(host.querySelector(`[data-testid="nav-${id}"]`), `nav ${id} must be hidden`).toBeNull();
    }
    expect(host.querySelector('[data-testid="nav-providers"]'), "Providers stays").not.toBeNull();
    expect(host.querySelector('[data-testid="nav-general"]'), "General stays").not.toBeNull();
    expect(
      host.querySelector('[data-testid="nav-profiles"]'),
      "Profiles stays visible in the Local edition",
    ).not.toBeNull();

    // The collapsed mobile section select must not offer the hidden panes
    // either — and it MUST offer profiles.
    const select = host.querySelector('select[aria-label="Section"]');
    expect(select, "the section select must render").not.toBeNull();
    const values = [...(select as HTMLSelectElement).options].map((o) => o.value);
    for (const id of HIDDEN_PANES) {
      expect(values, `option ${id} must be gone`).not.toContain(id);
    }
    expect(values).toContain("providers");
    expect(values).toContain("general");
    expect(values, "the profiles option stays").toContain("profiles");

    // The active section head is a visible pane (Profiles is the default
    // active section and is no longer hidden on Local).
    const head = host.querySelector("main h2")?.textContent ?? "";
    expect(HIDDEN_LABELS, `the active head must not be a hidden pane, got: ${head}`).not.toContain(head);
    expect(head, "Profiles is the default section on Local").toBe("Profiles");
  });

  it.each([
    ["full", FULL_BOOT],
    ["missing edition (old server)", undefined],
  ] as const)("%s: shows all six panes (unchanged)", async (_label, boot) => {
    stubSettingsFetch(boot === undefined ? meta() : meta({ boot }));
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    for (const id of ["providers", "mcps", "profiles", "general", "system", "raw"]) {
      expect(host.querySelector(`[data-testid="nav-${id}"]`), `nav ${id} must stay`).not.toBeNull();
    }
  });
});

// ─── General → Web section per edition ────────────────────────────────────

describe("General → Web section per edition", () => {
  function ctxValue(m: SettingsMeta): SettingsCtxValue {
    return {
      scope: "global",
      meta: m,
      doc: parse(""),
      globalDoc: parse(""),
      edit: () => {},
      errors: [],
      switchToGlobal: () => {},
    };
  }

  function mountPane(m: SettingsMeta) {
    mount(
      <SettingsCtx.Provider value={ctxValue(m)}>
        <GeneralPane />
      </SettingsCtx.Provider>,
    );
  }

  it("local: hides the Web server section", async () => {
    setBootEdition("local");
    mountPane(meta({ boot: LOCAL_BOOT }));
    await flush();

    expect(host.querySelector('[data-testid="general-access-local"]')).toBeNull();
    expect(host.querySelector('[data-testid="general-access-lan"]')).toBeNull();
    expect(host.textContent).not.toContain("Web server");
  });

  it("full: keeps the Web server section (unchanged)", async () => {
    mountPane(meta({ boot: FULL_BOOT }));
    await flush();

    expect(host.querySelector('[data-testid="general-access-local"]')).not.toBeNull();
    expect(host.textContent).toContain("Web server");
  });
});

// ─── first-run wizard per edition ─────────────────────────────────────────

describe("first-run wizard per edition", () => {
  function mountWizard(m: SettingsMeta) {
    mount(<FirstRun meta={m} global={MISSING_GLOBAL} onClose={() => {}} />);
  }

  it("local: has no web step", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, boot: LOCAL_BOOT }));

    expect(host.querySelector('[data-testid="firstrun-stepper-web"]')).toBeNull();
    for (const id of ["welcome", "provider", "agent", "review"]) {
      expect(host.querySelector(`[data-testid="firstrun-stepper-${id}"]`), `step ${id} stays`).not.toBeNull();
    }
  });

  it("local: has no PATH fact", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, boot: LOCAL_BOOT }));

    expect(host.querySelector('[data-testid="firstrun-fact-path"]')).toBeNull();
  });

  it("full: keeps the web step and the PATH fact (unchanged)", () => {
    mountWizard(meta({ needs_setup: true, boot: FULL_BOOT }));

    expect(host.querySelector('[data-testid="firstrun-stepper-web"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="firstrun-fact-path"]')).not.toBeNull();
  });

  // ── persona picker (fix 4): Local has its own built-in persona ──────────

  const q = (id: string) => host.querySelector(`[data-testid="${id}"]`) as HTMLElement | null;
  const press = (el: Element | null) => {
    expect(el, "element to click must exist").not.toBeNull();
    act(() => {
      el!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });
  };

  /** Welcome → Provider (Ollama needs no key; one preset model) → Agent. */
  function walkToAgentStep() {
    press(q("firstrun-start-fresh"));
    // Provider type select → ollama (no API key required).
    const sel = q("fr-provider-type")!.querySelector("select") as HTMLSelectElement;
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(sel, "ollama");
    act(() => {
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    press(q("fr-model-preset-0"));
    press(q("firstrun-next"));
    expect(q("firstrun-step-agent"), "must land on the agent step").not.toBeNull();
  }

  it("local: the agent step has no persona picker", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, boot: LOCAL_BOOT }));
    walkToAgentStep();

    for (const id of ["fr-persona-builtin", "fr-persona-neutral", "fr-persona-terse"]) {
      expect(q(id), `${id} must be hidden on Local`).toBeNull();
    }
    expect(host.textContent).not.toContain("Code Crusader");
    expect(q("fr-agent-model"), "the model choice stays").not.toBeNull();
  });

  it("local: the written config carries no persona key (the Local built-in persona applies)", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, boot: LOCAL_BOOT }));
    walkToAgentStep();
    press(q("firstrun-next")); // agent → review (no web step on Local)
    expect(q("firstrun-step-review")).not.toBeNull();
    expect(q("firstrun-yaml")!.textContent).not.toContain("persona");
  });

  it("local: the written config carries no general.web block (the Local core never reads it)", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, boot: LOCAL_BOOT }));
    walkToAgentStep();
    press(q("firstrun-next"));
    const yaml = q("firstrun-yaml")!.textContent!;
    expect(yaml).not.toContain("general");
    expect(yaml).not.toContain("7823");
  });

  it("full: the written config keeps the general.web bind block", () => {
    mountWizard(meta({ needs_setup: true, boot: FULL_BOOT }));
    walkToAgentStep();
    press(q("firstrun-next")); // agent → web
    press(q("firstrun-next")); // web → review
    expect(q("firstrun-yaml")!.textContent).toContain("bind: 127.0.0.1:7823");
  });

  it("full: the agent step keeps the persona picker (unchanged)", () => {
    mountWizard(meta({ needs_setup: true, boot: FULL_BOOT }));
    walkToAgentStep();

    for (const id of ["fr-persona-builtin", "fr-persona-neutral", "fr-persona-terse"]) {
      expect(q(id), `${id} must stay on full`).not.toBeNull();
    }
  });

  // ── "Config written" screen (fix 2): the user must not be stuck ─────────

  // needs_setup + the global file exists ⇒ the derived restart_pending
  // screen, shown straight away (also what a reload after Finish shows).
  const WRITTEN_GLOBAL: ConfigText = { ...MISSING_GLOBAL, exists: true, text: "x: 1", sha256: "abc" };

  function mountWritten(m: SettingsMeta) {
    mount(<FirstRun meta={m} global={WRITTEN_GLOBAL} onClose={() => {}} />);
  }

  it("mobile + local: the Config-written screen has Restart now → peakbot-mobile:restart", () => {
    setShell("mobile");
    setBootEdition("local");
    mountWritten(meta({ needs_setup: true, boot: LOCAL_BOOT }));

    expect(q("firstrun-restart"), "the Config-written screen").not.toBeNull();
    const a = host.querySelector('a[data-testid="firstrun-restart-now"]');
    expect(a, "the Local Config-written screen must offer Restart now").not.toBeNull();
    expect(a?.getAttribute("href")).toBe("peakbot-mobile:restart");
    expect(a?.textContent).toContain("Restart now");
  });

  it.each([
    ["mobile + full", "mobile", FULL_BOOT],
    ["browser + local", "browser", LOCAL_BOOT],
    ["desktop + local", "desktop", LOCAL_BOOT],
    ["browser + full", "browser", FULL_BOOT],
  ] as const)("%s: no Restart now link on the Config-written screen (unchanged)", (_l, shellMarker, boot) => {
    setShell(shellMarker);
    setBootEdition(boot.edition);
    mountWritten(meta({ needs_setup: true, boot }));

    expect(q("firstrun-restart")).not.toBeNull();
    expect(q("firstrun-restart-now")).toBeNull();
  });
});

// ─── CwdPicker / cwd line per edition ─────────────────────────────────────
// CwdPicker renders a button titled with the cwd — the DOM marker.

const CWD = "/home/u/proj";

// The REAL wire shape for a remote catalog entry (resolve.rs
// `Catalog::infos()`, pinned by selectionChip.remote.test.tsx): origin
// "remote", NO agents, resolved colour — plus a local profile so the
// local-origin cases test a populated catalogue, not an empty one.
const REMOTE_CATALOG: CatalogProfile[] = [
  {
    name: "dev",
    origin: "global",
    overridesGlobal: false,
    agents: [{ name: "team", model: "sonnet", subagents: [] }],
    color: "teal",
  },
  {
    name: "bonsol-dev",
    origin: "remote",
    overridesGlobal: false,
    agents: [],
    color: "teal",
  },
];

const REMOTE_SEL: Selection = { profile: "bonsol-dev", agent: "", subagentModels: {} };
const GHOST_SEL: Selection = { profile: "ghost", agent: "", subagentModels: {} };

/** A one-entry catalogue of a LOCAL profile with the given origin. */
function localCatalog(origin: "global" | "repo"): CatalogProfile[] {
  return [
    {
      name: origin === "repo" ? "repo-dev" : "dev",
      origin,
      overridesGlobal: false,
      agents: [{ name: "team", model: "sonnet", subagents: [] }],
      color: "teal",
    },
  ];
}

function topBarProps(
  cwd: string | null,
  over: { catalog?: CatalogProfile[]; selection?: Selection | null } = {},
): Parameters<typeof TopBar>[0] {
  return {
    isRunning: false,
    connected: true,
    statusMessage: null,
    subAgent: null,
    models: [],
    activeAlias: "gpt",
    hasTranscript: false,
    cwd,
    dirListing: null,
    recentDirs: [],
    conversations: [],
    conversationsError: null,
    send: () => {},
    onLoadConversation: () => {},
    notifyEnabled: false,
    notifyPermission: "unsupported",
    onToggleNotify: () => {},
    focusMode: true,
    onToggleFocus: () => {},
    catalog: over.catalog ?? [],
    selection: over.selection ?? null,
    onToast: () => {},
  } as unknown as Parameters<typeof TopBar>[0];
}

function bottomBarProps(
  cwd: string | null,
  over: { catalog?: CatalogProfile[]; selection?: Selection | null } = {},
): Parameters<typeof BottomBar>[0] {
  return {
    conversations: [],
    conversationsError: null,
    models: [],
    activeAlias: "gpt",
    hasTranscript: false,
    cwd,
    dirListing: null,
    recentDirs: [],
    send: () => {},
    onLoadConversation: () => {},
    catalog: over.catalog ?? [],
    selection: over.selection ?? null,
    onToast: () => {},
  } as unknown as Parameters<typeof BottomBar>[0];
}

describe("CwdPicker / cwd line per edition", () => {
  it("local: the top bar hides the cwd picker", () => {
    setBootEdition("local");
    mount(<TopBar {...topBarProps(CWD)} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });

  it("local: the bottom bar hides the cwd picker", () => {
    setBootEdition("local");
    mount(<BottomBar {...bottomBarProps(CWD)} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });

  it("full: the top bar keeps the cwd picker (unchanged)", () => {
    mount(<TopBar {...topBarProps(CWD)} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).not.toBeNull();
  });

  it("full: the bottom bar keeps the cwd picker (unchanged)", () => {
    mount(<BottomBar {...bottomBarProps(CWD)} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).not.toBeNull();
  });

  // ── the bug: local + a REMOTE profile keeps the picker ─────────────────
  // The Local server refuses list_dir for ITSELF, but the relay forwards
  // list_dir / request_recent_dirs / switch_cwd to the full remote server
  // — so a remote profile (catalog entry, origin "remote") must keep its
  // working-directory picker in BOTH bars.

  it("local + remote profile: the top bar shows the cwd picker (relay forwards list_dir)", () => {
    setBootEdition("local");
    mount(<TopBar {...topBarProps(CWD, { catalog: REMOTE_CATALOG, selection: REMOTE_SEL })} />);
    expect(
      host.querySelector(`button[title="${CWD}"]`),
      "the relay owns the remote's cwd — the picker must be reachable",
    ).not.toBeNull();
  });

  it("local + remote profile: the bottom bar shows the cwd picker (relay forwards list_dir)", () => {
    setBootEdition("local");
    mount(<BottomBar {...bottomBarProps(CWD, { catalog: REMOTE_CATALOG, selection: REMOTE_SEL })} />);
    expect(
      host.querySelector(`button[title="${CWD}"]`),
      "the relay owns the remote's cwd — the picker must be reachable",
    ).not.toBeNull();
  });

  // ── guards: local + NOT-remote stays hidden in both bars ───────────────

  it.each(["global", "repo"] as const)(
    "local + local profile (origin %s): the top bar keeps the cwd picker hidden",
    (origin) => {
      const catalog = localCatalog(origin);
      setBootEdition("local");
      mount(
        <TopBar
          {...topBarProps(CWD, {
            catalog,
            selection: { profile: catalog[0].name, agent: "team", subagentModels: {} },
          })}
        />,
      );
      expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
    },
  );

  it.each(["global", "repo"] as const)(
    "local + local profile (origin %s): the bottom bar keeps the cwd picker hidden",
    (origin) => {
      const catalog = localCatalog(origin);
      setBootEdition("local");
      mount(
        <BottomBar
          {...bottomBarProps(CWD, {
            catalog,
            selection: { profile: catalog[0].name, agent: "team", subagentModels: {} },
          })}
        />,
      );
      expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
    },
  );

  it("local + null selection: the top bar keeps the cwd picker hidden", () => {
    setBootEdition("local");
    mount(<TopBar {...topBarProps(CWD, { catalog: REMOTE_CATALOG, selection: null })} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });

  it("local + null selection: the bottom bar keeps the cwd picker hidden", () => {
    setBootEdition("local");
    mount(<BottomBar {...bottomBarProps(CWD, { catalog: REMOTE_CATALOG, selection: null })} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });

  it("local + profile not in catalog: the top bar keeps the cwd picker hidden", () => {
    setBootEdition("local");
    mount(<TopBar {...topBarProps(CWD, { catalog: REMOTE_CATALOG, selection: GHOST_SEL })} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });

  it("local + profile not in catalog: the bottom bar keeps the cwd picker hidden", () => {
    setBootEdition("local");
    mount(<BottomBar {...bottomBarProps(CWD, { catalog: REMOTE_CATALOG, selection: GHOST_SEL })} />);
    expect(host.querySelector(`button[title="${CWD}"]`)).toBeNull();
  });
});

// ─── bash panel per edition ───────────────────────────────────────────────

const BASH_PANEL: BashPanelView = {
  command: "ls -la",
  status: "running",
  pid: 4242,
  elapsed: "3s",
  tail: ["total 8", "drwxr-xr-x 2 u u 4096 ."],
};

describe("bash panel per edition", () => {
  it("local: renders nothing", () => {
    setBootEdition("local");
    mount(<BashPanel panel={BASH_PANEL} />);
    expect(rendered()).toBe("");
  });

  it("full: renders its output (unchanged)", () => {
    mount(<BashPanel panel={BASH_PANEL} />);
    expect(host.textContent).toContain("ls -la");
  });
});

// ─── composer attach input per edition ────────────────────────────────────

describe("composer attach input per edition", () => {
  const composerProps = {
    isRunning: false,
    connected: true,
    commands: [],
    onSend: () => {},
    onStop: () => {},
    subAgent: null,
    onPause: () => {},
    onResume: () => {},
    watchingRole: null,
    onClearWatch: () => {},
    pendingInput: 0,
  };

  it("local: the attach input accepts images only", () => {
    setBootEdition("local");
    mount(<Composer {...composerProps} />);
    const input = host.querySelector('input[type="file"]') as HTMLInputElement | null;
    expect(input, "the attach input must render").not.toBeNull();
    expect(input?.getAttribute("accept")).toBe("image/*");
  });

  it("full: the attach input stays unrestricted (unchanged)", () => {
    mount(<Composer {...composerProps} />);
    const input = host.querySelector('input[type="file"]') as HTMLInputElement | null;
    expect(input, "the attach input must render").not.toBeNull();
    expect(input?.getAttribute("accept")).toBeNull();
  });
});

// ─── "Restart now" per shell × edition ────────────────────────────────────
// The banner's control today: desktop → button (navigates
// peakbot-desktop:restart), everything else → plain text. The contract:
// mobile + local → an anchor with href EXACTLY `peakbot-mobile:restart`.

describe('"Restart now" per shell × edition', () => {
  function mountBanner() {
    mount(
      <RestartBanner
        keys={["general.web"]}
        desktop={isDesktop()}
        onLater={vi.fn()}
        onRestartNow={vi.fn()}
      />,
    );
  }

  it("mobile + local: is an anchor with the exact peakbot-mobile:restart href", () => {
    setShell("mobile");
    setBootEdition("local");
    mountBanner();

    const a = host.querySelector('a[data-testid="restart-banner-restart"]');
    expect(a, "the mobile-local 'Restart now' must be an anchor").not.toBeNull();
    expect(a?.getAttribute("href")).toBe("peakbot-mobile:restart");
    expect(a?.textContent).toContain("Restart now");
  });

  it("mobile + full: no restart control (unchanged)", () => {
    setShell("mobile");
    mountBanner();
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).toBeNull();
  });

  it.each([
    ["desktop + local", "desktop", "local"],
    ["desktop + full", "desktop", "full"],
  ] as const)("%s: keeps the desktop restart button (unchanged)", (_label, shellMarker, ed) => {
    setShell(shellMarker);
    setBootEdition(ed);
    mountBanner();
    const btn = host.querySelector('button[data-testid="restart-banner-restart"]');
    expect(btn, "the desktop 'Restart now' button must stay").not.toBeNull();
  });

  it("browser + local: no restart control (unchanged)", () => {
    setShell("browser");
    setBootEdition("local");
    mountBanner();
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).toBeNull();
  });
});

// ─── lifecycle ────────────────────────────────────────────────────────────

beforeEach(() => {
  setShell("browser");
  setBootEdition(undefined); // every test starts at the missing ⇒ full default
  // jsdom has no matchMedia; Composer's useMediaQuery needs it.
  vi.stubGlobal(
    "matchMedia",
    (media: string) =>
      ({
        get matches() {
          return false;
        },
        media,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList,
  );
  // No server by default: fetches settle into their .catch.
  vi.stubGlobal("fetch", vi.fn(async () => {
    throw new Error("no server in tests");
  }));
});

afterEach(() => {
  if (root) act(() => root.unmount());
  host?.remove();
  setShell("browser");
  setBootEdition(undefined);
  vi.unstubAllGlobals();
});
