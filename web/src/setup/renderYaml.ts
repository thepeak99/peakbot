/**
 * renderYaml — turns a SetupDraft into the YAML string the review page
 * shows and the backend (eventually) writes to config.yaml.
 *
 * Faithful to the key names and nesting documented in agents.md:
 *   providers: [{ name, type, api_key, base_url?, models: [{ name,
 *   alias?, max_tokens?, temperature?, vision?, ... }] }]
 *   default_model: <alias>
 *   tools: { disabled: [...] } | { only: [...] }
 *   pipeline: { enabled, orchestrator_prompt, agents: { role: { ... } } }
 *   web: { bind, token, tls }
 *   bash: { env: { ... } }
 *   searxng: { base_url, enabled, ... }
 *   context: { enabled, threshold, keep_recent, context_window }
 *   cost_tracking: true
 *   timeouts: { tool_secs, delegate_secs }
 *   http: { connect_timeout_secs, read_timeout_secs }
 *   memory: { enabled, threshold_bytes }
 *   mcp_servers: [...]
 *
 * Secrets (api_key, bearer tokens, web token) are masked as `****` —
 * the real backend, when it ships, will read the unmasked values from
 * the draft state, not the rendered string.
 *
 * STUB — returns "" so the file compiles and the tests fail (RED).
 */

import type { SetupDraft } from "./draft";

/** Render a SetupDraft as YAML. The dummy writes nothing; this is what
 *  the review page shows and what the future backend will consume. */
export function renderYaml(_draft: SetupDraft): string {
  // PLACEHOLDER — real implementation builds the YAML string above.
  return "";
}