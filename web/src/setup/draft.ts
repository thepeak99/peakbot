/**
 * SetupDraft — the in-memory shape of the /setup wizard.
 *
 * One slice per step from plan §8.4 (1=Welcome … 10=Review). `<Setup>` owns
 * the single `useState<SetupDraft>`; this module declares the shape, the
 * defaults, and the local validation the steps call.
 *
 * Validation is *shape* validation only — required/empty/duplicate/XOR — and
 * runs entirely in the browser. Nothing here talks to the binary.
 */

// ---------- slice types -----------------------------------------------------

/** Step 1 — Welcome / paths. */
export type WelcomeDraft = {
  /** "import" = load existing config, "fresh" = blank draft. */
  startMode?: "import" | "fresh";
  /** Canned summary shown after the fake import succeeds. */
  importedSummary?: string;
};

/** Step 2 — Locations. */
export type LocationsDraft = {
  configDir?: string;
  dataDir?: string;
  cacheDir?: string;
  skillsDirs?: string[];
  addToPath?: boolean;
};

/** Step 3 — Provider. Repeating (multi-provider config per agents.md). */
export type ProviderDraft = {
  name?: string;
  type?: "openrouter" | "openai" | "anthropic" | "llamacpp" | "ollama";
  baseUrl?: string;
  apiKey?: string;
};

/** Per-model overrides documented in agents.md. */
export type ModelDraft = {
  name?: string;
  alias?: string;
  maxTokens?: number;
  temperature?: number;
  vision?: boolean;
  contextWindowOverride?: number;
  /** Ollama-only. */
  numCtx?: number;
  /** LlamaCpp-only. */
  extraParams?: Record<string, unknown>;
  /** Anthropic-only: auto | auto_1h | manual | off. */
  promptCaching?: "auto" | "auto_1h" | "manual" | "off";
};

/** Step 3 — providers, each with its own models list. `models` may be
 *  omitted while the user is mid-edit; defaultSetupDraft initialises it. */
export type ProviderWithModelsDraft = ProviderDraft & {
  models?: ModelDraft[];
};

/** Step 5 — Persona. */
export type PersonaDraft = {
  mode?: "preset" | "custom";
  presetId?: string;
  custom?: string;
};

/** Step 6 — Services. `tools` enforces XOR (`disabled` vs `only`). */
export type ServicesDraft = {
  searxng?: {
    baseUrl?: string;
    enabled?: boolean;
    timeoutSeconds?: number;
    maxResults?: number;
    bearerToken?: string;
  };
  vectorDb?: {
    enabled?: boolean;
    dbPath?: string;
    embeddings?: {
      baseUrl?: string;
      apiKey?: string;
      model?: string;
      dimensions?: number;
    };
  };
  tools?: {
    disabled?: string[];
    only?: string[];
  };
};

/** Step 7 — Access. `mode = "lan"` requires `token`. */
export type AccessDraft = {
  mode?: "local" | "lan";
  bindAddress?: string;
  token?: string;
  tls?: boolean;
};

/** Step 8 — Start on boot is OS state (service/install endpoint), not draft state. */

/** Step 9 — Multi-agent pipeline. Per-role `model` aliases must resolve
 *  against `providers[*].models[*].alias` (live re-validated). */
export type PipelineAgentDraft = {
  role?: string;
  /** Alias from `providers[*].models[*].alias`, or omitted → default_model. */
  model?: string;
  prompt?: string;
  /** Per-role skills gate: only XOR disabled, or enabled: false. */
  skills?: {
    enabled?: boolean;
    only?: string[];
    disabled?: string[];
  };
  agentsMd?: boolean;
  env?: Record<string, string>;
};

/** One entry of the `pipelines:` list. The wizard writes at most one team;
 *  configs with several are imported read-only (see {@link importedPipelines}).
 *
 *  `include` replaced the legacy `enabled` flag: `pipelines:` has no
 *  `enabled:` key, so the boolean now only decides whether the wizard emits
 *  an entry at all. */
export type PipelineDraft = {
  include?: boolean;
  /** Typed after `/pipeline`, so `^[A-Za-z0-9_.-]+$`. Unset → "default". */
  name?: string;
  /** Orchestrator alias; omitted → default_model. */
  orchestratorModel?: string;
  /** Addendum to the orchestrator's recipe (not a whole persona). */
  orchestratorPrompt?: string;
  /** Replaces the global persona for this pipeline's orchestrator. */
  orchestratorPersona?: string;
  agents?: PipelineAgentDraft[];
};

