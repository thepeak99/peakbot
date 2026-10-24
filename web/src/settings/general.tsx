// D6 "General" pane (spec §9.7, contract.ts "./general (T29)"). One module:
// the pure helpers (accessModeOf / bindForAccess / generateToken /
// launchCommand) and the <GeneralPane/> that uses them.
//
// The pane reads the draft via useSettingsCtx() and writes by segment array
// under ["general", …] — the network keys stay FLAT (general.retry /
// general.timeouts / general.http / general.uploads — NEVER general.network,
// spec §9.7 card 6). Numbers are written as numbers; empty = key removed.
// Repo scope: read-only (general-readonly banner, every control disabled,
// no edits).

import { useSettingsCtx } from "./context";
import { get, type Doc } from "./yamlDoc";
import { errAt } from "./paths";
import { Badge, Field, SecretInput, Toggle } from "./widgets";
import type { Seg } from "./types";

// ── pure helpers (contract ./general) ────────────────────────────────────

export type AccessMode = "local" | "lan";

const DEFAULT_WEB_PORT = 7823;

/** The host of a `host:port` / `[v6]:port` bind string. */
function hostOf(bind: string): string {
  const bracket = bind.match(/^\[([^\]]*)\]/);
  if (bracket) return bracket[1];
  const i = bind.lastIndexOf(":");
  return i === -1 ? bind : bind.slice(0, i);
}

/**
 * loopback host (127.x.x.x, localhost, [::1]) or absent → "local";
 * anything else (0.0.0.0, [::], 192.168.x.x …) → "lan".
 */
export function accessModeOf(bind: string | undefined): AccessMode {
  if (bind === undefined || bind === "") return "local";
  const host = hostOf(bind);
  if (host === "localhost" || host === "::1") return "local";
  if (/^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host)) return "local";
  return "lan";
}

/** The port of a bind string; absent / unparseable → 7823. */
function portOf(bind: string | undefined): number {
  const m = bind?.match(/:(\d+)$/);
  if (m) {
    const p = Number(m[1]);
    if (Number.isFinite(p) && p >= 1 && p <= 65535) return p;
  }
  return DEFAULT_WEB_PORT;
}

/** "local" → `127.0.0.1:<port>`, "lan" → `0.0.0.0:<port>`, keeping the port. */
export function bindForAccess(bind: string | undefined, mode: AccessMode): string {
  return `${mode === "local" ? "127.0.0.1" : "0.0.0.0"}:${portOf(bind)}`;
}

