// Focus-mode preference (plan §6.1): default ON, persisted per browser under
// "peakbot-focus". Called ONCE in App with the value threaded to TopBar via
// props — a self-contained toggle would leave two useState copies to desync.

import { useCallback, useEffect, useState } from "react";

const STORAGE_KEY = "peakbot-focus";

export function readFocusPref(raw: string | null): boolean {
  return raw !== "off";
}

export function useFocusMode() {
  const [focus, setFocus] = useState(() => readFocusPref(localStorage.getItem(STORAGE_KEY)));

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, focus ? "on" : "off");
  }, [focus]);

  const toggle = useCallback(() => setFocus((f) => !f), []);

  return { focus, toggle };
}