/** Optional blocks the wizard surfaces in the review page. */
export type BashEnvDraft = Record<string, string>;

export type ContextDraft = {
  enabled?: boolean;
  threshold?: number;
  keepRecent?: number;
  contextWindow?: number;
};

export type MemoryDraft = {
  enabled?: boolean;
  thresholdBytes?: number;
};

export type TimeoutsDraft = {
  toolSecs?: number;
  delegateSecs?: number;
};

export type HttpDraft = {
  connectTimeoutSecs?: number;
  readTimeoutSecs?: number;
};

/** Root draft — one slice per wizard step. */
export type SetupDraft = {
  welcome: WelcomeDraft;
  locations: LocationsDraft;
  providers: ProviderWithModelsDraft[];
  defaultModel?: string;
  persona: PersonaDraft;
  services: ServicesDraft;
  access: AccessDraft;
  pipeline: PipelineDraft;
  bashEnv: BashEnvDraft;
  context: ContextDraft;
  memory: MemoryDraft;
  costTracking?: boolean;
  timeouts: TimeoutsDraft;
  http: HttpDraft;
  agentMaxTurns?: number;
  /** Top-level config keys not owned by the wizard, preserved verbatim. */
  passthrough: Record<string, unknown>;
};

// ---------- defaults --------------------------------------------------------

/** Empty draft used to bootstrap `<Setup>`. */
export function defaultSetupDraft(): SetupDraft {
  return {
    welcome: {},
    locations: {},
    providers: [],
    persona: {},
    services: {},
    access: { mode: "local" },
    pipeline: {},
    bashEnv: {},
    context: {},
    memory: {},
    timeouts: {},
    http: {},
    passthrough: {},
  };
}

// ---------- helpers ---------------------------------------------------------

/** Alias charset from agents.md: `^[A-Za-z0-9_./:-]+$`. */
export const ALIAS_PATTERN = /^[A-Za-z0-9_./:-]+$/;

/** Pipeline-name charset from `PipelineSet::build`: no spaces, because the
 *  name is typed after `/pipeline`. */
export const PIPELINE_NAME_PATTERN = /^[A-Za-z0-9_.-]+$/;

/** Name used when the user never touches the field. */
export const DEFAULT_PIPELINE_NAME = "default";

/** Reserved pipeline name — it means "no pipeline". */
export const RESERVED_PIPELINE_NAME = "none";

/** The name that will be emitted for a draft pipeline. */
export function pipelineName(pipeline: PipelineDraft): string {
  return pipeline.name ?? DEFAULT_PIPELINE_NAME;
}