/** 32 lowercase hex chars from 16 bytes of crypto.getRandomValues. */
export function generateToken(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** `peakbot --bind <bind>[ --tls][ --token ••••]` (the launch preview). */
export function launchCommand(o: { bind: string; tls: boolean; hasToken: boolean }): string {
  let s = `peakbot --bind ${o.bind}`;
  if (o.tls) s += " --tls";
  if (o.hasToken) s += " --token ••••";
  return s;
}

// ── derivations ──────────────────────────────────────────────────────────

function asRecord(v: unknown): Record<string, unknown> {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return {};
  return v as Record<string, unknown>;
}

function asString(v: unknown): string | undefined {
  return typeof v === "string" && v !== "" ? v : undefined;
}

function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

/** Model aliases in declaration order (the compaction_model select). */
function modelOptionsOf(doc: Doc): { alias: string; qualified: string }[] {
  const providers = get(doc, ["providers"]);
  if (!Array.isArray(providers)) return [];
  const out: { alias: string; qualified: string }[] = [];
  for (const p of providers) {
    const pname = asString(asRecord(p).name) ?? "";
    const models = asRecord(p).models;
    if (!Array.isArray(models)) continue;
    for (const m of models) {
      const alias = asString(asRecord(m).alias);
      if (alias === undefined) continue;
      out.push({ alias, qualified: `${pname}/${asString(asRecord(m).name) ?? ""}` });
    }
  }
  return out;
}

const CARD = "rounded-lg border border-zinc-800 bg-zinc-900/40 p-3";
const CARD_HEAD = "mb-2 flex items-baseline justify-between gap-2";
const CARD_H3 = "text-sm font-semibold";
const CARD_SUB = "text-[10px] text-zinc-500";
const SELECT_CLS =
  "w-full cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";
const GHOST_BTN =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60";

// ── pane ─────────────────────────────────────────────────────────────────

export function GeneralPane() {
  const { scope, meta, doc, edit, errors } = useSettingsCtx();
  // general: is global-only — repo scope renders the pane read-only (🔒).
  const readOnly = scope === "repo";

  const g = (rest: Seg[]) => get(doc, ["general", ...rest]);
  const web = asRecord(g(["web"]));
  const bind = asString(web.bind);
  const mode = accessModeOf(bind);
  const token = asString(web.token);
  const tls = web.tls === true;
  const source = meta?.boot.token_source ?? "none";
  const overridden = source === "env" || source === "cli";
  const hasEffectiveToken = token !== undefined || overridden;
  // W1 preview (spec §9.7): a LAN bind with no token anywhere in the
  // effective chain (config, env, CLI).
  const w1Hint = mode === "lan" && token === undefined && !overridden;
  const webTokenError = errAt(errors, ["general", "web", "token"]);

  const search = g(["search"]);
  const searchOn = search !== undefined && search !== null;
  const searchRec = asRecord(search);

  const ctx = asRecord(g(["context"]));
  const conv = asRecord(g(["conversation"]));
  const retry = asRecord(g(["retry"]));
  const timeouts = asRecord(g(["timeouts"]));
  const http = asRecord(g(["http"]));
  const uploads = asRecord(g(["uploads"]));

  const options = modelOptionsOf(doc);
  const compactionModel = asString(ctx.compaction_model);
  const maxTurns = asNumber(g(["agent_max_turns"]));
  const costTracking = g(["cost_tracking"]) !== false;

  function ed(path: Seg[], value: unknown) {
    if (readOnly) return;
    edit(path, value);
  }

  return (
    <div data-testid="general-pane" className="space-y-4">
      {readOnly && (
        <div
          data-testid="general-readonly"
          className="rounded-lg border border-zinc-700 bg-zinc-900/60 px-3 py-2 text-xs text-zinc-400"
        >
          🔒 General is machine-wide — read-only in this repo. Switch to Global to edit.
        </div>
      )}

      <fieldset disabled={readOnly || undefined} className="m-0 min-w-0 space-y-4 border-0 p-0">
        {/* 1. Web server (RESTART REQUIRED) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Web server</h3>
            <Badge testid="general-badge-restart-web" tone="amber">
              RESTART REQUIRED
            </Badge>
          </div>

          <div className="mb-3 grid gap-2 sm:grid-cols-2">
            <label
              className={`flex cursor-pointer items-start gap-2 rounded-lg border px-3 py-2 text-xs ${
                mode === "local"
                  ? "border-emerald-700/70 bg-emerald-950/40 text-emerald-200"
                  : "border-zinc-700 bg-zinc-900 text-zinc-400"
              }`}
            >
              <input
                data-testid="general-access-local"
                type="radio"
                name="general-access"
                checked={mode === "local"}
                onChange={() => ed(["general", "web", "bind"], bindForAccess(bind, "local"))}
                className="mt-0.5 h-3.5 w-3.5 accent-emerald-600"
              />
              <span>
                Local only
                <span className="block text-[10px] opacity-70">
                  127.0.0.1:7823 — no token needed.
                </span>
              </span>
            </label>
            <label
              className={`flex cursor-pointer items-start gap-2 rounded-lg border px-3 py-2 text-xs ${
                mode === "lan"
                  ? "border-emerald-700/70 bg-emerald-950/40 text-emerald-200"
                  : "border-zinc-700 bg-zinc-900 text-zinc-400"
              }`}
            >
              <input
                data-testid="general-access-lan"
                type="radio"
                name="general-access"
                checked={mode === "lan"}
                onChange={() => ed(["general", "web", "bind"], bindForAccess(bind, "lan"))}
                className="mt-0.5 h-3.5 w-3.5 accent-emerald-600"
              />
              <span>
                Reachable from other devices
                <span className="block text-[10px] opacity-70">
                  Binds the LAN address — a token is required.
                </span>
              </span>
            </label>
          </div>

          <div className="space-y-3">
            <Field
              testid="general-bind"
              label="bind address"
              value={bind}
              onChange={(v) => ed(["general", "web", "bind"], v)}
              disabled={readOnly}
            />
            <div className="flex flex-wrap items-start gap-2">
              <div className="min-w-52 flex-1">
                <SecretInput
                  testid="general-token"
                  label="access token"
                  value={token}
                  onChange={(v) => ed(["general", "web", "token"], v)}
                  hint="CLI --token > PEAKBOT_WEB_TOKEN > this."
                  invalid={webTokenError}
                  disabled={readOnly}
                  placeholder={mode === "lan" ? "•••• (required for LAN)" : "•••• (none — local only)"}
                />
              </div>
              <button
                data-testid="general-token-generate"
                type="button"
                disabled={readOnly}
                onClick={() => ed(["general", "web", "token"], generateToken())}
                className={GHOST_BTN}
              >
                Generate
              </button>
              <button
                data-testid="general-token-copy"
                type="button"
                disabled={readOnly || token === undefined}
                onClick={() => {
                  if (token !== undefined && navigator.clipboard) {
                    navigator.clipboard.writeText(token);
                  }
                }}
                className={GHOST_BTN}
              >
                Copy
              </button>
            </div>
            {source === "env" && (
              <span data-testid="general-token-note" className="block text-[10px] text-amber-400/80">
                Currently overridden by PEAKBOT_WEB_TOKEN.
              </span>
            )}
            {source === "cli" && (
              <span data-testid="general-token-note" className="block text-[10px] text-amber-400/80">
                Currently overridden by --token.
              </span>
            )}
            {w1Hint && (
              <span
                data-testid="general-token-w1"
                className="block text-[10px] text-amber-400/80"
              >
                A LAN bind requires a token — generate one, or switch back to local only.
              </span>
            )}
            <Toggle
              testid="general-tls"
              label="Serve over HTTPS (built-in CA)"
              checked={tls}
              onChange={(c) => ed(["general", "web", "tls"], c)}
              hint="Install the CA on phones from /peakbot-ca.crt. A fresh leaf is minted each boot."
              disabled={readOnly}
            />
            <Field
              testid="general-ttl"
              label="session_ttl_secs"
              type="number"
              value={asNumber(web.session_ttl_secs)}
              onChange={(v) => ed(["general", "web", "session_ttl_secs"], v)}
              hint="Kill a session this long after it goes idle."
              disabled={readOnly}
            />
            <Field
              testid="general-reaper"
              label="reaper_tick_secs"
              type="number"
              value={asNumber(web.reaper_tick_secs)}
              onChange={(v) => ed(["general", "web", "reaper_tick_secs"], v)}
              hint="How often the reaper scans for expired sessions."
              disabled={readOnly}
            />
            <div>
              <span className="mb-1 block text-[11px] text-zinc-400">Launch command (preview)</span>
              <code
                data-testid="general-launch"
                className="block rounded border border-zinc-800 bg-zinc-950 px-2 py-1.5 font-mono text-[11px] text-zinc-300"
              >
                {launchCommand({ bind: bind ?? "127.0.0.1:7823", tls, hasToken: hasEffectiveToken })}
              </code>
            </div>
          </div>
        </div>

        {/* 3. Search (SearXNG; presence = on; no restart badge) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Search</h3>
            <span className={CARD_SUB}>SearXNG backend for web_search</span>
          </div>
          <Toggle
            testid="general-search-toggle"
            label="Enable web search"
            checked={searchOn}
            onChange={(c) => ed(["general", "search"], c ? {} : undefined)}
            hint="Presence = on. Off removes the whole search block."
            disabled={readOnly}
          />
          {searchOn && (
            <div className="mt-3 space-y-3">
              <Field
                testid="general-search-baseurl"
                label="base_url"
                value={asString(searchRec.base_url)}
                onChange={(v) => ed(["general", "search", "base_url"], v)}
                disabled={readOnly}
              />
              <SecretInput
                testid="general-search-bearer"
                label="bearer_token"
                value={asString(searchRec.bearer_token)}
                onChange={(v) => ed(["general", "search", "bearer_token"], v)}
                hint="For instances behind an auth proxy."
                disabled={readOnly}
              />
              <Field
                testid="general-search-timeout"
                label="timeout_seconds"
                type="number"
                value={asNumber(searchRec.timeout_seconds)}
                onChange={(v) => ed(["general", "search", "timeout_seconds"], v)}
                hint="s"
                disabled={readOnly}
              />
              <Field
                testid="general-search-maxresults"
                label="max_results"
                type="number"
                value={asNumber(searchRec.max_results)}
                onChange={(v) => ed(["general", "search", "max_results"], v)}
                hint="results"
                disabled={readOnly}
              />
            </div>
          )}
        </div>

        {/* 4. Context compaction (no restart badge) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Context compaction</h3>
          </div>
          <div className="space-y-3">
            <Toggle
              testid="general-context-enabled"
              label="enabled"
              checked={ctx.enabled !== false}
              onChange={(c) => ed(["general", "context", "enabled"], c)}
              hint="Compact the conversation when it crosses the threshold."
              disabled={readOnly}
            />
            <Field
              testid="general-context-threshold"
              label="threshold"
              type="number"
              value={asNumber(ctx.threshold)}
              onChange={(v) => ed(["general", "context", "threshold"], v)}
              hint="0.5–1.0 — empty = default 0.8"
              disabled={readOnly}
            />
            <Field
              testid="general-context-keep"
              label="keep_recent"
              type="number"
              value={asNumber(ctx.keep_recent)}
              onChange={(v) => ed(["general", "context", "keep_recent"], v)}
              hint="msgs"
              disabled={readOnly}
            />
            <div data-testid="general-context-model" className="min-w-0">
              <span className="mb-1 block text-[11px] text-zinc-400">compaction_model</span>
              <select
                data-testid="input"
                value={compactionModel ?? ""}
                disabled={readOnly}
                onChange={(e) =>
                  ed(
                    ["general", "context", "compaction_model"],
                    e.target.value === "" ? undefined : e.target.value,
                  )
                }
                className={SELECT_CLS}
              >
                <option value="">(main model)</option>
                {options.map((o) => (
                  <option key={o.alias} value={o.alias}>
                    {o.alias} — {o.qualified}
                  </option>
                ))}
              </select>
              <span className="mt-1 block text-[10px] text-zinc-500">Omitted → main model.</span>
            </div>
          </div>
        </div>

        {/* 5. Conversations (RESTART REQUIRED) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Conversations</h3>
            <Badge testid="general-badge-restart-conversation" tone="amber">
              RESTART REQUIRED
            </Badge>
          </div>
          <div className="space-y-3">
            <Toggle
              testid="general-conv-autosave"
              label="auto_save"
              checked={conv.auto_save !== false}
              onChange={(c) => ed(["general", "conversation", "auto_save"], c)}
              hint="Save conversations to disk."
              disabled={readOnly}
            />
            <Field
              testid="general-conv-dir"
              label="storage_dir"
              value={asString(conv.storage_dir)}
              onChange={(v) => ed(["general", "conversation", "storage_dir"], v)}
              hint="Empty = the platform data dir."
              disabled={readOnly}
            />
          </div>
        </div>

        {/* 6. Network (visual grouping only — the YAML keys stay FLAT) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Network</h3>
            <span className={CARD_SUB}>retry · timeouts · http (flat keys)</span>
          </div>
          <div className="grid gap-4 sm:grid-cols-3">
            <div className="space-y-3">
              <span className="block text-[11px] font-medium text-zinc-400">Retry</span>
              <Field
                testid="general-retry-max"
                label="max_retries"
                type="number"
                value={asNumber(retry.max_retries)}
                onChange={(v) => ed(["general", "retry", "max_retries"], v)}
                disabled={readOnly}
              />
              <Field
                testid="general-retry-initial"
                label="initial_delay_ms"
                type="number"
                value={asNumber(retry.initial_delay_ms)}
                onChange={(v) => ed(["general", "retry", "initial_delay_ms"], v)}
                disabled={readOnly}
              />
              <Field
                testid="general-retry-maxdelay"
                label="max_delay_ms"
                type="number"
                value={asNumber(retry.max_delay_ms)}
                onChange={(v) => ed(["general", "retry", "max_delay_ms"], v)}
                disabled={readOnly}
              />
              <Field
                testid="general-retry-backoff"
                label="backoff_factor"
                type="number"
                value={asNumber(retry.backoff_factor)}
                onChange={(v) => ed(["general", "retry", "backoff_factor"], v)}
                disabled={readOnly}
              />
            </div>
            <div className="space-y-3">
              <span className="block text-[11px] font-medium text-zinc-400">Timeouts</span>
              <Field
                testid="general-timeout-tool"
                label="tool_secs"
                type="number"
                value={asNumber(timeouts.tool_secs)}
                onChange={(v) => ed(["general", "timeouts", "tool_secs"], v)}
                hint="One tool call. Range 1..86400."
                disabled={readOnly}
              />
              <Field
                testid="general-timeout-delegate"
                label="delegate_secs"
                type="number"
                value={asNumber(timeouts.delegate_secs)}
                onChange={(v) => ed(["general", "timeouts", "delegate_secs"], v)}
                hint="One delegation. Range 1..86400."
                disabled={readOnly}
              />
            </div>
            <div className="space-y-3">
              <span className="flex items-center gap-2 text-[11px] font-medium text-zinc-400">
                HTTP
                <Badge testid="general-badge-restart-http" tone="amber">
                  RESTART
                </Badge>
              </span>
              <Field
                testid="general-http-connect"
                label="connect_timeout_secs"
                type="number"
                value={asNumber(http.connect_timeout_secs)}
                onChange={(v) => ed(["general", "http", "connect_timeout_secs"], v)}
                hint="0 = disabled."
                disabled={readOnly}
              />
              <Field
                testid="general-http-read"
                label="read_timeout_secs"
                type="number"
                value={asNumber(http.read_timeout_secs)}
                onChange={(v) => ed(["general", "http", "read_timeout_secs"], v)}
                hint="0 = disabled."
                disabled={readOnly}
              />
            </div>
          </div>
        </div>

        {/* 7. Uploads (RESTART REQUIRED) */}
        <div className={CARD}>
          <div className={CARD_HEAD}>
            <h3 className={CARD_H3}>Uploads</h3>
            <Badge testid="general-badge-restart-uploads" tone="amber">
              RESTART REQUIRED
            </Badge>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              testid="general-uploads-maxfile"
              label="max_file_mb"
              type="number"
              value={asNumber(uploads.max_file_mb)}
              onChange={(v) => ed(["general", "uploads", "max_file_mb"], v)}
              hint="Per-file ceiling. Range 1..1024."
              disabled={readOnly}
            />
            <Field
              testid="general-uploads-maxfiles"
              label="max_files"
              type="number"
              value={asNumber(uploads.max_files)}
              onChange={(v) => ed(["general", "uploads", "max_files"], v)}
              hint="Per-message ceiling. Range 1..100."
              disabled={readOnly}
            />
          </div>
        </div>

        {/* 8. agent_max_turns + cost_tracking */}
        <div className={CARD}>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              testid="general-max-turns"
              label="agent_max_turns"
              type="number"
              value={maxTurns}
              onChange={(v) => ed(["general", "agent_max_turns"], v)}
              hint="Max tool turns per message."
              disabled={readOnly}
            />
            <Toggle
              testid="general-cost-tracking"
              label="cost_tracking"
              checked={costTracking}
              onChange={(c) => ed(["general", "cost_tracking"], c)}
              hint="Track token spend."
              disabled={readOnly}
            />
          </div>
        </div>
      </fieldset>
    </div>
  );
}
