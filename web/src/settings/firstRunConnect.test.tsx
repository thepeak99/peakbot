// First-run: the Welcome choice and the "Connect to my PeakBot server"
// path, plus the Local edition's first-profile default name.
//
// RED today: the Welcome step offers a single "Start fresh" button. The
// contract below adds the second path (a relay-only first run needs no
// provider or API key on the phone).
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
// WELCOME (all editions, all shells) offers exactly two choices:
//   * `firstrun-start-fresh` — label EXACTLY "Set up an AI provider on this
//     device" — the existing Provider → Agent → (Web) → Review path;
//   * `firstrun-connect-server` — label EXACTLY "Connect to my PeakBot
//     server" — the new connect path (RED until it exists).
//
// CONNECT PATH (Local AND full editions):
//   * the step pane `firstrun-step-connect` holds a paste box
//     `firstrun-connect-paste` with input `firstrun-connect-input` and a
//     continue button `firstrun-connect-continue`; in the MOBILE shell the
//     box gains a Scan QR button `firstrun-scan-qr` (fills the same input
//     via `window.__TAURI_INTERNALS__.invoke` — same seam as the Settings
//     paste box);
//   * an invalid link shows its error in `firstrun-connect-error` and
//     stays on the step;
//   * Review shows the entry name `<profile> on <host>` in
//     `firstrun-connect-name`;
//   * Finish PUTs the global config: EXACTLY ONE profile, named
//     `<profile> on <host>`, whose def is a `remote:` block (url/token/ca
//     [when the link carries one]/profile) — and NO `providers` key at all
//     (a relay-only config is valid; nothing local is invented);
//   * a 422 from the server shows its error message in `firstrun-error`
//     (the existing slot) and stays on Review.
//
// LOCAL PROVIDER PATH: in the Local edition the wizard's generated first
// profile is named "On this phone" (the merged list then reads "On this
// phone" vs. "dev on nas.lan"), in the written YAML, the Review preview
// and the agent step's generated banner. The full edition keeps "default".
//
// Harness: same as ./firstRun.test.tsx (createRoot/act + stubbed fetch).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { parse as parseYaml } from "yaml";
import { FirstRun, buildFirstRunYaml } from "./firstRun";
import type { ConfigText, PutOk, SettingsMeta } from "./types";
import type { FirstRunAnswers } from "./contract";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
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
    boot: { bind: "127.0.0.1:7823", token_source: "none", tls: false },
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

const LOCAL_BOOT: SettingsMeta["boot"] = {
  bind: "127.0.0.1:47823",
  token_source: "none",
  tls: false,
  edition: "local",
};

const MISSING_GLOBAL: ConfigText = {
  scope: "global",
  path: GLOBAL_PATH,
  exists: false,
  text: "",
  sha256: null,
};

const PUT_OK: PutOk = {
  path: GLOBAL_PATH,
  backup: null,
  sha256: "newsha",
  restart_required: [],
  applies_on_next_new: true,
};

const CA = `sha256:${"0123456789abcdef".repeat(4)}`;

/// A pairing link on a non-default port (the fixed /api/pairing shape).
const LINK = `https://10.2.0.48:7899/?token=pito&ca=${CA}&profile=dev`;
const EXPECTED_NAME = "dev on 10.2.0.48"; // host label port-less (paste tests)

const KEY = "sk-or-test-key-123456";

// ─── mount / DOM plumbing ───────────────────────────────────────────────

let root: Root | undefined;
let host: HTMLElement;

afterEach(() => {
  if (root) {
    act(() => root!.unmount());
    host.remove();
    root = undefined;
  }
  vi.unstubAllGlobals();
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  delete (globalThis as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  window.history.replaceState(null, "", "/");
});

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

const tid = (id: string) => `[data-testid="${id}"]`;
function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(tid(id));
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}
const has = (id: string) => host.querySelector(tid(id)) !== null;
const text = (id: string) => q(id).textContent ?? "";

// The interactive control: the testid'd node itself (if it is one), or the
// control inside the widget wrapper.
function ctl<T extends HTMLElement = HTMLInputElement>(id: string): T {
  const el = q(id);
  if (el.matches("input,select,textarea,button")) return el as T;
  const inner = el.querySelector("input,select,textarea,button");
  if (!inner) throw new Error(`no control inside testid: ${id}`);
  return inner as T;
}

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