// `pipelines` is deliberately absent: an imported multi-pipeline config must
// pass through verbatim, and owning the key would let the wizard silently
// delete the user's teams. The legacy `pipeline` key stays owned — it is a
// hard boot error now, so dropping it is the migration, and keeping it out of
// passthrough is also what stops the wizard writing both shapes at once.
const OWNED_KEYS = new Set([
  "providers", "default_model", "persona", "searxng", "vector_db", "tools", "bash",
  "context", "cost_tracking", "agent_max_turns", "memory", "timeouts", "http", "web", "pipeline",
]);

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/** Convert GET /api/setup existing.config JSON into the wizard's managed draft. */
export function configJsonToDraft(config: unknown): SetupDraft {
  const source = asRecord(config);
  const draft = defaultSetupDraft();
  const providers = Array.isArray(source.providers) ? source.providers : [];
  draft.providers = providers.filter((p): p is Record<string, unknown> => !!p && typeof p === "object").map((p) => ({
    name: typeof p.name === "string" ? p.name : undefined,
    type: typeof p.type === "string" && ["openrouter", "openai", "anthropic", "llamacpp", "ollama"].includes(p.type) ? p.type as ProviderDraft["type"] : undefined,
    baseUrl: typeof p.base_url === "string" ? p.base_url : undefined,
    apiKey: typeof p.api_key === "string" ? p.api_key : undefined,
    models: Array.isArray(p.models) ? p.models.filter((m): m is Record<string, unknown> => !!m && typeof m === "object").map((m) => ({
      name: typeof m.name === "string" ? m.name : undefined,
      alias: typeof m.alias === "string" ? m.alias : undefined,
      maxTokens: asNumber(m.max_tokens), temperature: asNumber(m.temperature), vision: typeof m.vision === "boolean" ? m.vision : undefined,
      contextWindowOverride: asNumber(m.context_window_override), numCtx: asNumber(m.num_ctx), extraParams: asRecord(m.extra_params),
      promptCaching: typeof m.prompt_caching === "string" ? m.prompt_caching as ModelDraft["promptCaching"] : undefined,
    })) : [],
  }));
  draft.defaultModel = typeof source.default_model === "string" ? source.default_model : undefined;
  if (typeof source.persona === "string") draft.persona = { mode: "custom", custom: source.persona };
  const s = asRecord(source.searxng);
  if (Object.keys(s).length) draft.services.searxng = { baseUrl: typeof s.base_url === "string" ? s.base_url : undefined, enabled: typeof s.enabled === "boolean" ? s.enabled : undefined, timeoutSeconds: asNumber(s.timeout_seconds), maxResults: asNumber(s.max_results), bearerToken: typeof s.bearer_token === "string" ? s.bearer_token : undefined };
  const v = asRecord(source.vector_db);
  if (Object.keys(v).length) { const e = asRecord(v.embeddings); draft.services.vectorDb = { enabled: typeof v.enabled === "boolean" ? v.enabled : undefined, dbPath: typeof v.db_path === "string" ? v.db_path : undefined, embeddings: Object.keys(e).length ? { baseUrl: typeof e.base_url === "string" ? e.base_url : undefined, apiKey: typeof e.api_key === "string" ? e.api_key : undefined, model: typeof e.model === "string" ? e.model : undefined, dimensions: asNumber(e.dimensions) } : undefined }; }
  const tools = asRecord(source.tools);
  if (Array.isArray(tools.disabled)) draft.services.tools = { disabled: tools.disabled.filter((x): x is string => typeof x === "string") };
  else if (Array.isArray(tools.only)) draft.services.tools = { only: tools.only.filter((x): x is string => typeof x === "string") };
  const passthrough: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(source)) if (!OWNED_KEYS.has(key)) passthrough[key] = value;
  draft.passthrough = passthrough;
  return draft;
}
/** Reserved alias that must never be declared. */
export const RESERVED_ALIAS = "unknown";

/** Returns every declared alias across all providers/models, in declaration
 *  order. Empty string entries are skipped. */
export function collectAliases(draft: SetupDraft): string[] {
  const out: string[] = [];
  for (const p of draft.providers) {
    for (const m of p.models ?? []) {
      if (typeof m.alias === "string" && m.alias.length > 0) out.push(m.alias);
    }
  }
  return out;
}

/** Total models declared across every provider — `default_model` is required
 *  iff this is > 0. */
function countModels(draft: SetupDraft): number {
  return draft.providers.reduce((n, p) => n + (p.models?.length ?? 0), 0);
}

/** The `pipelines:` entries an imported config brought along, or `undefined`
 *  when there are none. They live in `passthrough` because `pipelines` is not
 *  an owned key; their presence is the wizard's "hands off" signal — the
 *  Multi-agent step goes read-only and renderYaml emits only the passthrough
 *  copy. A non-array value still counts as present: a config we cannot read is
 *  even less ours to rewrite. */
export function importedPipelines(draft: SetupDraft): unknown[] | undefined {
  const value = draft.passthrough.pipelines;
  if (value === undefined) return undefined;
  return Array.isArray(value) ? value : [];
}

// ---------- validators ------------------------------------------------------

/** Errors from step 4 (Models). Empty array = valid. */
export function validateModels(draft: SetupDraft): string[] {
  const errors: string[] = [];
  const aliases = collectAliases(draft);
  const seen = new Set<string>();
  const reported = new Set<string>();

  for (const alias of aliases) {
    if (!ALIAS_PATTERN.test(alias)) {
      errors.push(
        `Model alias "${alias}" is outside the allowed charset ${ALIAS_PATTERN.source}.`,
      );
    }
    if (alias === RESERVED_ALIAS) {
      errors.push(`Model alias "${alias}" is reserved — pick another one.`);
    }
    // One message per duplicated alias, not one per extra occurrence.
    if (seen.has(alias) && !reported.has(alias)) {
      errors.push(
        `Duplicate model alias "${alias}" — aliases are globally unique across every provider.`,
      );
      reported.add(alias);
    }
    seen.add(alias);
  }

  if (countModels(draft) > 0 && !draft.defaultModel) {
    errors.push("default_model is required once any model is declared.");
  }
  if (draft.defaultModel && !aliases.includes(draft.defaultModel)) {
    errors.push(
      `default_model "${draft.defaultModel}" does not match any declared alias.`,
    );
  }

  return errors;
}

