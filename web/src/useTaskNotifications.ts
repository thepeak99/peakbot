// Browser notification on task completion (issue #119). Fires a Notification
// when the agent transitions running → idle *while the user has looked away*,
// so the user can walk away and be pinged when Shifu is done — success or not.
//
// Opt-in, persisted per browser under `peakbot-notify`; restored only while
// the browser still reports permission "granted". An optional beep
// (`peakbot-notify-sound`, default off) is independent of permission. Both
// fire on the running → idle edge only when the user has looked away: tab
// hidden OR window unfocused. Inside the Tauri shells the web Notification
// path is shimmed/broken, so we report "unsupported" (native notifications
// are a later change).
//
// Mobile browsers reject the `new Notification()` constructor with
// `TypeError: Illegal constructor` — they require a service worker's
// `showNotification()`. We register a notification-only SW (see public/sw.js)
// and prefer it, falling back to the constructor on desktop when no SW is
// ready. Without this, the bell "did nothing" on mobile: it lit up and
// permission was granted, but every ping threw and was silently swallowed.

import { useCallback, useEffect, useRef, useState } from "react";
import { shell } from "./desktop";

const NOTIFY_KEY = "peakbot-notify";
const SOUND_KEY = "peakbot-notify-sound";

// Registration of the notification service worker, resolved once ready. Skipped
// on insecure origins where the browser refuses to register it anyway.
let swRegistration: ServiceWorkerRegistration | null = null;
if (
  typeof navigator !== "undefined" &&
  "serviceWorker" in navigator &&
  (typeof window === "undefined" || window.isSecureContext !== false)
) {
  navigator.serviceWorker.register("/sw.js").catch(() => {
    /* registration failed (insecure context, etc.) — desktop fallback covers it */
  });
  void navigator.serviceWorker.ready.then((reg) => {
    swRegistration = reg;
  });
}

export type NotifyPermission = "unsupported" | "insecure" | "default" | "granted" | "denied";

// `localhost`/HTTPS are secure contexts; a phone reaching us over a LAN IP on
// plain HTTP is not. Browsers hard-gate both the Notification permission prompt
// and service-worker registration to secure contexts, so on an insecure origin
// the bell can never be enabled — we report `insecure` so the UI can say why
// (rather than the misleading "blocked in settings").
function isSecureContext(): boolean {
  return typeof window === "undefined" || window.isSecureContext !== false;
}

function currentPermission(): NotifyPermission {
  if (shell() !== "browser" || typeof Notification === "undefined") return "unsupported";
  if (!isSecureContext()) return "insecure";
  return Notification.permission as NotifyPermission;
}

// The user isn't watching: tab in the background, or another window has focus.
function lookingAway(): boolean {
  return document.hidden || !document.hasFocus();
}

// Fire-and-forget notification. Prefers the service worker's
// `showNotification()` (required on mobile), falling back to the
// `new Notification()` constructor on desktop when no SW is ready. Swallows
// the throw some browsers raise outside a secure context.
function notify(title: string, body: string) {
  const options: NotificationOptions = { body, tag: "peakbot-task" };
  if (swRegistration) {
    swRegistration.showNotification(title, options).catch(() => {
      /* mobile with no fallback — nothing more we can do */
    });
    return;
  }
  try {
    new Notification(title, options);
  } catch {
    /* insecure context, blocked, or mobile constructor rejection */
  }
}

// One lazily created context, reused: browsers cap live AudioContexts, and
// one unlocked by a click (the speaker toggle) stays allowed to play.
let audio: AudioContext | null = null;

// A short soft tone. Silently does nothing where WebAudio is missing or the
// autoplay policy still holds the context suspended.
function beep() {
  if (typeof AudioContext === "undefined") return;
  try {
    audio ??= new AudioContext();
    void audio.resume().catch(() => {});
    const t = audio.currentTime;
    const osc = audio.createOscillator();
    const gain = audio.createGain();
    osc.frequency.value = 880;
    gain.gain.setValueAtTime(0.15, t);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + 0.2);
    osc.connect(gain).connect(audio.destination);
    osc.start(t);
    osc.stop(t + 0.2);
  } catch {
    /* audio unavailable — the notification (if any) still fired */
  }
}

export interface TaskNotifications {
  /** User opted in (persisted; restored only while permission is "granted"). */
  enabled: boolean;
  permission: NotifyPermission;
  /** Flip the opt-in; turning on asks for permission if still "default". */
  toggle: () => void;
  /** Beep on turn end (persisted, default off). Independent of permission. */
  sound: boolean;
  toggleSound: () => void;
}

export function useTaskNotifications(isRunning: boolean): TaskNotifications {
  const [enabled, setEnabled] = useState(
    () => localStorage.getItem(NOTIFY_KEY) === "on" && currentPermission() === "granted",
  );
  const [sound, setSound] = useState(() => localStorage.getItem(SOUND_KEY) === "on");
  const [permission, setPermission] = useState<NotifyPermission>(currentPermission);
  // Remember the previous running state to detect the running → idle edge.
  const wasRunning = useRef(isRunning);

  useEffect(() => {
    localStorage.setItem(NOTIFY_KEY, enabled ? "on" : "off");
  }, [enabled]);
  useEffect(() => {
    localStorage.setItem(SOUND_KEY, sound ? "on" : "off");
  }, [sound]);

  const toggle = useCallback(() => {
    const next = !enabled;
    setEnabled(next);
    if (!next || typeof Notification === "undefined") return;
    if (Notification.permission === "granted") {
      notify("Shifu notifications on", "You'll be pinged when a task finishes.");
    } else if (Notification.permission === "default") {
      // Ask for permission the first time the user opts in; confirm once granted.
      void Notification.requestPermission().then((p) => {
        setPermission(p as NotifyPermission);
        if (p === "granted")
          notify("Shifu notifications on", "You'll be pinged when a task finishes.");
      });
    }
  }, [enabled]);

  const toggleSound = useCallback(() => {
    const next = !sound;
    setSound(next);
    if (next) beep(); // in the click: confirms it works and unlocks audio
  }, [sound]);

  useEffect(() => {
    const done = wasRunning.current && !isRunning; // running → idle edge
    wasRunning.current = isRunning;
    if (!done || !lookingAway()) return;
    if (enabled && currentPermission() === "granted") {
      notify("Shifu", "Task complete — ready for your next message.");
    }
    if (sound) beep();
  }, [isRunning, enabled, sound]);

  return { enabled, permission, toggle, sound, toggleSound };
}
