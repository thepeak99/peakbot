// SPA per-edition gating (mobile plan §2.5 / §4.3 test 5, L7) — written
// BEFORE the code, RED.
//
// The edition signal: `boot.edition: "full" | "local"` on `GET
// /api/settings` (the Rust side is locked in tests/edition_settings.rs).
// Missing ⇒ `"full"` (old servers keep today's behaviour).
//
// **The locked contract (builder L7 implements exactly this):**
//
// 1. `web/src/settings/types.ts` — `SettingsMeta` gains
//    `edition?: "full" | "local"` (OPTIONAL — old servers omit it).
//    NOTE: until that field lands, `npm run typecheck` is RED on the
//    `meta({ edition: … })` fixtures below (excess-property errors). That
//    is the expected TDD state; vitest (esbuild, no typecheck) runs the
//    file regardless.
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
//    `setBootEdition(meta.edition)` on every successful `/api/settings`
//    read (the single read point — Boot, Settings and the wizard all go
//    through it).
//
// 4. Gating when `edition() === "local"`:
//    - Settings hides the MCP, Profiles, System and Raw YAML panes (nav
//      buttons AND the collapsed `<select>`), and General hides its
//      "Web server" section;
//    - the first-run wizard has no `web` step and no PATH fact (driven by
//      the `meta` prop, which Boot passes from the same boot read);
//    - the TopBar/BottomBar CwdPicker (cwd line) is hidden and the Bash
//      panel renders nothing;
//    - the composer attach input gets `accept="image/*"`;
//    - with `shell() === "mobile"` AND local, the "Restart now" control is
//      an anchor with href EXACTLY `peakbot-mobile:restart` (the only
//      SPA→shell restart message; the desktop `peakbot-desktop:restart`
//      button stays desktop-only).
//
// 5. `full` (and missing edition): everything unchanged — the `full`
//    tests below are guards that must stay green before and after.
//
// RED today: `edition` / `setBootEdition` do not exist in ../desktop
// (whole-file import failure → every test below fails), none of the
// local-edition gating exists, and the typecheck is RED on the `edition`
// field.
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
import type { BashPanel as BashPanelView } from "../types";

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

// `meta({ edition: "local" })` does not typecheck until L4/L7 add
// `edition?: "full" | "local"` to SettingsMeta — the expected typecheck
// RED (see header). vitest runs the file regardless.
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
    boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
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

describe("edition() seam (L7)", () => {
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
    stubSettingsFetch(meta({ edition: "local" }));
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

const HIDDEN_PANES = ["mcps", "profiles", "system", "raw"] as const;
const HIDDEN_LABELS = ["MCP servers", "Profiles", "System", "Raw YAML"];

describe("Settings panes per edition", () => {
  it("local: hides the MCP, Profiles, System and Raw YAML panes", async () => {
    stubSettingsFetch(meta({ edition: "local" }));
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    for (const id of HIDDEN_PANES) {
      expect(host.querySelector(`[data-testid="nav-${id}"]`), `nav ${id} must be hidden`).toBeNull();
    }
    expect(host.querySelector('[data-testid="nav-providers"]'), "Providers stays").not.toBeNull();
    expect(host.querySelector('[data-testid="nav-general"]'), "General stays").not.toBeNull();

    // The collapsed mobile section select must not offer them either.
    const select = host.querySelector('select[aria-label="Section"]');
    expect(select, "the section select must render").not.toBeNull();
    const values = [...(select as HTMLSelectElement).options].map((o) => o.value);
    for (const id of HIDDEN_PANES) {
      expect(values, `option ${id} must be gone`).not.toContain(id);
    }
    expect(values).toContain("providers");
    expect(values).toContain("general");

    // The active section head must not be a hidden pane.
    const head = host.querySelector("main h2")?.textContent ?? "";
    expect(HIDDEN_LABELS, `the active head must not be a hidden pane, got: ${head}`).not.toContain(head);
  });

  it.each([
    ["full", "full"],
    ["missing edition (old server)", undefined],
  ] as const)("%s: shows all six panes (unchanged)", async (_label, ed) => {
    stubSettingsFetch(ed === undefined ? meta() : meta({ edition: ed }));
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
    mountPane(meta({ edition: "local" }));
    await flush();

    expect(host.querySelector('[data-testid="general-access-local"]')).toBeNull();
    expect(host.querySelector('[data-testid="general-access-lan"]')).toBeNull();
    expect(host.textContent).not.toContain("Web server");
  });

  it("full: keeps the Web server section (unchanged)", async () => {
    mountPane(meta({ edition: "full" }));
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
    mountWizard(meta({ needs_setup: true, edition: "local" }));

    expect(host.querySelector('[data-testid="firstrun-stepper-web"]')).toBeNull();
    for (const id of ["welcome", "provider", "agent", "review"]) {
      expect(host.querySelector(`[data-testid="firstrun-stepper-${id}"]`), `step ${id} stays`).not.toBeNull();
    }
  });

  it("local: has no PATH fact", () => {
    setBootEdition("local");
    mountWizard(meta({ needs_setup: true, edition: "local" }));

    expect(host.querySelector('[data-testid="firstrun-fact-path"]')).toBeNull();
  });

  it("full: keeps the web step and the PATH fact (unchanged)", () => {
    mountWizard(meta({ needs_setup: true, edition: "full" }));

    expect(host.querySelector('[data-testid="firstrun-stepper-web"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="firstrun-fact-path"]')).not.toBeNull();
  });
});

// ─── CwdPicker / cwd line per edition ─────────────────────────────────────
// CwdPicker renders a button titled with the cwd — the DOM marker.

const CWD = "/home/u/proj";

function topBarProps(cwd: string | null): Parameters<typeof TopBar>[0] {
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
    catalog: [],
    selection: null,
    onToast: () => {},
  } as unknown as Parameters<typeof TopBar>[0];
}

function bottomBarProps(cwd: string | null): Parameters<typeof BottomBar>[0] {
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
    catalog: [],
    selection: null,
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
