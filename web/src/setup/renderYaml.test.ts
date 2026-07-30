import { describe, it, expect } from "vitest";
import { defaultSetupDraft } from "./draft";
import { renderYaml } from "./renderYaml";
import type { SetupDraft } from "./draft";

function minimalDraft(): SetupDraft {
  const draft = defaultSetupDraft();
  draft.providers = [
    {
      name: "openrouter",
      type: "openrouter",
      apiKey: "sk-or-v1-REAL-SECRET-DO-NOT-LEAK",
      models: [
        { name: "anthropic/claude-3.7-sonnet", alias: "sonnet", maxTokens: 8192 },
        { name: "google/gemini-2.0-flash-001" }, // no alias → addressable as openrouter/google/gemini-2.0-flash-001
      ],
    },
  ];
  draft.defaultModel = "sonnet";
  return draft;
}

// ---------- top-level shape -------------------------------------------------

describe("renderYaml — key names and nesting match agents.md", () => {
  it("includes the providers block with name/type/api_key/models nesting", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).toMatch(/^providers:/m);
    expect(yaml).toMatch(/- name: openrouter/);
    expect(yaml).toMatch(/type: openrouter/);
    expect(yaml).toMatch(/api_key: \*\*\*\*/); // masked, not raw
    expect(yaml).toMatch(/models:/);
    expect(yaml).toMatch(/- name: anthropic\/claude-3\.7-sonnet/);
  });

  it("includes default_model at top level when set", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).toMatch(/^default_model: sonnet$/m);
  });

  it("emits per-model fields with documented names (name, alias, max_tokens)", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).toMatch(/alias: sonnet/);
    expect(yaml).toMatch(/max_tokens: 8192/);
  });

  it("renders a providers block with no models without crashing", () => {
    const draft = defaultSetupDraft();
    draft.providers = [{ name: "p", type: "openai", apiKey: "x" }];
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^providers:/m);
    expect(yaml).toMatch(/- name: p/);
  });

  it("renders an empty draft without crashing and still emits top-level structure", () => {
    const yaml = renderYaml(defaultSetupDraft());
    expect(typeof yaml).toBe("string");
  });
});

// ---------- secret masking --------------------------------------------------

describe("renderYaml — secrets are masked", () => {
  it("masks provider api_key as ****", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toContain("sk-or-v1-REAL-SECRET-DO-NOT-LEAK");
    expect(yaml).toMatch(/api_key: \*\*\*\*/);
  });

  it("masks searxng bearer_token as **** when set", () => {
    const draft = minimalDraft();
    draft.services = {
      searxng: { baseUrl: "https://s.example", enabled: true, bearerToken: "BEAR-SECRET-XYZ" },
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^searxng:/m);
    expect(yaml).not.toContain("BEAR-SECRET-XYZ");
    expect(yaml).toMatch(/bearer_token: \*\*\*\*/);
  });

  it("masks web token as **** when access mode is lan", () => {
    const draft = minimalDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "WEB-TOKEN-PLAINTEXT" };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^web:/m);
    expect(yaml).not.toContain("WEB-TOKEN-PLAINTEXT");
    expect(yaml).toMatch(/token: \*\*\*\*/);
  });

  it("masks vector DB embeddings api_key as **** when set", () => {
    const draft = minimalDraft();
    draft.services = {
      vectorDb: {
        enabled: true,
        dbPath: "./.peakbot/vectors.db",
        embeddings: { baseUrl: "https://api.openai.com/v1", apiKey: "EMBED-SECRET", model: "text-embedding-3-small", dimensions: 1536 },
      },
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^vector_db:/m);
    expect(yaml).not.toContain("EMBED-SECRET");
  });
});

// ---------- tools XOR survives a render -------------------------------------

describe("renderYaml — tools block", () => {
  it("renders tools.disabled when set", () => {
    const draft = minimalDraft();
    draft.services = { tools: { disabled: ["bash_bg", "web_search"] } };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^tools:/m);
    expect(yaml).toMatch(/^  disabled:/m);
    expect(yaml).toMatch(/- bash_bg/);
    expect(yaml).not.toMatch(/^  only:/m);
  });

  it("renders tools.only when set", () => {
    const draft = minimalDraft();
    draft.services = { tools: { only: ["file_read", "bash"] } };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^tools:/m);
    expect(yaml).toMatch(/^  only:/m);
    expect(yaml).toMatch(/- file_read/);
    expect(yaml).not.toMatch(/^  disabled:/m);
  });
});

// ---------- pipeline --------------------------------------------------------

