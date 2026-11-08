// Scan QR in the Settings → Profiles paste box — mobile shell only.
//
// RED today: the paste box has no Scan button, no `__TAURI_INTERNALS__.invoke`
// path, and no scan-error handling. The loopback origin (the on-device core)
// gains a capability that grants ONLY the barcode-scanner permissions to
// `http://127.0.0.1:*`, so the SPA calls the plugin straight through
// `window.__TAURI_INTERNALS__.invoke` (no guest JS package).
//
// ── CONTRACT pinned here ─────────────────────────────────────────────────
// LIST (ProfilesPane, the "Add remote profile" paste step):
//   * mobile shell ONLY: a "Scan QR" button (`data-testid="remote-add-scan"`)
//     in the paste box next to the input. Desktop and browser render no
//     button (their shells have no camera capability).
//   * click → permission flow through `window.__TAURI_INTERNALS__.invoke`:
//       1. `plugin:barcode-scanner|check_permissions` → `{ camera }`;
//       2. when not granted: `plugin:barcode-scanner|request_permissions`
//          → `{ camera }`;
//       3. `plugin:barcode-scanner|scan` (formats `["QR_CODE"]`) → the
//          scanned text lands in the SAME paste input (`remote-add-input`) —
//          one path, one validator (`Config::parse` via the Add button).
//   * camera denied (check AND request report it) → the locked message from
//     the old native page, verbatim, in the paste box's error slot
//     (`remote-add-error`); no scan call, nothing entered.
//   * the plugin's scan rejects with the OBJECT `{ message: "cancelled" }`
//     (user pressed the OS Back over the camera UI) → clean state: the
//     object is NEVER stringified into the DOM (`[object Object]` must not
//     appear), no error is claimed, the paste box stays open.
//   * Back cancels the scan: the Scan click handler pushes a history entry;
//     a `popstate` while the scan is in flight calls
//     `plugin:barcode-scanner|cancel` (the plugin then rejects with the
//     cancelled object above). Without the push, Back at the SPA root would
//     exit the app.
//   * a raw IPC failure STRING (e.g. one that embeds the pairing link) is
//     never rendered verbatim: the token must not leak into the DOM.
//
// Harness: same as ./profiles.t10.test.tsx (ctx provider + raw DOM queries).
// `window.__TAURI_INTERNALS__` is mocked per test and deleted in teardown.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilesPane } from "./profiles";
import { SettingsCtx, type SettingsCtxValue } from "./context";
import { parse } from "./yamlDoc";
import { ToastProvider } from "./Toast";
import type { SettingsMeta } from "./types";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// The locked camera-denied message (moved over from the old native page,
// verbatim).
const CAMERA_DENIED =
  "Camera access is needed to scan the QR code. Allow it in your phone's settings, then try again.";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
const g = globalThis as {
  peakbotShell?: unknown;
  __TAURI_INTERNALS__?: { invoke: Invoke };
};

let root: Root;
let host: HTMLElement;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(<div>{node}</div>));
}

function unmount() {
  act(() => root.unmount());
  host.remove();
}

