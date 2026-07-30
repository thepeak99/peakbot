/**
 * SetupDraft — the in-memory shape of the /setup wizard.
 *
 * One slice per step from plan §8.4 (1=Welcome … 10=Review). The senior
 * implementation owns `useState<SetupDraft>` inside `<Setup>`; this module
 * only declares the shape, defaults, and the validation functions the UI
 * will call.
 *
 * Stubs below return placeholder values so the file compiles and tests
 * can be written against the contract. The real validation logic is the
 * next PR.
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

/** Step 8 — Start on boot. Preview-only in the dummy. */
export type StartOnBootDraft = {
  enabled?: boolean;
  /** Human-readable unit/plist/task name shown in the preview. */
  serviceName?: string;
  /** The enable command shown in the preview. */
  command?: string;
};

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

export type PipelineDraft = {
  enabled?: boolean;
  orchestratorPrompt?: string;
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
  startOnBoot: StartOnBootDraft;
  pipeline: PipelineDraft;
  bashEnv: BashEnvDraft;
  context: ContextDraft;
  memory: MemoryDraft;
  costTracking?: boolean;
  timeouts: TimeoutsDraft;
  http: HttpDraft;
  agentMaxTurns?: number;
  mcpServers: Array<{
    name?: string;
    command?: string;
    args?: string[];
    url?: string;
    /** type: stdio | streamable-http. */
    type?: string;
    auth?: { type?: string; token?: string };
  }>;
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
    startOnBoot: {},
    pipeline: {},
    bashEnv: {},
    context: {},
    memory: {},
    timeouts: {},
    http: {},
    mcpServers: [],
  };
}

// ---------- helpers ---------------------------------------------------------

/** Alias charset from agents.md: `^[A-Za-z0-9_./:-]+$`. */
export const ALIAS_PATTERN = /^[A-Za-z0-9_./:-]+$/;

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
  // Aliases from the legacy single-provider block live on the model too; the
  // dummy currently doesn't surface them, but collectors should be ready.
  void draft;
  return out;
}

// ---------- validators ------------------------------------------------------

/** Errors from step 4 (Models). Empty array = valid. */
export function validateModels(draft: SetupDraft): string[] {
  // PLACEHOLDER — real implementation runs the four documented rules:
  //   - alias charset ^[A-Za-z0-9_./:-]+$
  //   - "unknown" reserved
  //   - aliases unique across all providers
  //   - default_model required iff >=1 model declared, must reference one
  void draft;
  return [];
}

/** Errors from step 6 (Services). Empty array = valid. The XOR on the
 *  `tools` filter (disabled vs only) is the documented hard rule. */
export function validateServices(draft: SetupDraft): string[] {
  // PLACEHOLDER — real implementation enforces tools.disabled XOR tools.only.
  void draft;
  return [];
}

/** Errors from step 9 (Multi-agent). Re-runs after step 4 edits; role
 *  `model` aliases must resolve against the live models slice. */
export function validateMultiAgent(draft: SetupDraft): string[] {
  // PLACEHOLDER — real implementation checks every pipeline.agents[*].model
  // against collectAliases(draft); flags references to aliases that no
  // longer exist after the user edited step 4.
  void draft;
  return [];
}

/** Errors from step 7 (Access). Empty array = valid. */
export function validateAccess(draft: SetupDraft): string[] {
  // PLACEHOLDER — real implementation rejects mode = "lan" with no token.
  void draft;
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

/** Classify a top-level config key. Unrecognised keys default to
 *  "boot-only" (conservative — telling the user "restart" is safer than
 *  silently telling them a change is hot-reloadable when it isn't). */
export function classifyChange(key: string): RestartImpact {
  // PLACEHOLDER — real implementation reads key against the documented
  // reload-safe / boot-only table in agents.md.
  void key;
  return "boot-only";
}