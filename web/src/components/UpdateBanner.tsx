// Browser-style auto-update banner (autoupdate plan, "Web" section).
//
// Renders only on desktop (`window.isTauri === true`). It polls `update_status`
// on mount and every POLL_MS; the *first* `update_status` rejection stops
// polling forever (IPC seam is dead) and renders nothing. `update_install`
// drives the in-flight state. Dismiss is local (`dismissedVersion`) and resets
// only when a *new* version arrives — nothing is persisted.
import { useEffect, useRef, useState } from "react";
import type { UpdateStatus } from "../desktop";
import { getUpdateStatus, installUpdate, isDesktop } from "../desktop";

export const POLL_MS = 30_000;

// Slim full-width bar (plan §Web: zinc/emerald); TopBar's small-button shape
// retinted to the emerald palette.
const bar =
  "flex items-center gap-3 w-full border-b border-zinc-800 bg-emerald-950/40 px-3 py-1.5 text-xs text-emerald-300";
const btn =
  "cursor-pointer rounded px-1.5 py-0.5 text-emerald-200 transition-colors hover:bg-emerald-800/40";

export function UpdateBanner() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  // The user clicked install and it is in flight; "Restarting…" until it
  // settles. Distinct from the server's "installing" state ("Installing…").
  const [installing, setInstalling] = useState(false);
  // Version whose install failed locally (from a reject of update_install);
  // shows the failed face without waiting for the next poll.
  const [localFailed, setLocalFailed] = useState<string | null>(null);

  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  // Once set (first update_status rejection), fetchStatus is a no-op and the
  // interval is dropped — polling never resumes.
  const pollingStopped = useRef(false);

  useEffect(() => {
    if (!isDesktop()) return; // browser: no fetch, no interval, zero IPC calls.

    intervalRef.current = setInterval(fetchStatus, POLL_MS);
    fetchStatus(); // initial check

    return () => {
      if (intervalRef.current !== null) clearInterval(intervalRef.current);
      intervalRef.current = null;
    };
  }, []);

  async function fetchStatus() {
    if (pollingStopped.current) return;
    try {
      setStatus(await getUpdateStatus());
    } catch {
      // First update_status rejection: IPC is gone, stop polling forever
      // (no backoff) and render nothing.
      pollingStopped.current = true;
      if (intervalRef.current !== null) clearInterval(intervalRef.current);
      intervalRef.current = null;
      setStatus(null);
    }
  }

  function handleInstall() {
    if (installing) return; // only one install in flight at a time.
    // Version being installed: the one from the current polled state
    // (available or failed), else the one already recorded failed locally.
    const version =
      status?.state === "available" || status?.state === "failed"
        ? status.version
        : localFailed;
    if (!version) return;
    setInstalling(true);
    installUpdate().then(
      () => setInstalling(false),
      () => {
        setInstalling(false);
        setLocalFailed(version);
      },
    );
  }

  function dismiss() {
    if (status?.state === "available") setDismissedVersion(status.version);
  }

  // Face to render (null = render nothing). localFailed and the in-flight
  // states override the polled status — an install rejection shows its failed
  // face immediately, without waiting for the next poll.
  if (localFailed !== null) {
    return (
      <div role="status" className={bar}>
        <span className="truncate">{`Update to v${localFailed} failed`}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <button type="button" className={btn} onClick={handleInstall}>
            Retry
          </button>
        </span>
      </div>
    );
  }
  if (status?.state === "installing") {
    return <div role="status" className={bar}>Installing update…</div>;
  }
  if (installing) {
    return (
      <div role="status" className={bar}>
        <span className="truncate">Restarting…</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          {/* Primary action stays visible but inert while the download+restart
              is in flight (test contract: disabled while "Restarting…"). */}
          <button type="button" className={btn} disabled>
            Restart to update
          </button>
        </span>
      </div>
    );
  }
  if (status?.state === "available" && status.version !== dismissedVersion) {
    const version = status.version;
    return (
      <div role="status" className={bar}>
        <span className="truncate">{`Update available — v${version}`}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <button type="button" className={btn} onClick={handleInstall}>
            Restart to update
          </button>
          <button type="button" className={btn} onClick={dismiss}>
            Later
          </button>
          <button type="button" className={btn} aria-label="Dismiss" onClick={dismiss}>
            ✕
          </button>
        </span>
      </div>
    );
  }
  if (status?.state === "failed") {
    return (
      <div role="status" className={bar}>
        <span className="truncate">{`Update to v${status.version} failed`}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <button type="button" className={btn} onClick={handleInstall}>
            Retry
          </button>
        </span>
      </div>
    );
  }
  return null;
}