function typeInto(el: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function setShell(marker: "desktop" | "mobile" | "browser") {
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  if (marker !== "browser") (globalThis as { peakbotShell?: unknown }).peakbotShell = marker;
}

// ─── fetch stub (same shape as ./firstRun.test.tsx) ─────────────────────

interface Call {
  url: string;
  method: string;
  body: unknown;
}

function stubApi(s: {
  settings?: SettingsMeta;
  put?: { status: number; body: unknown };
} = {}): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      let body: unknown;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body);
        } catch {
          body = init.body;
        }
      }
      calls.push({ url, method, body });
      const reply = (status: number, json: unknown) => ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => json,
        text: async () => JSON.stringify(json),
      });
      if (url.startsWith("/api/settings")) return reply(200, s.settings ?? meta());
      if (url.startsWith("/api/config") && method === "PUT") {
        const p = s.put ?? { status: 200, body: PUT_OK };
        return reply(p.status, p.body);
      }
      if (url.startsWith("/api/config")) return reply(200, MISSING_GLOBAL);
      return reply(404, { error: "unrouted " + url });
    }),
  );
  return calls;
}

function mountWizard(over: { m?: Partial<SettingsMeta>; g?: ConfigText } = {}) {
  mount(<FirstRun meta={meta(over.m)} global={over.g ?? MISSING_GLOBAL} onClose={() => {}} />);
}

/** Welcome → "Set up an AI provider on this device" → … (existing path). */
function intoProviderPath() {
  click(q("firstrun-start-fresh"));
  expect(has("firstrun-step-provider"), "the provider path must reach its step").toBe(true);
}

/** Welcome → "Connect to my PeakBot server" → paste box. */
function intoConnectPath() {
  click(q("firstrun-connect-server"));
  expect(has("firstrun-step-connect"), "the connect path must reach its step").toBe(true);
}

// ═══ Welcome: the two choices ═══════════════════════════════════════════

describe("FirstRun — Welcome offers the two choices", () => {
  it.each([
    ["full", {}],
    ["local", { boot: LOCAL_BOOT }],
  ] as const)("%s edition: both choices are offered with the locked labels", (_ed, boot) => {
    stubApi();
    mountWizard({ m: boot });
    expect(has("firstrun-step-welcome")).toBe(true);

    expect(q("firstrun-start-fresh").textContent).toBe("Set up an AI provider on this device");
    expect(q("firstrun-connect-server").textContent).toBe("Connect to my PeakBot server");
  });

  it("the provider choice keeps the existing path and testid", () => {
    stubApi();
    mountWizard();
    intoProviderPath();
    expect(has("firstrun-step-provider")).toBe(true);
  });

  it("the connect choice opens the paste box (full edition)", () => {
    stubApi();
    mountWizard();
    intoConnectPath();
    expect(has("firstrun-connect-paste")).toBe(true);
    expect(has("firstrun-connect-input")).toBe(true);
  });
});

// ═══ Connect path: paste → review → relay-only PUT ══════════════════════

describe("FirstRun — Connect to my PeakBot server", () => {
  it.each([
    ["full", {}],
    ["local", { boot: LOCAL_BOOT }],
  ] as const)("%s edition: paste → review shows <profile> on <host>", async (_ed, boot) => {
    stubApi();
    mountWizard({ m: boot });
    intoConnectPath();
    typeInto(q<HTMLInputElement>("firstrun-connect-input"), LINK);
    click(q("firstrun-connect-continue"));
    await flush();

    expect(has("firstrun-step-review"), "a valid link must reach Review").toBe(true);
    expect(text("firstrun-connect-name")).toBe(EXPECTED_NAME);
  });

  it.each([
    ["full", {}],
    ["local", { boot: LOCAL_BOOT }],
  ] as const)("%s edition: finish PUTs exactly one remote: profile and no providers", async (_ed, boot) => {
    const calls = stubApi();
    mountWizard({ m: boot });
    intoConnectPath();
    typeInto(q<HTMLInputElement>("firstrun-connect-input"), LINK);
    click(q("firstrun-connect-continue"));
    await flush();
    click(q("firstrun-finish"));
    await flush();

    const puts = calls.filter((c) => c.method === "PUT");
    expect(puts).toHaveLength(1);
    const body = puts[0].body as { scope: string; text: string; base_sha256: unknown };
    expect(body.scope).toBe("global");
    expect(body.base_sha256).toBeNull();

    const doc = parseYaml(body.text) as Record<string, Record<string, unknown>>;
    // EXACTLY one profile …
    const names = Object.keys(doc.profiles ?? {});
    expect(names).toEqual([EXPECTED_NAME]);
    // … a remote: block with the link's fields …
    const def = doc.profiles[EXPECTED_NAME] as Record<string, unknown>;
    const remote = def.remote as Record<string, unknown>;
    expect(remote.url).toBe("https://10.2.0.48:7899"); // port kept in the url
    expect(remote.token).toBe("pito");
    expect(remote.ca).toBe(CA);
    expect(remote.profile).toBe("dev");
    // … and NO providers (a relay-only config; nothing local invented).
    expect("providers" in doc).toBe(false);
  });

  it("an_invalid_link_shows_the_error_and_stays_on_the_step", async () => {
    stubApi();
    mountWizard();
    intoConnectPath();
    typeInto(q<HTMLInputElement>("firstrun-connect-input"), "not a link");
    click(q("firstrun-connect-continue"));
    await flush();

    expect(has("firstrun-step-connect"), "still on the connect step").toBe(true);
    expect(has("firstrun-step-review")).toBe(false);
    expect(text("firstrun-connect-error")).toBe(
      "Not a pairing link — expected https://host:port/?token=…",
    );
  });

  it("a_server_validation_error_is_shown_on_review", async () => {
    stubApi({
      put: {
        status: 422,
        body: {
          error: "config is not valid",
          errors: [
            {
              file: "global",
              path: ["profiles", EXPECTED_NAME, "remote", "ca"],
              message: "pin the server's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate",
            },
          ],
        },
      },
    });
    mountWizard();
    intoConnectPath();
    typeInto(q<HTMLInputElement>("firstrun-connect-input"), `https://10.2.0.48:7899/?token=pito&profile=dev`);
    click(q("firstrun-connect-continue"));
    await flush();
    click(q("firstrun-finish"));
    await flush();

    expect(has("firstrun-step-review"), "stays on Review after a failed PUT").toBe(true);
    expect(has("firstrun-restart"), "no success screen").toBe(false);
    expect(text("firstrun-error")).toContain("pin the server's CA");
  });

  it("mobile: the connect box offers Scan QR and a scan fills the input", async () => {
    setShell("mobile");
    stubApi();
    mountWizard();
    intoConnectPath();

    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") return LINK;
      throw new Error(`unexpected command: ${cmd}`);
    });
    (globalThis as { __TAURI_INTERNALS__?: { invoke: unknown } }).__TAURI_INTERNALS__ = { invoke };

    const scan = q("firstrun-scan-qr");
    expect(scan, "the mobile connect box must offer Scan QR").not.toBeNull();

    click(scan);
    await flush();
    expect(invoke).toHaveBeenCalledWith("plugin:barcode-scanner|scan", {
      formats: ["QR_CODE"],
    });
    expect(q<HTMLInputElement>("firstrun-connect-input").value).toBe(LINK);
  });

  it("browser: the connect box has no Scan QR button", () => {
    stubApi();
    mountWizard();
    intoConnectPath();
    expect(
      has("firstrun-scan-qr"),
      "no camera capability outside the mobile shell",
    ).toBe(false);
  });
});

