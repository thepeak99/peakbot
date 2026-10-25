// RED (T20 D1a): ./store — per-scope draft store. Spec §9.0 "State model":
// one store per scope holding savedText/sha256 (from GET /api/config),
// draftDoc (yaml Document), rawText (non-null only after a Raw YAML edit),
// and the last validate response. Dirty =
//   stringify(draftDoc) !== stringify(parse(savedText))
//   || (rawText !== null && rawText !== savedText)
//
// Assumed API (see contract.ts): createStore(): Store with
//   current / setScope / state() / setSaved(scope, text, sha256) /
//   setDraft(scope, doc) / setRaw(scope, text|null) /
//   setValidate(scope, res) / discard(scope) / isDirty(scope)
//
// Expected RED: `./store` and `./yamlDoc` do not exist yet → import errors.

import { describe, expect, it } from "vitest";
import { createStore } from "./store";
import { parse, set } from "./yamlDoc";

const GLOBAL_TEXT = [
  "providers:",
  "  - name: patchnotes",
  "    type: openai",
  "profiles:",
  "  dev:",
  "    agents:",
  "      team:",
  "        model: sonnet",
].join("\n");

const REPO_TEXT = ["profiles:", "  local-dev:", "    agents:", "      main:", "        model: sonnet"].join(
  "\n",
);

describe("store", () => {
  it("dirty_on_off", () => {
    const store = createStore();
    store.setSaved("global", GLOBAL_TEXT, "sha-global-1");

    // Freshly loaded = clean.
    expect(store.isDirty("global")).toBe(false);

    // A form edit (draft doc differs from the saved doc) = dirty.
    const doc = store.state().draftDoc;
    set(doc, ["general", "cost_tracking"], false);
    store.setDraft("global", doc);
    expect(store.isDirty("global")).toBe(true);

    // Discard = clean again, draft back to the saved text.
    store.discard("global");
    expect(store.isDirty("global")).toBe(false);
    expect(store.state().draftDoc).toEqual(parse(GLOBAL_TEXT));

    // Raw YAML text that differs from saved = dirty, even with an
    // unedited draft doc.
    store.setRaw("global", GLOBAL_TEXT + "\ngeneral:\n  cost_tracking: false\n");
    expect(store.isDirty("global")).toBe(true);

    // Clearing the raw text falls back to the doc comparison. Since T31 a
    // valid Raw edit parsed back into the draft (contract: null keeps
    // draftDoc), so the draft still differs from saved = dirty ...
    store.setRaw("global", null);
    expect(store.isDirty("global")).toBe(true);
    // ... and discard returns to clean.
    store.discard("global");
    expect(store.isDirty("global")).toBe(false);
  });

  it("set_saved_replaces_draft_raw_and_validate", () => {
    const store = createStore();
    store.setSaved("global", GLOBAL_TEXT, "sha-1");
    const doc = store.state().draftDoc;
    set(doc, ["general"], { cost_tracking: false });
    store.setDraft("global", doc);
    store.setRaw("global", "something else");
    store.setValidate("global", { ok: true, restart_required: ["general.web"], warnings: [] });
    expect(store.isDirty("global")).toBe(true);

    // A reload (e.g. after the stale banner's Reload) resets everything.
    store.setSaved("global", GLOBAL_TEXT, "sha-2");
    expect(store.isDirty("global")).toBe(false);
    expect(store.state().sha256).toBe("sha-2");
    expect(store.state().rawText).toBeNull();
    expect(store.state().validate).toBeNull();
  });

  it("per_scope_isolation", () => {
    const store = createStore();
    store.setSaved("global", GLOBAL_TEXT, "sha-global-1");
    store.setSaved("repo", REPO_TEXT, "sha-repo-1");

    store.setScope("repo");
    expect(store.current).toBe("repo");
    expect(store.state().savedText).toBe(REPO_TEXT);
    expect(store.state().sha256).toBe("sha-repo-1");
    expect(store.isDirty("repo")).toBe(false);

    // Editing the repo draft never touches the global scope.
    const repoDoc = store.state().draftDoc;
    set(repoDoc, ["profiles", "local-dev", "memory"], { enabled: true });
    store.setDraft("repo", repoDoc);
    expect(store.isDirty("repo")).toBe(true);
    expect(store.isDirty("global")).toBe(false);

    // Switching scopes keeps each scope's state intact.
    store.setScope("global");
    expect(store.state().savedText).toBe(GLOBAL_TEXT);
    expect(store.isDirty("global")).toBe(false);
    expect(store.isDirty("repo")).toBe(true);

    // Validate results do not leak across scopes.
    store.setValidate("global", { ok: true, restart_required: ["mcps"], warnings: [] });
    store.setScope("repo");
    expect(store.state().validate).toBeNull();
    store.setScope("global");
    expect(store.state().validate).toEqual({
      ok: true,
      restart_required: ["mcps"],
      warnings: [],
    });

    // Discard in one scope leaves the other untouched.
    store.discard("repo");
    expect(store.isDirty("repo")).toBe(false);
    expect(store.isDirty("global")).toBe(false);
    expect(store.state().validate).toEqual({
      ok: true,
      restart_required: ["mcps"],
      warnings: [],
    });
  });

  it("missing_repo_file_starts_clean_and_empty", () => {
    const store = createStore();
    // exists: false → text "" and sha256 null (spec §8.2).
    store.setSaved("repo", "", null);
    expect(store.state().savedText).toBe("");
    expect(store.state().sha256).toBeNull();
    expect(store.isDirty("repo")).toBe(false);
  });
});
