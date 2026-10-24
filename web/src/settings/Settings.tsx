// The Settings shell (spec §9.0). Settings is an in-app VIEW — App.tsx keeps
// it mounted as an overlay above the chat (chat state + the WebSocket stay
// alive underneath); it is not a route.
//
//   useSettingsToggle(): gear (TopBar) and Ctrl+, / ⌘+, toggle; Esc closes in
//   the priority order modal → popover → Settings (an already-handled
//   defaultPrevented Esc is left to the modal/popover).
//
// The section CONTENT panes (Providers, MCP, Profiles, Agent, General,
// System, Raw YAML) land in later M5 tasks — this foundation renders a
// neutral empty state per section so the shell is navigable, and wires the
// full save flow: debounced validate → PUT with base_sha256 → 409 stale
// banner / 422 error panel / 200 toast + restart banner.

import { useCallback, useEffect, useReducer, useState } from "react";
import { isDesktop } from "../desktop";
import { ApiError, getConfig, getSettings, putConfig, validateConfig } from "./api";
import { createStore } from "./store";
import { get, parse, set, stringify } from "./yamlDoc";
import type { ConfigErr, Scope, SettingsMeta, Seg } from "./types";
import { SettingsCtx } from "./context";
import { dotPath, route, type Section } from "./paths";
import { RestartBanner, SaveBar } from "./SaveBar";
import { ErrorPanel } from "./ErrorPanel";
import { DiffModal } from "./DiffModal";
import { ToastProvider, useToast } from "./Toast";

/**
 * Entry/exit of the Settings view (spec §9.0 "Entry and exit"):
 *  - key "," with ctrlKey OR metaKey (no shift/alt) → preventDefault + toggle
 *    (Ctrl+, / ⌘+,)
 *  - key "Escape" while open → close, UNLESS the event was already handled
 *    (defaultPrevented) — modals and popovers close first (Esc order is
 *    modal → popover → Settings).
 * The TopBar gear (tooltip `Settings  Ctrl ,`) calls the same toggle.
 */
