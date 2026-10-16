// Focus-mode toggle (plan §6.2). Dumb button — the preference lives in
// App's useFocusMode hook (two useState copies would desync), so this only
// renders `enabled` and reports clicks. Same 6×6 icon-button shape as
// ThemeToggle/NotifyToggle.

export function FocusToggle({
  enabled,
  onToggle,
}: {
  enabled: boolean;
  onToggle: () => void;
}) {
  const title = enabled
    ? "Focus mode on — showing only your messages and final answers (click to show all steps)"
    : "Focus mode off — showing every step (click to focus)";

  return (
    <button
      type="button"
      onClick={onToggle}
      title={title}
      aria-label="Focus mode"
      aria-pressed={enabled}
      className={`flex h-6 w-6 cursor-pointer items-center justify-center rounded transition-colors ${
        enabled ? "text-sky-400 hover:text-sky-300" : "text-zinc-500 hover:text-zinc-300"
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
        {enabled ? (
          // Eye-off: focus is filtering the transcript.
          <>
            <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" />
            <line x1="1" y1="1" x2="22" y2="22" />
          </>
        ) : (
          // Full eye: every step is on screen.
          <>
            <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
            <circle cx="12" cy="12" r="3" />
          </>
        )}
      </svg>
    </button>
  );
}
