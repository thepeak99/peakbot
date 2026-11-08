// The Local edition's provider consent — the notice that lives on the one
// action that sends data to a third party: saving a provider.
//
// The old native consent sheet is gone with the native Profiles page; the
// notice moves to the SPA, shown verbatim in the Local edition on
//   1. the first-run wizard's Provider step (whose continue button reads
//      "Agree and continue"), and
//   2. the Settings → Providers & Models pane.
// No stored flag, no native sheet — a remote-only user never sees it.
//
// RED today: neither surface carries the notice.
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
//   * the notice text is the old sheet's text, VERBATIM:
//       "Your messages are sent to the AI provider you configure, using your
//         own API key."
//   * wizard Provider step, `edition() === "local"` (i.e. meta.boot.edition
//     is "local"): a `data-testid="fr-consent-notice"` element carrying that
//     text, and the step's continue button reads EXACTLY "Agree and
//     continue" (`data-testid="fr-agree-continue"`); clicking it advances
//     to the agent step.
//   * wizard Provider step, full edition: no notice, no agree button — the
//     plain "Next" button stays.
//   * Settings → Providers pane, `edition() === "local"`: a
//     `data-testid="providers-consent-notice"` element carrying the text.
//   * full edition: the pane has no notice.
//
// Harness: wizard = ./firstRun.test.tsx conventions; pane =
// ./providers.test.tsx conventions (ctx provider).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FirstRun } from "./firstRun";
import { ProvidersPane } from "./providers";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import { setBootEdition } from "../desktop";
import type { ConfigText, SettingsMeta } from "./types";

// The locked notice (the old native sheet's text, verbatim).
const CONSENT =
  "Your messages are sent to the AI provider you configure, using your own API key.";

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

beforeEach(() => {
  setBootEdition(undefined);
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new Error("no server in tests");
    }),
  );
});

afterEach(() => {
  unmount();
  setBootEdition(undefined);
  vi.unstubAllGlobals();
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
});

// ─── fixtures ───────────────────────────────────────────────────────────

const GLOBAL_PATH = "/home/u/.config/peakbot/config.yaml";

function meta(over: Partial<SettingsMeta> = {}): SettingsMeta {
  return {
    needs_setup: true,
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
    boot: { bind: "127.0.0.1:47823", token_source: "none", tls: false, edition: "local" },
    facts: {
      os: "linux",
      arch: "arm64",
      exe_path: "/apex/peakbot",
      data_dir: "/data/data/org.peakbot/files",
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

function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(`[data-testid="${id}"]`);
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}
const has = (id: string) => host.querySelector(`[data-testid="${id}"]`) !== null;
const text = (id: string) => q(id).textContent ?? "";

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

/** Welcome → Provider (Ollama, one preset model — the step is then valid). */
function toProviderStep() {
  click(q("firstrun-start-fresh"));
  const preset = host.querySelector('[data-testid="fr-model-preset-0"]');
  if (preset) click(preset);
  expect(has("firstrun-step-provider")).toBe(true);
}

// ─── wizard Provider step ────────────────────────────────────────────────

describe("wizard Provider step — Local consent", () => {
  it("local: shows the notice verbatim and the button reads Agree and continue", () => {
    (globalThis as { peakbotShell?: unknown }).peakbotShell = "mobile";
    mount(<FirstRun meta={meta()} global={MISSING_GLOBAL} onClose={() => {}} />);
    toProviderStep();

    expect(has("fr-consent-notice"), "the Local wizard must show the consent notice").toBe(true);
    expect(text("fr-consent-notice")).toBe(CONSENT);
    expect(q("fr-agree-continue").textContent).toBe("Agree and continue");
  });

  it("local: agreeing advances to the agent step", () => {
    (globalThis as { peakbotShell?: unknown }).peakbotShell = "mobile";
    mount(<FirstRun meta={meta()} global={MISSING_GLOBAL} onClose={() => {}} />);
    toProviderStep();
    click(q("fr-agree-continue"));
    expect(has("firstrun-step-agent")).toBe(true);
  });

  it("full: no notice and no agree button — the plain Next stays", () => {
    mount(
      <FirstRun
        meta={meta({ boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false, edition: "full" } })}
        global={MISSING_GLOBAL}
        onClose={() => {}}
      />,
    );
    toProviderStep();

    expect(has("fr-consent-notice")).toBe(false);
    expect(has("fr-agree-continue")).toBe(false);
    expect(host.textContent).not.toContain(CONSENT);
    expect(q("firstrun-next").textContent).toBe("Next");
  });
});

// ─── Settings → Providers pane ───────────────────────────────────────────

const DOC = `
providers:
  - name: local
    type: llamacpp
    base_url: http://127.0.0.1:8080
    models:
      - name: llama3.1:8b
        alias: llama
        max_tokens: 8192
`;

function mountPane() {
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: null,
    doc: parse(DOC),
    globalDoc: parse(DOC),
    edit: () => {},
    errors: [],
    switchToGlobal: () => {},
  };
  mount(
    <SettingsCtx.Provider value={ctx}>
      <ProvidersPane />
    </SettingsCtx.Provider>,
  );
}

describe("Settings → Providers pane — Local consent", () => {
  it("local: the pane shows the notice verbatim", () => {
    setBootEdition("local");
    mountPane();
    expect(has("providers-consent-notice"), "the Local pane must show the consent notice").toBe(true);
    expect(text("providers-consent-notice")).toBe(CONSENT);
  });

  it("full: the pane has no notice (unchanged)", () => {
    setBootEdition("full");
    mountPane();
    expect(has("providers-consent-notice")).toBe(false);
    expect(host.textContent).not.toContain(CONSENT);
  });
});
