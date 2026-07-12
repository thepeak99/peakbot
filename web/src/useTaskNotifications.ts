// Browser notification on task completion (issue #119). Fires a Notification
// when the agent transitions running → idle *while the tab is not focused*, so
// the user can walk away and be pinged when PeakBot is done — success or not.
//
// Opt-in per session: `enabled` is user-controlled (a toggle in the UI) and
// starts off. Turning it on requests Notification permission; if the user
// denies it, we report `blocked` so the UI can explain why the toggle did
// nothing. Nothing fires while the tab is focused — you're already looking.

import { useCallback, useEffect, useRef, useState } from "react";

export type NotifyPermission = "unsupported" | "default" | "granted" | "denied";

function currentPermission(): NotifyPermission {
  if (typeof Notification === "undefined") return "unsupported";
  return Notification.permission as NotifyPermission;
}

export interface TaskNotifications {
  /** User opted in this session. */
  enabled: boolean;
  /** True once the browser has granted permission. */
  permission: NotifyPermission;
  /** Toggle opt-in. Turning on requests permission if not yet granted. */
  toggle: () => void;
}

export function useTaskNotifications(isRunning: boolean): TaskNotifications {
  const [enabled, setEnabled] = useState(false);
  const [permission, setPermission] = useState<NotifyPermission>(currentPermission);
  // Remember the previous running state to detect the running → idle edge.
  const wasRunning = useRef(isRunning);

  const toggle = useCallback(() => {
    setEnabled((on) => {
      const next = !on;
      // Ask for permission the first time the user opts in.
      if (next && typeof Notification !== "undefined" && Notification.permission === "default") {
        void Notification.requestPermission().then((p) =>
          setPermission(p as NotifyPermission),
        );
      }
      return next;
    });
  }, []);

  useEffect(() => {
    const done = wasRunning.current && !isRunning; // running → idle edge
    wasRunning.current = isRunning;
    if (!done || !enabled) return;
    if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
    // Only nag when the user has looked away — no point interrupting a
    // focused tab they're already watching.
    if (typeof document !== "undefined" && !document.hidden) return;
    try {
      const n = new Notification("PeakBot", {
        body: "Task complete — ready for your next message.",
        tag: "peakbot-task", // collapse repeats into one
      });
      n.onclick = () => {
        window.focus();
        n.close();
      };
    } catch {
      // Some browsers throw if the page isn't a secure context — ignore.
    }
  }, [isRunning, enabled]);

  return { enabled, permission, toggle };
}
