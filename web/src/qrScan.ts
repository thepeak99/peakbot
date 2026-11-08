// Scan a QR code through the Tauri barcode-scanner plugin, called straight
// from the loopback origin via `window.__TAURI_INTERNALS__.invoke` (no
// guest JS package). The scanned text lands in the pairing-link input —
// one path, one validator. Mobile shell only (the camera capability is
// granted there and nowhere else).

import { shell } from "./desktop";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

const CHECK = "plugin:barcode-scanner|check_permissions";
const REQUEST = "plugin:barcode-scanner|request_permissions";
const SCAN = "plugin:barcode-scanner|scan";
const CANCEL = "plugin:barcode-scanner|cancel";

/** The locked camera-denied message (moved over from the old native page, verbatim). */
export const CAMERA_DENIED =
  "Camera access is needed to scan the QR code. Allow it in your phone's settings, then try again.";

// Plugin failure strings can embed the full page URL (including `?token=`)
// — a raw message is only ever shown when it carries no URL at all.
const SCAN_FAILED = "Couldn't scan the QR code. Try again, or paste the link instead.";

export type ScanOutcome =
  | { ok: true; text: string }
  | { ok: false; cancelled: true }
  | { ok: false; cancelled: false; message: string };

function invokeAvailable(): Invoke | null {
  const internals = (globalThis as { __TAURI_INTERNALS__?: { invoke?: Invoke } }).__TAURI_INTERNALS__;
  return typeof internals?.invoke === "function" ? internals.invoke : null;
}

export async function scanQrCode(): Promise<ScanOutcome> {
  const invoke = invokeAvailable();
  if (shell() !== "mobile" || invoke === null) {
    return { ok: false, cancelled: false, message: SCAN_FAILED };
  }

  let granted = false;
  try {
    const res = (await invoke(CHECK)) as { camera?: string };
    granted = res?.camera === "granted";
    if (!granted) granted = ((await invoke(REQUEST)) as { camera?: string })?.camera === "granted";
  } catch {
    return { ok: false, cancelled: false, message: SCAN_FAILED };
  }
  if (!granted) return { ok: false, cancelled: false, message: CAMERA_DENIED };

  // The OS Back over the camera UI is a popstate (at the SPA root it would
  // exit the app): swallow it with a pushed entry, and cancel the scan.
  let backCancelled = false;
  const onPop = () => {
    backCancelled = true;
    void invoke(CANCEL).catch(() => {});
  };
  window.history.pushState(window.history.state, "");
  window.addEventListener("popstate", onPop);
  try {
    const text = await invoke(SCAN, { formats: ["QR_CODE"] });
    return typeof text === "string" && text !== ""
      ? { ok: true, text }
      : { ok: false, cancelled: false, message: SCAN_FAILED };
  } catch (e) {
    return scanFailure(e, backCancelled);
  } finally {
    window.removeEventListener("popstate", onPop);
    // The scan settled without the Back gesture: the pushed entry is
    // dangling — consume it so a later Back lands on the baseline.
    if (!backCancelled) window.history.back();
  }
}

function scanFailure(e: unknown, backCancelled: boolean): ScanOutcome {
  const message = e instanceof Error ? e.message : (e as { message?: unknown } | null)?.message;
  // The plugin's cancel rejects with the OBJECT { message: "cancelled" } —
  // it must never reach the DOM stringified.
  if (backCancelled || message === "cancelled") return { ok: false, cancelled: true };
  return {
    ok: false,
    cancelled: false,
    message: typeof message === "string" && !/https?:\/\//.test(message) ? message : SCAN_FAILED,
  };
}
