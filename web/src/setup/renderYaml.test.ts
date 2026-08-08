import { describe, it, expect } from "vitest";
import { defaultSetupDraft, type SetupDraft } from "./draft";
import { renderYaml } from "./renderYaml";

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
    expect(yaml).toMatch(/- name: "openrouter"/);
    expect(yaml).toMatch(/type: openrouter/);
    expect(yaml).toMatch(/api_key: \*\*\*\*/); // masked, not raw
    expect(yaml).toMatch(/models:/);
    expect(yaml).toMatch(/- name: "anthropic\/claude-3\.7-sonnet"/);
  });

  it("includes default_model at top level when set", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).toMatch(/^default_model: "sonnet"$/m);
  });

  it("emits per-model fields with documented names (name, alias, max_tokens)", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).toMatch(/alias: "sonnet"/);
    expect(yaml).toMatch(/max_tokens: 8192/);
  });

  it("renders a providers block with no models without crashing", () => {
    const draft = defaultSetupDraft();
    draft.providers = [{ name: "p", type: "openai", apiKey: "x" }];
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^providers:/m);
    expect(yaml).toMatch(/- name: "p"/);
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

  it("masks web token as **** when access is lan (and never emits bind/token)", () => {
    const draft = minimalDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "WEB-TOKEN-PLAINTEXT", tls: true };
    const yaml = renderYaml(draft);
    // plan §A-Q5: bind/token are launch command line, not YAML — no `web.bind`
    // or `web.token` keys. `web.tls` IS a real config key and survives.
    expect(yaml).not.toContain("WEB-TOKEN-PLAINTEXT");
    expect(yaml).not.toMatch(/^ {2}bind:/m);
    expect(yaml).not.toMatch(/^ {2}token:/m);
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
    expect(yaml).toMatch(/^ {2}disabled:/m);
    expect(yaml).toMatch(/- "bash_bg"/);
    expect(yaml).not.toMatch(/^ {2}only:/m);
  });

  it("renders tools.only when set", () => {
    const draft = minimalDraft();
    draft.services = { tools: { only: ["file_read", "bash"] } };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^tools:/m);
    expect(yaml).toMatch(/^ {2}only:/m);
    expect(yaml).toMatch(/- "file_read"/);
    expect(yaml).not.toMatch(/^ {2}disabled:/m);
  });
});

// ---------- pipelines -------------------------------------------------------

describe("renderYaml — pipelines list", () => {
  function teamDraft(pipeline: Partial<SetupDraft["pipeline"]> = {}): SetupDraft {
    const draft = minimalDraft();
    draft.pipeline = {
      include: true,
      agents: [{ role: "researcher", model: "sonnet", prompt: "Research things." }],
      ...pipeline,
    };
    return draft;
  }

  it("emits a pipelines list with name, orchestrator, and agents", () => {
    const yaml = renderYaml(
      teamDraft({
        name: "review-team",
        orchestratorModel: "sonnet",
        orchestratorPrompt: "You lead a small team.",
        agents: [
          { role: "researcher", model: "sonnet", prompt: "Research things." },
          { role: "reviewer", model: "sonnet", prompt: "Review diffs." },
        ],
      }),
    );
    expect(yaml).toMatch(/^pipelines:/m);
    expect(yaml).toMatch(/^ {2}- name: "review-team"$/m);
    expect(yaml).toMatch(/^ {4}orchestrator:$/m);
    expect(yaml).toMatch(/^ {6}model: "sonnet"$/m);
    expect(yaml).toMatch(/^ {6}prompt: \|2-$/m);
    expect(yaml).toMatch(/^ {4}agents:$/m);
    expect(yaml).toMatch(/^ {6}researcher:$/m);
    expect(yaml).toMatch(/^ {6}reviewer:$/m);
  });

  it("falls back to the default pipeline name when none is typed", () => {
    const yaml = renderYaml(teamDraft());
    expect(yaml).toMatch(/^ {2}- name: "default"$/m);
  });

  it("emits the orchestrator persona override when set", () => {
    const yaml = renderYaml(teamDraft({ orchestratorPersona: "You are terse." }));
    expect(yaml).toMatch(/^ {6}persona: \|2-$/m);
    expect(yaml).toMatch(/You are terse\./);
  });

  it("omits unset orchestrator fields, emitting an empty mapping", () => {
    // `orchestrator:` with no children would deserialize as null and fail
    // the parse; `{}` means "all defaults", which is what we mean.
    const yaml = renderYaml(teamDraft());
    expect(yaml).toMatch(/^ {4}orchestrator: \{\}$/m);
    expect(yaml).not.toMatch(/^ {6}model:/m);
    expect(yaml).not.toMatch(/^ {6}prompt:/m);
    expect(yaml).not.toMatch(/^ {6}persona:/m);
  });

  it("never emits `enabled:` or the legacy `pipeline:` block", () => {
    const yaml = renderYaml(
      teamDraft({ name: "team", orchestratorPrompt: "Lead.", orchestratorModel: "sonnet" }),
    );
    expect(yaml).not.toMatch(/^pipeline:/m);
    expect(yaml).not.toMatch(/enabled:/);
    expect(yaml).not.toMatch(/orchestrator_prompt:/);
  });

  it("renders the agents.<role>.env block when set", () => {
    const yaml = renderYaml(
      teamDraft({
        agents: [{ role: "reviewer", model: "sonnet", prompt: "x", env: { REVIEW_STRICT: "1" } }],
      }),
    );
    expect(yaml).toMatch(/env:/);
    expect(yaml).toMatch(/REVIEW_STRICT: "1"/);
  });

  it("renders agents.<role>.skills only block", () => {
    const yaml = renderYaml(
      teamDraft({
        agents: [
          { role: "researcher", model: "sonnet", prompt: "x", skills: { only: ["github"] } },
        ],
      }),
    );
    expect(yaml).toMatch(/skills:/);
    expect(yaml).toMatch(/only:/);
    expect(yaml).toMatch(/- "github"/);
  });

  it("omits pipelines entirely when the draft declares none", () => {
    const yaml = renderYaml(minimalDraft());
    expect(yaml).not.toMatch(/^pipelines:/m);
    expect(yaml).not.toMatch(/^pipeline:/m);
  });

  it("does not double-emit when the imported config already has pipelines", () => {
    // `pipelines` is unowned, so an imported list lives in passthrough and
    // is emitted verbatim there — the wizard must not add a second block.
    const draft = teamDraft({ name: "wizard-team" });
    draft.passthrough.pipelines = [
      { name: "imported", orchestrator: { model: "sonnet" }, agents: { pm: { prompt: "p" } } },
    ];
    const yaml = renderYaml(draft);
    expect(yaml.match(/^pipelines:/gm)).toHaveLength(1);
    expect(yaml).toMatch(/- name: "imported"/);
    expect(yaml).not.toMatch(/wizard-team/);
  });
});

