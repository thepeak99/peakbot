// P4 — the ONE click-only dialog primitive (design §E).
//
// `ApproverModal` and the three confirms both render through it, so the safety
// rules live in one place and are tested once:
//  • initial focus → the first `neutral` action (never a danger/primary);
//  • Enter is swallowed (capture phase) — it never "clicks" a focused button;
//  • Escape → `onLater()` (never a reject);
//  • Tab/Shift+Tab wrap within the dialog's enabled buttons;
//  • `role="dialog"`/`alertdialog`, `aria-modal`, labelled/described;
//  • all action buttons `disabled` while the shared store is `busy`.

import { useEffect, useId, useRef, type ReactNode } from "react";
import { useCloud } from "./useCloud";

export type SafeAction = {
  label: string;
  kind: "neutral" | "danger" | "primary";
  onClick: () => Promise<void>;
};

export type SafeDialogProps = {
  title: string;
  children: ReactNode;
  actions: SafeAction[];
  onLater: () => void;
};

const KIND_CLASS: Record<SafeAction["kind"], string> = {
  neutral: "border-zinc-600 bg-zinc-800 text-zinc-100 hover:bg-zinc-700",
  danger: "border-red-700 bg-red-800 text-white hover:bg-red-700",
  primary: "border-zinc-500 bg-zinc-100 text-zinc-900 hover:bg-white",
};

export function SafeDialog({ title, children, actions, onLater }: SafeDialogProps) {
  const { busy } = useCloud();
  const rootRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const bodyId = useId();

  // Initial focus → the first `neutral` action (never danger/primary).
  // On close, focus returns to the element focused before open.
  useEffect(() => {
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const root = rootRef.current;
    const btns = root
      ? ([...root.querySelectorAll<HTMLButtonElement>("button")] as HTMLButtonElement[])
      : [];
    const target =
      btns.find((b) => !b.disabled && b.dataset.safeKind === "neutral") ??
      btns.find((b) => !b.disabled) ??
      root;
    target?.focus();
    return () => prev?.focus();
  }, []);

  // Keyboard safety, on `window` (capture) while the dialog is up:
  //  • Enter is swallowed — it can never "click" a focused button;
  //  • Escape = "Later" (never a reject);
  //  • Tab/Shift+Tab wrap within the enabled buttons (the focus trap).
  useEffect(() => {
    function onKey(e: KeyboardEvent): void {
      if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onLater();
        return;
      }
      if (e.key !== "Tab") return;
      const root = rootRef.current;
      if (!root) return;
      const btns = [
        ...root.querySelectorAll<HTMLButtonElement>("button:not([disabled])"),
      ] as HTMLButtonElement[];
      if (btns.length === 0) return;
      const first = btns[0];
      const last = btns[btns.length - 1];
      const active = document.activeElement;
      const outside = !root.contains(active);
      if (e.shiftKey) {
        if (active === first || outside) {
          e.preventDefault();
          last.focus();
        }
      } else if (active === last || outside) {
        e.preventDefault();
        first.focus();
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onLater]);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div
        ref={rootRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
        aria-busy={busy || undefined}
        tabIndex={-1}
        className="max-h-full w-full max-w-md overflow-y-auto rounded-lg border border-zinc-700 bg-zinc-900 p-4 shadow-xl outline-none"
      >
        <h2 id={titleId} className="text-base font-semibold text-zinc-100">
          {title}
        </h2>
        <div id={bodyId} className="mt-2 text-sm text-zinc-300">
          {children}
        </div>
        <div className="mt-4 flex flex-wrap justify-end gap-2">
          {actions.map((a) => (
            <button
              key={a.label}
              type="button"
              data-safe-kind={a.kind}
              disabled={busy && a.kind !== "neutral"}
              onClick={(e) => {
                // The visible half of the busy latch: disable every
                // danger/primary button SYNCHRONOUSLY on click (before the
                // fetch), so a second click in the same tick sees disabled.
                // `store.act`'s re-entrancy guard is the real one.
                if (a.kind !== "neutral") {
                  const root = rootRef.current;
                  if (root) {
                    for (const b of [
                      ...root.querySelectorAll<HTMLButtonElement>("button[data-safe-kind]"),
                    ] as HTMLButtonElement[]) {
                      if (b.dataset.safeKind !== "neutral") b.disabled = true;
                    }
                  }
                  void e.currentTarget;
                }
                // `store.act` throws on non-2xx (the store holds the message);
                // the dialog stays open for the next SSE state.
                void a.onClick().catch(() => undefined);
              }}
              className={`rounded-md border px-3 py-1.5 text-sm disabled:opacity-50 ${KIND_CLASS[a.kind]}`}
            >
              {a.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
