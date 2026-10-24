// T33 (D10 trigger + D12 `/setup` alias, spec §9.13): the first-run gate.
//
// `Boot` wraps <App/> in main.tsx and owns the `needs_setup` decision so the
// chat (App → useAgent → /ws) is NEVER mounted on an unconfigured server —
// the socket would 503. On mount it fetches GET /api/settings; only when
// `needs_setup` does it also fetch GET /api/config?scope=global (for the §6.9
// derived restart-pending state). Any failure of the settings endpoint (404
// on an older server, 403, network) falls back to rendering the children —
// Boot must never block the chat.
//
// `/setup` is an alias for old bookmarks: with `needs_setup` it lands on the
// same guided FirstRun; once configured it is just the chat.

import { useEffect, useState, type ReactNode } from "react";
import { getConfig, getSettings } from "./settings/api";
import { FirstRun } from "./settings/firstRun";
import type { ConfigText, SettingsMeta } from "./settings/types";

type Phase =
  | { kind: "loading" }
  | { kind: "app" }
  | { kind: "setup"; meta: SettingsMeta; global: ConfigText; closed: boolean };

export function Boot({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const meta = await getSettings();
        if (cancelled) return;
        if (!meta.needs_setup) {
          setPhase({ kind: "app" });
          return;
        }
        const global = await getConfig("global");
        if (cancelled) return;
        setPhase({ kind: "setup", meta, global, closed: false });
      } catch {
        // 404 (older server without /api/settings), 403, network — never
        // block the chat over the settings probe.
        if (!cancelled) setPhase({ kind: "app" });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  if (phase.kind === "loading") {
    return (
      <div
        data-testid="boot-loading"
        className="flex h-dvh items-center justify-center bg-zinc-950 text-sm text-zinc-500"
      >
        Loading…
      </div>
    );
  }

  if (phase.kind === "app") return <>{children}</>;

  // Wizard closed with Esc: the strip stays, with a way back in.
  if (phase.closed) {
    const strip = phase.meta.legacy_layout
      ? "PeakBot couldn't migrate your old config"
      : "PeakBot isn't configured yet — no provider or model is set up.";
    return (
      <div className="flex h-dvh flex-col bg-zinc-950 text-zinc-100">
        <div className="mx-auto mt-4 flex w-full max-w-3xl items-center gap-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-4 py-2">
          <span data-testid="setup-strip" className="flex-1 text-xs text-amber-200">
            ⚠ {strip}
          </span>
          <button
            data-testid="setup-strip-finish"
            type="button"
            onClick={() => setPhase({ ...phase, closed: false })}
            className="cursor-pointer rounded-md bg-emerald-700 px-3 py-1 text-xs font-medium text-white hover:bg-emerald-600"
          >
            Finish setup
          </button>
        </div>
      </div>
    );
  }

  return (
    <FirstRun
      meta={phase.meta}
      global={phase.global}
      onClose={() => setPhase({ ...phase, closed: true })}
    />
  );
}
