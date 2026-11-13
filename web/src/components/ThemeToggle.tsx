// Light/dark toggle. Self-contained: owns the useTheme hook so no theme
// state has to be threaded through props (nothing else needs it). Styled to
// match NotifyToggle — a 6×6 icon button that swaps a sun (light) for a moon
// (dark), showing the theme you'll switch TO.

import { useTheme } from "../useTheme";
import { IB, IBI } from "./phoneUi";

// `phone` (the Chats header) draws the mock's 34px `.ibi` box; the default is
// today's desktop button, unchanged.
export function ThemeToggle({ phone = false }: { phone?: boolean }) {
  const { theme, toggle } = useTheme();
  const isDark = theme === "dark";
  const title = isDark ? "Switch to light theme" : "Switch to dark theme";

  const icon = (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={phone ? "h-5 w-5" : "h-4 w-4"}
    >
      {isDark ? (
        // Sun — click to go light.
        <>
          <circle cx="12" cy="12" r="4" />
          <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
        </>
      ) : (
        // Moon — click to go dark.
        <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
      )}
    </svg>
  );

  if (phone) {
    return (
      <button type="button" onClick={toggle} title={title} aria-label={title} className={IB}>
        <span className={IBI}>{icon}</span>
      </button>
    );
  }
  return (
    <button
      type="button"
      onClick={toggle}
      title={title}
      aria-label={title}
      className="flex h-6 w-6 cursor-pointer items-center justify-center rounded text-zinc-500 transition-colors hover:text-zinc-300"
    >
      {icon}
    </button>
  );
}
