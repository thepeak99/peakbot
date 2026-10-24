// D2 "Providers & Models" pane (spec §9.8, contract.ts "./providers (T25)").
//
// Reads the draft via useSettingsCtx(); every change goes through
// `edit(path, value)` by segment array. Renders the REAL ModelEntry fields —
// `context_size`, NOT the mockup's `num_ctx`. `type` changes auto-fill
// `base_url` from the type's preset (openrouter is fixed/disabled).
// `extra_params` is a JSON textarea that MUST parse as an object: invalid
// input shows an inline error and writes nothing; a valid object is written
// as a parsed object, not a string.
//
// Global-only section: in repo scope the pane renders read-only (🔒) —
// `providers:` may only live in the global file.

import { useState } from "react";
import { useSettingsCtx } from "./context";
import { get } from "./yamlDoc";
import { errAt } from "./paths";
import { Badge, Field, SecretInput, Tri, type VisionTri } from "./widgets";
import type { ConfigErr, Seg } from "./types";
import {
  BASE_URL_PRESETS,
  MODEL_PRESETS,
  PROVIDER_TYPES as TYPES,
  type ProviderType,
} from "./providerTypes";

const CACHING_MODES = ["auto", "auto_1h", "manual", "off"] as const;

const SELECT_CLS =
  "w-full cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";

// The draft values are plain JS (yamlDoc.get → toJS); these accessors keep
// the pane honest about absent / malformed keys.
function asString(v: unknown): string | undefined {
  return typeof v === "string" && v !== "" ? v : undefined;
}
function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}
function asBoolTri(v: unknown): VisionTri {
  return v === true ? "on" : v === false ? "off" : "auto";
}

interface ProviderDraft {
  name?: unknown;
  type?: unknown;
  base_url?: unknown;
  api_key?: unknown;
  models?: unknown;
}