// ---------- web / access ----------------------------------------------------

describe("renderYaml — web block", () => {
  it("emits web.tls only when access.tls is set (no bind/token keys)", () => {
    // plan §A-Q5: bind/token are launch command line, not YAML. Only web.tls
    // remains a real config key.
    const draft = minimalDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "T", tls: true };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^web:/m);
    expect(yaml).toMatch(/tls: true/);
    expect(yaml).not.toMatch(/bind:/);
    expect(yaml).not.toMatch(/^ {2}token:/m);
  });

  it("omits web entirely when access is local (default bind)", () => {
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
    expect(yaml).toMatch(/^ {2}env:/m);
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
    expect(yaml).toMatch(/base_url: "https:\/\/s\.example"/);
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

// ---------- persona emission (plan §A-Q7) -----------------------------------
//
// These tests cover the W1 / T4 contract: renderYaml emits `persona: |2-`
// for a configured persona, with explicit indent indicator 2 and strip
// chomping, and OMITS the key entirely for an empty/whitespace persona.
// Status: RED until W1 lands — today `renderYaml` has no persona branch
// and these tests fail at runtime.

describe("renderYaml — persona emission (plan §A-Q7)", () => {
  it("emits `persona: |2-` with explicit indent indicator for a custom persona", () => {
    const draft = minimalDraft();
    draft.persona = {
      mode: "custom",
      custom:
        "You are a coding agent working in the user's local filesystem.\n\nState what you are about to do.",
    };
    const yaml = renderYaml(draft);
    // Explicit indicator `2` is the load-bearing detail — without it a
    // persona whose first line starts with a space silently corrupts every
    // following line.
    expect(yaml).toMatch(/^persona: \|2-$/m);
  });

  it("emits every persona text line indented by two spaces", () => {
    const draft = minimalDraft();
    draft.persona = {
      mode: "custom",
      custom: "first line\nsecond line\nthird line",
    };
    const yaml = renderYaml(draft);
    // Each line of the block must be indented by exactly two spaces
    // (the explicit `2` indicator's offset).
    expect(yaml).toMatch(/^ {2}first line$/m);
    expect(yaml).toMatch(/^ {2}second line$/m);
    expect(yaml).toMatch(/^ {2}third line$/m);
  });

  it("emits a blank interior line as two spaces (≤ declared indent = empty line)", () => {
    const draft = minimalDraft();
    draft.persona = {
      mode: "custom",
      custom: "first paragraph\n\nsecond paragraph",
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^ {2}first paragraph$/m);
    expect(yaml).toMatch(/^ {2}$/m); // the empty interior line as `"  "`
    expect(yaml).toMatch(/^ {2}second paragraph$/m);
  });

  it("normalises CRLF in persona text to LF before emitting", () => {
    // Plan §A-Q7: "The client normalises `\r\n?` → `\n` when resolving the
    // persona text (one line, kills invisible `\r` characters from pastes)."
    const draft = minimalDraft();
    draft.persona = {
      mode: "custom",
      custom: "line one\r\nline two\rline three",
    };
    const yaml = renderYaml(draft);
    // No CR should appear anywhere in the YAML output.
    expect(yaml).not.toContain("\r");
    expect(yaml).toMatch(/^ {2}line one$/m);
    expect(yaml).toMatch(/^ {2}line two$/m);
    expect(yaml).toMatch(/^ {2}line three$/m);
  });

  it("omits the persona key entirely when the persona is empty or whitespace-only", () => {
    // Plan §A-Q7: "Empty/whitespace-only persona ⇒ the key is not emitted
    // at all. `persona:` is `Option<String>` and 'absent' is the only
    // representation of 'default'. There is no `persona: ""` state."
    const draft = minimalDraft();
    draft.persona = { mode: "custom", custom: "" };
    expect(renderYaml(draft)).not.toMatch(/^persona:/m);

    const ws = minimalDraft();
    ws.persona = { mode: "custom", custom: "   \n  \t  " };
    expect(renderYaml(ws)).not.toMatch(/^persona:/m);
  });

  it("emits persona when preset mode picks a preset's prompt text", () => {
    // plan §A-Q7: the renderer consumes the resolved string. The W6 wizard
    // populates `persona.custom` from `personaText(draft)` before reaching
    // renderYaml, so we assert the renderer emits `persona:` for whatever
    // non-empty text it receives (custom textarea, or a preset's prompt
    // copied in by the resolver).
    const draft = minimalDraft();
    draft.persona = { mode: "custom", custom: "NEUTRAL-ENGINEER-PROMPT" };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^persona: \|2-$/m);
    expect(yaml).toMatch(/^ {2}NEUTRAL-ENGINEER-PROMPT$/m);
  });

  it("preserves a leading-space first line in the persona block", () => {
    // The whole point of `|2-` is that this works.
    const draft = minimalDraft();
    draft.persona = {
      mode: "custom",
      custom: " line starting with a space\n  next",
    };
    const yaml = renderYaml(draft);
    // ` ` + ` line starting with a space` = three leading spaces.
    expect(yaml).toMatch(/^ {3}line starting with a space$/m);
    expect(yaml).toMatch(/^ {4}next$/m);
  });
});

// ---------- masked vs unmasked (W1 / W4 contract) --------------------------
//
// The review pane must show ****; the POST body to /api/setup/config must
// carry the real secret. One function, two call sites — these tests lock
// the structural identity of the two outputs.

describe("renderYaml — masked vs unmasked output (plan §A-Q4)", () => {
  it("preview renders api_key as ****; the unmasked body carries the real value", () => {
    const draft = minimalDraft();
    const preview = renderYaml(draft, { mask: true });
    const body = renderYaml(draft, { mask: false });
    expect(preview).toMatch(/api_key: \*\*\*\*/);
    expect(preview).not.toContain("sk-or-v1-REAL-SECRET-DO-NOT-LEAK");
    expect(body).toContain("sk-or-v1-REAL-SECRET-DO-NOT-LEAK");
    expect(body).not.toMatch(/api_key: \*\*\*\*/);
  });

  it("masks embeddings api_key and searxng bearer_token when masked; reveals when not", () => {
    const draft = minimalDraft();
    draft.services = {
      searxng: { baseUrl: "https://s.example", enabled: true, bearerToken: "BEAR-SECRET" },
      vectorDb: { enabled: true, dbPath: "./.peakbot/vectors.db", embeddings: { apiKey: "EMB-SECRET" } },
    };
    const preview = renderYaml(draft, { mask: true });
    const body = renderYaml(draft, { mask: false });
    expect(preview).toMatch(/bearer_token: \*\*\*\*/);
    expect(preview).toMatch(/api_key: \*\*\*\*/);
    expect(body).toContain("BEAR-SECRET");
    expect(body).toContain("EMB-SECRET");
  });
});

// ---------- passthrough emission (W4 contract) -----------------------------

describe("renderYaml — passthrough block (plan §A-Q5 / §D-W4)", () => {
  it("emits unmanaged top-level keys verbatim and after managed blocks", () => {
    const draft = minimalDraft();
    draft.passthrough = {
      mcp_servers: [{ name: "github", command: "mcp-github" }],
      retry: { max_attempts: 3, initial_backoff_secs: 1 },
    };
    const yaml = renderYaml(draft);
    expect(yaml).toMatch(/^mcp_servers:/m);
    expect(yaml).toMatch(/- name: "github"/);
    expect(yaml).toMatch(/^retry:/m);
    expect(yaml).toMatch(/max_attempts: 3/);
  });
});

// ---------- regression: Access step never emits web.bind / web.token ------

describe("renderYaml — Access step no web.bind / web.token (regression)", () => {
  it("LAN mode never produces a web.bind or web.token YAML key", () => {
    const draft = minimalDraft();
    draft.access = { mode: "lan", bindAddress: "0.0.0.0:7823", token: "long-token", tls: true };
    const yaml = renderYaml(draft);
    // The key `web:` is allowed only for the `tls` sub-key per plan §A-Q5.
    expect(yaml).not.toMatch(/^ {2}bind:/m);
    expect(yaml).not.toMatch(/^ {2}token:/m);
  });
});