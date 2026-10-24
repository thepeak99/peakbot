// T28 (RED): D5 "Agent page" (spec §9.4, plan T28). The ./agent module
// does not exist yet — this file must fail with a missing-module import
// error. The page reads get(doc, ["profiles", p, "agents", a]) via
// useSettingsCtx() and writes through context.ts `edit(path, value)` by
// segment array. Props/testids: contract.ts ("./agent (T28)").

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { AgentPage } from "./agent";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import { ToastProvider } from "./Toast";
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

// meta.builtin_persona: the placeholder is its FIRST line (spec §9.4).
const BUILTIN_PERSONA =
  "You are the Code Crusader: a careful, pragmatic engineer.\n" +
  "You verify before you commit.";

function meta(): SettingsMeta {
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
    skills: ["research", "deploy", "review"],
    builtin_persona: BUILTIN_PERSONA,
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

const DOC = `
providers:
  - name: openrouter
    type: openrouter
    models:
      - name: anthropic/claude-sonnet-4.5
        alias: sonnet
      - name: qwen/qwen3
        alias: qwen
profiles:
  dev:
    agents:
      lead:
        model: sonnet
        persona: "A custom persona."
        subagents:
          tester:
            model: qwen
            prompt: "Write the tests."
`;

// Same shape, no `persona` key — for the placeholder test.
const DOC_NO_PERSONA = `
providers:
  - name: openrouter
    type: openrouter
    models:
      - name: anthropic/claude-sonnet-4.5
        alias: sonnet
profiles:
  dev:
    agents:
      lead:
        model: sonnet
`;

const AGENT: Seg[] = ["profiles", "dev", "agents", "lead"];

function mountAgent(yaml: string): Edit[] {
  const edits: Edit[] = [];
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: meta(),
    doc: parse(yaml),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: [],
  };
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <AgentPage profile="dev" agent="lead" />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
  return edits;
}

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
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

