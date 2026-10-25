// D3 "MCP servers" pane (spec §9.9, contract.ts "./mcp (T26)").
//
// Reads the draft via useSettingsCtx(); servers = get(doc, ["mcps"]). The
// real transport key is `type` (NOT the mockup's `transport`), and `auth`
// is a TAGGED map — `{type:"bearer", token}` / `{type:"oauth", client_id?,
// client_secret?, scopes}` — never a scalar, never the legacy `auth_token`
// key. A legacy `auth_token` (when present with no `auth`) renders
// read-only: "Deprecated — use auth: bearer".
//
// Global-only section: in repo scope the pane renders read-only (🔒).

import { useEffect, useState } from "react";
import { useSettingsCtx } from "./context";
import { get } from "./yamlDoc";
import { errAt } from "./paths";
import { Badge, ChipRow, Field, KvRows, SecretInput, Toggle, type KvRow } from "./widgets";
import type { ConfigErr, Seg } from "./types";

const TRANSPORTS = ["stdio", "streamable-http", "sse"] as const;
const AUTH_KINDS = ["none", "bearer", "oauth"] as const;
type AuthKind = (typeof AUTH_KINDS)[number];

const SELECT_CLS =
  "w-full cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";

interface McpDraft {
  name?: unknown;
  type?: unknown;
  enabled?: unknown;
  command?: unknown;
  args?: unknown;
  env?: unknown;
  url?: unknown;
  auth?: unknown;
  auth_token?: unknown;
  headers?: unknown;
}