/** Errors from step 6 (Services). Empty array = valid. The XOR on the
 *  `tools` filter (disabled vs only) is the documented hard rule. */
export function validateServices(draft: SetupDraft): string[] {
  const tools = draft.services.tools;
  if ((tools?.disabled?.length ?? 0) > 0 && (tools?.only?.length ?? 0) > 0) {
    return [
      "tools: disabled and only are XOR — keep the blocklist or the allowlist, not both.",
    ];
  }
  return [];
}

/** Errors from step 9 (Multi-agent). Mirrors the rules `PipelineSet::build`
 *  enforces at boot (plan §3.4), so the wizard refuses locally what the binary
 *  would refuse to start with. Re-runs after step 4 edits; every `model` here
 *  must resolve against the live models slice. */
export function validateMultiAgent(draft: SetupDraft): string[] {
  // An imported `pipelines:` list is passthrough — not ours to judge.
  if (importedPipelines(draft)) return [];
  const pipeline = draft.pipeline;
  if (!pipeline.include) return [];
  const aliases = collectAliases(draft);
  const errors: string[] = [];

  const name = pipelineName(pipeline);
  if (!name.trim()) {
    errors.push("The pipeline needs a name — it is what you type after /pipeline.");
  } else if (name === RESERVED_PIPELINE_NAME) {
    errors.push(`"${name}" is a reserved pipeline name — it means "no pipeline".`);
  } else if (!PIPELINE_NAME_PATTERN.test(name)) {
    errors.push(
      `Pipeline name "${name}" is outside ${PIPELINE_NAME_PATTERN.source} — no spaces, you type it after /pipeline.`,
    );
  }

  // An omitted orchestrator model is legal — it falls back to default_model.
  if (pipeline.orchestratorModel && !aliases.includes(pipeline.orchestratorModel)) {
    errors.push(
      `The orchestrator points at model alias "${pipeline.orchestratorModel}", which no longer exists in the Models step.`,
    );
  }

  const agents = pipeline.agents ?? [];
  // Rows without a role are dropped by renderYaml, so they cannot count as
  // members: the emitted entry would have no `agents:` map, which the binary
  // rejects at parse time.
  if (agents.filter((a) => a.role?.trim()).length === 0) {
    errors.push(`Pipeline "${name}" needs at least one sub-agent role.`);
  }
  for (const agent of agents) {
    // An omitted model is legal — the role falls back to default_model.
    if (!agent.model) continue;
    if (!aliases.includes(agent.model)) {
      errors.push(
        `Role "${agent.role ?? ""}" points at model alias "${agent.model}", which no longer exists in the Models step.`,
      );
    }
  }
  return errors;
}

/** Errors from step 7 (Access). Empty array = valid. */
export function validateAccess(draft: SetupDraft): string[] {
  if (draft.access.mode === "lan" && !draft.access.token?.trim()) {
    return [
      "A LAN bind requires a token — generate one, or switch back to local only.",
    ];
  }
  return [];
}

/** Every documented rule, unioned. The Next button calls this on every
 *  step transition. */
export function validateDraft(draft: SetupDraft): string[] {
  return [
    ...validateModels(draft),
    ...validateServices(draft),
    ...validateMultiAgent(draft),
    ...validateAccess(draft),
  ];
}

// ---------- restart matrix --------------------------------------------------

/** Whether a top-level YAML key applies on the next session verb or
 *  requires a full restart. From agents.md "Live reload on session verbs". */
export type RestartImpact = "reload-safe" | "boot-only";

/** Top-level keys the session verbs re-read (`/new`, `/model`, `/cd`,
 *  `/load`). Everything else — including the boot-only blocks and any key we
 *  don't recognise — needs a restart. */
const RELOAD_SAFE_KEYS = new Set([
  "providers",
  "default_model",
  "skills",
  "searxng",
  "bash",
  "agent_max_turns",
  "cost_tracking",
  "context",
  "retry",
  "memory",
  "timeouts",
  "tools",
]);

/** Classify a top-level config key. Unrecognised keys default to
 *  "boot-only" (conservative — telling the user "restart" is safer than
 *  silently telling them a change is hot-reloadable when it isn't). */
export function classifyChange(key: string): RestartImpact {
  return RELOAD_SAFE_KEYS.has(key) ? "reload-safe" : "boot-only";
}