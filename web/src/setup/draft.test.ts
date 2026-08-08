import { describe, it, expect } from "vitest";
import {
  ALIAS_PATTERN,
  DEFAULT_PIPELINE_NAME,
  RESERVED_ALIAS,
  classifyChange,
  collectAliases,
  configJsonToDraft,
  defaultSetupDraft,
  importedPipelines,
  pipelineName,
  validateAccess,
  validateDraft,
  validateModels,
  validateMultiAgent,
  validateServices,
} from "./draft";
import type { PipelineDraft } from "./draft";

// ---------- helpers ---------------------------------------------------------

/** Build a minimal valid one-provider / one-model draft. */
function oneModelDraft(overrides: {
  alias?: string;
  defaultModel?: string;
  providerName?: string;
} = {}): import("./draft").SetupDraft {
  const draft = defaultSetupDraft();
  draft.providers = [
    {
      name: overrides.providerName ?? "openrouter",
      type: "openrouter",
      apiKey: "sk-or-v1-xxx",
      models: [{ name: "anthropic/claude-3.7-sonnet", alias: overrides.alias ?? "sonnet", maxTokens: 8192 }],
    },
  ];
  if (overrides.defaultModel !== undefined) {
    draft.defaultModel = overrides.defaultModel;
  }
  return draft;
}

function hasErrorContaining(errors: string[], needle: string): boolean {
  return errors.some((e) => e.toLowerCase().includes(needle.toLowerCase()));
}

// ---------- ALIAS_PATTERN / RESERVED_ALIAS ----------------------------------

describe("ALIAS_PATTERN (the documented charset)", () => {
  it("accepts the documented chars", () => {
    for (const a of ["sonnet", "gpt-4o", "claude/3", "qwen_vl", "name.with.dot", "a:b"]) {
      expect(ALIAS_PATTERN.test(a)).toBe(true);
    }
  });

  it("rejects whitespace and other punctuation outside the set", () => {
    for (const a of ["bad space", "has!bang", "comma,here", "semi;colon", "quote\""]) {
      expect(ALIAS_PATTERN.test(a)).toBe(false);
    }
  });

  it("rejects empty string", () => {
    expect(ALIAS_PATTERN.test("")).toBe(false);
  });
});

describe("RESERVED_ALIAS", () => {
  it("is the literal 'unknown'", () => {
    expect(RESERVED_ALIAS).toBe("unknown");
  });
});

// ---------- validateModels --------------------------------------------------

describe("validateModels", () => {
  it("accepts a single model with a unique alias", () => {
    expect(validateModels(oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" }))).toEqual([]);
  });

  it("rejects duplicate aliases across providers", () => {
    const draft = defaultSetupDraft();
    draft.providers = [
      { name: "p1", type: "openrouter", models: [{ name: "m1", alias: "dup" }] },
      { name: "p2", type: "openai", models: [{ name: "m2", alias: "dup" }] },
    ];
    const errs = validateModels(draft);
    expect(hasErrorContaining(errs, "dup")).toBe(true);
    expect(hasErrorContaining(errs, "alias")).toBe(true);
  });

  it("rejects an alias that fails the charset check", () => {
    const draft = oneModelDraft({ alias: "bad space" });
    const errs = validateModels(draft);
    expect(hasErrorContaining(errs, "alias")).toBe(true);
    expect(hasErrorContaining(errs, "charset")).toBe(true);
  });

  it("rejects the reserved literal 'unknown' as an alias", () => {
    const draft = oneModelDraft({ alias: "unknown" });
    const errs = validateModels(draft);
    expect(hasErrorContaining(errs, "unknown")).toBe(true);
    expect(hasErrorContaining(errs, "reserved")).toBe(true);
  });

  it("accepts an empty providers list with no default_model", () => {
    const draft = defaultSetupDraft();
    expect(validateModels(draft)).toEqual([]);
  });

  it("requires default_model iff >=1 model is declared", () => {
    // Model declared but no default_model → error.
    const noDefault = oneModelDraft({ alias: "sonnet" });
    delete noDefault.defaultModel;
    expect(hasErrorContaining(validateModels(noDefault), "default_model")).toBe(true);

    // Default_model set with no models → error (or at least a flag).
    const orphanDefault = defaultSetupDraft();
    orphanDefault.defaultModel = "ghost";
    expect(hasErrorContaining(validateModels(orphanDefault), "default_model")).toBe(true);
  });

  it("rejects default_model that references an undeclared alias", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "ghost" });
    const errs = validateModels(draft);
    expect(hasErrorContaining(errs, "ghost")).toBe(true);
    expect(hasErrorContaining(errs, "default_model")).toBe(true);
  });

  it("accepts default_model that references a declared alias", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    expect(validateModels(draft)).toEqual([]);
  });
});

