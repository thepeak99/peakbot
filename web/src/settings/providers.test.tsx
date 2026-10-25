// T25 (RED): D2 "Providers & Models" pane (spec §9.8, plan T25). The
// ./providers module does not exist yet — this file must fail with a
// missing-module import error. The pane reads the draft via useSettingsCtx()
// and edits it through context.ts `edit(path, value)` by segment array.
// Props/testids: contract.ts ("./providers (T25)").

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProvidersPane } from "./providers";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import type { Seg } from "./types";

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

const DOC = `
providers:
  - name: local
    type: llamacpp
    base_url: http://127.0.0.1:8080
    api_key: pn-live-1
    models:
      - name: llama3.1:8b
        alias: llama
        max_tokens: 8192
`;

function mountPane(yaml: string): Edit[] {
  const edits: Edit[] = [];
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: null,
    doc: parse(yaml),
    globalDoc: parse(yaml),
    edit: (path, value) => {
      edits.push({ path, value });
    },
    errors: [],
    switchToGlobal: () => {},
  };
  mount(
    <SettingsCtx.Provider value={ctx}>
      <ProvidersPane />
    </SettingsCtx.Provider>,
  );
  return edits;
}

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

function typeInput(el: HTMLInputElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  set.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function typeTextarea(el: HTMLTextAreaElement, value: string) {
  const set = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    "value",
  )!.set!;
  set.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
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

function wrote(edits: Edit[], path: Seg[], value: unknown): boolean {
  return edits.some((e) => eqPath(e.path, path) && e.value === value);
}

describe("providers pane (T25)", () => {
  // §9.8: the type select auto-fills base_url from the type's preset.
  it("provider_type_autofills_base_url", () => {
    const presets: [string, string][] = [
      ["openai", "https://api.openai.com/v1"],
      ["anthropic", "https://api.anthropic.com"],
      ["openrouter", "https://openrouter.ai/api/v1"],
      ["ollama", "http://127.0.0.1:11434"],
      ["llamacpp", "http://127.0.0.1:8080"],
    ];
    for (const [type, url] of presets) {
      const edits = mountPane(DOC);
      select(q<HTMLSelectElement>('[data-testid="provider-type-0"]'), type);
      expect(
        wrote(edits, ["providers", 0, "type"], type),
        `type=${type} written`,
      ).toBe(true);
      expect(
        wrote(edits, ["providers", 0, "base_url"], url),
        `base_url=${url} auto-filled for ${type}`,
      ).toBe(true);
      unmount();
    }
  });

  // §9.8: extra_params is "a JSON textarea that must parse as an object".
  it("extra_params_must_be_object", () => {
    const edits = mountPane(DOC);
    const extraPath: Seg[] = ["providers", 0, "models", 0, "extra_params"];
    click(q('[data-testid="model-expand-0-0"]'));
    const ta = q<HTMLTextAreaElement>('[data-testid="model-extra-0-0"]');

    // A JSON array is not an object → inline error, nothing written.
    typeTextarea(ta, "[1, 2]");
    expect(
      q('[data-testid="model-extra-0-0-error"]').textContent,
    ).toMatch(/object/i);
    // A bare string is not an object.
    typeTextarea(ta, '"nucleus"');
    expect(
      q('[data-testid="model-extra-0-0-error"]').textContent,
    ).toMatch(/object/i);
    // Unparseable JSON.
    typeTextarea(ta, "nucleus: 0.9");
    expect(
      q('[data-testid="model-extra-0-0-error"]').textContent,
    ).toMatch(/object/i);
    expect(
      edits.some((e) => eqPath(e.path, extraPath)),
      "no extra_params write while invalid",
    ).toBe(false);

    // A valid object is written as a parsed object (not a string).
    typeTextarea(ta, '{"seed": 42, "nucleus": 0.9}');
    expect(host.querySelector('[data-testid="model-extra-0-0-error"]')).toBe(
      null,
    );
    const write = edits.find((e) => eqPath(e.path, extraPath));
    expect(write, "extra_params written once valid").toBeDefined();
    expect(write!.value).toEqual({ seed: 42, nucleus: 0.9 });
    expect(typeof write!.value).toBe("object");
  });

  // §9.8 / plan T25: the ollama column is `context_size` — the mockup's
  // `num_ctx` is not a ModelEntry field.
  it("context_field_writes_context_size_not_num_ctx", () => {
    const edits = mountPane(DOC);
    click(q('[data-testid="model-expand-0-0"]'));
    typeInput(
      q<HTMLInputElement>('[data-testid="model-context-0-0"] [data-testid="input"]'),
      "32768",
    );
    const ctxWrite = edits.find((e) =>
      eqPath(e.path, ["providers", 0, "models", 0, "context_size"]),
    );
    expect(ctxWrite, "context_size written").toBeDefined();
    expect(ctxWrite!.value).toBe(32768);
    expect(typeof ctxWrite!.value).toBe("number");
    expect(
      edits.some((e) => e.path.includes("num_ctx")),
      "no num_ctx path ever",
    ).toBe(false);
  });

  it("providers_pane_renders_cards_from_doc", () => {
    mountPane(DOC);
    const cards = host.querySelectorAll('[data-testid^="provider-card-"]');
    expect(cards.length).toBe(1);
    expect(q('[data-testid="provider-name-0"]').textContent).toContain("local");
    // The model row shows the wire name and the alias.
    const row = q('[data-testid="model-row-0-0"]');
    expect(row.textContent).toContain("llama3.1:8b");
    expect(row.textContent).toContain("llama");
    // api_key renders as a SecretInput — masked, secret not in the DOM.
    expect(
      q('[data-testid="provider-apikey-0"]').textContent,
    ).not.toContain("pn-live-1");

    // A doc without `providers:` renders no cards, but `+ Add provider`
    // appends an entry through edit(["providers"], …).
    unmount();
    const edits = mountPane("general:\n  cost_tracking: true\n");
    expect(host.querySelectorAll('[data-testid^="provider-card-"]').length).toBe(
      0,
    );
    click(q('[data-testid="provider-add"]'));
    const write = edits.find((e) => eqPath(e.path, ["providers"]));
    expect(write, "+ Add provider writes the providers list").toBeDefined();
    expect(Array.isArray(write!.value)).toBe(true);
    expect((write!.value as unknown[]).length).toBe(1);
  });
});
