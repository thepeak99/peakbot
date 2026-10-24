// T33 (D10, spec §9.13 / §6.9): first-run guided setup.
//
// Three pieces:
//  - buildFirstRunYaml — PURE builder from the wizard answers to the global
//    config YAML (key order: providers, mcps, profiles, general; profiles and
//    agents are MAPS, never `- name:` lists; secrets omitted when empty).
//  - deriveSetupPhase — the §6.9 pending-restart derivation: needs_setup +
//    global file exists + !legacy_layout ⇒ "restart_pending". Derived on
//    every load, never stored.
//  - <FirstRun> — the wizard (Welcome → Provider → Agent → Web → Review).
//    Self-contained: it only talks to the server via ./api putConfig on
//    Finish. Desktop Finish navigates to the `peakbot-desktop:restart`
//    sentinel (no success screen); the browser gets the restart screen.
//
// Esc closes the wizard (onClose — Boot then shows the setup strip).

import { useEffect, useMemo, useState } from "react";
import { stringify } from "yaml";
import { isDesktop } from "../desktop";
import { putConfig } from "./api";
import { generateToken } from "./general";
import { NEUTRAL_PERSONA, TERSE_PERSONA } from "./agent";
import type { FirstRunAnswers, FirstRunProviderType, SetupPhase } from "./contract";
import type { ConfigText, PutOk, SettingsMeta } from "./types";

// ── pure: buildFirstRunYaml ───────────────────────────────────────────────

/** The generated global config for a fresh setup (spec §9.13 example).
 * PURE: same answers in, same bytes out; the input is never mutated. */
export function buildFirstRunYaml(a: FirstRunAnswers): string {
  const p = a.provider;
  const provider: Record<string, unknown> = { name: p.name, type: p.type };
  const apiKey = (p.api_key ?? "").trim();
  if (apiKey !== "") provider.api_key = apiKey;
  // OpenRouter's endpoint is fixed — the builder never writes base_url for
  // it, even when the wizard was handed one.
  const baseUrl = p.type === "openrouter" ? "" : (p.base_url ?? "").trim();
  if (baseUrl !== "") provider.base_url = baseUrl;
  provider.models = p.models.map((m) => ({
    name: m.name,
    alias: m.alias,
    max_tokens: m.max_tokens,
  }));

  // Profile `default` › agent `main`, as MAPS. `persona` is omitted for the
  // built-in (Code Crusader) — the built-in must never drift into the file.
  const main: Record<string, unknown> = { model: a.agent.model };
  if (a.agent.persona !== null && a.agent.persona !== undefined) {
    main.persona = a.agent.persona;
  }
  if (a.agent.subagents) {
    main.subagents = { helper: { prompt: "Take focused sub-tasks and report back." } };
  }

  // Local never writes a token, even a leftover one; LAN always does.
  const web: Record<string, unknown> = {
    bind: a.web.access === "lan" ? "0.0.0.0:7823" : "127.0.0.1:7823",
  };
  if (a.web.access === "lan" && (a.web.token ?? "").trim() !== "") {
    web.token = a.web.token;
  }

  const doc = {
    providers: [provider],
    mcps: [] as unknown[],
    profiles: { default: { agents: { main } } },
    general: { web },
  };
  return stringify(doc);
}

// ── pure: deriveSetupPhase ────────────────────────────────────────────────

/** §6.9 — the restart-pending state is DERIVED, never stored:
 *  - configured server (`!needs_setup`) ⇒ "ready"
 *  - needs_setup + our config file exists + not a legacy file ⇒
 *    "restart_pending" (a previous Finish wrote it; the server must restart)
 *  - everything else (fresh machine, legacy file that failed to migrate) ⇒
 *    "guided" (run the wizard). */
export function deriveSetupPhase(
  meta: { needs_setup: boolean; legacy_layout: boolean },
  globalExists: boolean,
): SetupPhase {
  if (!meta.needs_setup) return "ready";
  if (globalExists && !meta.legacy_layout) return "restart_pending";
  return "guided";
}

// ── wizard ────────────────────────────────────────────────────────────────

type StepName = "welcome" | "provider" | "agent" | "web" | "review";

const STEPS: { name: StepName; title: string }[] = [
  { name: "welcome", title: "Welcome" },
  { name: "provider", title: "Provider & model" },
  { name: "agent", title: "Your first agent" },
  { name: "web", title: "Web access" },
  { name: "review", title: "Review" },
];

