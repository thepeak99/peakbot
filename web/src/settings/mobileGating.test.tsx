// Mobile SPA gating — written BEFORE the
// code.
//
// The one new seam: `shell()` from ../desktop — "desktop" | "mobile" |
// "browser", read from `window.peakbotShell` (the shell's init-script
// marker). When `shell() === "mobile"`:
//   - Settings gains a "Switch profile" row whose href is EXACTLY
//     `peakbot-mobile:profiles` (the sentinel the mobile nav guard
//     classifies — never a `peakbot-desktop:*` path);
//   - the desktop tab strip, the update banner, every `peakbot-desktop:*`
//     navigation, and the "Connect a phone or another PeakBot" panel are
//     ABSENT;
//   - `browser` and `desktop` keep today's behaviour.
//
// RED today: `shell()` does not exist and neither the "Switch
// profile" row nor the mobile gating exist. Both Tauri shells set
// `window.isTauri = true`, so today a mobile shell is mistaken for
// desktop: the mobile rows fail on that, while the desktop and
// browser rows are GUARDS that must stay green before and after. The
// marker globals are
// set/deleted per test; `__TAURI__` is stubbed like the other desktop
// tests; `globalThis.fetch` rejects (no server in tests — the settings
// components catch and toast).
//
// Harness: plain createRoot/act + raw DOM queries (project convention,
// no testing-library). jsdom via vitest.config.ts (src/settings/**/*.tsx).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { isDesktop } from "../desktop";
import { UpdateBanner } from "../components/UpdateBanner";
import { ConvoTabBar } from "../components/ConvoTabBar";
import { bootDesktopTabs } from "../useConvoTabs";
import { tabViews } from "../convoTabs";
import { Settings } from "./Settings";
import { GeneralPane } from "./general";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { RestartBanner } from "./SaveBar";
import { getPairing } from "./api";
import { parse } from "./yamlDoc";

vi.mock("./api", async () => {
  const actual = await vi.importActual<typeof import("./api")>("./api");
  return { ...actual, getPairing: vi.fn() };
});
const mockedGetPairing = getPairing as unknown as ReturnType<typeof vi.fn>;

type ShellGlobal = {
  peakbotShell?: unknown;
  isTauri?: unknown;
  __TAURI__?: { core?: { invoke?: unknown } };
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
  await act(async () => {});
}

/** marker: "desktop" / "mobile" set the peakbotShell global AND
 *  `isTauri = true` — both real Tauri shells set `window.isTauri` on every
 *  origin, which is exactly why the SPA must stop reading it (R18);
 *  "browser" leaves both absent. */
function setShell(marker: "desktop" | "mobile" | "browser") {
  delete g.peakbotShell;
  delete g.isTauri;
  if (marker !== "browser") {
    g.peakbotShell = marker;
    g.isTauri = true;
  }
}

beforeEach(() => {
  setShell("browser");
  mockedGetPairing.mockReset();
  mockedGetPairing.mockResolvedValue({
    Available: { links: ["https://nas.lan:7823/?token=pito"] },
  });
  // No server in tests: every settings fetch settles into its .catch.
  vi.stubGlobal("fetch", vi.fn(async () => {
    throw new Error("no server in tests");
  }));
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  delete g.__TAURI__;
  setShell("browser");
  vi.unstubAllGlobals();
});

// ─── "Switch profile" row ────────────────────────────────────────────────

describe('Settings "Switch profile" row (F2)', () => {
  it("mobile: renders the row with the exact peakbot-mobile:profiles href", async () => {
    setShell("mobile");
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();

    const row = host.querySelector('a[href="peakbot-mobile:profiles"]');
    expect(row, "the mobile shell must offer 'Switch profile'").not.toBeNull();
    expect(row?.textContent).toContain("Switch profile");
  });

  it.each([
    ["desktop", "desktop"],
    ["browser", "browser"],
  ] as const)("%s: no 'Switch profile' row (unchanged)", (_label, marker) => {
    setShell(marker);
    mount(<Settings convo={null} onClose={() => {}} />);
    expect(host.querySelector('a[href="peakbot-mobile:profiles"]')).toBeNull();
  });
});

// ─── update banner per shell ─────────────────────────────────────────────

