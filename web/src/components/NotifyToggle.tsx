// Opt-in toggle for task-completion notifications (issue #119). A bell that
// lights up when enabled; clicking it flips the per-session opt-in (and, the
// first time, prompts for browser permission). Hidden entirely when the
// browser has no Notification API.

import type { NotifyPermission } from "../useTaskNotifications";

export function NotifyToggle({
  enabled,
  permission,
  onToggle,
}: {
  enabled: boolean;
  permission: NotifyPermission;
  onToggle: () => void;
}) {
  if (permission === "unsupported") return null;

  const blocked = permission === "denied";
  const on = enabled && permission === "granted";
  const title = blocked
    ? "Notifications blocked in browser settings"
    : on
      ? "Notify me when a task finishes (on) — click to disable"
      : "Notify me when a task finishes (off) — click to enable";

  return (
    <button
      type="button"
      onClick={onToggle}
      disabled={blocked}
      title={title}
      aria-label={title}
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
  );
}
