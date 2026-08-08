import { describe, it, expect } from "vitest";
import { configJsonToDraft, defaultSetupDraft } from "./draft";
import { renderYaml, personaText } from "./renderYaml";

// ---------- persona import (plan §A-Q7) -------------------------------------

describe("configJsonToDraft — persona", () => {
  it("maps a string `persona:` to { mode: 'custom', custom }", () => {
    const draft = configJsonToDraft({ persona: "You are a coding agent." });
    expect(personaText(draft)).toBe("You are a coding agent.");
    expect(draft.persona.mode).toBe("custom");
  });

  it("omits persona entirely when the file has none", () => {
    const draft = configJsonToDraft({});
    expect(personaText(draft)).toBeUndefined();
  });
});

// ---------- providers / models / default_model ------------------------------

describe("configJsonToDraft — providers", () => {
  it("maps snake_case keys and round-trips a model alias", () => {
    const draft = configJsonToDraft({
      providers: [{
        name: "openrouter",
        type: "openrouter",
        api_key: "sk-or",
        base_url: "https://openrouter.ai/api/v1",
        models: [{ name: "anthropic/claude-sonnet-4.5", alias: "sonnet", max_tokens: 8192 }],
      }],
      default_model: "sonnet",
    });
    expect(draft.providers).toHaveLength(1);
    expect(draft.providers[0].name).toBe("openrouter");
    expect(draft.providers[0].type).toBe("openrouter");
    expect(draft.providers[0].apiKey).toBe("sk-or");
    expect(draft.providers[0].baseUrl).toBe("https://openrouter.ai/api/v1");
    expect(draft.providers[0].models?.[0].alias).toBe("sonnet");
    expect(draft.providers[0].models?.[0].maxTokens).toBe(8192);
    expect(draft.defaultModel).toBe("sonnet");
  });
});

// ---------- unmanaged-key passthrough (plan §A-Q5 / §D-W4) -----------------

describe("configJsonToDraft — passthrough for unmanaged keys", () => {
  it("keeps mcp_servers, retry, and conversation in draft.passthrough", () => {
    const draft = configJsonToDraft({
      mcp_servers: [{ name: "github", command: "mcp-github" }],
      retry: { max_attempts: 3 },
      conversation: { max_messages: 50 },
    });
    expect(draft.passthrough.mcp_servers).toEqual([{ name: "github", command: "mcp-github" }]);
    expect(draft.passthrough.retry).toEqual({ max_attempts: 3 });
    expect(draft.passthrough.conversation).toEqual({ max_messages: 50 });
  });

  it("an unmanaged config round-trips JSON → draft → YAML with the blocks preserved", () => {
    const json = {
      providers: [{ name: "openrouter", type: "openrouter", api_key: "k", models: [{ name: "m", alias: "x" }] }],
      default_model: "x",
      mcp_servers: [{ name: "github", command: "mcp-github" }],
      retry: { max_attempts: 3 },
      conversation: { max_messages: 50 },
    };
    const draft = configJsonToDraft(json);
    const masked = renderYaml(draft);
    expect(masked).toMatch(/^mcp_servers:/m);
    expect(masked).toMatch(/- name: "github"/);
    expect(masked).toMatch(/^retry:/m);
    expect(masked).toMatch(/^conversation:/m);
  });

  it("an imported `pipelines:` list round-trips JSON → draft → YAML unchanged", () => {
    // The wizard does not own `pipelines`, so an existing team must survive
    // import and re-render byte-compatibly — including the nested member map,
    // the deepest structure passthrough has to carry.
    const draft = configJsonToDraft({
      providers: [{ name: "openrouter", type: "openrouter", api_key: "k", models: [{ name: "m", alias: "x" }] }],
      default_model: "x",
      pipelines: [
        {
          name: "review-team",
          orchestrator: { model: "x", prompt: "You lead." },
          agents: { reviewer: { model: "x", prompt: "Review diffs.", agents_md: true } },
        },
      ],
    });
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^pipelines:$/m);
    expect(yaml).toMatch(/^ {2}- name: "review-team"$/m);
    expect(yaml).toMatch(/^ {4}orchestrator:$/m);
    expect(yaml).toMatch(/^ {6}model: "x"$/m);
    expect(yaml).toMatch(/^ {6}prompt: "You lead\."$/m);
    expect(yaml).toMatch(/^ {4}agents:$/m);
    expect(yaml).toMatch(/^ {6}reviewer:$/m);
    expect(yaml).toMatch(/^ {8}agents_md: true$/m);
  });
});

// ---------- empty / malformed inputs ----------------------------------------

describe("configJsonToDraft — empty / malformed inputs", () => {
  it("returns a default draft for null", () => {
    expect(configJsonToDraft(null).providers).toEqual([]);
  });

  it("returns a default draft for an array (a config is a map)", () => {
    expect(configJsonToDraft([]).providers).toEqual([]);
  });

  it("drops providers entries that are not objects", () => {
    const draft = configJsonToDraft({ providers: [null, "x", { name: "ok", type: "openai" }] });
    expect(draft.providers).toHaveLength(1);
    expect(draft.providers[0].name).toBe("ok");
  });
});

// ---------- smoke: import -> render -> write body is the unmasked text -----

describe("configJsonToDraft — unmasked write body round-trip", () => {
  it("the unmasked body carries the real api_key and the persona text", () => {
    const draft = configJsonToDraft({ providers: [{ name: "p", type: "openai", api_key: "SECRET-1" }], persona: "Be terse." });
    const body = renderYaml(draft, { mask: false });
    expect(body).toContain("SECRET-1");
    expect(body).toMatch(/persona: \|2-/);
    expect(body).toContain("Be terse.");
  });
});

describe("defaultSetupDraft — passthrough is empty by default", () => {
  it("starts with no passthrough keys", () => {
    expect(defaultSetupDraft().passthrough).toEqual({});
  });
});
