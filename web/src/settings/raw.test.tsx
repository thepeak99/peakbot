// T31 — Raw YAML pane (D9). Written BEFORE the code (TDD).
//
// Spec: make-config-great-again.md §9.0 "State model" + §9.11; contract:
// ./contract.ts ("./raw (T31) + store additions").
//   - raw_shows_saved_text_initially  — Raw shows the saved bytes verbatim.
//   - raw_edit_reflects_in_form       — Raw edits parse back into the draft
//     (store level AND through the shell into the General form).
//   - syntax_error_blocks_save        — inline `line L, col C: …`, Save
//     disabled and never PUTs.
//   - form_edit_after_raw_shows_banner — the comment-loss banner.
// Plus store-level rules for rawError / rewriteWarning and SaveBar's
// `saveBlocked`.
//
// Harness: plain createRoot/act, jsdom, stubbed fetch (project convention).
//
// Expected RED: `./raw` does not exist yet → import error (whole file);
// store/SaveBar/banner assertions fail on the missing behaviour.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { RawPane } from "./raw";
import { Settings } from "./Settings";
import { SaveBar } from "./SaveBar";
import { createStore } from "./store";
import { get } from "./yamlDoc";
import type { ScopeState } from "./store";
import type { RawError, ScopeStateRawExt } from "./contract";
import type { SettingsMeta } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | undefined;
let host: HTMLElement;

afterEach(() => {
  if (root) {
    act(() => root!.unmount());
    host.remove();
    root = undefined;
  }
  vi.unstubAllGlobals();
});

type Ext = ScopeState & ScopeStateRawExt;
const ext = (s: ScopeState) => s as Ext;

const BANNER_COPY =
  "Form edits rewrite the file in normalized YAML — comments and anchors are lost.";

const COMMENTED = `# my notes — hand written
general:
  web:
    bind: 127.0.0.1:7823 # local only
`;

const PLAIN = `general:
  web:
    bind: 127.0.0.1:7823
`;

// ── helpers ─────────────────────────────────────────────────────────────
function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/home/u/.config/peakbot/config.yaml",
    repo_path: "/work/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "",
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
  };
}

interface Call {
  url: string;
  method: string;
  body: unknown;
}