describe("agent page (T28)", () => {
  // §9.4: the subagents toggle is DERIVED (subagents.length > 0); turning
  // it off clears the map and offers an Undo that restores the PREVIOUS
  // subagents map verbatim.
  it("subagents_toggle_undo_restores", () => {
    const edits = mountAgent(DOC);
    const subs: Seg[] = [...AGENT, "subagents"];

    // Derived checked state: the doc has one subagent.
    const toggle = q<HTMLInputElement>(
      "[data-testid='agent-subs-toggle'] [data-testid='input']",
    );
    expect(toggle.checked).toBe(true);

    // Off → the subagents key is removed (undefined, not {}).
    click(toggle);
    const clear = lastWrite(edits, subs);
    expect(clear, "toggle off clears subagents").toBeDefined();
    expect(clear!.value).toBeUndefined();

    // The Undo affordance appears.
    const undo = q("[data-testid='agent-subs-undo']");

    // Undo → the previous map comes back, verbatim.
    click(undo);
    const restore = lastWrite(edits, subs);
    expect(restore, "undo restores subagents").toBeDefined();
    expect(restore!.value).toEqual({
      tester: { model: "qwen", prompt: "Write the tests." },
    });

    // The stash is consumed: the affordance is gone.
    expect(host.querySelector("[data-testid='agent-subs-undo']")).toBeNull();
  });

  // §9.4 [changed, Q6]: the "Built-in" chip CLEARS the persona — the key
  // is omitted; a copy of the built-in text is never written.
  it("builtin_chip_omits_persona", () => {
    const edits = mountAgent(DOC);
    const persona: Seg[] = [...AGENT, "persona"];

    // The doc has a custom persona in the textarea…
    const ta = q<HTMLTextAreaElement>("[data-testid='agent-persona']");
    expect(ta.value).toBe("A custom persona.");

    const chip = q("[data-testid='agent-persona-chip-builtin']");
    expect(chip.textContent).toMatch(/Built-in/);
    click(chip);

    const w = lastWrite(edits, persona);
    expect(w, "persona written").toBeDefined();
    expect(w!.value, "Built-in chip omits the persona key").toBeUndefined();
    expect(
      edits.some((e) =>
        eqPath(e.path, persona) && typeof e.value === "string" && e.value.length > 0,
      ),
      "no persona text ever written by the chip",
    ).toBe(false);
  });

  // §9.4: the subagent model select's first option is
  // `inherits <agent model>` (value "") — choosing it omits `model`.
  it("inherit_option_omits_model", () => {
    const edits = mountAgent(DOC);
    const model: Seg[] = [...AGENT, "subagents", "tester", "model"];

    const sel = q<HTMLSelectElement>("[data-testid='agent-sub-model-0']");
    const first = sel.options[0];
    expect(first.value).toBe("");
    expect(first.text).toMatch(/inherits sonnet/);

    // The subagent currently pins qwen; inheriting removes the key.
    select(sel, "");
    const w = lastWrite(edits, model);
    expect(w, "subagent model written").toBeDefined();
    expect(w!.value, "inherit omits the model key").toBeUndefined();
  });

  // §9.4: while `persona` is absent, the textarea placeholder is the
  // first line of meta.builtin_persona.
  it("persona_placeholder_is_builtin_persona", () => {
    mountAgent(DOC_NO_PERSONA);
    const ta = q<HTMLTextAreaElement>("[data-testid='agent-persona']");
    expect(ta.value).toBe("");
    expect(ta.placeholder).toBe(
      "You are the Code Crusader: a careful, pragmatic engineer.",
    );
    // The full text is NOT in the placeholder (first line only).
    expect(ta.placeholder).not.toContain("You verify before you commit.");
  });

  // §9.4: the skills filter is the tri-state NameFilter — all → omit,
  // only → {only:[…]}, disabled → {disabled:[…]}, only-with-zero →
  // {enabled:false} (via ./filter serializeFilter).
  it("skills_filter_writes_tri_state", () => {
    const edits = mountAgent(DOC);
    const skills: Seg[] = [...AGENT, "skills"];

    // Universe comes from meta.skills — all three chips render.
    for (const s of ["research", "deploy", "review"]) {
      expect(
        q(`[data-testid='agent-skills'] [data-testid='chip-${s}']`),
      ).toBeDefined();
    }

    // → allowlist (mode switch clears the set; zero checked =
    // {enabled:false}).
    click(q("[data-testid='agent-skills'] [data-testid='mode-only']"));
    let e = lastWrite(edits, skills);
    expect(e, "skills written").toBeDefined();
    expect(e!.value).toEqual({ enabled: false });

    // → allowlist with one checked.
    click(q("[data-testid='agent-skills'] [data-testid='chip-research']"));
    e = lastWrite(edits, skills);
    expect(e!.value).toEqual({ only: ["research"] });

    // → blocklist (switch clears; zero checked = key omitted).
    click(q("[data-testid='agent-skills'] [data-testid='mode-disabled']"));
    e = lastWrite(edits, skills);
    expect(e, "skills rewritten").toBeDefined();
    expect(e!.value).toBeUndefined();

    // → blocklist with one checked.
    click(q("[data-testid='agent-skills'] [data-testid='chip-deploy']"));
    e = lastWrite(edits, skills);
    expect(e!.value).toEqual({ disabled: ["deploy"] });

    // → all: key omitted again.
    click(q("[data-testid='agent-skills'] [data-testid='mode-all']"));
    e = lastWrite(edits, skills);
    expect(e, "skills rewritten to all").toBeDefined();
    expect(e!.value).toBeUndefined();
  });

  // §9.4: the main model select writes the chosen alias.
  it("agent_model_select_writes_alias", () => {
    const edits = mountAgent(DOC);
    const sel = q<HTMLSelectElement>(
      "[data-testid='agent-model'] [data-testid='select']",
    );
    // Options come from the draft's providers[].models.
    const texts = [...sel.options].map((o) => o.value);
    expect(texts).toContain("sonnet");
    expect(texts).toContain("qwen");
    select(sel, "qwen");
    expect(lastWrite(edits, [...AGENT, "model"])?.value).toBe("qwen");
  });

  // §9.4: the preset chips write the mockup's texts (lines 748–752).
  it("persona_preset_writes_text", () => {
    const edits = mountAgent(DOC);
    const persona: Seg[] = [...AGENT, "persona"];

    click(q("[data-testid='agent-persona-chip-neutral']"));
    expect(lastWrite(edits, persona)?.value).toBe(
      "You are a coding agent working in the user's local filesystem. State what you are about to do in one line, do it, then report what changed. Prefer the smallest correct change.",
    );

    click(q("[data-testid='agent-persona-chip-terse']"));
    expect(lastWrite(edits, persona)?.value).toBe(
      "You are a senior engineer reviewing and writing code. Be direct and brief. No filler. Name the file and the line. If something is wrong, say so plainly.",
    );
  });
});
