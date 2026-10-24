// T33 (RED): D10 first-run guided setup + D12 `/setup` alias. Spec §9.13,
// §6.9, plan T33. Written BEFORE the code: ./firstRun and ../Boot do not
// exist yet. Props / data-testids / pure signatures: contract.ts
// ("T33 … ./firstRun", "../Boot").
//
// Decisions pinned here (flagged in the T32/T33 RED report):
//  - buildFirstRunYaml is compared to the spec's example SEMANTICALLY (yaml
//    parse → deep-equal + top-level key order), not byte-for-byte: the spec
//    example mixes flow `{ … }` maps and trailing comments that no
//    serializer reproduces exactly. The PUT body text is `buildFirstRunYaml`
//    output verbatim.
//  - SPA derives pending-restart from (needs_setup, GET /api/config global
//    exists, legacy_layout) — SettingsMeta has no "file exists" flag.
//  - Boot (src/Boot.tsx) owns the needs_setup decision so App/useAgent never
//    mount (the socket would 503).
//
// Harness: plain createRoot/act + stubbed globalThis.fetch (project
// convention). jsdom via vitest.config.ts (src/settings/**/*.test.tsx).
//
// Expected RED: whole-file import error (missing ./firstRun, ../Boot).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { parse as parseYaml } from "yaml";
import { FirstRun, buildFirstRunYaml, deriveSetupPhase } from "./firstRun";
import { Boot } from "../Boot";
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
      arch: "amd64",
      exe_path: "/opt/peakbot/bin/peakbot",
      data_dir: "/home/u/.local/share/peakbot",
      version: "0.24.0",
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

const KEY = "sk-or-test-key-123456";

// The spec §9.13 "Generated YAML for a fresh setup", verbatim except the
// masked key `sk-or-…` (a real key is what the wizard collects).
const SPEC_EXAMPLE = `providers:
  - name: openrouter
    type: openrouter
    api_key: ${KEY}
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }
mcps: []
profiles:
  default:
    agents:
      main:
        model: sonnet
        # subagents: { helper: { prompt: "Take focused sub-tasks and report back." } }   # if toggled
general:
  web: { bind: 127.0.0.1:7823 }      # LAN: bind: 0.0.0.0:7823 + token
`;

const ANSWERS: FirstRunAnswers = {
  provider: {
    type: "openrouter",
    name: "openrouter",
    api_key: KEY,
    models: [{ name: "anthropic/claude-sonnet-4.5", alias: "sonnet", max_tokens: 8192 }],
  },
  agent: { model: "sonnet", persona: null, subagents: false },
  web: { access: "local", token: null },
};

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

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
  delete (globalThis as { isTauri?: unknown }).isTauri;
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
const has = (id: string) => host.querySelector(tid(id)) !== null;
function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(tid(id));
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}
const text = (id: string) => q(id).textContent ?? "";

/** The interactive element: the testid'd node itself, or the control inside a widget wrapper. */
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

function typeInto(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement : HTMLInputElement;
  Object.getOwnPropertyDescriptor(proto.prototype, "value")!.set!.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function choose(el: HTMLSelectElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

const step = (name: string) => has(`firstrun-step-${name}`);
const next = () => click(q("firstrun-next"));

// ─── fetch stub ─────────────────────────────────────────────────────────

interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: unknown;
}
interface ApiStub {
  /** number = HTTP error status for GET /api/settings; "reject" = network error. */
  settings?: SettingsMeta | number | "reject";
  config?: ConfigText;
  put?: { status: number; body: unknown };
}

const PUT_OK: PutOk = {
  path: GLOBAL_PATH,
  backup: null,
  sha256: "newsha",
  restart_required: [],
  applies_on_next_new: true,
};

function stubApi(s: ApiStub = {}): Call[] {
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
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries((init?.headers ?? {}) as Record<string, string>)) {
        headers[k.toLowerCase()] = v;
      }
      calls.push({ url, method, headers, body });
      const reply = (status: number, json: unknown) => ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => json,
        text: async () => JSON.stringify(json),
      });
      if (url.startsWith("/api/settings")) {
        if (s.settings === "reject") throw new TypeError("network down");
        if (typeof s.settings === "number") return reply(s.settings, { error: "nope" });
        return reply(200, s.settings ?? meta());
      }
      if (url.startsWith("/api/config") && method === "PUT") {
        const p = s.put ?? { status: 200, body: PUT_OK };
        return reply(p.status, p.body);
      }
      if (url.startsWith("/api/config")) {
        return reply(200, s.config ?? MISSING_GLOBAL);
      }
      return reply(404, { error: "unrouted " + url });
    }),
  );
  return calls;
}