function stubShellFetch(configText: string): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input.toString();
      const method = (init?.method ?? "GET").toUpperCase();
      let body: unknown = undefined;
      if (typeof init?.body === "string") {
        try {
          body = JSON.parse(init.body);
        } catch {
          body = init.body;
        }
      }
      calls.push({ url, method, body });
      const path = url.split("?")[0];
      const ok = (json: unknown) =>
        new Response(JSON.stringify(json), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      if (method === "GET" && path === "/api/settings") return ok(meta());
      if (method === "GET" && path === "/api/config")
        return ok({
          scope: "global",
          path: "/home/u/.config/peakbot/config.yaml",
          exists: true,
          text: configText,
          sha256: "sha-1",
        });
      if (method === "POST" && path === "/api/config/validate")
        return ok({ ok: true, restart_required: [], warnings: [] });
      if (method === "PUT" && path === "/api/config")
        return ok({
          path: "/home/u/.config/peakbot/config.yaml",
          backup: null,
          sha256: "sha-2",
          restart_required: [],
          applies_on_next_new: true,
        });
      return new Response(JSON.stringify({ error: `no route ${method} ${path}` }), { status: 404 });
    }),
  );
  return calls;
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    await act(async () => {
      await Promise.resolve();
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

function q<T extends Element = HTMLElement>(id: string): T {
  const el = host.querySelector(`[data-testid='${id}']`);
  if (!el) throw new Error(`missing testid: ${id}`);
  return el as T;
}
const has = (id: string) => host.querySelector(`[data-testid='${id}']`) !== null;

function click(el: Element) {
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
}

function typeText(el: HTMLTextAreaElement | HTMLInputElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement : HTMLInputElement;
  Object.getOwnPropertyDescriptor(proto.prototype, "value")!.set!.call(el, value);
  act(() => {
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** Mount the Settings shell with `configText` as the global file. */
async function mountShell(configText: string): Promise<Call[]> {
  const calls = stubShellFetch(configText);
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<Settings convo={null} onClose={() => {}} />));
  await flush();
  return calls;
}

async function openRaw() {
  click(q("nav-raw"));
  await flush();
}
async function openGeneral() {
  click(q("nav-general"));
  await flush();
}
function rawBox(): HTMLTextAreaElement {
  return q<HTMLTextAreaElement>("raw-text");
}
function ttlInput(): HTMLInputElement {
  return q("general-ttl").querySelector("[data-testid='input']") as HTMLInputElement;
}

// ── store level ─────────────────────────────────────────────────────────
describe("store: Raw edits parse back into the draft", () => {
  it("raw_edit_reflects_in_draft_doc", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");
    expect(get(store.state().draftDoc, ["general", "web", "bind"])).toBe("127.0.0.1:7823");

    store.setRaw("global", "general:\n  web:\n    bind: 0.0.0.0:9000\n    session_ttl_secs: 60\n");

    const st = store.state();
    expect(get(st.draftDoc, ["general", "web", "bind"])).toBe("0.0.0.0:9000");
    expect(get(st.draftDoc, ["general", "web", "session_ttl_secs"])).toBe(60);
    expect(ext(st).rawError).toBeNull();
    expect(st.rawText).toContain("0.0.0.0:9000");
    expect(store.isDirty("global")).toBe(true);
  });

  it("raw_edit_back_to_saved_text_is_clean_again", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");
    store.setRaw("global", PLAIN + "# x\n");
    expect(store.isDirty("global")).toBe(true);
    store.setRaw("global", PLAIN);
    expect(store.isDirty("global")).toBe(false);
  });

  it("raw_syntax_error_sets_raw_error_and_keeps_last_good_draft", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");

    store.setRaw("global", "a: b: c\n");

    const st = store.state();
    const err = ext(st).rawError as RawError;
    expect(err).not.toBeNull();
    expect(err.line).toBe(1);
    expect(err.col).toBeGreaterThanOrEqual(1);
    expect(err.message.length).toBeGreaterThan(0);
    // Draft is the last good parse; the broken text is still the raw text.
    expect(get(st.draftDoc, ["general", "web", "bind"])).toBe("127.0.0.1:7823");
    expect(st.rawText).toBe("a: b: c\n");
    expect(store.isDirty("global")).toBe(true);

    // Fixing the text clears the error.
    store.setRaw("global", "general:\n  web:\n    bind: 1.2.3.4:5\n");
    expect(ext(store.state()).rawError).toBeNull();
    expect(get(store.state().draftDoc, ["general", "web", "bind"])).toBe("1.2.3.4:5");
  });

  it("raw_error_reports_line_of_the_problem", () => {
    const store = createStore();
    store.setSaved("global", "", null);
    store.setRaw("global", "general:\n  ok: 1\nbad: a: b\n");
    const err = ext(store.state()).rawError as RawError;
    expect(err.line).toBe(3);
  });

  it("raw_cleared_with_null_drops_rawText_and_error", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");
    store.setRaw("global", "a: b: c\n");
    store.setRaw("global", null);
    expect(store.state().rawText).toBeNull();
    expect(ext(store.state()).rawError).toBeNull();
  });

  it("form_edit_and_discard_and_setSaved_clear_raw_error", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");

    store.setRaw("global", "a: b: c\n");
    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rawError).toBeNull();
    expect(store.state().rawText).toBeNull();

    store.setRaw("global", "a: b: c\n");
    store.discard("global");
    expect(ext(store.state()).rawError).toBeNull();

    store.setRaw("global", "a: b: c\n");
    store.setSaved("global", PLAIN, "sha-2");
    expect(ext(store.state()).rawError).toBeNull();
  });

  it("raw_error_is_per_scope", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "s");
    store.setSaved("repo", "", null);
    store.setRaw("global", "a: b: c\n");
    expect(ext(store.state()).rawError).not.toBeNull();
    store.setScope("repo");
    expect(ext(store.state()).rawError).toBeNull();
  });
});

describe("store: rewriteWarning (comment-loss banner flag)", () => {
  it("form_edit_after_raw_edit_sets_rewrite_warning", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");
    store.setRaw("global", "general:\n  web:\n    bind: 0.0.0.0:9\n");
    // A raw edit alone never warns.
    expect(ext(store.state()).rewriteWarning).toBe(false);

    store.setDraft("global", store.state().draftDoc);

    expect(ext(store.state()).rewriteWarning).toBe(true);
    expect(store.state().rawText).toBeNull();
  });

  it("first_form_edit_on_commented_file_sets_rewrite_warning", () => {
    const store = createStore();
    store.setSaved("global", COMMENTED, "sha-1");
    expect(ext(store.state()).rewriteWarning).toBe(false);
    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rewriteWarning).toBe(true);
  });

  it("form_edit_on_comment_free_file_without_raw_does_not_warn", () => {
    const store = createStore();
    store.setSaved("global", PLAIN, "sha-1");
    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rewriteWarning).toBe(false);
  });

  it("hash_inside_a_quoted_value_is_not_a_comment", () => {
    const store = createStore();
    store.setSaved("global", 'general:\n  web:\n    token: "ab#cd"\n', "sha-1");
    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rewriteWarning).toBe(false);
  });

  it("discard_and_setSaved_reset_rewrite_warning", () => {
    const store = createStore();
    store.setSaved("global", COMMENTED, "sha-1");
    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rewriteWarning).toBe(true);

    store.discard("global");
    expect(ext(store.state()).rewriteWarning).toBe(false);

    store.setDraft("global", store.state().draftDoc);
    expect(ext(store.state()).rewriteWarning).toBe(true);
    store.setSaved("global", PLAIN, "sha-2");
    expect(ext(store.state()).rewriteWarning).toBe(false);
  });

  it("rewrite_warning_is_per_scope", () => {
    const store = createStore();
    store.setSaved("global", COMMENTED, "g");
    store.setSaved("repo", "", null);
    store.setDraft("global", store.state().draftDoc);
    store.setScope("repo");
    expect(ext(store.state()).rewriteWarning).toBe(false);
  });
});

