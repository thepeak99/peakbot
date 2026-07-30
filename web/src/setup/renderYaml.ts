/**
 * renderYaml — turns a SetupDraft into the YAML string the review page shows
 * and the backend (eventually) writes to config.yaml.
 *
 * Faithful to the key names and nesting documented in agents.md:
 *   providers: [{ name, type, api_key, base_url?, models: [{ name, alias?,
 *   max_tokens?, temperature?, vision?, context_window_override? }] }]
 *   default_model: <alias>
 *   searxng / vector_db / tools / bash.env / context / cost_tracking /
 *   memory / timeouts / http / web / pipeline
 *
 * Two deliberate rules:
 *  * Only *documented* keys are emitted. Things the wizard collects that have
 *    no config key yet (locations, persona, start-on-boot) are listed by the
 *    review page instead of being invented here — the output has to be YAML
 *    the binary would actually accept.
 *  * Secrets (api_key, bearer_token, web token) render as `****`. The draft
 *    still holds the real value; only this rendering is masked, so a review
 *    page — or a copied YAML — can never leak a key.
 */

import type { SetupDraft } from "./draft";

const MASK = "****";

export function renderYaml(draft: SetupDraft): string {
  const out: string[] = [];
  /** Emit one line at `indent` levels of two spaces. */
  const push = (indent: number, text: string) =>
    out.push("  ".repeat(indent) + text);
  /** Emit `key: |` plus the text as a block scalar — prompts are free text
   *  and often multi-line, so quoting/escaping them is not worth it. */
  const pushBlock = (indent: number, key: string, text: string) => {
    push(indent, `${key}: |`);
    for (const line of text.split("\n")) push(indent + 1, line);
  };
  /** Emit a block sequence of plain strings. */
  const pushList = (indent: number, key: string, items: string[]) => {
    push(indent, `${key}:`);
    for (const item of items) push(indent + 1, `- ${item}`);
  };

  if (draft.providers.length > 0) {
    push(0, "providers:");
    for (const p of draft.providers) {
      push(1, `- name: ${p.name ?? ""}`);
      if (p.type) push(2, `type: ${p.type}`);
      if (p.apiKey) push(2, `api_key: ${MASK}`);
      if (p.baseUrl) push(2, `base_url: ${p.baseUrl}`);
      const models = p.models ?? [];
      if (models.length > 0) {
        push(2, "models:");
        for (const m of models) {
          push(3, `- name: ${m.name ?? ""}`);
          if (m.alias) push(4, `alias: ${m.alias}`);
          if (m.maxTokens !== undefined) push(4, `max_tokens: ${m.maxTokens}`);
          if (m.temperature !== undefined) {
            push(4, `temperature: ${m.temperature}`);
          }
          if (m.vision !== undefined) push(4, `vision: ${m.vision}`);
          if (m.contextWindowOverride !== undefined) {
            push(4, `context_window_override: ${m.contextWindowOverride}`);
          }
        }
      }
    }
  }
  if (draft.defaultModel) push(0, `default_model: ${draft.defaultModel}`);

  const searxng = draft.services.searxng;
  if (searxng && (searxng.baseUrl || searxng.enabled !== undefined)) {
    push(0, "searxng:");
    if (searxng.baseUrl) push(1, `base_url: ${searxng.baseUrl}`);
    if (searxng.enabled !== undefined) push(1, `enabled: ${searxng.enabled}`);
    if (searxng.timeoutSeconds !== undefined) {
      push(1, `timeout_seconds: ${searxng.timeoutSeconds}`);
    }
    if (searxng.maxResults !== undefined) {
      push(1, `max_results: ${searxng.maxResults}`);
    }
    if (searxng.bearerToken) push(1, `bearer_token: ${MASK}`);
  }

  const vectorDb = draft.services.vectorDb;
  if (vectorDb && (vectorDb.enabled !== undefined || vectorDb.dbPath)) {
    push(0, "vector_db:");
    if (vectorDb.enabled !== undefined) push(1, `enabled: ${vectorDb.enabled}`);
    if (vectorDb.dbPath) push(1, `db_path: ${vectorDb.dbPath}`);
    const emb = vectorDb.embeddings;
    if (emb && (emb.baseUrl || emb.model || emb.dimensions !== undefined)) {
      push(1, "embeddings:");
      if (emb.baseUrl) push(2, `base_url: ${emb.baseUrl}`);
      if (emb.apiKey) push(2, `api_key: ${MASK}`);
      if (emb.model) push(2, `model: ${emb.model}`);
      if (emb.dimensions !== undefined) {
        push(2, `dimensions: ${emb.dimensions}`);
      }
    }
  }

  // tools is XOR by contract (validateServices); render whichever list is set.
  const tools = draft.services.tools;
  if (tools && (tools.disabled?.length ?? 0) > 0) {
    push(0, "tools:");
    pushList(1, "disabled", tools.disabled ?? []);
  } else if (tools && (tools.only?.length ?? 0) > 0) {
    push(0, "tools:");
    pushList(1, "only", tools.only ?? []);
  }

  const bashEnv = Object.entries(draft.bashEnv);
  if (bashEnv.length > 0) {
    push(0, "bash:");
    push(1, "env:");
    // Env values are always quoted: "1" must stay a string, not become an int.
    for (const [k, v] of bashEnv) push(2, `${k}: ${JSON.stringify(v)}`);
  }

  const ctx = draft.context;
  if (
    ctx.enabled !== undefined ||
    ctx.threshold !== undefined ||
    ctx.keepRecent !== undefined ||
    ctx.contextWindow !== undefined
  ) {
    push(0, "context:");
    if (ctx.enabled !== undefined) push(1, `enabled: ${ctx.enabled}`);
    if (ctx.threshold !== undefined) push(1, `threshold: ${ctx.threshold}`);
    if (ctx.keepRecent !== undefined) push(1, `keep_recent: ${ctx.keepRecent}`);
    if (ctx.contextWindow !== undefined) {
      push(1, `context_window: ${ctx.contextWindow}`);
    }
  }

  if (draft.costTracking) push(0, "cost_tracking: true");
  if (draft.agentMaxTurns !== undefined) {
    push(0, `agent_max_turns: ${draft.agentMaxTurns}`);
  }

  const memory = draft.memory;
  if (memory.enabled !== undefined || memory.thresholdBytes !== undefined) {
    push(0, "memory:");
    if (memory.enabled !== undefined) push(1, `enabled: ${memory.enabled}`);
    if (memory.thresholdBytes !== undefined) {
      push(1, `threshold_bytes: ${memory.thresholdBytes}`);
    }
  }

  const timeouts = draft.timeouts;
  if (timeouts.toolSecs !== undefined || timeouts.delegateSecs !== undefined) {
    push(0, "timeouts:");
    if (timeouts.toolSecs !== undefined) {
      push(1, `tool_secs: ${timeouts.toolSecs}`);
    }
    if (timeouts.delegateSecs !== undefined) {
      push(1, `delegate_secs: ${timeouts.delegateSecs}`);
    }
  }

  const http = draft.http;
  if (
    http.connectTimeoutSecs !== undefined ||
    http.readTimeoutSecs !== undefined
  ) {
    push(0, "http:");
    if (http.connectTimeoutSecs !== undefined) {
      push(1, `connect_timeout_secs: ${http.connectTimeoutSecs}`);
    }
    if (http.readTimeoutSecs !== undefined) {
      push(1, `read_timeout_secs: ${http.readTimeoutSecs}`);
    }
  }

  // Local-only is the binary's default bind, so it needs no web: block.
  if (draft.access.mode === "lan") {
    push(0, "web:");
    if (draft.access.bindAddress) push(1, `bind: ${draft.access.bindAddress}`);
    if (draft.access.token) push(1, `token: ${MASK}`);
    if (draft.access.tls !== undefined) push(1, `tls: ${draft.access.tls}`);
  }

  if (draft.pipeline.enabled) {
    push(0, "pipeline:");
    push(1, "enabled: true");
    if (draft.pipeline.orchestratorPrompt) {
      pushBlock(1, "orchestrator_prompt", draft.pipeline.orchestratorPrompt);
    }
    // agents is a map keyed by role, so a nameless role has nowhere to live.
    const agents = (draft.pipeline.agents ?? []).filter((a) => a.role);
    if (agents.length > 0) {
      push(1, "agents:");
      for (const a of agents) {
        push(2, `${a.role}:`);
        if (a.model) push(3, `model: ${a.model}`);
        if (a.prompt) pushBlock(3, "prompt", a.prompt);
        const skills = a.skills;
        if (skills?.enabled === false) {
          push(3, "skills:");
          push(4, "enabled: false");
        } else if ((skills?.only?.length ?? 0) > 0) {
          push(3, "skills:");
          pushList(4, "only", skills?.only ?? []);
        } else if ((skills?.disabled?.length ?? 0) > 0) {
          push(3, "skills:");
          pushList(4, "disabled", skills?.disabled ?? []);
        }
        if (a.agentsMd) push(3, "agents_md: true");
        const env = Object.entries(a.env ?? {});
        if (env.length > 0) {
          push(3, "env:");
          for (const [k, v] of env) push(4, `${k}: ${JSON.stringify(v)}`);
        }
      }
    }
  }

  return out.length > 0 ? out.join("\n") + "\n" : "";
}