afterEach(() => {
  unmount();
  delete g.peakbotShell;
  delete g.__TAURI_INTERNALS__;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

function setShell(marker: "desktop" | "mobile" | "browser") {
  delete g.peakbotShell;
  if (marker !== "browser") g.peakbotShell = marker;
}

function meta(): SettingsMeta {
  return {
    needs_setup: false,
    legacy_layout: false,
    migrate_error: null,
    settings_available: true,
    global_path: "/tmp/peakbot/config.yaml",
    repo_path: "/tmp/repo/.peakbot/config.yaml",
    repo_exists: false,
    builtin_tools: [],
    tab_names: ["session", "settings"],
    skills: [],
    builtin_persona: "",
    mcp_status: [],
    boot: { bind: "127.0.0.1:1", token_source: "none", tls: false },
    facts: {
      os: "linux",
      arch: "arm64",
      exe_path: "/apex/peakbot",
      data_dir: "/tmp/peakbot-data",
      version: "0.27.0",
      install: null,
    },
  };
}

const DOC = `
profiles:
  dev:
    agents:
      main:
        model: sonnet
`;

function mountList(): void {
  const ctx: SettingsCtxValue = {
    scope: "global",
    meta: meta(),
    doc: parse(DOC),
    globalDoc: parse(DOC),
    edit: () => {},
    errors: [],
    switchToGlobal: () => {},
  };
  mount(
    <ToastProvider>
      <SettingsCtx.Provider value={ctx}>
        <ProfilesPane />
      </SettingsCtx.Provider>
    </ToastProvider>,
  );
}

/** Open the "Add remote profile" paste step. */
function openPasteStep() {
  act(() => {
    host
      .querySelector('[data-testid="profile-add-remote"]')!
      .dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  expect(
    host.querySelector('[data-testid="remote-add-paste"]'),
    "the paste step appears",
  ).not.toBeNull();
}

/** Click the Scan QR button (asserting it exists, so a missing feature fails
 *  as an assertion, not a null dereference). */
function clickScan() {
  const scan = host.querySelector('[data-testid="remote-add-scan"]');
  expect(scan, "the Scan QR button must render in the mobile paste box").not.toBeNull();
  act(() => {
    scan!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

/** A mock `__TAURI_INTERNALS__.invoke` wired to the camera permission states. */
function installInvoke(impl: Invoke): Invoke {
  const invoke = vi.fn(impl);
  g.__TAURI_INTERNALS__ = { invoke };
  return invoke;
}

const GRANTED = (impl: Invoke) => installInvoke(impl) as ReturnType<typeof vi.fn>;

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await new Promise((r) => setTimeout(r, 0));
  });
}

// ─── button presence per shell ────────────────────────────────────────────

describe("Scan QR button presence per shell", () => {
  it("mobile: the paste box offers a Scan QR button", () => {
    setShell("mobile");
    mountList();
    openPasteStep();
    const scan = host.querySelector('[data-testid="remote-add-scan"]');
    expect(scan, "the mobile paste box must offer Scan QR").not.toBeNull();
    expect(scan!.textContent).toContain("Scan QR");
  });

  it.each([
    ["desktop", "desktop"],
    ["browser", "browser"],
  ] as const)("%s: no Scan QR button (unchanged)", (_label, marker) => {
    setShell(marker);
    mountList();
    openPasteStep();
    expect(
      host.querySelector('[data-testid="remote-add-scan"]'),
      "no camera capability outside the mobile shell",
    ).toBeNull();
  });
});

// ─── the scan flow ────────────────────────────────────────────────────────

describe("Scan QR flow (mobile shell, mocked __TAURI_INTERNALS__)", () => {
  const LINK = `https://10.2.0.48:7899/?token=pito&ca=sha256:${"ab".repeat(32)}&profile=dev`;

  beforeEach(() => setShell("mobile"));

  it("a_granted_camera_scan_fills_the_paste_input", async () => {
    mountList();
    openPasteStep();
    const invoke = GRANTED(async (cmd) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") return LINK;
      throw new Error(`unexpected command: ${cmd}`);
    });

    clickScan();
    await flush();

    expect(invoke).toHaveBeenCalledWith("plugin:barcode-scanner|check_permissions");
    expect(invoke).toHaveBeenCalledWith("plugin:barcode-scanner|scan", {
      formats: ["QR_CODE"],
    });
    // The text lands in the SAME input the paste flow uses — one path, one
    // validator.
    const input = host.querySelector(
      '[data-testid="remote-add-input"]',
    ) as HTMLInputElement;
    expect(input.value).toBe(LINK);
  });

  it("a_prompt_camera_triggers_the_permission_request_then_scans", async () => {
    mountList();
    openPasteStep();
    const invoke = GRANTED(async (cmd) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "prompt" };
      if (cmd === "plugin:barcode-scanner|request_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") return "ok";
      throw new Error(`unexpected command: ${cmd}`);
    });

    clickScan();
    await flush();

    expect(invoke).toHaveBeenCalledWith("plugin:barcode-scanner|request_permissions");
    expect(invoke).toHaveBeenCalledWith(
      "plugin:barcode-scanner|scan",
      expect.anything(),
    );
  });

  it("a_denied_camera_shows_the_locked_message_and_never_scans", async () => {
    mountList();
    openPasteStep();
    const invoke = GRANTED(async (cmd) => {
      if (
        cmd === "plugin:barcode-scanner|check_permissions" ||
        cmd === "plugin:barcode-scanner|request_permissions"
      ) {
        return { camera: "denied" };
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    clickScan();
    await flush();

    expect(
      host.querySelector('[data-testid="remote-add-error"]')?.textContent,
      "the denied message is visible in the paste box",
    ).toBe(CAMERA_DENIED);
    expect(
      invoke.mock.calls.some(([c]) => c === "plugin:barcode-scanner|scan"),
      "no scan call when the camera is denied",
    ).toBe(false);
    expect((host.querySelector('[data-testid="remote-add-input"]') as HTMLInputElement).value).toBe("");
  });

  it("a_cancelled_scan_object_never_reaches_the_dom", async () => {
    mountList();
    openPasteStep();
    installInvoke(async (cmd) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") {
        // The plugin rejects with an OBJECT (not a string) on cancel.
        throw { message: "cancelled" };
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    clickScan();
    await flush();
    await flush();

    // Clean state: the object is swallowed, never stringified.
    expect(host.textContent).not.toContain("[object Object]");
    expect(host.textContent).not.toContain("cancelled");
    // The paste box is still open and usable.
    expect(host.querySelector('[data-testid="remote-add-paste"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="remote-add-input"]')).not.toBeNull();
  });

  it("the_scan_click_pushes_a_history_entry_and_popstate_cancels", async () => {
    mountList();
    openPasteStep();
    const invoke = GRANTED(async (cmd) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") {
        // Keep the scan "in flight" (never settles): Back must cancel it.
        return new Promise(() => {});
      }
      return undefined;
    });

    const push = vi.spyOn(window.history, "pushState");
    clickScan();
    await flush();
    expect(push, "the Scan click handler must push a history entry").toHaveBeenCalledTimes(1);

    // The OS Back over the camera UI surfaces as a popstate: the SPA must
    // cancel the scan (leaving it running would let the plugin win the
    // Back gesture and exit the app).
    act(() => {
      window.dispatchEvent(new PopStateEvent("popstate"));
    });
    await flush();
    expect(invoke).toHaveBeenCalledWith("plugin:barcode-scanner|cancel");
  });

  it("a_raw_ipc_error_string_with_a_token_is_never_rendered_verbatim", async () => {
    mountList();
    openPasteStep();
    installInvoke(async (cmd) => {
      if (cmd === "plugin:barcode-scanner|check_permissions") return { camera: "granted" };
      if (cmd === "plugin:barcode-scanner|scan") {
        // A raw plugin failure string that embeds the pairing link.
        throw new Error("scan failed: https://10.2.0.48:7899/?token=abc123&ca=sha256:aa");
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    clickScan();
    await flush();
    await flush();

    // The token must not leak into the DOM in any form.
    expect(host.textContent).not.toContain("token=abc123");
    expect(host.textContent).not.toContain("scan failed: https://");
    // …and it must not be stringified into the paste input.
    expect((host.querySelector('[data-testid="remote-add-input"]') as HTMLInputElement).value).toBe("");
  });
});