// ---------- validateServices (XOR) -----------------------------------------

describe("validateServices tools XOR (disabled vs only)", () => {
  it("accepts when neither disabled nor only is set", () => {
    const draft = defaultSetupDraft();
    expect(hasErrorContaining(validateServices(draft), "disabled")).toBe(false);
    expect(hasErrorContaining(validateServices(draft), "only")).toBe(false);
  });

  it("accepts when only disabled is set", () => {
    const draft = defaultSetupDraft();
    draft.services = { tools: { disabled: ["bash_bg"] } };
    expect(validateServices(draft)).toEqual([]);
  });

  it("accepts when only only is set", () => {
    const draft = defaultSetupDraft();
    draft.services = { tools: { only: ["file_read"] } };
    expect(validateServices(draft)).toEqual([]);
  });

  it("rejects when both disabled and only are set (the XOR)", () => {
    const draft = defaultSetupDraft();
    draft.services = { tools: { disabled: ["bash_bg"], only: ["file_read"] } };
    const errs = validateServices(draft);
    expect(hasErrorContaining(errs, "disabled")).toBe(true);
    expect(hasErrorContaining(errs, "only")).toBe(true);
    expect(hasErrorContaining(errs, "xor")).toBe(true);
  });
});

// ---------- validateMultiAgent ----------------------------------------------

describe("validateMultiAgent (live re-validation against models slice)", () => {
  it("accepts when the draft declares no pipeline (no role checks fire)", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = { include: false, agents: [{ role: "pm", model: "ghost" }] };
    expect(validateMultiAgent(draft)).toEqual([]);
  });

  it("accepts a role whose model references a declared alias", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = { include: true, agents: [{ role: "pm", model: "sonnet", prompt: "…" }] };
    expect(validateMultiAgent(draft)).toEqual([]);
  });

  it("accepts a role that omits model (→ resolves to default_model later)", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = { include: true, agents: [{ role: "pm", prompt: "…" }] };
    expect(validateMultiAgent(draft)).toEqual([]);
  });

  it("rejects a role whose model references an undeclared alias", () => {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = { include: true, agents: [{ role: "pm", model: "ghost", prompt: "…" }] };
    const errs = validateMultiAgent(draft);
    expect(hasErrorContaining(errs, "ghost")).toBe(true);
    expect(hasErrorContaining(errs, "pm")).toBe(true);
  });

  it("re-runs after models edits — removing an alias breaks the pipeline", () => {
    // 1. Start clean: pipeline references "sonnet", which exists.
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = { include: true, agents: [{ role: "pm", model: "sonnet", prompt: "…" }] };
    expect(validateMultiAgent(draft)).toEqual([]);

    // 2. User edits step 4 — the alias they removed is "sonnet" itself.
    draft.providers[0].models = []; // clear all models
    draft.defaultModel = undefined;

    const errs = validateMultiAgent(draft);
    expect(hasErrorContaining(errs, "sonnet")).toBe(true);
  });
});

// ---------- validateMultiAgent — the pipelines-list rules -------------------
//
// These mirror `PipelineSet::build`'s validation table (plan §3.4): the
// wizard refuses locally what the binary would refuse at boot.

