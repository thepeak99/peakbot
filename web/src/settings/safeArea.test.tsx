// The first-run wizard and the Settings overlay are full-screen on the
// phone, and on iOS the Dynamic Island / clock sit in the top safe area
// (`viewport-fit=cover`). TopBar already clears it; these two headers must
// too, or "Welcome to PeakBot" / "← Back to chat" render under the clock.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FirstRun } from "./firstRun";
import { Settings } from "./Settings";
import type { ConfigText, SettingsMeta } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

const PATH = "/home/u/.config/peakbot/config.yaml";

const META: SettingsMeta = {
  needs_setup: true,
  legacy_layout: false,
  migrate_error: null,
  settings_available: true,
  global_path: PATH,
  repo_path: "/work/repo/.peakbot/config.yaml",
  repo_exists: false,
  builtin_tools: [],
  tab_names: ["session", "settings"],
  skills: [],
  builtin_persona: "You are Code Crusader.",
  mcp_status: [],
  boot: { bind: "127.0.0.1:47823", token_source: "none", tls: false, edition: "local" },
  facts: {
    os: "ios",
    arch: "arm64",
    exe_path: "/app/PeakBot",
    data_dir: "/data",
    version: "0.27.0",
    install: null,
  },
};

const MISSING: ConfigText = { scope: "global", path: PATH, exists: false, text: "", sha256: null };

let root: Root | undefined;
let host: HTMLElement;

afterEach(() => {
  act(() => root?.unmount());
  root = undefined;
  host?.remove();
  vi.unstubAllGlobals();
});

function mount(node: React.ReactElement) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

function stubApi() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      const json = url.startsWith("/api/settings") ? META : MISSING;
      return { ok: true, status: 200, json: async () => json, text: async () => JSON.stringify(json) };
    }),
  );
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

const clearsTopInset = (header: Element | null) =>
  (header?.getAttribute("class") ?? "").includes("env(safe-area-inset-top)");

describe("full-screen phone headers clear the top safe area", () => {
  it("the first-run wizard header", () => {
    stubApi();
    mount(<FirstRun meta={META} global={MISSING} onClose={() => {}} />);
    expect(clearsTopInset(host.querySelector('[data-testid="firstrun"] > header'))).toBe(true);
  });

  it("the Settings overlay header", async () => {
    stubApi();
    mount(<Settings convo={null} onClose={() => {}} />);
    await flush();
    expect(clearsTopInset(host.querySelector(".fixed.inset-0 > header"))).toBe(true);
  });
});
