// RED (T20 D1a): ./yamlDoc — thin wrapper over `yaml` (parse/stringify/
// get/set by SEGMENT ARRAY). Spec §9.0: "Field paths are segment arrays
// everywhere … yamlDoc get/set take segment arrays; nothing splits on `.`."
//
// Assumed API (see contract.ts):
//   parse(text: string): Doc                    (yaml.parseDocument)
//   stringify(doc: Doc): string                 (doc.toString())
//   get(doc: Doc, path: Seg[]): unknown         ([] = whole doc value)
//   set(doc: Doc, path: Seg[], value: unknown)  (mutates; undefined REMOVES)
//
// Expected RED: `./yamlDoc` does not exist yet → import error.

import { describe, expect, it } from "vitest";
import { parse, set, get, stringify } from "./yamlDoc";

const SAMPLE = [
  "# peakbot config",
  "providers:",
  "  - name: patchnotes",
  "    type: openai",
  "    base_url: https://ai.patchnotes.com/v1",
  "    models:",
  "      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }",
  "profiles:",
  "  dev:",
  "    agents:",
  "      team:",
  "        model: sonnet",
  "general:",
  "  cost_tracking: true",
].join("\n");

describe("yamlDoc", () => {
  it("getset_preserves_order", () => {
    const doc = parse(SAMPLE);

    // Reads by segment array, incl. numeric indices into lists.
    expect(get(doc, ["providers", 0, "name"])).toBe("patchnotes");
    expect(get(doc, ["providers", 0, "models", 0, "alias"])).toBe("sonnet");
    expect(get(doc, ["profiles", "dev", "agents", "team", "model"])).toBe("sonnet");
    // Whole doc for the empty path.
    expect(get(doc, [])).toEqual({
      providers: [
        {
          name: "patchnotes",
          type: "openai",
          base_url: "https://ai.patchnotes.com/v1",
          models: [{ name: "anthropic/claude-sonnet-4.5", alias: "sonnet", max_tokens: 8192 }],
        },
      ],
      profiles: { dev: { agents: { team: { model: "sonnet" } } } },
      general: { cost_tracking: true },
    });

    // A new nested key is appended, existing top-level order is untouched.
    set(doc, ["general", "agent_max_turns"], 50);
    expect(get(doc, ["general", "agent_max_turns"])).toBe(50);
    const out = stringify(doc);
    const p = out.indexOf("providers:");
    const pr = out.indexOf("profiles:");
    const g = out.indexOf("general:");
    expect(p).toBeGreaterThanOrEqual(0);
    expect(pr).toBeGreaterThan(p);
    expect(g).toBeGreaterThan(pr);

    // A brand-new top-level key goes to the END (order = insertion order).
    set(doc, ["mcps"], []);
    const out2 = stringify(doc);
    expect(out2.indexOf("mcps:")).toBeGreaterThan(out2.indexOf("general:"));
  });

  it("set_undefined_removes_key", () => {
    const doc = parse(SAMPLE);

    // Top-level key.
    set(doc, ["general"], undefined);
    const out = stringify(doc);
    expect(out).not.toContain("general");
    expect(out).not.toContain("cost_tracking");
    expect(get(doc, ["general"])).toBeUndefined();

    // Nested key.
    set(doc, ["profiles", "dev", "agents", "team", "model"], undefined);
    const out2 = stringify(doc);
    expect(out2).not.toContain("model:");
    expect(get(doc, ["profiles", "dev", "agents", "team", "model"])).toBeUndefined();

    // Removing a key that is absent is a no-op (no throw).
    set(doc, ["general", "nope"], undefined);
    expect(get(doc, ["general"])).toBeUndefined();
  });

  it("comments_survive_set", () => {
    const doc = parse(SAMPLE);
    set(doc, ["general", "agent_max_turns"], 50);
    set(doc, ["profiles", "dev", "memory"], { enabled: true, threshold_bytes: 51200 });
    const out = stringify(doc);
    expect(out).toContain("# peakbot config");
    expect(out).toContain("agent_max_turns: 50");
  });

  it("get_missing_path_is_undefined", () => {
    const doc = parse(SAMPLE);
    expect(get(doc, ["profiles", "nope", "agents"])).toBeUndefined();
    expect(get(doc, ["providers", 99, "name"])).toBeUndefined();
  });
});