describe("validateMultiAgent (pipeline name / orchestrator / members)", () => {
  /** A draft with one model and one legal pipeline. */
  function pipelineDraft(pipeline: Partial<PipelineDraft> = {}) {
    const draft = oneModelDraft({ alias: "sonnet", defaultModel: "sonnet" });
    draft.pipeline = {
      include: true,
      agents: [{ role: "pm", prompt: "…" }],
      ...pipeline,
    };
    return draft;
  }

  it("accepts a pipeline that omits the name (→ the default name)", () => {
    expect(validateMultiAgent(pipelineDraft())).toEqual([]);
    expect(pipelineName(pipelineDraft().pipeline)).toBe(DEFAULT_PIPELINE_NAME);
  });

  it("accepts an explicit legal name", () => {
    expect(validateMultiAgent(pipelineDraft({ name: "review-team.2" }))).toEqual([]);
  });

  it("rejects an empty name (the user cleared the field)", () => {
    const errs = validateMultiAgent(pipelineDraft({ name: "  " }));
    expect(hasErrorContaining(errs, "name")).toBe(true);
  });

  it("accepts a name with spaces — /pipeline takes the rest of the line", () => {
    expect(validateMultiAgent(pipelineDraft({ name: "Generic Dev Team" }))).toEqual([]);
  });

  it("trims the emitted name, so padding is not part of it", () => {
    const draft = pipelineDraft({ name: "  Generic Dev Team  " });
    expect(validateMultiAgent(draft)).toEqual([]);
    expect(pipelineName(draft.pipeline)).toBe("Generic Dev Team");
  });

  it("rejects a name outside the charset (a tab is not a space)", () => {
    const errs = validateMultiAgent(pipelineDraft({ name: "review\tteam" }));
    expect(hasErrorContaining(errs, "outside")).toBe(true);
  });

  it("rejects the reserved name `none`", () => {
    const errs = validateMultiAgent(pipelineDraft({ name: "none" }));
    expect(hasErrorContaining(errs, "none")).toBe(true);
    expect(hasErrorContaining(errs, "reserved")).toBe(true);
  });

  it("accepts an orchestrator model that resolves against a declared alias", () => {
    expect(validateMultiAgent(pipelineDraft({ orchestratorModel: "sonnet" }))).toEqual([]);
  });

  it("accepts an omitted orchestrator model (→ default_model)", () => {
    expect(validateMultiAgent(pipelineDraft({ orchestratorModel: undefined }))).toEqual([]);
  });

  it("rejects an orchestrator model that resolves to nothing", () => {
    const errs = validateMultiAgent(pipelineDraft({ orchestratorModel: "ghost" }));
    expect(hasErrorContaining(errs, "orchestrator")).toBe(true);
    expect(hasErrorContaining(errs, "ghost")).toBe(true);
  });

  it("rejects a pipeline with zero members", () => {
    const errs = validateMultiAgent(pipelineDraft({ agents: [] }));
    expect(hasErrorContaining(errs, "at least one")).toBe(true);
  });

  it("rejects a pipeline whose only member row has no role yet", () => {
    // A role-less row is dropped by renderYaml, so it cannot count as a
    // member: the emitted entry would have no `agents:` map at all.
    const errs = validateMultiAgent(pipelineDraft({ agents: [{ model: "sonnet" }] }));
    expect(hasErrorContaining(errs, "at least one")).toBe(true);
  });

  it("stays silent when the imported config already carries `pipelines:`", () => {
    // Passthrough pipelines are not the wizard's to validate or rewrite.
    const draft = pipelineDraft({ name: "none", agents: [] }); // would fail twice
    draft.passthrough.pipelines = [{ name: "imported" }];
    expect(validateMultiAgent(draft)).toEqual([]);
  });
});

// ---------- importedPipelines ------------------------------------------------

describe("importedPipelines (passthrough is the wizard's hands-off signal)", () => {
  it("returns undefined when the draft has no passthrough pipelines", () => {
    expect(importedPipelines(defaultSetupDraft())).toBeUndefined();
  });

  it("returns the imported entries verbatim", () => {
    const draft = defaultSetupDraft();
    const entries = [{ name: "a" }, { name: "b" }];
    draft.passthrough.pipelines = entries;
    expect(importedPipelines(draft)).toEqual(entries);
  });

  it("`pipelines` is NOT an owned key — an imported list survives the round trip", () => {
    // Adding it to OWNED_KEYS would let the wizard silently delete the
    // user's teams; instead they ride along in passthrough.
    const draft = configJsonToDraft({
      pipelines: [{ name: "team", orchestrator: { model: "sonnet" }, agents: { pm: {} } }],
    });
    expect(importedPipelines(draft)).toEqual([
      { name: "team", orchestrator: { model: "sonnet" }, agents: { pm: {} } },
    ]);
  });

  it("drops the legacy `pipeline` block instead of passing it through", () => {
    // Legacy stays owned: it hard-errors at boot, and keeping it out of
    // passthrough also stops the wizard writing both shapes at once.
    const draft = configJsonToDraft({ pipeline: { enabled: true, agents: { pm: {} } } });
    expect(draft.passthrough.pipeline).toBeUndefined();
  });
});

