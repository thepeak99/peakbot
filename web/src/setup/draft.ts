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
  return out;
}

/** Total models declared across every provider — `default_model` is required
 *  iff this is > 0. */
function countModels(draft: SetupDraft): number {
  return draft.providers.reduce((n, p) => n + (p.models?.length ?? 0), 0);
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

/** Errors from step 9 (Multi-agent). Re-runs after step 4 edits; role
 *  `model` aliases must resolve against the live models slice. */
export function validateMultiAgent(draft: SetupDraft): string[] {
  if (!draft.pipeline.enabled) return [];
  const aliases = collectAliases(draft);
  const errors: string[] = [];
  for (const agent of draft.pipeline.agents ?? []) {
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