// ═══ Local provider path: the first profile is "On this phone" ══════════

const ANSWERS: FirstRunAnswers = {
  provider: {
    type: "ollama",
    name: "ollama",
    base_url: "http://127.0.0.1:11434",
    models: [{ name: "llama3.1:8b", alias: "llama", max_tokens: 8192 }],
  },
  agent: { model: "llama", persona: null, subagents: false },
  web: { access: "local", token: null },
};

describe("FirstRun — Local edition first profile name", () => {
  it("the_local_builder_names_the_first_profile_on_this_phone", () => {
    const doc = parseYaml(buildFirstRunYaml(ANSWERS, { local: true })) as {
      profiles: Record<string, unknown>;
    };
    expect(Object.keys(doc.profiles)).toEqual(["On this phone"]);
  });

  it("the_full_builder_keeps_the_default_name", () => {
    const doc = parseYaml(buildFirstRunYaml(ANSWERS)) as {
      profiles: Record<string, unknown>;
    };
    expect(Object.keys(doc.profiles)).toEqual(["default"]);
  });

  it("local: the_agent_step_announces_the_on_this_phone_profile", () => {
    setShell("mobile");
    stubApi();
    mountWizard({ m: { boot: LOCAL_BOOT } });
    intoProviderPath();
    const preset = host.querySelector('[data-testid="fr-model-preset-0"]');
    if (preset) click(preset);
    // The hosted default (openrouter) needs a key; type it so the step is
    // valid and Next advances.
    typeInto(ctl<HTMLInputElement>("fr-provider-apikey"), KEY);
    click(q("firstrun-next"));
    expect(has("firstrun-step-agent")).toBe(true);
    expect(text("firstrun-agent-banner")).toContain("On this phone");
  });

  it("local: the_review_yaml_carries_the_on_this_phone_profile", async () => {
    setShell("mobile");
    stubApi();
    mountWizard({ m: { boot: LOCAL_BOOT } });
    intoProviderPath();
    const preset = host.querySelector('[data-testid="fr-model-preset-0"]');
    if (preset) click(preset);
    // The hosted default (openrouter) needs a key; type it so the step is
    // valid and Next advances.
    typeInto(ctl<HTMLInputElement>("fr-provider-apikey"), KEY);
    click(q("firstrun-next")); // → agent
    click(q("firstrun-next")); // → review (no web step on Local)
    expect(has("firstrun-step-review")).toBe(true);
    const yaml = text("firstrun-yaml");
    expect(yaml).toContain("On this phone");
    expect(yaml).not.toMatch(/^profiles:\s*\n\s*default:/m);
  });
});
