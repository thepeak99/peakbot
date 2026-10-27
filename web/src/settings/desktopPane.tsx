// D10 Desktop pane (contract.ts "D10"): the desktop app's own options, shown
// inside web Settings → MACHINE → Desktop (only in the desktop app). Talks to
// the three Tauri commands the desktop grants this page — see ../desktop.ts.
// Any IPC rejection is FINAL for the mount: the pane shows the fallback
// (open the bundled settings page) and stops polling.

import { useEffect, useState } from "react";
import {
  getDesktopSettings,
  toggleDesktopBackground,
  type DesktopView,
} from "../desktop";

type State =
  | { kind: "loading" }
  | { kind: "ready"; view: DesktopView }
  | { kind: "unavailable" };

const CARD = "max-w-2xl rounded-lg border border-zinc-800 bg-zinc-900/40 p-4";
const BTN =
  "cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-200 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60";

export function DesktopPane() {
  const [state, setState] = useState<State>({ kind: "loading" });

  // Poll the desktop view: once on mount, then every second. The first
  // rejection ends it (unavailable is final per mount).
  useEffect(() => {
    let live = true;
    let timer: ReturnType<typeof setInterval> | undefined;
    const stop = () => {
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
    };
    const poll = () => {
      getDesktopSettings().then(
        (view) => {
          if (live) setState({ kind: "ready", view });
        },
        () => {
          stop();
          if (live) setState({ kind: "unavailable" });
        },
      );
    };
    poll();
    timer = setInterval(poll, 1000);
    return () => {
      live = false;
      stop();
    };
  }, []);

  const toggle = () => {
    // No optimistic flip: the controller confirms (dialog) and the next poll
    // reports the real state.
    toggleDesktopBackground().catch(() => setState({ kind: "unavailable" }));
  };

  return (
    <div data-testid="desktop-pane" className="space-y-4">
      <section className={CARD}>
        <h3 className="mb-2 text-xs font-semibold text-zinc-300">Background</h3>
        {state.kind === "loading" && (
          <p data-testid="desktop-loading" className="text-xs text-zinc-500">
            Loading…
          </p>
        )}
        {state.kind === "ready" && state.view.background_visible && (
          <div className="space-y-2">
            <label className="flex items-center gap-2 text-xs text-zinc-300">
              <input
                data-testid="desktop-background"
                type="checkbox"
                checked={state.view.background}
                disabled={!state.view.background_enabled}
                onChange={toggle}
              />
              Keep PeakBot running in background
            </label>
            <p data-testid="desktop-background-note" className="text-xs text-zinc-500">
              Runs PeakBot as a background service that keeps going after you quit the app.
              Changing this restarts PeakBot.
            </p>
          </div>
        )}
        {state.kind === "ready" && !state.view.background_visible && (
          <p data-testid="desktop-background-unsupported" className="text-xs text-zinc-500">
            Background mode is not available on this platform yet.
          </p>
        )}
        {state.kind === "unavailable" && (
          <div className="space-y-3">
            <p data-testid="desktop-unavailable" className="text-xs text-zinc-400">
              Desktop options can't be loaded here — PeakBot is not on the desktop app's default
              address, or the desktop app is older.
            </p>
            <button
              data-testid="desktop-open-settings-page"
              onClick={() => {
                window.location.href = "peakbot-desktop:settings";
              }}
              className={BTN}
            >
              Open desktop settings
            </button>
          </div>
        )}
      </section>
    </div>
  );
}