// ── RawPane (controlled component) ──────────────────────────────────────
describe("RawPane", () => {
  function mountRaw(over: Partial<ComponentProps<typeof RawPane>> = {}) {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    const props = {
      scope: "global" as const,
      text: COMMENTED,
      error: null,
      onChange: vi.fn(),
      onBlur: vi.fn(),
      ...over,
    };
    act(() => root!.render(<RawPane {...props} />));
    return props;
  }

  it("renders_text_verbatim_in_a_textarea_with_the_banner", () => {
    mountRaw();
    expect(has("raw-pane")).toBe(true);
    expect(rawBox().tagName).toBe("TEXTAREA");
    expect(rawBox().value).toBe(COMMENTED);
    expect(q("raw-banner").textContent).toContain("unknown keys are rejected at boot");
    expect(q("raw-banner").textContent).not.toContain("only contain profiles:");
    expect(has("raw-error")).toBe(false);
  });

  it("repo_scope_banner_adds_profiles_only_note", () => {
    mountRaw({ scope: "repo", text: "" });
    expect(q("raw-banner").textContent).toContain("only contain profiles:");
  });

  it("typing_calls_onChange_and_blur_calls_onBlur", () => {
    const p = mountRaw({ text: "" });
    typeText(rawBox(), "general:\n  web: {}\n");
    expect(p.onChange).toHaveBeenCalledTimes(1);
    expect(p.onChange).toHaveBeenCalledWith("general:\n  web: {}\n");
    act(() => {
      rawBox().dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
      rawBox().dispatchEvent(new FocusEvent("blur"));
    });
    expect(p.onBlur).toHaveBeenCalled();
  });

  it("shows_inline_error_as_line_col_message", () => {
    mountRaw({ error: { line: 3, col: 7, message: "Nested mappings are not allowed" } });
    expect(q("raw-error").textContent).toBe("line 3, col 7: Nested mappings are not allowed");
  });

  it("keeps_empty_text_and_unicode_exact", () => {
    mountRaw({ text: "" });
    expect(rawBox().value).toBe("");
    act(() => root!.unmount());
    host.remove();
    root = undefined;
    mountRaw({ text: "# ünïcödé ✓ 日本語\nname: «x»\n" });
    expect(rawBox().value).toBe("# ünïcödé ✓ 日本語\nname: «x»\n");
  });
});

// ── SaveBar: saveBlocked ────────────────────────────────────────────────
describe("SaveBar saveBlocked", () => {
  it("savebar_save_disabled_when_blocked", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    const onSave = vi.fn();
    const base = {
      scope: "global" as const,
      dirty: true,
      draftVersion: 1,
      restartRequired: [] as string[],
      saving: false,
      stale: false,
      staleMessage: null,
      onValidate: vi.fn(),
      onSave,
      onDiscard: vi.fn(),
      onReload: vi.fn(),
      onViewDiff: vi.fn(),
    };
    // `saveBlocked` is the new T31 prop (contract.ts) — not yet in SaveBar's
    // real prop type, hence the spread through an untyped object.
    const blocked = { saveBlocked: true } as object;
    act(() => root!.render(<SaveBar {...base} {...blocked} />));
    const save = q<HTMLButtonElement>("savebar-save");
    expect(save.disabled).toBe(true);
    click(save);
    expect(onSave).not.toHaveBeenCalled();

    const unblocked = { saveBlocked: false } as object;
    act(() => root!.render(<SaveBar {...base} {...unblocked} />));
    expect(q<HTMLButtonElement>("savebar-save").disabled).toBe(false);
  });
});