function asString(v: unknown): string | undefined {
  return typeof v === "string" && v !== "" ? v : undefined;
}
function asStringArray(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/** A map value (env / headers / auth) → editable key/value rows. */
function toRows(v: unknown): KvRow[] {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return [];
  return Object.entries(v as Record<string, unknown>).map(([k, val]) => ({
    key: k,
    value: typeof val === "string" ? val : String(val),
  }));
}

/** Rows → a map to write (empty = key removed, `undefined`). */
function toMap(rows: KvRow[]): Record<string, string> | undefined {
  const kept = rows.filter((r) => r.key.trim() !== "");
  if (kept.length === 0) return undefined;
  const out: Record<string, string> = {};
  for (const r of kept) out[r.key] = r.value;
  return out;
}

/** The `auth` value as a tagged map, or undefined (no auth). */
function authMap(v: unknown): Record<string, unknown> | undefined {
  if (v === null || typeof v !== "object" || Array.isArray(v)) return undefined;
  return v as Record<string, unknown>;
}

export function McpsPane() {
  const { scope, doc, globalDoc, edit, errors, meta, switchToGlobal } = useSettingsCtx();
  // mcps: is global-only — repo scope renders the GLOBAL draft's values
  // read-only (🔒); the repo file may only contain `profiles:`.
  const readOnly = scope === "repo";
  const paneDoc = readOnly ? globalDoc : doc;

  const raw = get(paneDoc, ["mcps"]);
  const servers = (Array.isArray(raw) ? raw : []) as McpDraft[];

  function statusOf(name: string): "connected" | "failed" | "disabled" | undefined {
    return meta?.mcp_status.find((s) => s.name === name)?.state;
  }

  function addServer() {
    if (readOnly) return;
    edit(["mcps"], [...servers, { name: "", type: "stdio" }]);
  }

  return (
    <div className="space-y-4">
      <div
        data-testid="mcp-banner"
        className="rounded-lg border border-amber-800/60 bg-amber-950/40 px-3 py-2 text-xs text-amber-300"
      >
        ⚠ MCP server changes are <strong>restart-required</strong> — they take effect after a
        restart, not on <code>/new</code>.
      </div>

      {readOnly && (
        <div
          data-testid="mcp-readonly"
          className="flex flex-wrap items-center gap-2 rounded-lg border border-zinc-700 bg-zinc-900/60 px-3 py-2 text-xs text-zinc-400"
        >
          <span>🔒 Machine-wide — edit in Global scope. This section is read-only here.</span>
          <button
            data-testid="switch-to-global"
            type="button"
            onClick={switchToGlobal}
            className="cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-sky-400 hover:bg-zinc-800"
          >
            Switch to Global
          </button>
        </div>
      )}

      <div className="space-y-3">
        {servers.map((s, i) => (
          <McpCard
            key={i}
            server={s}
            index={i}
            readOnly={readOnly}
            status={statusOf(asString(s.name) ?? "")}
            onRemove={(idx) => {
              if (!readOnly) edit(["mcps"], servers.filter((_, k) => k !== idx));
            }}
            edit={edit}
            errors={errors}
          />
        ))}
      </div>

      <button
        data-testid="mcp-add"
        type="button"
        disabled={readOnly}
        onClick={addServer}
        className="cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60"
      >
        + Add MCP server
      </button>
    </div>
  );
}

function McpCard({
  server: s,
  index: i,
  readOnly,
  status,
  onRemove,
  edit,
  errors,
}: {
  server: McpDraft;
  index: number;
  readOnly: boolean;
  status: "connected" | "failed" | "disabled" | undefined;
  onRemove(i: number): void;
  edit(path: Seg[], value: unknown): void;
  errors: ConfigErr[];
}) {
  const tp = (seg: string): Seg[] => ["mcps", i, seg];
  const type = asString(s.type) ?? "stdio";
  const remote = type !== "stdio";
  const enabled = s.enabled !== false; // absent reads as true
  const auth = authMap(s.auth);
  const kindFromDoc: AuthKind =
    auth?.type === "bearer" ? "bearer" : auth?.type === "oauth" ? "oauth" : "none";
  // The auth kind composes across edits (select → token → scopes…) the way
  // the user makes them; it follows the draft when the draft changes on its
  // own (discard / raw edit / reload).
  const [kind, setKind] = useState(kindFromDoc);
  useEffect(() => {
    setKind((cur) => (cur === kindFromDoc ? cur : kindFromDoc));
  }, [kindFromDoc]);
  // Legacy `auth_token` (present, no `auth`) → read-only deprecation note.
  const legacyToken = auth === undefined ? asString(s.auth_token) : undefined;

  // Compose the full tagged `auth` map for a kind, keeping the parts the
  // user already entered. Empty client_id/secret are omitted; scopes is
  // always a list (possibly empty).
  function authFor(kind: AuthKind): Record<string, unknown> | undefined {
    if (kind === "none") return undefined;
    if (kind === "bearer") {
      const token = asString(auth?.token);
      return { type: "bearer", ...(token !== undefined ? { token } : {}) };
    }
    return {
      type: "oauth",
      ...(asString(auth?.client_id) !== undefined ? { client_id: asString(auth?.client_id) } : {}),
      ...(asString(auth?.client_secret) !== undefined
        ? { client_secret: asString(auth?.client_secret) }
        : {}),
      scopes: asStringArray(auth?.scopes),
    };
  }

  function writeAuth(k: AuthKind) {
    setKind(k);
    if (readOnly) return;
    edit(tp("auth"), authFor(k));
  }

  const typeError = errAt(errors, tp("type"));
  const authError = errAt(errors, tp("auth"));

  return (
    <div data-testid={`mcp-card-${i}`} className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
      {/* Card head: name · transport tag · status pill · enabled toggle · ✕ */}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold">{asString(s.name) ?? "unnamed server"}</h3>
        <span className="rounded border border-zinc-700 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
          {type}
        </span>
        {status && (
          <Badge
            testid={`mcp-status-${i}`}
            tone={status === "connected" ? "green" : status === "failed" ? "red" : "neutral"}
          >
            {status}
          </Badge>
        )}
        <div className="ml-auto flex items-center gap-2">
          <div className="w-28">
            <Toggle
              testid={`mcp-enabled-${i}`}
              label="enabled"
              checked={enabled}
              onChange={(c) => {
                if (!readOnly) edit(tp("enabled"), c);
              }}
            />
          </div>
          <button
            data-testid={`mcp-remove-${i}`}
            type="button"
            title="Remove server"
            disabled={readOnly}
            onClick={() => onRemove(i)}
            className="cursor-pointer rounded px-1 text-xs text-zinc-500 hover:text-red-400 disabled:cursor-default disabled:opacity-60"
          >
            ✕
          </button>
        </div>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <Field
          testid={`mcp-name-${i}`}
          label="name"
          value={asString(s.name)}
          onChange={(v) => {
            if (!readOnly) edit(tp("name"), v);
          }}
          disabled={readOnly}
          invalid={errAt(errors, tp("name"))}
        />
        <div>
          <span className="mb-1 block text-[11px] text-zinc-400">type</span>
          <select
            data-testid={`mcp-type-${i}`}
            value={type}
            disabled={readOnly}
            onChange={(e) => {
              if (!readOnly) edit(tp("type"), e.target.value);
            }}
            className={SELECT_CLS}
          >
            {TRANSPORTS.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
          {typeError && (
            <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
              {typeError}
            </span>
          )}
        </div>
      </div>

      {!remote ? (
        <div className="mt-3 space-y-3">
          <Field
            testid={`mcp-command-${i}`}
            label="command"
            value={asString(s.command)}
            onChange={(v) => {
              if (!readOnly) edit(tp("command"), v);
            }}
            disabled={readOnly}
            invalid={errAt(errors, tp("command"))}
          />
          <ChipRow
            testid={`mcp-args-${i}`}
            label="args"
            items={asStringArray(s.args)}
            placeholder="add arg"
            onChange={(items) => {
              if (!readOnly) edit(tp("args"), items.length > 0 ? items : undefined);
            }}
          />
          <KvRows
            testid={`mcp-env-${i}`}
            label="env"
            rows={toRows(s.env)}
            onChange={(rows) => {
              if (!readOnly) edit(tp("env"), toMap(rows));
            }}
          />
        </div>
      ) : (
        <div className="mt-3 space-y-3">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field
              testid={`mcp-url-${i}`}
              label="url"
              value={asString(s.url)}
              onChange={(v) => {
                if (!readOnly) edit(tp("url"), v);
              }}
              disabled={readOnly}
              invalid={errAt(errors, tp("url"))}
            />
            <div>
              <span className="mb-1 block text-[11px] text-zinc-400">auth</span>
              <select
                data-testid={`mcp-auth-${i}`}
                value={kind}
                disabled={readOnly}
                onChange={(e) => writeAuth(e.target.value as AuthKind)}
                className={SELECT_CLS}
              >
                {AUTH_KINDS.map((k) => (
                  <option key={k} value={k}>
                    {k}
                  </option>
                ))}
              </select>
              {authError && (
                <span data-testid="error" className="mt-1 block text-[11px] text-red-400">
                  {authError}
                </span>
              )}
            </div>
          </div>

          {kind === "bearer" && (
            <SecretInput
              testid={`mcp-auth-token-${i}`}
              label="token"
              value={asString(auth?.token)}
              onChange={(v) => {
                if (readOnly) return;
                edit(tp("auth"), { type: "bearer", ...(v !== undefined ? { token: v } : {}) });
              }}
            />
          )}

          {kind === "oauth" && (
            <div className="grid gap-3 sm:grid-cols-3">
              <Field
                testid={`mcp-auth-clientid-${i}`}
                label="client_id"
                value={asString(auth?.client_id)}
                onChange={(v) => {
                  if (readOnly) return;
                  edit(tp("auth"), {
                    type: "oauth",
                    ...(v !== undefined ? { client_id: v } : {}),
                    ...(asString(auth?.client_secret) !== undefined
                      ? { client_secret: asString(auth?.client_secret) }
                      : {}),
                    scopes: asStringArray(auth?.scopes),
                  });
                }}
              />
              <SecretInput
                testid={`mcp-auth-secret-${i}`}
                label="client_secret"
                value={asString(auth?.client_secret)}
                onChange={(v) => {
                  if (readOnly) return;
                  edit(tp("auth"), {
                    type: "oauth",
                    ...(asString(auth?.client_id) !== undefined
                      ? { client_id: asString(auth?.client_id) }
                      : {}),
                    ...(v !== undefined ? { client_secret: v } : {}),
                    scopes: asStringArray(auth?.scopes),
                  });
                }}
              />
              <ChipRow
                testid={`mcp-auth-scopes-${i}`}
                label="scopes"
                items={asStringArray(auth?.scopes)}
                placeholder="add scope"
                onChange={(items) => {
                  if (readOnly) return;
                  edit(tp("auth"), {
                    type: "oauth",
                    ...(asString(auth?.client_id) !== undefined
                      ? { client_id: asString(auth?.client_id) }
                      : {}),
                    ...(asString(auth?.client_secret) !== undefined
                      ? { client_secret: asString(auth?.client_secret) }
                      : {}),
                    scopes: items,
                  });
                }}
              />
            </div>
          )}

          <KvRows
            testid={`mcp-headers-${i}`}
            label="headers"
            rows={toRows(s.headers)}
            onChange={(rows) => {
              if (!readOnly) edit(tp("headers"), toMap(rows));
            }}
          />
        </div>
      )}

      {legacyToken !== undefined && (
        <div
          data-testid={`mcp-authtoken-${i}`}
          className="mt-3 rounded border border-amber-800/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-300"
        >
          <code className="font-mono">{legacyToken}</code> — Deprecated — use{" "}
          <code className="font-mono">auth: bearer</code>.
        </div>
      )}
    </div>
  );
}