const puts = (calls: Call[]) => calls.filter((c) => c.method === "PUT");

// ─── wizard driver (real UI, no seams) ──────────────────────────────────

function fillProvider(key = KEY) {
  typeInto(ctl("fr-provider-apikey") as HTMLInputElement, key);
  click(ctl("fr-model-add"));
  typeInto(ctl("fr-model-name-0") as HTMLInputElement, "anthropic/claude-sonnet-4.5");
  typeInto(ctl("fr-model-alias-0") as HTMLInputElement, "sonnet");
  typeInto(ctl("fr-model-maxtokens-0") as HTMLInputElement, "8192");
}

/** Welcome → … → Review, all defaults + the provider fields above. */
function driveToReview(opts: { subagents?: boolean } = {}) {
  click(q("firstrun-start-fresh"));
  fillProvider();
  next(); // → agent
  if (opts.subagents) click(ctl("fr-agent-subagents"));
  next(); // → web
  next(); // → review
}

function mountFirstRun(over: { m?: Partial<SettingsMeta>; g?: ConfigText; onClose?: () => void } = {}) {
  mount(<FirstRun meta={meta(over.m)} global={over.g ?? MISSING_GLOBAL} onClose={over.onClose} />);
}

// ═══ buildFirstRunYaml (pure) ═══════════════════════════════════════════

