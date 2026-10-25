// Minimal local toast primitive for the Settings view (M5 decision: a
// self-contained Toast.tsx in web/src/settings/, no global toast system).
// Plain toasts, no actions — auto-dismiss after a few seconds, stacked
// bottom-right (the mockup's position).

import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";

export type ToastKind = "info" | "error";

interface ToastItem {
  id: number;
  text: string;
  kind: ToastKind;
}

interface ToastContextValue {
  toast(text: string, kind?: ToastKind): void;
}

const ToastContext = createContext<ToastContextValue>({ toast: () => {} });

/** Show a toast (info by default). Safe to call from event handlers. */
export function useToast(): ToastContextValue {
  return useContext(ToastContext);
}

const TOAST_TTL_MS = 4000;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => {
    setToasts((ts) => ts.filter((t) => t.id !== id));
  }, []);

  const toast = useCallback(
    (text: string, kind: ToastKind = "info") => {
      const id = nextId.current++;
      setToasts((ts) => [...ts, { id, text, kind }]);
      setTimeout(() => dismiss(id), TOAST_TTL_MS);
    },
    [dismiss],
  );

  return (
    <ToastContext.Provider value={{ toast }}>
      {children}
      {toasts.length > 0 && (
        <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex flex-col items-end gap-2">
          {toasts.map((t) => (
            <div
              key={t.id}
              role="status"
              className={`pointer-events-auto flex items-center gap-2 rounded-md border px-3 py-2 text-xs shadow-lg ${
                t.kind === "error"
                  ? "border-red-900 bg-red-950/95 text-red-200"
                  : "border-zinc-700 bg-zinc-900/95 text-zinc-200"
              }`}
            >
              {t.kind === "error" && <span aria-hidden="true">✕</span>}
              <span>{t.text}</span>
              <button
                onClick={() => dismiss(t.id)}
                className="ml-1 cursor-pointer text-zinc-500 hover:text-zinc-300"
                aria-label="Dismiss"
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
    </ToastContext.Provider>
  );
}