export function useSettingsToggle(): { open: boolean; toggle(): void; close(): void } {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "," && (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey) {
        e.preventDefault();
        setOpen((o) => !o);
      } else if (e.key === "Escape" && !e.defaultPrevented) {
        setOpen(false);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const toggle = useCallback(() => setOpen((o) => !o), []);
  const close = useCallback(() => setOpen(false), []);
  return { open, toggle, close };
}

export interface SettingsProps {
  /** The conversation id for ?convo= (repo scope resolves against its cwd); null = boot cwd. */
  convo: string | null;
  /** "← Back to chat" — closes the view (chat stays mounted underneath). */
  onClose(): void;
}

const SECTIONS: Record<Section, { label: string; desc: string }> = {
  providers: {
    label: "Providers & Models",
    desc: "Providers declare the models and their globally unique aliases; agents pick an alias, /model switches it per conversation.",
  },
  mcps: {
    label: "MCP servers",
    desc: "Declare the MCP servers PeakBot can launch. Changes apply after a restart, not on /new.",
  },
  profiles: {
    label: "Profiles",
    desc: "A profile is a set of capabilities — tools, MCP access, env, memory, knowledge, UI — plus its ordered agents.",
  },
  general: {
    label: "General",
    desc: "Web access, search, compaction, conversations, network and the rest of the machine-wide settings.",
  },
  system: {
    label: "System",
    desc: "Where PeakBot lives and runs: config and data locations, the service, the installed binary.",
  },
  raw: {
    label: "Raw YAML",
    desc: "The generated YAML for this scope, verbatim. Edits here replace the file on save.",
  },
};

const REPO_BANNER =
  "The repo file may only contain profiles:. Repo profiles are listed first and the first one is the default for conversations started here. A repo profile with the same name as a global one replaces it entirely (no merge).";

export function Settings({ convo, onClose }: SettingsProps) {
  return (
    <ToastProvider>
      <SettingsInner convo={convo} onClose={onClose} />
    </ToastProvider>
  );
}

function SettingsInner({ convo, onClose }: SettingsProps) {
  const { toast } = useToast();
  const [store] = useState(createStore);
  // The store is a plain object: a version counter is the re-render seam.
  const [, bump] = useReducer((n: number) => n + 1, 0);

  const [scope, setScopeState] = useState<Scope>("global");
  const [meta, setMeta] = useState<SettingsMeta | null>(null);
  const [active, setActive] = useState<Section>("profiles");
  const [loaded, setLoaded] = useState<Record<Scope, boolean>>({ global: false, repo: false });
  const [saving, setSaving] = useState(false);
  const [staleMsg, setStaleMsg] = useState<string | null>(null);
  const [restartKeys, setRestartKeys] = useState<string[] | null>(null);
  const [diffOpen, setDiffOpen] = useState(false);
  // Increments on every draft edit — re-arms the SaveBar's 400 ms validate.
  const [draftVersion, setDraftVersion] = useState(0);
  const [flash, setFlash] = useState<{ section: Section; pathKey: string } | null>(null);

  const desktop = isDesktop();

  // ── Loading ──────────────────────────────────────────────────────────
  useEffect(() => {
    let live = true;
    getSettings(convo ?? undefined)
      .then((m) => {
        if (live) setMeta(m);
      })
      .catch(() => {
        // The gear is already gated on the WS `settings_available`; a failed
        // meta fetch keeps the shell usable (paths fall back to the defaults).
      });
    return () => {
      live = false;
    };
  }, [convo]);

  // Fetch each scope's file once (on first open / on convo change).
  useEffect(() => {
    if (loaded[scope]) return;
    let live = true;
    getConfig(scope, convo ?? undefined)
      .then((ct) => {
        if (!live) return;
        store.setSaved(scope, ct.text, ct.sha256);
        setLoaded((l) => ({ ...l, [scope]: true }));
        bump();
      })
      .catch((e: unknown) => {
        toast(e instanceof Error ? e.message : "Failed to load the config file", "error");
      });
    return () => {
      live = false;
    };
  }, [scope, convo, loaded, store, toast, bump]);

  // ── Actions ──────────────────────────────────────────────────────────
  function switchScope(s: Scope) {
    store.setScope(s);
    setScopeState(s);
    setFlash(null);
  }

  async function validate() {
    if (!store.isDirty(scope)) return;
    const st = store.state();
    const text = st.rawText ?? stringify(st.draftDoc);
    try {
      const res = await validateConfig(scope, text, convo ?? undefined);
      store.setValidate(scope, res);
    } catch (e) {
      if (e instanceof ApiError && e.status === 422) {
        store.setValidate(scope, {
          error: e.body.error ?? "config is not valid",
          errors: e.body.errors ?? [],
        });
      }
    }
    bump();
  }

  async function save() {
    const st = store.state();
    const text = st.rawText ?? stringify(st.draftDoc);
    setSaving(true);
    setStaleMsg(null);
    try {
      const res = await putConfig(scope, text, st.sha256, convo ?? undefined);
      store.setSaved(scope, text, res.sha256);
      setRestartKeys(res.restart_required.length > 0 ? res.restart_required : null);
      toast(`Saved to ${res.path}`);
      if (res.backup) toast(`backup: ${res.backup.split("/").pop()}`);
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        setStaleMsg(e.body.error ?? "The file changed on disk since you opened Settings.");
      } else if (e instanceof ApiError && e.status === 422) {
        const errs = e.body.errors ?? [];
        store.setValidate(scope, {
          error: e.body.error ?? "config is not valid",
          errors: errs,
        });
        toast(`Save failed — ${errs.length} validation problem${errs.length === 1 ? "" : "s"}`, "error");
      } else {
        toast(e instanceof Error ? e.message : "Save failed", "error");
      }
    } finally {
      setSaving(false);
      bump();
    }
  }

  // The pane edit seam (context.ts): mutate the draft by segment array, store
  // it (drops rawText), and re-arm the validate debounce.
  function edit(path: Seg[], value: unknown) {
    const doc = store.state().draftDoc;
    set(doc, path, value);
    store.setDraft(scope, doc);
    setDraftVersion((v) => v + 1);
    bump();
  }

  function discard() {
    store.discard(scope);
    setStaleMsg(null);
    toast("Changes discarded");
    bump();
  }

  async function reload() {
    setStaleMsg(null);
    try {
      const ct = await getConfig(scope, convo ?? undefined);
      store.setSaved(scope, ct.text, ct.sha256);
      bump();
    } catch (e) {
      toast(e instanceof Error ? e.message : "Reload failed", "error");
    }
  }

  // Fix (spec §9.6): route on the segment array; a repo-file error while in
  // global scope switches scope first. The pane's matching error row flashes.
  function fixError(err: ConfigErr) {
    const target = route(err.path);
    if (err.file !== scope) switchScope(err.file);
    setActive(target.section);
    const pathKey = dotPath(err.path);
    setFlash({ section: target.section, pathKey });
    window.setTimeout(() => setFlash((f) => (f && f.pathKey === pathKey ? null : f)), 1600);
  }

  // ── Derivations ──────────────────────────────────────────────────────
  const st = store.state();
  const dirty = store.isDirty(scope);
  const validateRes = st.validate;
  const allErrors: ConfigErr[] = validateRes && "errors" in validateRes ? validateRes.errors : [];
  const restartRequired: string[] =
    validateRes && "ok" in validateRes ? validateRes.restart_required : [];
  const errorSections = new Set(
    allErrors.filter((e) => e.file === scope).map((e) => route(e.path).section),
  );
  const paneErrors = allErrors.filter(
    (e) => e.file === scope && route(e.path).section === active,
  );
  const profiles = get(st.draftDoc, ["profiles"]);
  const profileCount =
    profiles && typeof profiles === "object" ? Object.keys(profiles).length : 0;

  const globalPath = meta?.global_path ?? "~/.config/peakbot/config.yaml";
  const repoPath = meta?.repo_path ?? ".peakbot/config.yaml";
  const filePath = scope === "global" ? globalPath : repoPath;

  const navGroups: { group: string; items: { id: Section; label: string; dot?: "amber" | "blue"; lockedInRepo?: boolean }[] }[] = [
    {
      group: "CONFIG.YAML",
      items: [
        { id: "providers", label: "Providers & Models", lockedInRepo: true },
        { id: "mcps", label: "MCP servers", dot: "amber", lockedInRepo: true },
        { id: "profiles", label: "Profiles" },
        { id: "general", label: "General", dot: "blue", lockedInRepo: true },
      ],
    },
    { group: "MACHINE", items: [{ id: "system", label: "System" }] },
    { group: "ADVANCED", items: [{ id: "raw", label: "Raw YAML" }] },
  ];

  function navButton(item: { id: Section; label: string; dot?: "amber" | "blue"; lockedInRepo?: boolean }) {
    const locked = scope === "repo" && item.lockedInRepo;
    const hasErrors = errorSections.has(item.id);
    return (
      <button
        key={item.id}
        onClick={() => setActive(item.id)}
        title={locked ? "Machine-wide — read-only in repo scope" : undefined}
        className={`flex w-full cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-left text-xs ${
          active === item.id ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
        }`}
      >
        <span className={locked ? "opacity-50" : undefined}>
          {item.label}
          {locked && <span className="ml-1">🔒</span>}
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          {item.id === "profiles" && profileCount > 0 && (
            <span className="rounded-full border border-zinc-700 px-1.5 text-[10px] text-zinc-400">
              {profileCount}
            </span>
          )}
          {hasErrors && (
            <span className="h-1.5 w-1.5 rounded-full bg-red-500" title="has validation errors" />
          )}
          {!hasErrors && item.dot === "amber" && (
            <span className="h-1.5 w-1.5 rounded-full bg-amber-400" title="restart required" />
          )}
          {!hasErrors && item.dot === "blue" && (
            <span className="h-1.5 w-1.5 rounded-full bg-sky-400" title="partly restart-required" />
          )}
        </span>
      </button>
    );
  }

  const allItems = navGroups.flatMap((g) => g.items);

  return (
    <div className="fixed inset-0 z-40 flex flex-col bg-zinc-950 text-zinc-100">
      {/* Head: ← Back to chat · Settings title · scope switch · Ctrl , hint */}
      <header className="flex min-h-14 flex-wrap items-center gap-x-4 gap-y-2 border-b border-zinc-800 px-4 py-2">
        <button
          onClick={onClose}
          className="cursor-pointer text-xs text-zinc-400 hover:text-zinc-200"
        >
          ← Back to chat
        </button>
        <h1 className="text-lg font-semibold">Settings</h1>

        <div className="ml-auto flex items-center gap-3">
          <div className="flex rounded-md border border-zinc-700 p-0.5" role="group" aria-label="Scope">
            {(["global", "repo"] as const).map((s) => (
              <button
                key={s}
                onClick={() => switchScope(s)}
                className={`cursor-pointer rounded px-3 py-1 text-left text-xs ${
                  scope === s
                    ? "bg-zinc-100 font-medium text-zinc-900"
                    : "text-zinc-400 hover:text-zinc-200"
                }`}
              >
                <span>{s === "global" ? "Global" : "This repo"}</span>
                <span
                  className={`block font-mono text-[10px] ${
                    scope === s ? "text-zinc-600" : "text-zinc-500"
                  }`}
                >
                  {s === "global" ? globalPath : repoPath}
                </span>
              </button>
            ))}
          </div>
          <kbd className="rounded border border-zinc-700 px-1.5 py-0.5 font-mono text-[10px] text-zinc-500">
            Ctrl ,
          </kbd>
        </div>
      </header>

      {/* Banners */}
      {meta?.needs_setup && (
        <div className="border-b border-amber-800/60 bg-amber-950/40 px-4 py-1.5 text-xs text-amber-300">
          ⚠ PeakBot isn't configured yet — no provider or model is set up.
        </div>
      )}
      {scope === "repo" && (
        <div className="border-b border-zinc-800 bg-zinc-900/60 px-4 py-1.5 text-xs text-zinc-400">
          📁 {REPO_BANNER}
        </div>
      )}

      {/* 422 error panel (spec §9.6) */}
      <ErrorPanel errors={allErrors} scope={scope} onFix={fixError} />

      {/* Body: left nav (desktop) + pane */}
      <div className="flex min-h-0 flex-1">
        <nav className="hidden w-56 shrink-0 space-y-4 overflow-auto border-r border-zinc-800 p-3 min-[801px]:block">
          {navGroups.map((g) => (
            <div key={g.group}>
              <div className="px-2 pb-1 text-[10px] font-semibold tracking-wider text-zinc-600">
                {g.group}
              </div>
              <div className="space-y-0.5">
                {g.items.map(navButton)}
                {g.group === "MACHINE" && desktop && (
                  <a
                    href="peakbot-desktop:settings"
                    className="flex w-full cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-xs text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                  >
                    Desktop app settings ↗
                  </a>
                )}
              </div>
            </div>
          ))}
        </nav>

        <SettingsCtx.Provider
          value={{ scope, meta, doc: st.draftDoc, edit, errors: allErrors.filter((e) => e.file === scope) }}
        >
        <main className="min-w-0 flex-1 overflow-auto p-4 sm:p-6">
          {/* Mobile: the nav collapses into a select under the head */}
          <select
            value={active}
            onChange={(e) => setActive(e.target.value as Section)}
            aria-label="Section"
            className="mb-4 block w-full max-w-xs cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-200 min-[801px]:hidden"
          >
            {allItems.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
                {scope === "repo" && item.lockedInRepo ? " 🔒" : ""}
              </option>
            ))}
          </select>

          {/* Section head */}
          <div className="mb-4 flex items-baseline gap-3">
            <h2 className="text-base font-semibold">{SECTIONS[active].label}</h2>
            {active === "profiles" && profileCount > 0 && (
              <span className="rounded-full border border-zinc-700 px-2 py-0.5 text-[10px] text-zinc-400">
                {profileCount}
              </span>
            )}
          </div>

          {/* Placeholder pane — the real section content lands in later M5
              tasks. Neutral empty state: title + a short description. */}
          <p className="max-w-2xl text-xs text-zinc-500">{SECTIONS[active].desc}</p>
          {active === "raw" && (
            <p className="mt-2 max-w-2xl text-xs text-zinc-500">
              ⚠ Advanced: unknown keys are rejected at boot. Edits here replace the generated
              file on save.
              {scope === "repo" && " The repo file may only contain profiles:."}
            </p>
          )}

          {/* Fields whose path matches a validation error render red (spec
              §9.6) — until the section panes exist, the pane lists them. */}
          {paneErrors.length > 0 && (
            <ul className="mt-4 max-w-2xl space-y-2">
              {paneErrors.map((e, i) => {
                const pathKey = dotPath(e.path);
                const flashing = flash?.section === active && flash.pathKey === pathKey;
                return (
                  <li
                    key={`${pathKey}-${i}`}
                    className={`rounded border border-red-900/70 bg-red-950/30 px-3 py-2 text-xs text-red-300 ${
                      flashing ? "ring-2 ring-red-500" : ""
                    }`}
                  >
                    <code className="rounded bg-red-950/70 px-1.5 py-0.5 font-mono text-[11px]">
                      {pathKey}
                    </code>{" "}
                    <span className="text-red-200">{e.message}</span>
                  </li>
                );
              })}
            </ul>
          )}
        </main>
        </SettingsCtx.Provider>
      </div>

      {/* Save flow (spec §9.6): the bars stack in normal flow at the bottom */}
      <div className="shrink-0">
        {restartKeys !== null && (
          <RestartBanner
            keys={restartKeys}
            desktop={desktop}
            onLater={() => setRestartKeys(null)}
            onRestartNow={() => {
              window.location.href = "peakbot-desktop:restart";
            }}
          />
        )}
        <SaveBar
          scope={scope}
          dirty={dirty}
          draftVersion={draftVersion}
          restartRequired={restartRequired}
          saving={saving}
          stale={staleMsg !== null}
          staleMessage={staleMsg}
          onValidate={validate}
          onSave={save}
          onDiscard={discard}
          onReload={reload}
          onViewDiff={() => setDiffOpen(true)}
        />
      </div>

      {diffOpen && (
        <DiffModal
          path={filePath}
          oldText={stringify(parse(st.savedText))}
          newText={st.rawText ?? stringify(st.draftDoc)}
          onClose={() => setDiffOpen(false)}
        />
      )}
    </div>
  );
}