describe("buildFirstRunYaml (spec §9.13 generated YAML)", () => {
  it("review_yaml_matches_spec_example", () => {
    const out = buildFirstRunYaml(ANSWERS);
    const got = parseYaml(out) as Record<string, unknown>;
    const want = parseYaml(SPEC_EXAMPLE) as Record<string, unknown>;
    expect(got).toEqual(want);
    // Key order is part of the example (providers, mcps, profiles, general),
    // at every level of the generated blocks.
    expect(JSON.stringify(got)).toBe(JSON.stringify(want));
    expect(Object.keys(got)).toEqual(["providers", "mcps", "profiles", "general"]);
    expect(out.endsWith("\n")).toBe(true);
  });

  it("profiles_and_agents_are_maps_never_name_lists", () => {
    const out = buildFirstRunYaml(ANSWERS);
    expect(out).not.toMatch(/^\s*-\s*name:\s*default\b/m);
    expect(out).not.toMatch(/^\s*-\s*name:\s*main\b/m);
    const doc = parseYaml(out) as { profiles: unknown };
    expect(Array.isArray(doc.profiles)).toBe(false);
  });

  it("always_writes_empty_mcps_list", () => {
    const doc = parseYaml(buildFirstRunYaml(ANSWERS)) as { mcps: unknown };
    expect(doc.mcps).toEqual([]);
  });

  it("subagents_toggle_adds_helper_with_spec_prompt", () => {
    const a = clone(ANSWERS);
    a.agent.subagents = true;
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      profiles: { default: { agents: { main: Record<string, unknown> } } };
    };
    expect(doc.profiles.default.agents.main.subagents).toEqual({
      helper: { prompt: "Take focused sub-tasks and report back." },
    });
    // Off → no key at all.
    const off = parseYaml(buildFirstRunYaml(ANSWERS)) as typeof doc;
    expect("subagents" in off.profiles.default.agents.main).toBe(false);
  });

  it("builtin_persona_omits_persona_key", () => {
    const doc = parseYaml(buildFirstRunYaml(ANSWERS)) as {
      profiles: { default: { agents: { main: Record<string, unknown> } } };
    };
    expect("persona" in doc.profiles.default.agents.main).toBe(false);
  });

  it("custom_persona_is_written_verbatim_even_with_yaml_specials", () => {
    const persona = "Terse: reviewer — says 'no' #1\nsecond line ✨";
    const a = clone(ANSWERS);
    a.agent.persona = persona;
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      profiles: { default: { agents: { main: { persona: string } } } };
    };
    expect(doc.profiles.default.agents.main.persona).toBe(persona);
  });

  it("lan_access_writes_all_interfaces_bind_and_token", () => {
    const a = clone(ANSWERS);
    a.web = { access: "lan", token: "abc123" };
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      general: { web: Record<string, unknown> };
    };
    expect(doc.general.web).toEqual({ bind: "0.0.0.0:7823", token: "abc123" });
  });

  it("local_access_never_writes_a_token_even_if_one_was_typed", () => {
    const a = clone(ANSWERS);
    a.web = { access: "local", token: "leftover" };
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      general: { web: Record<string, unknown> };
    };
    expect(doc.general.web).toEqual({ bind: "127.0.0.1:7823" });
  });

  it("openrouter_omits_base_url_even_when_given", () => {
    const a = clone(ANSWERS);
    a.provider.base_url = "https://openrouter.ai/api/v1";
    const doc = parseYaml(buildFirstRunYaml(a)) as { providers: Record<string, unknown>[] };
    expect("base_url" in doc.providers[0]).toBe(false);
  });

  it("local_provider_keeps_base_url_and_omits_empty_api_key", () => {
    const a: FirstRunAnswers = {
      provider: {
        type: "llamacpp",
        name: "local",
        base_url: "http://127.0.0.1:8080",
        api_key: "",
        models: [{ name: "local-model", alias: "local", max_tokens: 4096 }],
      },
      agent: { model: "local", persona: null, subagents: false },
      web: { access: "local", token: null },
    };
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      providers: Record<string, unknown>[];
      profiles: { default: { agents: { main: { model: string } } } };
    };
    expect(doc.providers[0]).toEqual({
      name: "local",
      type: "llamacpp",
      base_url: "http://127.0.0.1:8080",
      models: [{ name: "local-model", alias: "local", max_tokens: 4096 }],
    });
    expect(doc.profiles.default.agents.main.model).toBe("local");
  });

  it("multiple_models_keep_order_and_number_types", () => {
    const a = clone(ANSWERS);
    a.provider.models.push({ name: "openai/gpt-5", alias: "gpt", max_tokens: 16000 });
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      providers: { models: { alias: string; max_tokens: unknown }[] }[];
    };
    expect(doc.providers[0].models.map((m) => m.alias)).toEqual(["sonnet", "gpt"]);
    expect(doc.providers[0].models.map((m) => m.max_tokens)).toEqual([8192, 16000]);
  });

  it("yaml_looking_strings_roundtrip_as_strings", () => {
    // Aliases / keys that a naive serializer would turn into bool/number/null.
    const a = clone(ANSWERS);
    a.provider.api_key = "123456";
    a.provider.models = [{ name: "m:1 #x", alias: "yes", max_tokens: 1 }];
    a.agent.model = "yes";
    const doc = parseYaml(buildFirstRunYaml(a)) as {
      providers: { api_key: unknown; models: { name: unknown; alias: unknown }[] }[];
      profiles: { default: { agents: { main: { model: unknown } } } };
    };
    expect(doc.providers[0].api_key).toBe("123456");
    expect(doc.providers[0].models[0]).toMatchObject({ name: "m:1 #x", alias: "yes" });
    expect(doc.profiles.default.agents.main.model).toBe("yes");
  });

  it("is_pure_and_does_not_mutate_its_input", () => {
    const a = clone(ANSWERS);
    const frozen = JSON.stringify(a);
    const one = buildFirstRunYaml(a);
    const two = buildFirstRunYaml(a);
    expect(one).toBe(two);
    expect(JSON.stringify(a)).toBe(frozen);
  });
});