// ---------- validateAccess --------------------------------------------------

describe("validateAccess (token required for non-loopback bind)", () => {
  it("accepts local mode without a token", () => {
    const draft = defaultSetupDraft();
    draft.access = { mode: "local" };
    expect(validateAccess(draft)).toEqual([]);
  });

  it("accepts lan mode with a token", () => {
    const draft = defaultSetupDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "tok" };
    expect(validateAccess(draft)).toEqual([]);
  });

  it("rejects lan mode without a token", () => {
    const draft = defaultSetupDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823" };
    const errs = validateAccess(draft);
    expect(hasErrorContaining(errs, "token")).toBe(true);
    expect(hasErrorContaining(errs, "lan")).toBe(true);
  });
});

// ---------- validateDraft (union) ------------------------------------------

describe("validateDraft", () => {
  it("returns the union of every validator", () => {
    const draft = defaultSetupDraft();
    draft.providers = [
      { name: "p", type: "openrouter", models: [{ name: "m", alias: "x" }] },
    ];
    draft.services = { tools: { disabled: ["a"], only: ["b"] } }; // XOR
    draft.access = { mode: "lan" }; // missing token
    draft.pipeline = { include: true, agents: [{ role: "pm", model: "ghost" }] }; // alias ref

    const all = validateDraft(draft);
    expect(all.length).toBeGreaterThanOrEqual(3);
    expect(hasErrorContaining(all, "xor")).toBe(true);
    expect(hasErrorContaining(all, "token")).toBe(true);
    expect(hasErrorContaining(all, "ghost")).toBe(true);
  });

  it("returns [] on the empty defaults draft", () => {
    expect(validateDraft(defaultSetupDraft())).toEqual([]);
  });
});

// ---------- classifyChange (restart matrix) --------------------------------

describe("classifyChange (restart matrix from agents.md)", () => {
  const RELOAD_SAFE = [
    "providers",
    "default_model",
    "searxng",
    "bash",
    "agent_max_turns",
    "cost_tracking",
    "context",
    "retry",
    "memory",
    "timeouts",
    "tools",
    "skills",
  ];
  const BOOT_ONLY = ["mcp_servers", "vector_db", "web", "pipeline", "pipelines", "http"];

  for (const k of RELOAD_SAFE) {
    it(`classifies ${k} as reload-safe`, () => {
      expect(classifyChange(k)).toBe("reload-safe");
    });
  }

  for (const k of BOOT_ONLY) {
    it(`classifies ${k} as boot-only`, () => {
      expect(classifyChange(k)).toBe("boot-only");
    });
  }

  it("defaults unknown keys to boot-only (conservative — better to say 'restart' than lie)", () => {
    expect(classifyChange("totally_unrelated_thing")).toBe("boot-only");
  });
});

// ---------- collectAliases --------------------------------------------------

describe("collectAliases", () => {
  it("returns declared aliases across all providers, in declaration order", () => {
    const draft = defaultSetupDraft();
    draft.providers = [
      { name: "p1", type: "openrouter", models: [{ name: "m1", alias: "a" }, { name: "m2" }] },
      { name: "p2", type: "openai", models: [{ name: "m3", alias: "b" }, { name: "m4", alias: "c" }] },
    ];
    expect(collectAliases(draft)).toEqual(["a", "b", "c"]);
  });

  it("skips models without an alias", () => {
    const draft = defaultSetupDraft();
    draft.providers = [
      { name: "p", type: "openrouter", models: [{ name: "m1" }, { name: "m2", alias: "x" }] },
    ];
    expect(collectAliases(draft)).toEqual(["x"]);
  });

  it("returns [] for the empty default draft", () => {
    expect(collectAliases(defaultSetupDraft())).toEqual([]);
  });
});