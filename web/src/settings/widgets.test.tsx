// T24 (RED): form widgets Field/Toggle/KvRows/ChipRow/Tri/FilterBlock/
// SecretInput/ModelSelect/Crumbs/Badge (spec §9.6 red fields, §9.8/§9.9
// widgets, plan T24). The ./widgets module does not exist yet — this file
// must fail with a missing-module import error. Props and data-testids are
// the contract in contract.ts ("./widgets (T24)").

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  Badge,
  ChipRow,
  Crumbs,
  Field,
  FilterBlock,
  KvRows,
  ModelSelect,
  SecretInput,
  Toggle,
  Tri,
} from "./widgets";

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

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function q<T extends Element = HTMLElement>(sel: string): T {
  const el = host.querySelector(sel);
  if (!el) throw new Error(`missing element: ${sel}`);
  return el as T;
}

/** Fire a native value change the way a user typing does (React-safe). */
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

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function pressEnter(el: HTMLInputElement) {
  act(() => {
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
    );
  });
}

describe("widgets", () => {
  it("number_field_writes_number", () => {
    const edits: (string | number | undefined)[] = [];
    mount(
      <Field
        testid="f"
        label="Max tokens"
        type="number"
        value={4096}
        onChange={(v) => edits.push(v)}
      />,
    );
    const input = q<HTMLInputElement>('[data-testid="f"] [data-testid="input"]');
    expect(input.value).toBe("4096");
    typeInput(input, "8192");
    expect(edits).toHaveLength(1);
    expect(edits[0]).toBe(8192);
    expect(typeof edits[0]).toBe("number"); // never a string
  });

  it("empty_number_removes_key", () => {
    const edits: (string | number | undefined)[] = [];
    mount(
      <Field
        testid="f"
        label="Max tokens"
        type="number"
        value={4096}
        onChange={(v) => edits.push(v)}
      />,
    );
    typeInput(
      q<HTMLInputElement>('[data-testid="f"] [data-testid="input"]'),
      "",
    );
    expect(edits[edits.length - 1]).toBeUndefined(); // key removed, not 0/null
  });

  it("text_field_writes_string", () => {
    const edits: (string | number | undefined)[] = [];
    mount(
      <Field
        testid="f"
        label="Name"
        value="local"
        onChange={(v) => edits.push(v)}
      />,
    );
    typeInput(
      q<HTMLInputElement>('[data-testid="f"] [data-testid="input"]'),
      "patchnotes",
    );
    expect(edits[edits.length - 1]).toBe("patchnotes");
  });

  it("field_invalid_renders_error", () => {
    mount(
      <Field
        testid="f"
        label="Alias"
        value="gpt5"
        onChange={() => {}}
        invalid="Unknown model alias gpt5 — declared: sonnet, opus"
      />,
    );
    expect(q('[data-testid="f"] [data-testid="error"]').textContent).toBe(
      "Unknown model alias gpt5 — declared: sonnet, opus",
    );
  });

  it("toggle_reports_change", () => {
    const edits: boolean[] = [];
    mount(
      <Toggle
        testid="t"
        label="Enabled"
        checked
        onChange={(c) => edits.push(c)}
      />,
    );
    click(q('[data-testid="t"] [data-testid="input"]'));
    expect(edits).toEqual([false]);
  });

  it("chiprow_add_and_remove", () => {
    const edits: string[][] = [];
    mount(
      <ChipRow
        testid="c"
        label="args"
        items={["a"]}
        onChange={(items) => edits.push(items)}
      />,
    );
    const addInput = q<HTMLInputElement>(
      '[data-testid="c"] [data-testid="add-input"]',
    );
    typeInput(addInput, "b");
    pressEnter(addInput);
    expect(edits[edits.length - 1]).toEqual(["a", "b"]);
    // Duplicate is ignored (no new emit).
    typeInput(addInput, "b");
    pressEnter(addInput);
    expect(edits[edits.length - 1]).toEqual(["a", "b"]);
    // ✕ removes.
    click(q('[data-testid="c"] [data-testid="chip-a-remove"]'));
    expect(edits[edits.length - 1]).toEqual(["b"]);
  });

  it("kvrows_edit_add_remove", () => {
    const edits: { key: string; value: string }[][] = [];
    mount(
      <KvRows
        testid="k"
        label="env"
        rows={[{ key: "GIT_PAGER", value: "cat" }]}
        onChange={(rows) => edits.push(rows)}
      />,
    );
    typeInput(
      q<HTMLInputElement>('[data-testid="k"] [data-testid="row-0-value"]'),
      "less",
    );
    expect(edits[edits.length - 1]).toEqual([
      { key: "GIT_PAGER", value: "less" },
    ]);
    click(q('[data-testid="k"] [data-testid="add"]'));
    const rows = edits[edits.length - 1];
    expect(rows).toHaveLength(2);
    click(q('[data-testid="k"] [data-testid="row-0-remove"]'));
    expect(edits[edits.length - 1]).toEqual([rows[1]]);
  });

  it("tri_reports_state", () => {
    const edits: ("auto" | "on" | "off")[] = [];
    mount(
      <Tri
        testid="v"
        label="vision"
        value="auto"
        onChange={(v) => edits.push(v)}
      />,
    );
    click(q('[data-testid="v"] [data-testid="on"]'));
    expect(edits[edits.length - 1]).toBe("on");
  });

  it("filterblock_emits_state_on_mode_and_check", () => {
    const edits: { mode: string; names: string[] }[] = [];
    mount(
      <FilterBlock
        testid="fb"
        label="Tools"
        universe={["bash", "think"]}
        value={{ mode: "all", names: [] }}
        onChange={(v) => edits.push(v)}
      />,
    );
    click(q('[data-testid="fb"] [data-testid="mode-only"]'));
    // Mode switch clears the checked set (none were checked — stays []).
    expect(edits[edits.length - 1]).toEqual({ mode: "only", names: [] });
    click(q('[data-testid="fb"] [data-testid="chip-bash"]'));
    expect(edits[edits.length - 1]).toEqual({ mode: "only", names: ["bash"] });
    click(q('[data-testid="fb"] [data-testid="mode-disabled"]'));
    // Switching from an allowlist with checks clears them.
    expect(edits[edits.length - 1]).toEqual({ mode: "disabled", names: [] });
  });

  it("secret_input_masks_and_reveals", () => {
    const secret = "pn-live-9f3e5a7c1b2d4e6f";
    const edits: (string | undefined)[] = [];
    mount(
      <SecretInput
        testid="s"
        label="API key"
        value={secret}
        onChange={(v) => edits.push(v)}
      />,
    );
    // Masked by default: dots in the DOM, the secret itself is not.
    const masked = q('[data-testid="s"] [data-testid="value"]');
    expect(masked.textContent).not.toContain(secret);
    expect(masked.textContent).toMatch(/•/);
    // Reveal: a text input holding the secret appears.
    click(q('[data-testid="s"] [data-testid="reveal"]'));
    const revealed = q<HTMLInputElement>(
      '[data-testid="s"] [data-testid="value"]',
    );
    expect(revealed.value).toBe(secret);
    // Editing the revealed input calls onChange with the new value.
    typeInput(revealed, "pn-live-rotated");
    expect(edits[edits.length - 1]).toBe("pn-live-rotated");
  });

  it("modelselect_reports_alias", () => {
    const edits: string[] = [];
    mount(
      <ModelSelect
        testid="m"
        label="Model"
        options={[
          { alias: "sonnet", qualified: "patchnotes/anthropic/claude-sonnet-4.5" },
          { alias: "llama", qualified: "local/llama3.1:8b" },
        ]}
        value="sonnet"
        onChange={(a) => edits.push(a)}
      />,
    );
    const select = q<HTMLSelectElement>(
      '[data-testid="m"] [data-testid="select"]',
    );
    const set = Object.getOwnPropertyDescriptor(
      HTMLSelectElement.prototype,
      "value",
    )!.set!;
    set.call(select, "llama");
    act(() => {
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(edits).toEqual(["llama"]);
  });

  it("crumbs_navigate", () => {
    const navs: number[] = [];
    mount(
      <Crumbs
        testid="b"
        items={["Profiles", "dev"]}
        onNavigate={(i) => navs.push(i)}
      />,
    );
    expect(q('[data-testid="b"]').textContent).toContain("dev");
    click(q('[data-testid="b"] [data-testid="crumb-0"]'));
    expect(navs).toEqual([0]);
  });

  it("badge_renders_text_and_tone", () => {
    mount(
      <Badge testid="bd" tone="green">
        connected
      </Badge>,
    );
    expect(q('[data-testid="bd"]').textContent).toBe("connected");
  });
});