// ═══ deriveSetupPhase (pure) ════════════════════════════════════════════

describe("deriveSetupPhase / pending_restart_derived (spec §6.9)", () => {
  it("pending_restart_derived", () => {
    // needs_setup && file exists && !legacy_layout ⇒ restart pending
    expect(deriveSetupPhase({ needs_setup: true, legacy_layout: false }, true)).toBe(
      "restart_pending",
    );
    // fresh machine
    expect(deriveSetupPhase({ needs_setup: true, legacy_layout: false }, false)).toBe("guided");
    // legacy file that failed to migrate: the file exists but is NOT ours
    expect(deriveSetupPhase({ needs_setup: true, legacy_layout: true }, true)).toBe("guided");
    expect(deriveSetupPhase({ needs_setup: true, legacy_layout: true }, false)).toBe("guided");
    // configured server
    expect(deriveSetupPhase({ needs_setup: false, legacy_layout: false }, true)).toBe("ready");
    expect(deriveSetupPhase({ needs_setup: false, legacy_layout: false }, false)).toBe("ready");
  });
});

// ═══ FirstRun component ═════════════════════════════════════════════════

describe("FirstRun — Welcome (spec §9.13 step 1)", () => {
  it("failure_banner_shows_migrate_error", () => {
    stubApi();
    const reason = "pipelines[0].name: expected a string, found number 7";
    mountFirstRun({
      m: { legacy_layout: true, migrate_error: reason },
      g: { ...MISSING_GLOBAL, exists: true, text: "pipelines: [7]\n", sha256: "legacysha" },
    });
    expect(step("welcome")).toBe(true);
    const banner = text("firstrun-banner-migrate");
    expect(banner).toContain("PeakBot couldn't migrate your old config");
    expect(banner).toContain(reason); // verbatim
    expect(banner).toContain("Your config.yaml was not changed.");
    expect(banner).toContain("config.yaml.pre-0.24");
  });

  it("welcome_has_no_migrate_button_and_start_fresh_is_the_only_action", () => {
    stubApi();
    mountFirstRun({
      m: { legacy_layout: true, migrate_error: "boom" },
      g: { ...MISSING_GLOBAL, exists: true, text: "x: 1\n", sha256: "s" },
    });
    const pane = q("firstrun-step-welcome");
    const buttons = Array.from(pane.querySelectorAll("button"));
    expect(buttons.map((b) => b.getAttribute("data-testid"))).toEqual(["firstrun-start-fresh"]);
    expect(buttons.some((b) => /migrate/i.test(b.textContent ?? ""))).toBe(false);
  });

  it("no_failure_banner_on_a_fresh_machine", () => {
    stubApi();
    mountFirstRun();
    expect(has("firstrun-banner-migrate")).toBe(false);
    expect(text("firstrun-step-welcome")).not.toContain("couldn't migrate");
  });

  it("welcome_lists_machine_facts", () => {
    stubApi();
    mountFirstRun();
    const facts = text("firstrun-facts");
    for (const f of ["linux", "/opt/peakbot/bin/peakbot", GLOBAL_PATH, "/home/u/.local/share/peakbot", "0.24.0"]) {
      expect(facts).toContain(f);
    }
  });

  it("start_fresh_goes_to_the_provider_step", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    expect(step("provider")).toBe(true);
    expect(step("welcome")).toBe(false);
  });

  it("later_steps_are_disabled_until_visited", () => {
    stubApi();
    mountFirstRun();
    expect(ctl("firstrun-rail-provider").hasAttribute("disabled")).toBe(true);
    expect(ctl("firstrun-rail-agent").hasAttribute("disabled")).toBe(true);
    expect(ctl("firstrun-rail-review").hasAttribute("disabled")).toBe(true);
    click(q("firstrun-start-fresh"));
    expect(ctl("firstrun-rail-provider").hasAttribute("disabled")).toBe(false);
    expect(ctl("firstrun-rail-agent").hasAttribute("disabled")).toBe(true);
    expect(ctl("firstrun-rail-review").hasAttribute("disabled")).toBe(true);
  });

  it("esc_calls_onClose", () => {
    stubApi();
    const onClose = vi.fn();
    mountFirstRun({ onClose });
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("FirstRun — step validation (spec §9.13 steps 2–4)", () => {
  it("provider_step_requires_key_and_model", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    const errs = text("firstrun-step-errors");
    expect(errs).toContain("An API key is required for openrouter.");
    expect(errs).toContain("Add at least one model.");
    expect(ctl<HTMLButtonElement>("firstrun-next").disabled).toBe(true);

    // clicking a disabled Next does not advance
    click(q("firstrun-next"));
    expect(step("provider")).toBe(true);

    fillProvider();
    expect(has("firstrun-step-errors") ? text("firstrun-step-errors") : "").not.toContain("API key");
    expect(ctl<HTMLButtonElement>("firstrun-next").disabled).toBe(false);
    next();
    expect(step("agent")).toBe(true);
  });

  it("local_provider_type_does_not_require_an_api_key", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    choose(ctl<HTMLSelectElement>("fr-provider-type"), "ollama");
    const errs = has("firstrun-step-errors") ? text("firstrun-step-errors") : "";
    expect(errs).not.toContain("An API key is required");
    expect(errs).toContain("Add at least one model.");
  });

  // T34 regression: changing the type left base_url empty and the name on
  // "openrouter", so an Ollama setup was written as `name: openrouter` with
  // no base_url. Spec §9.13: base_url is auto-filled.
  it("provider_type_change_autofills_base_url_and_default_name", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    choose(ctl<HTMLSelectElement>("fr-provider-type"), "ollama");
    expect(ctl("fr-provider-baseurl").value).toBe("http://127.0.0.1:11434");
    expect(ctl("fr-provider-name").value).toBe("ollama");

    // A name the user typed is theirs: a later type change keeps it.
    typeInto(ctl("fr-provider-name"), "my-box");
    choose(ctl<HTMLSelectElement>("fr-provider-type"), "llamacpp");
    expect(ctl("fr-provider-baseurl").value).toBe("http://127.0.0.1:8080");
    expect(ctl("fr-provider-name").value).toBe("my-box");

    // OpenRouter's endpoint is fixed: no base_url.
    choose(ctl<HTMLSelectElement>("fr-provider-type"), "openrouter");
    expect(ctl("fr-provider-baseurl").value).toBe("");
  });

  it("agent_step_announces_generated_profile_agent_and_model", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    fillProvider();
    next();
    expect(text("firstrun-agent-banner")).toContain(
      "Generated: profile default › agent main using model sonnet",
    );
    expect(ctl<HTMLSelectElement>("fr-agent-model").value).toBe("sonnet");
  });

  it("web_lan_without_token_blocks_next_until_a_token_is_generated", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    fillProvider();
    next(); // agent
    next(); // web
    expect(step("web")).toBe(true);
    expect(ctl("fr-web-local").checked).toBe(true);
    click(ctl("fr-web-lan"));
    expect(text("firstrun-step-errors")).toContain(
      "A LAN bind requires a token — generate one, or switch back to local only.",
    );
    expect(ctl<HTMLButtonElement>("firstrun-next").disabled).toBe(true);
    click(ctl("fr-web-token-generate"));
    expect(ctl("fr-web-token").value.length).toBeGreaterThan(0);
    expect(has("firstrun-step-errors") ? text("firstrun-step-errors") : "").not.toContain("LAN bind");
    expect(ctl<HTMLButtonElement>("firstrun-next").disabled).toBe(false);
  });

  it("back_returns_to_the_previous_step_keeping_answers", () => {
    stubApi();
    mountFirstRun();
    click(q("firstrun-start-fresh"));
    fillProvider();
    next();
    click(q("firstrun-back"));
    expect(step("provider")).toBe(true);
    expect(ctl("fr-model-alias-0").value).toBe("sonnet");
  });
});