// ── Shell integration ───────────────────────────────────────────────────
describe("Raw YAML through the Settings shell", () => {
  it("raw_shows_saved_text_initially", async () => {
    await mountShell(COMMENTED);
    await openRaw();

    // Verbatim saved bytes: comments, order and trailing newline intact.
    expect(rawBox().value).toBe(COMMENTED);
    expect(has("raw-error")).toBe(false);
    // Clean: nothing to save.
    expect(has("savebar")).toBe(false);
  });

  it("raw_shows_normalized_draft_after_a_form_edit", async () => {
    await mountShell(PLAIN);
    await openGeneral();
    typeText(ttlInput(), "99");
    await openRaw();

    expect(rawBox().value).toContain("session_ttl_secs: 99");
    expect(rawBox().value).toContain("bind: 127.0.0.1:7823");
  });

  it("raw_edit_reflects_in_form", async () => {
    await mountShell(PLAIN);
    await openRaw();

    typeText(rawBox(), "general:\n  web:\n    bind: 127.0.0.1:7823\n    session_ttl_secs: 4242\n");
    expect(has("raw-error")).toBe(false);
    expect(has("savebar")).toBe(true); // a raw edit marks the scope dirty

    await openGeneral();
    expect(ttlInput().value).toBe("4242");
  });

  it("raw_edit_is_what_gets_saved_byte_for_byte", async () => {
    const calls = await mountShell(COMMENTED);
    await openRaw();
    const edited = COMMENTED + "# appended in raw\n";
    typeText(rawBox(), edited);

    click(q("savebar-save"));
    await flush();

    const puts = calls.filter((c) => c.method === "PUT");
    expect(puts).toHaveLength(1);
    const body = puts[0].body as { text: string; base_sha256: string | null; scope: string };
    expect(body.text).toBe(edited);
    expect(body.base_sha256).toBe("sha-1");
    expect(body.scope).toBe("global");
  });

  it("syntax_error_blocks_save", async () => {
    const calls = await mountShell(PLAIN);
    await openRaw();

    typeText(rawBox(), "general:\n  web: [a\nfoo: bar\n");

    expect(q("raw-error").textContent).toMatch(/^line \d+, col \d+: \S+/);
    const save = q<HTMLButtonElement>("savebar-save");
    expect(save.disabled).toBe(true);

    click(save);
    await flush();
    expect(calls.filter((c) => c.method === "PUT")).toHaveLength(0);

    // Broken text is never sent to /api/config/validate either (debounce is
    // 400 ms — wait it out).
    await act(async () => {
      await new Promise((r) => setTimeout(r, 600));
    });
    const bad = calls.filter(
      (c) =>
        c.method === "POST" &&
        c.url.startsWith("/api/config/validate") &&
        (c.body as { text?: string }).text?.includes("[a"),
    );
    expect(bad).toHaveLength(0);
  });

  it("syntax_error_clears_and_save_reenables_when_fixed", async () => {
    await mountShell(PLAIN);
    await openRaw();
    typeText(rawBox(), "a: b: c\n");
    expect(has("raw-error")).toBe(true);
    expect(q<HTMLButtonElement>("savebar-save").disabled).toBe(true);

    typeText(rawBox(), "general:\n  web:\n    bind: 127.0.0.1:1\n");

    expect(has("raw-error")).toBe(false);
    expect(q<HTMLButtonElement>("savebar-save").disabled).toBe(false);
  });

  it("form_edit_after_raw_shows_banner", async () => {
    await mountShell(PLAIN);
    await openRaw();
    typeText(rawBox(), "general:\n  web:\n    bind: 127.0.0.1:7823\n    session_ttl_secs: 5\n");
    expect(has("rewrite-banner")).toBe(false); // raw edit alone: no banner

    await openGeneral();
    expect(has("rewrite-banner")).toBe(false); // still just looking
    typeText(ttlInput(), "6");

    expect(q("rewrite-banner").textContent).toContain(BANNER_COPY);

    // The Raw pane itself does not show the form-rewrite banner.
    await openRaw();
    expect(has("rewrite-banner")).toBe(false);
  });

  it("first_form_edit_on_commented_file_shows_banner", async () => {
    await mountShell(COMMENTED);
    await openGeneral();
    expect(has("rewrite-banner")).toBe(false);

    typeText(ttlInput(), "10");

    expect(q("rewrite-banner").textContent).toContain(BANNER_COPY);
  });

  it("form_edit_on_comment_free_file_shows_no_banner", async () => {
    await mountShell(PLAIN);
    await openGeneral();
    typeText(ttlInput(), "10");
    expect(has("rewrite-banner")).toBe(false);
  });

  it("discard_removes_the_banner", async () => {
    await mountShell(COMMENTED);
    await openGeneral();
    typeText(ttlInput(), "10");
    expect(has("rewrite-banner")).toBe(true);

    click(q("savebar-discard"));
    await flush();

    expect(has("rewrite-banner")).toBe(false);
  });
});