// Hosted providers need an API key; the self-hosted ones (llama.cpp, Ollama)
// don't.
const HOSTED_TYPES: FirstRunProviderType[] = ["openrouter", "openai", "anthropic"];

const PROVIDER_TYPES: { value: FirstRunProviderType; label: string }[] = [
  { value: "openrouter", label: "OpenRouter" },
  { value: "openai", label: "OpenAI" },
  { value: "anthropic", label: "Anthropic" },
  { value: "llamacpp", label: "llama.cpp" },
  { value: "ollama", label: "Ollama" },
];

const INPUT_CLS =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";
const GHOST_BTN =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-50";
const FOOTER_BTN =
  "cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-40";

function Field({
  testid,
  label,
  children,
}: {
  testid: string;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div data-testid={testid} className="min-w-0">
      <span className="mb-1 block text-[11px] text-zinc-400">{label}</span>
      {children}
    </div>
  );
}

export interface FirstRunProps {
  meta: SettingsMeta;
  global: ConfigText;
  onClose?: () => void;
}

export function FirstRun({ meta, global, onClose }: FirstRunProps) {
  const [step, setStep] = useState<StepName>("welcome");
  // Rail steps are disabled until VISITED (the wizard is forward-biased).
  const [visited, setVisited] = useState<Set<StepName>>(() => new Set(["welcome"]));
  const [answers, setAnswers] = useState<FirstRunAnswers>({
    provider: { type: "openrouter", name: "openrouter", models: [] },
    agent: { model: "", persona: null, subagents: false },
    web: { access: "local", token: null },
  });
  const [putting, setPutting] = useState(false);
  const [putError, setPutError] = useState<string | null>(null);
  const [putOk, setPutOk] = useState<PutOk | null>(null);

  const phase = deriveSetupPhase(meta, global.exists);

  // Esc closes the wizard (Boot shows the setup strip underneath).
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose?.();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const goTo = (s: StepName) => {
    setStep(s);
    setVisited((v) => {
      if (v.has(s)) return v;
      const next = new Set(v);
      next.add(s);
      return next;
    });
  };

  const setProvider = (patch: Partial<FirstRunAnswers["provider"]>) =>
    setAnswers((a) => ({ ...a, provider: { ...a.provider, ...patch } }));
  const setAgent = (patch: Partial<FirstRunAnswers["agent"]>) =>
    setAnswers((a) => ({ ...a, agent: { ...a.agent, ...patch } }));
  const setWeb = (patch: Partial<FirstRunAnswers["web"]>) =>
    setAnswers((a) => ({ ...a, web: { ...a.web, ...patch } }));

  const addModel = () =>
    setProvider({
      models: [
        ...answers.provider.models,
        { name: "", alias: "", max_tokens: 4096 },
      ],
    });
  const setModel = (k: number, patch: Partial<FirstRunAnswers["provider"]["models"][number]>) =>
    setProvider({
      models: answers.provider.models.map((m, i) => (i === k ? { ...m, ...patch } : m)),
    });

  // Step validation (computed for the CURRENT step; Next is disabled while
  // non-empty).
  const providerErrors: string[] = [];
  if (
    HOSTED_TYPES.includes(answers.provider.type) &&
    (answers.provider.api_key ?? "").trim() === ""
  ) {
    providerErrors.push(`An API key is required for ${answers.provider.type}.`);
  }
  if (answers.provider.models.length === 0) {
    providerErrors.push("Add at least one model.");
  }
  const webErrors: string[] = [];
  if (answers.web.access === "lan" && (answers.web.token ?? "").trim() === "") {
    webErrors.push("A LAN bind requires a token — generate one, or switch back to local only.");
  }
  const stepErrors = step === "provider" ? providerErrors : step === "web" ? webErrors : [];

  // The agent's model defaults to the first alias the wizard collected — and
  // falls back to it if the chosen alias was since renamed/removed. The
  // EFFECTIVE answers (not the raw draft) feed the preview and the PUT.
  const aliases = answers.provider.models.map((m) => m.alias);
  const modelValue = aliases.includes(answers.agent.model)
    ? answers.agent.model
    : (aliases[0] ?? "");
  const effective = useMemo<FirstRunAnswers>(
    () => ({ ...answers, agent: { ...answers.agent, model: modelValue } }),
    [answers, modelValue],
  );

  // Review preview: the EXACT text that will be PUT, with the API key masked
  // VISUALLY only (the real key is what gets sent).
  const yamlText = useMemo(() => buildFirstRunYaml(effective), [effective]);
  const maskedYaml = useMemo(() => {
    const key = (answers.provider.api_key ?? "").trim();
    if (key === "") return yamlText;
    return yamlText.split(key).join("••••••••");
  }, [yamlText, answers.provider.api_key]);

  async function finish() {
    setPutting(true);
    setPutError(null);
    try {
      const ok = await putConfig("global", yamlText, global.sha256);
      if (isDesktop()) {
        // The desktop restarts only the PeakBot server and reloads the chat.
        // No success screen — the page is about to go away.
        window.location.href = "peakbot-desktop:restart";
        return;
      }
      setPutOk(ok);
    } catch (e) {
      setPutError(e instanceof Error ? e.message : String(e));
    } finally {
      setPutting(false);
    }
  }

  // Restart screen: shown straight away for the derived "restart_pending"
  // phase (config written, server not yet restarted — e.g. on reload), and
  // after a successful browser Finish.
  if (phase === "restart_pending" || putOk !== null) {
    const finished = putOk !== null;
    return (
      <div data-testid="firstrun" className="flex h-dvh items-center justify-center bg-zinc-950 text-zinc-100">
        <div className="w-full max-w-md rounded-xl border border-zinc-800 bg-zinc-900 p-6">
          <div data-testid="firstrun-restart" className="space-y-3">
            <div className="text-lg font-semibold text-emerald-400">✓ Config written</div>
            {finished && (
              <p className="text-sm text-zinc-300">PeakBot wrote your config. A backup was kept.</p>
            )}
            <p className="text-sm text-zinc-300">Restart PeakBot to start chatting.</p>
            <div className="space-y-1 font-mono text-[11px] text-zinc-500">
              <div>Config: {global.path}</div>
              <div>Version: {meta.facts.version}</div>
              {finished && putOk.backup !== null && (
                <div data-testid="firstrun-restart-backup">Backup: {putOk.backup}</div>
              )}
            </div>
          </div>
        </div>
      </div>
    );
  }

  const idx = STEPS.findIndex((s) => s.name === step);
  const pType = answers.provider.type;

  return (
    <div data-testid="firstrun" className="flex h-dvh flex-col bg-zinc-950 text-zinc-100">
      <header className="flex items-center justify-between border-b border-zinc-800 px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold">Welcome to PeakBot</h1>
          <p className="text-xs text-zinc-500">let's get you set up</p>
        </div>
        <span className="text-[11px] text-zinc-600">Esc to close</span>
      </header>

      <div className="flex min-h-0 flex-1">
        {/* Left rail: numbered steps with status dots. Later steps are
            disabled until visited. */}
        <nav className="w-56 shrink-0 overflow-y-auto border-r border-zinc-800 p-4">
          <div className="mb-2 text-[10px] uppercase tracking-wide text-zinc-600">Setup</div>
          {STEPS.map((s, i) => {
            const isHere = s.name === step;
            const isVisited = visited.has(s.name);
            const stepOk =
              s.name === "provider" ? providerErrors.length === 0 :
              s.name === "web" ? webErrors.length === 0 : true;
            return (
              <button
                key={s.name}
                data-testid={`firstrun-rail-${s.name}`}
                type="button"
                disabled={!isVisited}
                onClick={() => goTo(s.name)}
                className={`flex w-full cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-left text-xs ${
                  isHere ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:bg-zinc-900"
                } disabled:cursor-default disabled:opacity-40`}
              >
                <span
                  className={`h-2 w-2 shrink-0 rounded-full ${
                    !isVisited
                      ? "border border-zinc-600"
                      : isHere
                        ? (stepOk ? "bg-emerald-400" : "bg-red-400")
                        : (stepOk ? "bg-emerald-600" : "bg-red-500")
                  }`}
                />
                {i + 1}. {s.title}
              </button>
            );
          })}
          <div className="mt-3 text-[10px] text-zinc-600">
            green = done · red = needs attention · hollow = pending
          </div>
        </nav>

        <main className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1 overflow-y-auto p-6">
            {step === "welcome" && (
              <div data-testid="firstrun-step-welcome" className="max-w-lg space-y-4">
                <div>
                  <h2 className="text-sm font-semibold">Welcome</h2>
                  <p className="mt-1 text-xs text-zinc-400">
                    Point an agent at a provider and it is ready to work. Three steps and a review;
                    one of them optional.
                  </p>
                </div>
                {meta.legacy_layout && (
                  <div
                    data-testid="firstrun-banner-migrate"
                    className="space-y-1 rounded-md border border-red-900/70 bg-red-950/40 p-3 text-xs text-red-200"
                  >
                    <div className="font-medium">PeakBot couldn't migrate your old config</div>
                    <div className="font-mono text-red-300">{meta.migrate_error}</div>
                    <div className="text-red-200/80">
                      Your config.yaml was not changed. Fix it and restart PeakBot to try again, or
                      start fresh — the old file is kept as config.yaml.pre-0.24.
                    </div>
                  </div>
                )}
                <div
                  data-testid="firstrun-facts"
                  className="space-y-1 rounded-md border border-zinc-800 bg-zinc-900/50 p-3 font-mono text-[11px] text-zinc-400"
                >
                  <div>OS · {meta.facts.os}</div>
                  <div>Binary · {meta.facts.exe_path}</div>
                  <div>Config · {meta.global_path}</div>
                  <div>Data dir · {meta.facts.data_dir}</div>
                  <div>Version · {meta.facts.version}</div>
                </div>
                <button
                  data-testid="firstrun-start-fresh"
                  type="button"
                  onClick={() => goTo("provider")}
                  className="cursor-pointer rounded-md bg-emerald-700 px-4 py-1.5 text-sm font-medium text-white hover:bg-emerald-600"
                >
                  Start fresh
                </button>
              </div>
            )}

            {step === "provider" && (
              <div data-testid="firstrun-step-provider" className="max-w-lg space-y-3">
                <div>
                  <h2 className="text-sm font-semibold">Provider &amp; model</h2>
                  <p className="mt-1 text-xs text-zinc-400">
                    Choose a provider, name at least one model, and add its API key.
                  </p>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <Field testid="fr-provider-type" label="Provider type">
                    <select
                      data-testid="input"
                      className={INPUT_CLS}
                      value={pType}
                      onChange={(e) => setProvider({ type: e.target.value as FirstRunProviderType })}
                    >
                      {PROVIDER_TYPES.map((t) => (
                        <option key={t.value} value={t.value}>
                          {t.label}
                        </option>
                      ))}
                    </select>
                  </Field>
                  <Field testid="fr-provider-name" label="Provider name">
                    <input
                      data-testid="input"
                      className={INPUT_CLS}
                      value={answers.provider.name}
                      onChange={(e) => setProvider({ name: e.target.value })}
                    />
                  </Field>
                </div>
                <Field testid="fr-provider-baseurl" label="Base URL">
                  <input
                    data-testid="input"
                    className={INPUT_CLS}
                    value={answers.provider.base_url ?? ""}
                    disabled={pType === "openrouter"}
                    placeholder={pType === "openrouter" ? "Fixed for OpenRouter." : "http://host:port"}
                    onChange={(e) => setProvider({ base_url: e.target.value === "" ? undefined : e.target.value })}
                  />
                </Field>
                <Field testid="fr-provider-apikey" label="API key">
                  <input
                    data-testid="input"
                    className={INPUT_CLS}
                    value={answers.provider.api_key ?? ""}
                    placeholder="Required for hosted providers."
                    onChange={(e) => setProvider({ api_key: e.target.value === "" ? undefined : e.target.value })}
                  />
                </Field>
                <div>
                  <span className="mb-1 block text-[11px] text-zinc-400">Models</span>
                  {answers.provider.models.map((m, k) => (
                    <div key={k} className="mb-2 grid grid-cols-[1fr_6rem_5rem] gap-2">
                      <Field testid={`fr-model-name-${k}`} label={`model ${k + 1}`}>
                        <input
                          data-testid="input"
                          className={INPUT_CLS}
                          value={m.name}
                          placeholder="anthropic/claude-sonnet-4.5"
                          onChange={(e) => setModel(k, { name: e.target.value })}
                        />
                      </Field>
                      <Field testid={`fr-model-alias-${k}`} label="alias">
                        <input
                          data-testid="input"
                          className={INPUT_CLS}
                          value={m.alias}
                          placeholder="sonnet"
                          onChange={(e) => setModel(k, { alias: e.target.value })}
                        />
                      </Field>
                      <Field testid={`fr-model-maxtokens-${k}`} label="max tokens">
                        <input
                          data-testid="input"
                          type="number"
                          className={INPUT_CLS}
                          value={m.max_tokens}
                          onChange={(e) => setModel(k, { max_tokens: Number(e.target.value) || 0 })}
                        />
                      </Field>
                    </div>
                  ))}
                  <button
                    data-testid="fr-model-add"
                    type="button"
                    onClick={addModel}
                    className={GHOST_BTN}
                  >
                    + Add model
                  </button>
                </div>
                {stepErrors.length > 0 && (
                  <div data-testid="firstrun-step-errors" className="space-y-0.5 text-[11px] text-red-400">
                    {stepErrors.map((e) => (
                      <div key={e}>{e}</div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {step === "agent" && (
              <div data-testid="firstrun-step-agent" className="max-w-lg space-y-3">
                <div>
                  <h2 className="text-sm font-semibold">Your first agent</h2>
                  <p className="mt-1 text-xs text-zinc-400">
                    Every config has a profile and an agent. We generate profile &ldquo;default&rdquo;
                    with agent &ldquo;main&rdquo; for you.
                  </p>
                </div>
                <div
                  data-testid="firstrun-agent-banner"
                  className="rounded-md border border-sky-900/60 bg-sky-950/30 p-2.5 text-xs text-sky-300"
                >
                  Generated: profile default › agent main using model {modelValue}. You can add more
                  profiles and agents later in Settings → Profiles.
                </div>
                <Field testid="fr-agent-model" label="Model">
                  <select
                    data-testid="input"
                    className={INPUT_CLS}
                    value={modelValue}
                    onChange={(e) => setAgent({ model: e.target.value })}
                  >
                    {answers.provider.models.map((m) => (
                      <option key={m.alias} value={m.alias}>
                        {m.alias} — {m.name}
                      </option>
                    ))}
                  </select>
                </Field>
                <div>
                  <span className="mb-1 block text-[11px] text-zinc-400">Persona</span>
                  <div className="flex flex-wrap gap-1.5">
                    <button
                      data-testid="fr-persona-builtin"
                      type="button"
                      onClick={() => setAgent({ persona: null })}
                      className={
                        GHOST_BTN +
                        (answers.agent.persona === null ? " border-emerald-700 text-emerald-300" : "")
                      }
                    >
                      Built-in (Code Crusader)
                    </button>
                    <button
                      data-testid="fr-persona-neutral"
                      type="button"
                      onClick={() => setAgent({ persona: NEUTRAL_PERSONA })}
                      className={
                        GHOST_BTN +
                        (answers.agent.persona === NEUTRAL_PERSONA ? " border-emerald-700 text-emerald-300" : "")
                      }
                    >
                      Neutral engineer
                    </button>
                    <button
                      data-testid="fr-persona-terse"
                      type="button"
                      onClick={() => setAgent({ persona: TERSE_PERSONA })}
                      className={
                        GHOST_BTN +
                        (answers.agent.persona === TERSE_PERSONA ? " border-emerald-700 text-emerald-300" : "")
                      }
                    >
                      Terse reviewer
                    </button>
                  </div>
                  <span className="mt-1 block text-[10px] text-zinc-600">
                    Built-in omits <span className="font-mono">persona</span> — the built-in persona
                    never drifts into your file.
                  </span>
                </div>
                <div
                  data-testid="fr-agent-subagents"
                  className="flex items-start justify-between gap-3"
                >
                  <span className="text-xs text-zinc-300">
                    Subagents <span className="text-zinc-600">(optional)</span>
                    <span className="mt-0.5 block text-[10px] text-zinc-500">
                      On → adds a <span className="font-mono">helper</span> subagent
                      (Take focused sub-tasks and report back.)
                    </span>
                  </span>
                  <input
                    data-testid="input"
                    type="checkbox"
                    checked={answers.agent.subagents}
                    onChange={(e) => setAgent({ subagents: e.target.checked })}
                    className="mt-0.5 h-4 w-4 cursor-pointer accent-emerald-600"
                  />
                </div>
              </div>
            )}

            {step === "web" && (
              <div data-testid="firstrun-step-web" className="max-w-lg space-y-3">
                <div>
                  <h2 className="text-sm font-semibold">Web access</h2>
                  <p className="mt-1 text-xs text-zinc-400">Optional — where PeakBot listens.</p>
                </div>
                <label className="flex cursor-pointer items-center gap-2 text-xs text-zinc-200">
                  <input
                    data-testid="fr-web-local"
                    type="radio"
                    name="firstrun-web-access"
                    checked={answers.web.access === "local"}
                    onChange={() => setWeb({ access: "local" })}
                    className="h-3.5 w-3.5 cursor-pointer accent-emerald-600"
                  />
                  Local only <span className="text-zinc-600">(127.0.0.1:7823)</span>
                </label>
                <label className="flex cursor-pointer items-center gap-2 text-xs text-zinc-200">
                  <input
                    data-testid="fr-web-lan"
                    type="radio"
                    name="firstrun-web-access"
                    checked={answers.web.access === "lan"}
                    onChange={() => setWeb({ access: "lan" })}
                    className="h-3.5 w-3.5 cursor-pointer accent-emerald-600"
                  />
                  Reachable from other devices <span className="text-zinc-600">(0.0.0.0:7823)</span>
                </label>
                {answers.web.access === "lan" && (
                  <div className="flex items-end gap-2">
                    <div className="flex-1">
                      <Field testid="fr-web-token" label="Token">
                        <input
                          data-testid="input"
                          className={INPUT_CLS + " font-mono"}
                          value={answers.web.token ?? ""}
                          placeholder="required for LAN"
                          onChange={(e) => setWeb({ token: e.target.value === "" ? null : e.target.value })}
                        />
                      </Field>
                    </div>
                    <button
                      data-testid="fr-web-token-generate"
                      type="button"
                      onClick={() => setWeb({ token: generateToken() })}
                      className={GHOST_BTN + " mb-[1px]"}
                    >
                      Generate
                    </button>
                  </div>
                )}
                {stepErrors.length > 0 && (
                  <div data-testid="firstrun-step-errors" className="space-y-0.5 text-[11px] text-red-400">
                    {stepErrors.map((e) => (
                      <div key={e}>{e}</div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {step === "review" && (
              <div data-testid="firstrun-step-review" className="max-w-2xl space-y-3">
                <div>
                  <h2 className="text-sm font-semibold">Review</h2>
                  <p className="mt-1 text-xs text-zinc-400">
                    This is the exact config that will be written.
                  </p>
                </div>
                <div
                  data-testid="firstrun-review-path"
                  className="rounded border border-zinc-800 bg-zinc-900/50 px-2 py-1 font-mono text-[11px] text-zinc-400"
                >
                  {meta.global_path}
                </div>
                <pre
                  data-testid="firstrun-yaml"
                  className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-950/70 p-3 font-mono text-[11px] leading-relaxed text-zinc-300"
                >
                  {maskedYaml}
                </pre>
                <div data-testid="firstrun-review-note" className="text-xs text-zinc-400">
                  {isDesktop()
                    ? "Finishing restarts PeakBot with this config."
                    : "After Finish, restart PeakBot to start chatting."}
                </div>
                {meta.legacy_layout && (
                  <div
                    data-testid="firstrun-review-legacy-note"
                    className="text-xs text-amber-300/80"
                  >
                    old file kept as config.yaml.pre-0.24
                  </div>
                )}
                {putError !== null && (
                  <div
                    data-testid="firstrun-error"
                    className="rounded-md border border-red-900/70 bg-red-950/40 p-2.5 text-xs text-red-300"
                  >
                    {putError}
                  </div>
                )}
              </div>
            )}
          </div>

          <footer className="flex items-center gap-2 border-t border-zinc-800 px-6 py-3">
            <button
              data-testid="firstrun-back"
              type="button"
              disabled={idx === 0}
              onClick={() => goTo(STEPS[idx - 1].name)}
              className={FOOTER_BTN}
            >
              Back
            </button>
            <span className="flex-1" />
            <button data-testid="firstrun-skip" type="button" onClick={() => goTo("review")} className={FOOTER_BTN}>
              Skip to review
            </button>
            {step !== "review" ? (
              <button
                data-testid="firstrun-next"
                type="button"
                disabled={stepErrors.length > 0}
                onClick={() => goTo(STEPS[idx + 1].name)}
                className={FOOTER_BTN + " border-emerald-800 bg-emerald-900/40 text-emerald-200 hover:bg-emerald-900/70"}
              >
                Next
              </button>
            ) : (
              <button
                data-testid="firstrun-finish"
                type="button"
                disabled={putting}
                onClick={finish}
                className={FOOTER_BTN + " border-emerald-700 bg-emerald-700 font-medium text-white hover:bg-emerald-600"}
              >
                Finish setup
              </button>
            )}
          </footer>
        </main>
      </div>
    </div>
  );
}
