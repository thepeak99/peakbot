// Turn-end alert controls: a bell (browser notification opt-in; prompts for
// permission the first time) and a speaker (beep opt-in). Both persist per
// browser. Hidden when the web Notification API is unavailable, including
// inside the PeakBot apps.

import type { TaskNotifications } from "../useTaskNotifications";

const INSECURE =
  "Notifications need HTTPS — this page was opened over plain HTTP. With PeakBot's --tls, this device must first trust the PeakBot CA (download it from /peakbot-ca.crt).";
const DENIED =
  "Notifications are blocked for this site — re-enable them in your browser's site settings, then reload.";
const BELL_ON = "Notify me when a task finishes (on) — click to disable";
const BELL_OFF = "Notify me when a task finishes (off) — click to enable";
const SOUND_ON = "Sound when a task finishes (on) — click to mute";
const SOUND_OFF = "Sound when a task finishes (off) — click to enable";

export function NotifyToggle({ notify }: { notify: TaskNotifications }) {
  const { enabled, permission, toggle, sound, toggleSound } = notify;
  if (permission === "unsupported") return null;

  // Both `denied` (user/site blocked it) and `insecure` (plain-HTTP origin, e.g.
  // a phone on the LAN) make the bell unusable — but for different reasons, so
  // the tooltip must say which.
  const insecure = permission === "insecure";
  const blocked = permission === "denied" || insecure;
  // `on` reflects user intent (the click) — not whether the browser has granted
  // permission yet. The first click on a fresh tab lights the bell even while
  // the permission prompt is still open; the browser prompt resolves in parallel
  // and either flips the bell functional or shows the disabled/blocked state.
  const on = enabled && !blocked;
  const bellTitle = insecure
    ? INSECURE
    : permission === "denied"
      ? DENIED
      : on
        ? BELL_ON
        : BELL_OFF;
  const soundTitle = sound ? SOUND_ON : SOUND_OFF;

  return (
    <>
      <button
        type="button"
        onClick={toggle}
        disabled={blocked}
        title={bellTitle}
        aria-label={bellTitle}
        aria-pressed={on}
        className={`flex h-6 w-6 cursor-pointer items-center justify-center rounded transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
          on ? "text-amber-400 hover:text-amber-300" : "text-zinc-500 hover:text-zinc-300"
        }`}
      >
        <svg
          viewBox="0 0 24 24"
          fill={on ? "currentColor" : "none"}
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className="h-4 w-4"
        >
          <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
          <path d="M13.73 21a2 2 0 0 1-3.46 0" />
          {blocked && <line x1="3" y1="3" x2="21" y2="21" />}
        </svg>
      </button>
      {/* Never disabled: WebAudio has no secure-context gate, so on a plain-HTTP
          LAN phone the beep is the one alert that still works. */}
      <button
        type="button"
        onClick={toggleSound}
        title={soundTitle}
        aria-label={soundTitle}
        aria-pressed={sound}
        className={`flex h-6 w-6 cursor-pointer items-center justify-center rounded transition-colors ${
          sound ? "text-amber-400 hover:text-amber-300" : "text-zinc-500 hover:text-zinc-300"
        }`}
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className="h-4 w-4"
        >
          <path d="M11 5 6 9H2v6h4l5 4V5z" fill={sound ? "currentColor" : "none"} />
          {sound ? (
            <path d="M15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14" />
          ) : (
            <>
              <line x1="23" y1="9" x2="17" y2="15" />
              <line x1="17" y1="9" x2="23" y2="15" />
            </>
          )}
        </svg>
      </button>
    </>
  );
}