describe("FirstRun — Review + Finish (spec §9.13 steps 5–6)", () => {
  it("review_preview_masks_the_api_key_visually", () => {
    stubApi();
    mountFirstRun();
    driveToReview();
    expect(step("review")).toBe(true);
    const preview = text("firstrun-yaml");
    expect(preview).toContain("providers:");
    expect(preview).toContain("api_key:");
    expect(preview).not.toContain(KEY); // masked on screen…
    expect(text("firstrun-review-path")).toContain(GLOBAL_PATH);
    expect(has("firstrun-review-legacy-note")).toBe(false);
  });

  it("finish_puts_global_with_null_sha", async () => {
    const calls = stubApi();
    mountFirstRun();
    driveToReview({ subagents: true });
    click(q("firstrun-finish"));
    await flush();

    const sent = puts(calls);
    expect(sent).toHaveLength(1);
    expect(sent[0].url).toBe("/api/config");
    expect(sent[0].headers["content-type"]).toBe("application/json");
    const body = sent[0].body as { scope: string; text: string; base_sha256: unknown };
    expect(body.scope).toBe("global");
    expect(body.base_sha256).toBeNull(); // from GET: file absent ⇒ null
    // …but the REAL key is what gets written.
    expect(body.text).toContain(KEY);
    const want: FirstRunAnswers = clone(ANSWERS);
    want.agent.subagents = true;
    expect(body.text).toBe(buildFirstRunYaml(want));
    expect(parseYaml(body.text)).toEqual({
      providers: [
        {
          name: "openrouter",
          type: "openrouter",
          api_key: KEY,
          models: [{ name: "anthropic/claude-sonnet-4.5", alias: "sonnet", max_tokens: 8192 }],
        },
      ],
      mcps: [],
      profiles: {
        default: {
          agents: {
            main: {
              model: "sonnet",
              subagents: { helper: { prompt: "Take focused sub-tasks and report back." } },
            },
          },
        },
      },
      general: { web: { bind: "127.0.0.1:7823" } },
    });
  });

  it("finish_over_legacy_file_sends_its_sha_and_review_notes_pre024_copy", async () => {
    const calls = stubApi();
    mountFirstRun({
      m: { legacy_layout: true, migrate_error: "bad pipeline" },
      g: { ...MISSING_GLOBAL, exists: true, text: "pipelines: [7]\n", sha256: "legacysha" },
    });
    driveToReview();
    expect(text("firstrun-review-legacy-note")).toContain("old file kept as config.yaml.pre-0.24");
    click(q("firstrun-finish"));
    await flush();
    const body = puts(calls)[0].body as { base_sha256: unknown };
    expect(body.base_sha256).toBe("legacysha");
  });

  it("review_note_in_browser_says_restart_after_finish", () => {
    stubApi();
    mountFirstRun();
    driveToReview();
    expect(text("firstrun-review-note")).toContain("After Finish, restart PeakBot to start chatting.");
  });

  it("desktop_finish_navigates_restart_sentinel", async () => {
    (globalThis as { isTauri?: unknown }).isTauri = true;
    const calls = stubApi();
    // jsdom cannot navigate to a custom scheme: record the assignment.
    const loc = { href: "http://localhost/", pathname: "/", search: "" };
    vi.stubGlobal("location", loc);
    mountFirstRun();
    driveToReview();
    expect(text("firstrun-review-note")).toContain("Finishing restarts PeakBot with this config.");
    click(q("firstrun-finish"));
    await flush();
    expect(puts(calls)).toHaveLength(1);
    expect(loc.href).toBe("peakbot-desktop:restart");
    // No Success screen on desktop.
    expect(has("firstrun-restart")).toBe(false);
  });

  it("browser_finish_shows_restart_screen_without_open_chat", async () => {
    stubApi();
    const loc = { href: "http://localhost/", pathname: "/", search: "" };
    vi.stubGlobal("location", loc);
    mountFirstRun();
    driveToReview();
    click(q("firstrun-finish"));
    await flush();

    const screen = text("firstrun-restart");
    expect(screen).toContain("Config written");
    expect(screen).toContain("PeakBot wrote your config. A backup was kept.");
    expect(screen).toContain("Restart PeakBot to start chatting.");
    expect(screen).toContain(GLOBAL_PATH);
    expect(screen).toContain("0.24.0");
    expect(host.textContent ?? "").not.toMatch(/open chat/i);
    expect(host.querySelectorAll("a[href]").length).toBe(0);
    expect(loc.href).toBe("http://localhost/"); // no navigation in a browser
    expect(has("firstrun-restart-backup")).toBe(false); // PutOk.backup === null
  });

  // T34 regression (spec §9.13): the success screen's facts include the
  // Provider and the Agent — they were missing.
  it("restart_screen_lists_provider_and_agent_facts", async () => {
    stubApi();
    mountFirstRun();
    driveToReview();
    click(q("firstrun-finish"));
    await flush();
    expect(text("firstrun-restart-provider")).toMatch(/^Provider: \S+ \(\w+\)$/);
    expect(text("firstrun-restart-agent")).toMatch(/^Agent: default › main — \S+$/);
  });

  it("restart_screen_lists_backup_only_when_one_exists", async () => {
    stubApi({ put: { status: 200, body: { ...PUT_OK, backup: GLOBAL_PATH + ".bak" } } });
    mountFirstRun();
    driveToReview();
    click(q("firstrun-finish"));
    await flush();
    expect(text("firstrun-restart-backup")).toContain(GLOBAL_PATH + ".bak");
  });

  it("failed_put_keeps_review_and_shows_the_error_without_restart_screen", async () => {
    stubApi({
      put: {
        status: 422,
        body: { error: "invalid config", errors: [{ file: "global", path: ["providers", 0], message: "bad" }] },
      },
    });
    mountFirstRun();
    driveToReview();
    click(q("firstrun-finish"));
    await flush();
    expect(step("review")).toBe(true);
    expect(has("firstrun-restart")).toBe(false);
    expect(text("firstrun-error")).toContain("invalid config");
  });
});

