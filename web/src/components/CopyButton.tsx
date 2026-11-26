// Shared icon-only copy button used for code blocks (phone and desktop).
// Contract (locked by CopyButton.test.tsx):
// - button type="button", icon-only, idle aria-label={label}
// - success aria-label="Copied" for exactly 1500ms then back to {label}
// - getText() evaluated once per click, NOT at render
// - navigator.clipboard.writeText used; missing API or rejected write = no throw, no success
// - repeated clicks restart the 1500ms window; unmount clears any pending timer
import { useEffect, useRef, useState } from "react";

// Lifted from Message.tsx's existing CodeBlock icons; kept module-private here.
const COPY_ICON = (
  <svg
    aria-hidden
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15V6a2 2 0 0 1 2-2h9" />
  </svg>
);

const CHECK_ICON = (
  <svg
    aria-hidden
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <path d="M5 12l5 5 9-10" />
  </svg>
);

export interface CopyButtonProps {
  getText(): string;
  label: string;
  className?: string;
}

export function CopyButton({ getText, label, className }: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);

  // Clear the pending success timer on unmount.
  useEffect(() => {
    return () => {
      if (timer.current !== null) {
        window.clearTimeout(timer.current);
      }
    };
  }, []);

  const onClick = () => {
    const text = getText();
    const clipboard = navigator.clipboard;
    if (!clipboard || typeof clipboard.writeText !== "function") return;
    // `writeText` returns a Promise in browsers; `Promise.resolve` keeps older
    // stubs/hosts that return nothing from throwing on `.then`.
    Promise.resolve(clipboard.writeText(text))
      .then(() => {
        if (timer.current !== null) window.clearTimeout(timer.current);
        setCopied(true);
        timer.current = window.setTimeout(() => {
          timer.current = null;
          setCopied(false);
        }, 1500);
      })
      .catch(() => {});
  };

  return (
    <button
      type="button"
      aria-label={copied ? "Copied" : label}
      onClick={onClick}
      className={className}
    >
      {copied ? CHECK_ICON : COPY_ICON}
    </button>
  );
}