export function ProvidersPane() {
  const { scope, doc, globalDoc, edit, errors, switchToGlobal } = useSettingsCtx();
  // providers: is global-only — repo scope renders the GLOBAL draft's
  // values read-only (🔒); the repo file may only contain `profiles:`.
  const readOnly = scope === "repo";
  const paneDoc = readOnly ? globalDoc : doc;
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());

  const raw = get(paneDoc, ["providers"]);
  const providers = (Array.isArray(raw) ? raw : []) as ProviderDraft[];

  function toggle(i: number, j: number) {
    const key = `${i}.${j}`;
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  function addProvider() {
    if (readOnly) return;
    edit(["providers"], [
      ...providers,
      { name: "provider", type: "openrouter", base_url: BASE_URL_PRESETS.openrouter, models: [] },
    ]);
  }

  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <h3 className="text-sm font-semibold">Providers</h3>
          <span className="text-[11px] text-zinc-500">
            aliases are picked per agent — no default_model
          </span>
        </div>
        <p className="mt-1 text-xs text-zinc-500">
          Every provider declares models with globally unique <code>alias</code>es. Agents pick an
          alias; <code>/model</code> switches it per conversation.
        </p>
      </div>

      {readOnly && (
        <div
          data-testid="provider-readonly"
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
        {providers.map((p, i) => (
          <ProviderCard
            key={i}
            provider={p}
            index={i}
            readOnly={readOnly}
            expanded={expanded}
            onToggle={toggle}
            onRemove={(idx) => {
              if (!readOnly) edit(["providers"], providers.filter((_, k) => k !== idx));
            }}
            edit={edit}
            errors={errors}
          />
        ))}
      </div>

      <button
        data-testid="provider-add"
        type="button"
        disabled={readOnly}
        onClick={addProvider}
        className="cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60"
      >
        + Add provider
      </button>
    </div>
  );
}

function ProviderCard({
  provider,
  index: i,
  readOnly,
  expanded,
  onToggle,
  onRemove,
  edit,
  errors,
}: {
  provider: ProviderDraft;
  index: number;
  readOnly: boolean;
  expanded: ReadonlySet<string>;
  onToggle(i: number, j: number): void;
  onRemove(i: number): void;
  edit(path: Seg[], value: unknown): void;
  errors: ConfigErr[];
}) {
  const type = (asString(provider.type) ?? "openrouter") as ProviderType;
  const models = (Array.isArray(provider.models) ? provider.models : []) as Record<string, unknown>[];

  function changeType(t: string) {
    if (readOnly) return;
    edit(["providers", i, "type"], t);
    // Changing type auto-fills base_url from the preset (spec §9.8).
    const preset = BASE_URL_PRESETS[t as ProviderType];
    if (preset !== undefined) edit(["providers", i, "base_url"], preset);
  }

  function addModel() {
    if (readOnly) return;
    edit(["providers", i, "models"], [...models, { name: "", alias: "" }]);
  }

  function addPreset(k: number) {
    if (readOnly) return;
    const preset = MODEL_PRESETS[type][k];
    if (!preset) return;
    const [name, alias, maxTokens] = preset;
    edit(["providers", i, "models"], [
      ...models,
      { name, alias, max_tokens: maxTokens },
    ]);
  }

  const typeError = errAt(errors, ["providers", i, "type"]);
  const localHint = type === "ollama" || type === "llamacpp";

  return (
    <div data-testid={`provider-card-${i}`} className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
      {/* Card head: the provider name (testid provider-name-<i>) + type tag + remove. */}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div data-testid={`provider-name-${i}`} className="flex min-w-0 flex-1 items-center gap-2">
          <h3 className="truncate text-sm font-semibold">
            {asString(provider.name) ?? "unnamed provider"}
          </h3>
          <span className="shrink-0 rounded border border-zinc-700 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
            {type}
          </span>
        </div>
        <button
          data-testid={`provider-remove-${i}`}
          type="button"
          title="Remove provider"
          disabled={readOnly}
          onClick={() => onRemove(i)}
          className="cursor-pointer rounded px-1 text-xs text-zinc-500 hover:text-red-400 disabled:cursor-default disabled:opacity-60"
        >
          ✕
        </button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div>
          <span className="mb-1 block text-[11px] text-zinc-400">type</span>
          <select
            data-testid={`provider-type-${i}`}
            value={type}
            disabled={readOnly}
            onChange={(e) => changeType(e.target.value)}
            className={SELECT_CLS}
          >
            {TYPES.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
          {typeError && <span data-testid="error" className="mt-1 block text-[11px] text-red-400">{typeError}</span>}
        </div>
        <Field
          testid={`provider-name-input-${i}`}
          label="name"
          value={asString(provider.name)}
          onChange={(v) => {
            if (!readOnly) edit(["providers", i, "name"], v);
          }}
          disabled={readOnly}
          invalid={errAt(errors, ["providers", i, "name"])}
        />
        <Field
          testid={`provider-baseurl-${i}`}
          label="base_url"
          value={asString(provider.base_url)}
          onChange={(v) => {
            if (!readOnly) edit(["providers", i, "base_url"], v);
          }}
          disabled={readOnly || type === "openrouter"}
          hint={type === "openrouter" ? "Fixed for OpenRouter." : undefined}
          invalid={errAt(errors, ["providers", i, "base_url"])}
        />
        <SecretInput
          testid={`provider-apikey-${i}`}
          label="api_key"
          value={asString(provider.api_key)}
          onChange={(v) => {
            if (!readOnly) edit(["providers", i, "api_key"], v);
          }}
          hint={localHint ? "Optional for local servers." : undefined}
          invalid={errAt(errors, ["providers", i, "api_key"])}
        />
      </div>

      {/* Models table: ▸ expand · name · alias · max_tok · temp · vision · type column */}
      <div className="mt-3 border-t border-zinc-800 pt-3">
        <div className="space-y-1.5">
          {models.map((m, j) => (
            <ModelRow
              key={j}
              model={m}
              providerType={type}
              i={i}
              j={j}
              open={expanded.has(`${i}.${j}`)}
              onToggle={onToggle}
              onRemove={(idx) => {
                if (!readOnly)
                  edit(["providers", i, "models"], models.filter((_, k) => k !== idx));
              }}
              edit={edit}
              readOnly={readOnly}
              errors={errors}
            />
          ))}
          {models.length === 0 && (
            <p className="text-xs text-zinc-600">No models declared.</p>
          )}
        </div>

        <div className="mt-2 flex flex-wrap items-center gap-2">
          <button
            data-testid={`model-add-${i}`}
            type="button"
            disabled={readOnly}
            onClick={addModel}
            className="cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60"
          >
            + Add model
          </button>
          <span className="text-[10px] text-zinc-600">presets:</span>
          {MODEL_PRESETS[type].map(([name, alias], k) => (
            <button
              key={`${k}-${alias}`}
              data-testid={`model-preset-${i}-${k}`}
              type="button"
              title={name}
              disabled={readOnly}
              onClick={() => addPreset(k)}
              className="cursor-pointer rounded-full border border-zinc-700 bg-zinc-900 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60"
            >
              {alias}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

function ModelRow({
  model: m,
  providerType,
  i,
  j,
  open,
  onToggle,
  onRemove,
  edit,
  readOnly,
  errors,
}: {
  model: Record<string, unknown>;
  providerType: ProviderType;
  i: number;
  j: number;
  open: boolean;
  onToggle(i: number, j: number): void;
  onRemove(j: number): void;
  edit(path: Seg[], value: unknown): void;
  readOnly: boolean;
  errors: ConfigErr[];
}) {
  const mp = (seg: string): Seg[] => ["providers", i, "models", j, seg];
  const vision = asBoolTri(m.vision);
  const caching = asString(m.prompt_caching) ?? "auto";
  const showExtra = providerType !== "anthropic" && providerType !== "ollama";
  // The type-dependent last column (spec §9.8): caching (anthropic),
  // context (= context_size) (ollama), extra otherwise.
  const lastCol =
    providerType === "anthropic"
      ? caching
      : providerType === "ollama"
        ? String(asNumber(m.context_size) ?? "—")
        : "—";

  return (
    <div>
      <div
        data-testid={`model-row-${i}-${j}`}
        className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded border border-zinc-800/70 bg-zinc-900/30 px-2 py-1.5 text-xs"
      >
        <button
          data-testid={`model-expand-${i}-${j}`}
          type="button"
          title={open ? "Collapse" : "Expand"}
          onClick={() => onToggle(i, j)}
          className="cursor-pointer text-zinc-500 hover:text-zinc-300"
        >
          {open ? "▾" : "▸"}
        </button>
        <span className="font-mono">{asString(m.name) ?? "—"}</span>
        <span className="font-mono text-zinc-400">{asString(m.alias) ?? "—"}</span>
        <span className="font-mono text-zinc-500">
          {asNumber(m.max_tokens) ?? "—"}
        </span>
        <span className="font-mono text-zinc-500">
          {asNumber(m.temperature) ?? "—"}
        </span>
        <Badge
          tone={vision === "on" ? "green" : vision === "off" ? "red" : "neutral"}
        >
          {vision}
        </Badge>
        <span className="ml-auto font-mono text-zinc-600">{lastCol}</span>
      </div>

      {open && (
        <div className="mt-2 grid gap-3 rounded border border-zinc-800/70 bg-zinc-950/40 p-3 sm:grid-cols-2 lg:grid-cols-3">
          <Field
            testid={`model-name-${i}-${j}`}
            label="name"
            value={asString(m.name)}
            onChange={(v) => {
              if (!readOnly) edit(mp("name"), v);
            }}
            disabled={readOnly}
            invalid={errAt(errors, mp("name"))}
          />
          <Field
            testid={`model-alias-${i}-${j}`}
            label="alias"
            value={asString(m.alias)}
            onChange={(v) => {
              if (!readOnly) edit(mp("alias"), v);
            }}
            disabled={readOnly}
            hint="Globally unique across providers."
            invalid={errAt(errors, mp("alias"))}
          />
          <Field
            testid={`model-maxtokens-${i}-${j}`}
            label="max_tokens"
            type="number"
            value={asNumber(m.max_tokens)}
            onChange={(v) => {
              if (!readOnly) edit(mp("max_tokens"), v);
            }}
            disabled={readOnly}
            invalid={errAt(errors, mp("max_tokens"))}
          />
          <Field
            testid={`model-temp-${i}-${j}`}
            label="temperature"
            type="number"
            value={asNumber(m.temperature)}
            onChange={(v) => {
              if (!readOnly) edit(mp("temperature"), v);
            }}
            disabled={readOnly}
            invalid={errAt(errors, mp("temperature"))}
          />
          <Field
            testid={`model-context-${i}-${j}`}
            label="context"
            type="number"
            value={asNumber(m.context_size)}
            onChange={(v) => {
              if (!readOnly) edit(mp("context_size"), v);
            }}
            disabled={readOnly}
            hint="Empty = auto-detect."
            invalid={errAt(errors, mp("context_size"))}
          />
          <Tri
            testid={`model-vision-${i}-${j}`}
            label="vision"
            value={vision}
            onChange={(v) => {
              if (!readOnly) edit(mp("vision"), v === "auto" ? undefined : v === "on");
            }}
          />
          {providerType === "anthropic" && (
            <div>
              <span className="mb-1 block text-[11px] text-zinc-400">prompt_caching</span>
              <select
                data-testid={`model-caching-${i}-${j}`}
                value={caching}
                disabled={readOnly}
                onChange={(e) => {
                  if (!readOnly)
                    edit(mp("prompt_caching"), e.target.value === "auto" ? undefined : e.target.value);
                }}
                className={SELECT_CLS}
              >
                {CACHING_MODES.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            </div>
          )}
          {showExtra && (
            <ExtraParams
              testid={`model-extra-${i}-${j}`}
              value={m.extra_params}
              disabled={readOnly}
              onWrite={(v) => {
                if (!readOnly) edit(mp("extra_params"), v);
              }}
            />
          )}

          {/* Advanced: the two reasoning toggles (real ModelEntry fields). */}
          <details className="lg:col-span-2 sm:col-span-2">
            <summary className="cursor-pointer text-[11px] text-zinc-400 hover:text-zinc-200">
              Advanced
            </summary>
            <div className="mt-2 space-y-2">
              <AdvancedToggle
                testid={`model-preserve-${i}-${j}`}
                label="preserve_reasoning"
                checked={m.preserve_reasoning !== false}
                hint="Capture thinking blocks and replay them verbatim on the next turn."
                onChange={(c) => {
                  if (!readOnly) edit(mp("preserve_reasoning"), c);
                }}
              />
              <AdvancedToggle
                testid={`model-display-${i}-${j}`}
                label="display_reasoning"
                checked={m.display_reasoning === true}
                hint="Show captured thinking blocks in the web transcript."
                onChange={(c) => {
                  if (!readOnly) edit(mp("display_reasoning"), c);
                }}
              />
            </div>
          </details>

          <div className="sm:col-span-2 lg:col-span-3">
            <button
              data-testid={`model-remove-${i}-${j}`}
              type="button"
              disabled={readOnly}
              onClick={() => onRemove(j)}
              className="cursor-pointer rounded border border-red-900/70 px-3 py-1 text-xs text-red-300 hover:bg-red-950/40 disabled:cursor-default disabled:opacity-60"
            >
              Remove model
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// A dumb checkbox row for the Advanced disclosure.
function AdvancedToggle({
  testid,
  label,
  checked,
  hint,
  onChange,
}: {
  testid: string;
  label: string;
  checked: boolean;
  hint?: string;
  onChange(checked: boolean): void;
}) {
  return (
    <div data-testid={testid} className="flex items-start justify-between gap-3">
      <span className="min-w-0">
        <span className="block font-mono text-xs text-zinc-300">{label}</span>
        {hint && <span className="mt-0.5 block text-[10px] text-zinc-500">{hint}</span>}
      </span>
      <input
        data-testid="input"
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-0.5 h-4 w-4 shrink-0 cursor-pointer accent-emerald-600"
      />
    </div>
  );
}

// extra_params: a JSON textarea that MUST parse as an object. Invalid input
// (not JSON, an array, a scalar) shows an inline error and writes nothing;
// a valid object is written as a parsed object, not a string. The typed text
// lives in local state so intermediate (invalid) edits stay editable.
function ExtraParams({
  testid,
  value,
  disabled,
  onWrite,
}: {
  testid: string;
  value: unknown;
  disabled?: boolean;
  onWrite(v: unknown): void;
}) {
  const [text, setText] = useState(value === undefined ? "" : JSON.stringify(value, null, 2));
  const [error, setError] = useState<string | null>(null);

  function handle(next: string) {
    setText(next);
    if (next.trim() === "") {
      setError(null);
      onWrite(undefined); // cleared → key removed
      return;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(next);
    } catch {
      setError("Must be a JSON object.");
      return;
    }
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      setError("Must be a JSON object.");
      return;
    }
    setError(null);
    onWrite(parsed);
  }

  return (
    <div className="min-w-0">
      <span className="mb-1 block text-[11px] text-zinc-400">extra_params</span>
      <textarea
        data-testid={testid}
        value={text}
        rows={2}
        disabled={disabled}
        placeholder='{"n_gpu_layers": -1}'
        onChange={(e) => handle(e.target.value)}
        className={
          "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 font-mono text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60" +
          (error ? " border-red-600" : "")
        }
      />
      {error && (
        <span data-testid={`${testid}-error`} className="mt-1 block text-[11px] text-red-400">
          {error}
        </span>
      )}
    </div>
  );
}
