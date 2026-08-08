/**
 * renderYaml — turns a SetupDraft into the YAML string the review page shows
 * and the backend (eventually) writes to config.yaml.
 *
 * Faithful to the key names and nesting documented in agents.md:
 *   providers: [{ name, type, api_key, base_url?, models: [{ name, alias?,
 *   max_tokens?, temperature?, vision?, context_window_override? }] }]
 *   default_model: <alias>
 *   searxng / vector_db / tools / bash.env / context / cost_tracking /
 *   memory / timeouts / http / web / pipelines
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

import { PERSONA_PRESETS } from "./catalog";
import { importedPipelines, pipelineName } from "./draft";
import type { SetupDraft } from "./draft";

const MASK = "****";

export type RenderOptions = { mask?: boolean };

export function renderYaml(draft: SetupDraft, options: RenderOptions = {}): string {
  const mask = options.mask ?? true;
  const secret = (value: string) => (mask ? MASK : JSON.stringify(value));
  const scalar = (value: string) => JSON.stringify(value);
  const out: string[] = [];
  const push = (indent: number, text: string) => out.push("  ".repeat(indent) + text);
  const pushBlock = (indent: number, key: string, text: string) => {
    const normalized = text.replace(/\r\n?/g, "\n");
    push(indent, `${key}: |2-`);
    for (const line of normalized.split("\n")) push(indent + 1, line);
  };
  const pushList = (indent: number, key: string, items: string[]) => {
    push(indent, `${key}:`);
    for (const item of items) push(indent + 1, `- ${scalar(item)}`);
  };

  if (draft.providers.length > 0) {
    push(0, "providers:");
    for (const p of draft.providers) {
      push(1, `- name: ${scalar(p.name ?? "")}`);
      if (p.type) push(2, `type: ${p.type}`);
      if (p.apiKey) push(2, `api_key: ${mask ? MASK : scalar(p.apiKey)}`);
      if (p.baseUrl) push(2, `base_url: ${scalar(p.baseUrl)}`);
      const models = p.models ?? [];
      if (models.length) {
        push(2, "models:");
        for (const m of models) {
          push(3, `- name: ${scalar(m.name ?? "")}`);
          if (m.alias) push(4, `alias: ${scalar(m.alias)}`);
          if (m.maxTokens !== undefined) push(4, `max_tokens: ${m.maxTokens}`);
          if (m.temperature !== undefined) push(4, `temperature: ${m.temperature}`);
          if (m.vision !== undefined) push(4, `vision: ${m.vision}`);
          if (m.contextWindowOverride !== undefined) push(4, `context_window_override: ${m.contextWindowOverride}`);
          if (m.numCtx !== undefined) push(4, `num_ctx: ${m.numCtx}`);
          if (m.extraParams && Object.keys(m.extraParams).length) {
            push(4, "extra_params:");
            for (const [k, v] of Object.entries(m.extraParams)) push(5, `${k}: ${typeof v === "string" ? scalar(v) : JSON.stringify(v)}`);
          }
          if (m.promptCaching) push(4, `prompt_caching: ${m.promptCaching}`);
        }
      }
    }
  }
  if (draft.defaultModel) push(0, `default_model: ${scalar(draft.defaultModel)}`);
  const persona = personaText(draft);
  if (persona) pushBlock(0, "persona", persona);

  const s = draft.services.searxng;
  if (s && (s.baseUrl || s.enabled !== undefined || s.bearerToken)) {
    push(0, "searxng:");
    if (s.baseUrl) push(1, `base_url: ${scalar(s.baseUrl)}`);
    if (s.enabled !== undefined) push(1, `enabled: ${s.enabled}`);
    if (s.timeoutSeconds !== undefined) push(1, `timeout_seconds: ${s.timeoutSeconds}`);
    if (s.maxResults !== undefined) push(1, `max_results: ${s.maxResults}`);
    if (s.bearerToken) push(1, `bearer_token: ${secret(s.bearerToken)}`);
  }
  const v = draft.services.vectorDb;
  if (v && (v.enabled !== undefined || v.dbPath || v.embeddings)) {
    push(0, "vector_db:");
    if (v.enabled !== undefined) push(1, `enabled: ${v.enabled}`);
    if (v.dbPath) push(1, `db_path: ${scalar(v.dbPath)}`);
    const e = v.embeddings;
    if (e && (e.baseUrl || e.apiKey || e.model || e.dimensions !== undefined)) {
      push(1, "embeddings:");
      if (e.baseUrl) push(2, `base_url: ${scalar(e.baseUrl)}`);
      if (e.apiKey) push(2, `api_key: ${secret(e.apiKey)}`);
      if (e.model) push(2, `model: ${scalar(e.model)}`);
      if (e.dimensions !== undefined) push(2, `dimensions: ${e.dimensions}`);
    }
  }
  const tools = draft.services.tools;
  if (tools?.disabled?.length) { push(0, "tools:"); pushList(1, "disabled", tools.disabled); }
  else if (tools?.only?.length) { push(0, "tools:"); pushList(1, "only", tools.only); }
  const env = Object.entries(draft.bashEnv);
  if (env.length) { push(0, "bash:"); push(1, "env:"); for (const [k, v] of env) push(2, `${k}: ${scalar(v)}`); }
  const c = draft.context;
  if (c.enabled !== undefined || c.threshold !== undefined || c.keepRecent !== undefined || c.contextWindow !== undefined) {
    push(0, "context:"); if (c.enabled !== undefined) push(1, `enabled: ${c.enabled}`); if (c.threshold !== undefined) push(1, `threshold: ${c.threshold}`); if (c.keepRecent !== undefined) push(1, `keep_recent: ${c.keepRecent}`); if (c.contextWindow !== undefined) push(1, `context_window: ${c.contextWindow}`);
  }
  if (draft.costTracking) push(0, "cost_tracking: true");
  if (draft.agentMaxTurns !== undefined) push(0, `agent_max_turns: ${draft.agentMaxTurns}`);
  const mem = draft.memory;
  if (mem.enabled !== undefined || mem.thresholdBytes !== undefined) { push(0, "memory:"); if (mem.enabled !== undefined) push(1, `enabled: ${mem.enabled}`); if (mem.thresholdBytes !== undefined) push(1, `threshold_bytes: ${mem.thresholdBytes}`); }
  const t = draft.timeouts;
  if (t.toolSecs !== undefined || t.delegateSecs !== undefined) { push(0, "timeouts:"); if (t.toolSecs !== undefined) push(1, `tool_secs: ${t.toolSecs}`); if (t.delegateSecs !== undefined) push(1, `delegate_secs: ${t.delegateSecs}`); }
  const h = draft.http;
  if (h.connectTimeoutSecs !== undefined || h.readTimeoutSecs !== undefined) { push(0, "http:"); if (h.connectTimeoutSecs !== undefined) push(1, `connect_timeout_secs: ${h.connectTimeoutSecs}`); if (h.readTimeoutSecs !== undefined) push(1, `read_timeout_secs: ${h.readTimeoutSecs}`); }
  if (draft.access.tls !== undefined) { push(0, "web:"); push(1, `tls: ${draft.access.tls}`); }
  // An imported `pipelines:` list rides along in passthrough and is emitted
  // verbatim below — emitting the wizard's entry too would produce a duplicate
  // top-level key (and silently drop one of the two teams).
  const pipe = draft.pipeline;
  if (pipe.include && !importedPipelines(draft)) {
    push(0, "pipelines:");
    push(1, `- name: ${scalar(pipelineName(pipe))}`);
    // `orchestrator:` is required by the binary, and a childless key would
    // parse as null; `{}` is the "all defaults" spelling.
    if (pipe.orchestratorModel || pipe.orchestratorPrompt || pipe.orchestratorPersona) {
      push(2, "orchestrator:");
      if (pipe.orchestratorModel) push(3, `model: ${scalar(pipe.orchestratorModel)}`);
      if (pipe.orchestratorPrompt) pushBlock(3, "prompt", pipe.orchestratorPrompt);
      if (pipe.orchestratorPersona) pushBlock(3, "persona", pipe.orchestratorPersona);
    } else {
      push(2, "orchestrator: {}");
    }
    const agents = (pipe.agents ?? []).filter((a) => a.role);
    if (agents.length) { push(2, "agents:"); for (const a of agents) { push(3, `${a.role}:`); if (a.model) push(4, `model: ${scalar(a.model)}`); if (a.prompt) pushBlock(4, "prompt", a.prompt); if (a.skills?.enabled === false) { push(4, "skills:"); push(5, "enabled: false"); } else if (a.skills?.only?.length) { push(4, "skills:"); pushList(5, "only", a.skills.only); } else if (a.skills?.disabled?.length) { push(4, "skills:"); pushList(5, "disabled", a.skills.disabled); } if (a.agentsMd) push(4, "agents_md: true"); const ae = Object.entries(a.env ?? {}); if (ae.length) { push(4, "env:"); for (const [k, value] of ae) push(5, `${k}: ${scalar(value)}`); } } }
  }
  for (const [key, value] of Object.entries(draft.passthrough)) {
    if (value === undefined) continue;
    push(0, `${key}:`);
    emitValue(push, 1, value);
  }
  return out.length ? `${out.join("\n")}\n` : "";
}

function emitValue(push: (indent: number, text: string) => void, indent: number, value: unknown): void {
  if (value === null) { push(indent, "null"); return; }
  if (typeof value !== "object") {
    push(indent, typeof value === "string" ? JSON.stringify(value) : String(value));
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      if (item && typeof item === "object" && !Array.isArray(item)) {
        const entries = Object.entries(item as Record<string, unknown>);
        if (entries.length === 0) { push(indent, "- {}"); continue; }
        const [firstKey, firstVal] = entries[0];
        push(indent, `- ${firstKey}: ${renderScalar(firstVal)}`);
        for (const [key, child] of entries.slice(1)) emitValue(push, indent + 1, { [key]: child } as unknown);
      } else if (Array.isArray(item)) {
        push(indent, "-");
        emitValue(push, indent + 1, item);
      } else {
        push(indent, `- ${item === null ? "null" : typeof item === "string" ? JSON.stringify(item) : String(item)}`);
      }
    }
    return;
  }
  for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
    if (child && typeof child === "object") { push(indent, `${key}:`); emitValue(push, indent + 1, child); }
    else push(indent, `${key}: ${renderScalar(child)}`);
  }
}

function renderScalar(value: unknown): string {
  if (value === null) return "null";
  if (typeof value === "string") return JSON.stringify(value);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return JSON.stringify(value);
}

export function personaText(draft: SetupDraft): string | undefined {
  if (draft.persona.mode === "custom") return draft.persona.custom?.trim() ? draft.persona.custom : undefined;
  if (draft.persona.mode === "preset") return PERSONA_PRESETS.find((p) => p.id === draft.persona.presetId)?.prompt;
  return undefined;
}