describe("update banner per shell (F1/F2)", () => {
  function installInvoke(impl: () => Promise<unknown>) {
    const invoke = vi.fn(impl);
    g.__TAURI__ = { core: { invoke } };
    return invoke;
  }

  it("mobile: renders nothing and makes ZERO IPC calls", async () => {
    setShell("mobile");
    const invoke = installInvoke(async () => ({ state: "available", version: "9.9.9" }));
    mount(<UpdateBanner />);
    await flush();

    expect(rendered()).toBe("");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("desktop: shows the available-update banner (unchanged)", async () => {
    setShell("desktop");
    installInvoke(async () => ({ state: "available", version: "9.9.9" }));
    mount(<UpdateBanner />);
    await flush();

    expect(host.textContent).toContain("Update available — v9.9.9");
  });

  it("browser: renders nothing (unchanged)", async () => {
    const invoke = installInvoke(async () => null);
    mount(<UpdateBanner />);
    await flush();

    expect(rendered()).toBe("");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("a Tauri window WITHOUT the marker renders nothing (isTauri not consulted)", async () => {
    setShell("browser");
    g.isTauri = true; // old desktop apps set this automatically
    const invoke = installInvoke(async () => ({ state: "available", version: "9.9.9" }));
    mount(<UpdateBanner />);
    await flush();

    expect(rendered()).toBe("");
    expect(invoke).not.toHaveBeenCalled();
  });
});

// ─── desktop tab strip per shell ─────────────────────────────────────────
// main.tsx computes `isDesktop() ? bootDesktopTabs() : null` before first
// render and App renders the strip iff the value is non-null. The probe
// reproduces exactly that chain (no socket needed): the pure tab views
// over the booted state, wrapped in the real ConvoTabBar.

function TabStripProbe() {
  const initial = isDesktop() ? bootDesktopTabs() : null;
  if (initial === null) return <div data-testid="probe-no-strip" />;
  return (
    <ConvoTabBar
      tabs={tabViews(initial, {}, false)}
      onActivate={() => {}}
      onClose={() => {}}
      onNew={() => {}}
    />
  );
}

describe("desktop tab strip per shell (F1/F2)", () => {
  it("mobile: no tab strip", () => {
    setShell("mobile");
    mount(<TabStripProbe />);
    expect(host.querySelector('[data-testid="probe-no-strip"]')).not.toBeNull();
    expect(host.querySelector(".ctabbar")).toBeNull();
  });

  it("desktop: the strip renders (unchanged)", () => {
    setShell("desktop");
    mount(<TabStripProbe />);
    expect(host.querySelector('[data-testid="probe-no-strip"]')).toBeNull();
    expect(host.querySelector(".ctabbar")).not.toBeNull();
  });

  it("browser: no tab strip (unchanged)", () => {
    mount(<TabStripProbe />);
    expect(host.querySelector('[data-testid="probe-no-strip"]')).not.toBeNull();
    expect(host.querySelector(".ctabbar")).toBeNull();
  });
});

// ─── peakbot-desktop:restart path per shell ──────────────────────────────
// SettingsInner passes `desktop={isDesktop()}` to the RestartBanner; its
// "Restart now" button is the SPA's only `peakbot-desktop:restart`
// navigation. In the mobile shell the button must not render at all.

describe("peakbot-desktop:restart path per shell (F2)", () => {
  function mountBanner(): void {
    mount(
      <RestartBanner
        keys={["general.web"]}
        desktop={isDesktop()}
        onLater={vi.fn()}
        onRestartNow={vi.fn()}
      />,
    );
  }

  it("mobile: no 'Restart now' button", () => {
    setShell("mobile");
    mountBanner();
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).toBeNull();
  });

  it("desktop: 'Restart now' present (unchanged)", () => {
    setShell("desktop");
    mountBanner();
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).not.toBeNull();
  });

  it("browser: no 'Restart now' button (unchanged)", () => {
    mountBanner();
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).toBeNull();
  });
});

// ─── "Connect a phone or another PeakBot" panel per shell ────────────────
// GeneralPane renders the panel whenever the pairing facts are available;
// the mobile shell must not render it at all (the phone IS the pairing
// target — the panel would tell it to pair with itself).

describe('"Connect a phone or another PeakBot" panel per shell (F2)', () => {
  function ctxValue(): SettingsCtxValue {
    return {
      scope: "global",
      meta: null,
      doc: parse(""),
      globalDoc: parse(""),
      edit: () => {},
      errors: [],
      switchToGlobal: () => {},
    };
  }

  function mountPane() {
    mount(
      <SettingsCtx.Provider value={ctxValue()}>
        <GeneralPane />
      </SettingsCtx.Provider>,
    );
  }

  it("mobile: the panel is absent (pairing facts available)", async () => {
    setShell("mobile");
    mountPane();
    await flush();
    expect(mockedGetPairing).toHaveBeenCalledTimes(1);
    expect(host.textContent).not.toContain("Connect a phone or another PeakBot");
  });

  it.each([
    ["desktop", "desktop"],
    ["browser", "browser"],
  ] as const)("%s: the panel is present (unchanged)", async (_label, marker) => {
    setShell(marker);
    mountPane();
    await flush();
    expect(host.textContent).toContain("Connect a phone or another PeakBot");
  });
});