describe("FirstRun — pending restart derived (spec §6.9)", () => {
  it("pending_restart_derived_shows_restart_screen_on_reload", () => {
    stubApi();
    mountFirstRun({
      g: { ...MISSING_GLOBAL, exists: true, text: "providers: []\n", sha256: "written" },
    });
    expect(text("firstrun-restart")).toContain("Restart PeakBot to start chatting.");
    expect(step("welcome")).toBe(false);
    expect(has("firstrun-start-fresh")).toBe(false);
  });

  it("existing_legacy_file_is_not_a_pending_restart", () => {
    stubApi();
    mountFirstRun({
      m: { legacy_layout: true, migrate_error: "x" },
      g: { ...MISSING_GLOBAL, exists: true, text: "pipelines: []\n", sha256: "l" },
    });
    expect(has("firstrun-restart")).toBe(false);
    expect(step("welcome")).toBe(true);
  });

  it("missing_file_is_not_a_pending_restart", () => {
    stubApi();
    mountFirstRun();
    expect(has("firstrun-restart")).toBe(false);
    expect(step("welcome")).toBe(true);
  });
});

// ═══ Boot (D12 /setup alias + trigger) ══════════════════════════════════

function Child() {
  return <div data-testid="chat-app">chat</div>;
}

async function mountBoot() {
  mount(
    <Boot>
      <Child />
    </Boot>,
  );
  await flush();
}