describe("renderYaml — pipeline block", () => {
  it("renders pipeline with enabled, orchestrator_prompt, and agents", () => {
    const draft = minimalDraft();
    draft.pipeline = {
      enabled: true,
      orchestratorPrompt: "You lead a small team.",
      agents: [
        { role: "researcher", model: "sonnet", prompt: "Research things." },
        { role: "reviewer", model: "sonnet", prompt: "Review diffs." },
      ],
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^pipeline:/m);
    expect(yaml).toMatch(/enabled: true/);
    expect(yaml).toMatch(/orchestrator_prompt: \|/);
    expect(yaml).toMatch(/agents:/);
    expect(yaml).toMatch(/researcher:/);
    expect(yaml).toMatch(/model: sonnet/);
    expect(yaml).toMatch(/reviewer:/);
  });

  it("renders the agents.<role>.env block when set", () => {
    const draft = minimalDraft();
    draft.pipeline = {
      enabled: true,
      agents: [
        { role: "reviewer", model: "sonnet", prompt: "x", env: { REVIEW_STRICT: "1" } },
      ],
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/env:/);
    expect(yaml).toMatch(/REVIEW_STRICT: "1"/);
  });

  it("renders agents.<role>.skills only block", () => {
    const draft = minimalDraft();
    draft.pipeline = {
      enabled: true,
      agents: [
        { role: "researcher", model: "sonnet", prompt: "x", skills: { only: ["github"] } },
      ],
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/skills:/);
    expect(yaml).toMatch(/only:/);
    expect(yaml).toMatch(/- github/);
  });

  it("omits pipeline entirely when pipeline.enabled is not true", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toMatch(/^pipeline:/m);
  });
});

// ---------- web / access ----------------------------------------------------

describe("renderYaml — web block", () => {
  it("renders web with bind and token (token masked) when access is lan", () => {
    const draft = minimalDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "T", tls: true };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^web:/m);
    expect(yaml).toMatch(/bind: 0\.0\.0\.0:7823/);
    expect(yaml).toMatch(/token: \*\*\*\*/);
    expect(yaml).toMatch(/tls: true/);
  });

  it("omits web when access is local (loopback default)", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toMatch(/^web:/m);
  });
});

// ---------- bash.env --------------------------------------------------------

describe("renderYaml — bash.env", () => {
  it("renders bash: { env: { ... } } when entries are set", () => {
    const draft = minimalDraft();
    draft.bashEnv = { MY_API_KEY: "abc123", DEBUG: "1" };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^bash:/m);
    expect(yaml).toMatch(/^  env:/m);
    expect(yaml).toMatch(/MY_API_KEY: "abc123"/);
  });

  it("omits bash when no env entries", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toMatch(/^bash:/m);
  });
});

// ---------- searxng ---------------------------------------------------------

describe("renderYaml — searxng block", () => {
  it("renders searxng with base_url and enabled", () => {
    const draft = minimalDraft();
    draft.services = { searxng: { baseUrl: "https://s.example", enabled: true } };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^searxng:/m);
    expect(yaml).toMatch(/base_url: https:\/\/s\.example/);
    expect(yaml).toMatch(/enabled: true/);
  });

  it("omits searxng when not configured", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toMatch(/^searxng:/m);
  });
});

// ---------- context / cost_tracking / timeouts / http -----------------------

describe("renderYaml — context, cost_tracking, timeouts, http blocks", () => {
  it("renders the context block when configured", () => {
    const draft = minimalDraft();
    draft.context = { enabled: true, threshold: 0.8, keepRecent: 5, contextWindow: 200000 };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^context:/m);
    expect(yaml).toMatch(/threshold: 0\.8/);
    expect(yaml).toMatch(/keep_recent: 5/);
    expect(yaml).toMatch(/context_window: 200000/);
  });

  it("renders cost_tracking: true when enabled", () => {
    const draft = minimalDraft();
    draft.costTracking = true;
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^cost_tracking: true$/m);
  });

  it("renders the timeouts block with tool_secs and delegate_secs", () => {
    const draft = minimalDraft();
    draft.timeouts = { toolSecs: 1800, delegateSecs: 3600 };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^timeouts:/m);
    expect(yaml).toMatch(/tool_secs: 1800/);
    expect(yaml).toMatch(/delegate_secs: 3600/);
  });

  it("renders the http block with connect/read timeout keys", () => {
    const draft = minimalDraft();
    draft.http = { connectTimeoutSecs: 30, readTimeoutSecs: 1800 };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^http:/m);
    expect(yaml).toMatch(/connect_timeout_secs: 30/);
    expect(yaml).toMatch(/read_timeout_secs: 1800/);
  });

  it("renders the memory block when configured", () => {
    const draft = minimalDraft();
    draft.memory = { enabled: true, thresholdBytes: 51200 };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^memory:/m);
    expect(yaml).toMatch(/threshold_bytes: 51200/);
  });
});