describe("Boot — first-run trigger and /setup alias (spec §9.13, D12)", () => {
  it("setup_path_opens_guided", async () => {
    window.history.replaceState(null, "", "/setup");
    stubApi({ settings: meta({ needs_setup: true }) });
    await mountBoot();
    expect(step("welcome")).toBe(true);
    expect(has("chat-app")).toBe(false);
  });

  it("needs_setup_opens_guided_on_the_root_path_too_and_never_mounts_the_app", async () => {
    stubApi({ settings: meta({ needs_setup: true }) });
    await mountBoot();
    expect(step("welcome")).toBe(true);
    expect(has("chat-app")).toBe(false); // no useAgent ⇒ no /ws attempt
  });

  it("setup_path_is_just_the_chat_once_configured", async () => {
    window.history.replaceState(null, "", "/setup");
    stubApi({ settings: meta({ needs_setup: false }) });
    await mountBoot();
    expect(has("chat-app")).toBe(true);
    expect(has("firstrun")).toBe(false);
  });

  it("configured_server_renders_the_app_and_never_fetches_the_global_config", async () => {
    const calls = stubApi({ settings: meta({ needs_setup: false }) });
    await mountBoot();
    expect(has("chat-app")).toBe(true);
    expect(calls.some((c) => c.url.startsWith("/api/config"))).toBe(false);
  });

  it("settings_endpoint_failure_falls_back_to_the_app", async () => {
    stubApi({ settings: 404 }); // older server without /api/settings
    await mountBoot();
    expect(has("chat-app")).toBe(true);
    expect(has("firstrun")).toBe(false);
  });

  it("network_failure_on_settings_falls_back_to_the_app", async () => {
    stubApi({ settings: "reject" });
    await mountBoot();
    expect(has("chat-app")).toBe(true);
  });

  it("shows_loading_placeholder_before_settings_arrive_and_no_app", () => {
    stubApi({ settings: meta() });
    mount(
      <Boot>
        <Child />
      </Boot>,
    );
    // Before any microtask flush: neither the app nor the wizard yet.
    expect(has("chat-app")).toBe(false);
    expect(has("boot-loading")).toBe(true);
  });

  it("pending_restart_derived_on_reload_via_boot", async () => {
    stubApi({
      settings: meta({ needs_setup: true, legacy_layout: false }),
      config: { ...MISSING_GLOBAL, exists: true, text: "providers: []\n", sha256: "w" },
    });
    await mountBoot();
    expect(text("firstrun-restart")).toContain("Restart PeakBot to start chatting.");
    expect(has("chat-app")).toBe(false);
  });

  it("failed_migration_boots_into_welcome_with_the_failure_banner", async () => {
    stubApi({
      settings: meta({ needs_setup: true, legacy_layout: true, migrate_error: "pipelines[0]: bad" }),
      config: { ...MISSING_GLOBAL, exists: true, text: "pipelines: [7]\n", sha256: "l" },
    });
    await mountBoot();
    expect(text("firstrun-banner-migrate")).toContain("pipelines[0]: bad");
    expect(has("firstrun-restart")).toBe(false);
  });

  it("esc_closes_the_wizard_into_a_setup_strip_with_finish_setup", async () => {
    stubApi({ settings: meta({ needs_setup: true }) });
    await mountBoot();
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(has("firstrun")).toBe(false);
    expect(text("setup-strip")).toContain(
      "PeakBot isn't configured yet — no provider or model is set up.",
    );
    expect(has("chat-app")).toBe(false);
    click(q("setup-strip-finish"));
    expect(has("firstrun")).toBe(true);
  });

  it("setup_strip_names_the_migration_failure_when_legacy_layout", async () => {
    stubApi({
      settings: meta({ needs_setup: true, legacy_layout: true, migrate_error: "bad" }),
      config: { ...MISSING_GLOBAL, exists: true, text: "x: 1\n", sha256: "l" },
    });
    await mountBoot();
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(text("setup-strip")).toContain("PeakBot couldn't migrate your old config");
  });
});
