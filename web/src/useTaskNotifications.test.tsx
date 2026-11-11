// @vitest-environment jsdom
//
// PR-A "browser notification basics" — tests T1–T13 of
// /tmp/peakbot/design/notify-pr-a.md §7. Written BEFORE the implementation, so
// they are RED today for behavioural reasons:
//   - the opt-in is not persisted / restored (`peakbot-notify`),
//   - the "looked away" gate is `document.hidden` only (no `hasFocus()`),
//   - there is no sound pref / `toggleSound` / WebAudio beep,
//   - `toggle` runs its side effects inside a state updater (StrictMode doubles
//     the permission prompt),
//   - the Tauri shells are not reported as "unsupported".
// Typecheck is also RED until `TaskNotifications` gains `sound`/`toggleSound`
// and `NotifyToggle` takes a single `notify` prop; vitest (esbuild) still runs.
//
// Harness: createRoot/act (no testing-library), per repo convention. `src/` is
// node by default (vitest.config.ts), hence the jsdom header above.

import { StrictMode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotifyToggle } from "./components/NotifyToggle";
import { useTaskNotifications, type TaskNotifications } from "./useTaskNotifications";

const NOTIFY_KEY = "peakbot-notify";
const SOUND_KEY = "peakbot-notify-sound";

// ── fakes ────────────────────────────────────────────────────────────────

// Every oscillator start() lands here, so "a beep happened" is `oscStart` calls.
const oscStart = vi.fn();

// WebAudio fake. `connect` returns its argument so `osc.connect(gain)
// .connect(destination)` chains, like the real API.
class FakeCtx {
  currentTime = 0;
  destination = {};
  resume = vi.fn(async () => {});
  createGain() {
    return {
      gain: { setValueAtTime: vi.fn(), exponentialRampToValueAtTime: vi.fn() },
      connect: <T,>(n: T) => n,
    };
  }
  createOscillator() {
    return {
      frequency: { value: 0 },
      connect: <T,>(n: T) => n,
      start: oscStart,
      stop: vi.fn(),
    };
  }
}

// Mutable "world" the stubs read from.
let hidden = false;
let focused = true;
let hasFocusSpy: { mockRestore(): void } | null = null;

type NotificationStub = ReturnType<typeof vi.fn> & {
  permission: string;
  requestPermission: ReturnType<typeof vi.fn>;
};

function notificationStub(): NotificationStub {
  return Object.assign(vi.fn(), {
    permission: "granted",
    requestPermission: vi.fn(async () => "granted"),
  }) as NotificationStub;
}

let N: NotificationStub;

function setPermission(p: string) {
  N.permission = p;
}

function setShell(s: "desktop" | "mobile" | undefined) {
  const g = globalThis as { peakbotShell?: unknown };
  if (s === undefined) delete g.peakbotShell;
  else g.peakbotShell = s;
}

// ── harness ──────────────────────────────────────────────────────────────

let latest: TaskNotifications;
let root: Root | null = null;
let container: HTMLElement | null = null;

function Probe({ running, withToggle }: { running: boolean; withToggle?: boolean }) {
  const n = useTaskNotifications(running);
  latest = n;
  return withToggle ? <NotifyToggle notify={n} /> : null;
}

function mount(running: boolean, opts: { strict?: boolean; withToggle?: boolean } = {}) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  const el = <Probe running={running} withToggle={opts.withToggle} />;
  act(() => {
    root!.render(opts.strict ? <StrictMode>{el}</StrictMode> : el);
  });
  return container;
}

function rerender(running: boolean, opts: { withToggle?: boolean } = {}) {
  act(() => {
    root!.render(<Probe running={running} withToggle={opts.withToggle} />);
  });
}

function unmount() {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
}

// mount running, then finish the turn.
function endTurn() {
  mount(true);
  rerender(false);
}

function turnEndPings() {
  return N.mock.calls.filter(
    (c) => c[0] === "Shifu" && String(c[1]?.body ?? "").includes("Task complete"),
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  localStorage.clear();
  setShell(undefined);
  Object.defineProperty(window, "isSecureContext", { value: true, configurable: true });
  N = notificationStub();
  vi.stubGlobal("Notification", N);
  vi.stubGlobal("AudioContext", FakeCtx);
  hidden = false;
  focused = true;
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  hasFocusSpy = vi.spyOn(document, "hasFocus").mockImplementation(() => focused);
  oscStart.mockClear();
});

afterEach(() => {
  if (root) unmount();
  hasFocusSpy?.mockRestore();
  hasFocusSpy = null;
  delete (document as unknown as { hidden?: boolean }).hidden;
  setShell(undefined);
  vi.unstubAllGlobals();
});

// ── opt-in persistence ───────────────────────────────────────────────────

describe("useTaskNotifications — opt-in persistence", () => {
  it("T1 opt-in persists across remount when permission is granted", () => {
    mount(false);
    expect(latest.enabled).toBe(false);
    act(() => latest.toggle());
    expect(latest.enabled).toBe(true);
    unmount();

    mount(false);
    expect(latest.enabled).toBe(true);
    expect(localStorage.getItem(NOTIFY_KEY)).toBe("on");
  });

  it("T1b turning the bell off persists 'off' across remount", () => {
    localStorage.setItem(NOTIFY_KEY, "on");
    mount(false);
    expect(latest.enabled).toBe(true);
    act(() => latest.toggle());
    expect(latest.enabled).toBe(false);
    unmount();

    mount(false);
    expect(latest.enabled).toBe(false);
    expect(localStorage.getItem(NOTIFY_KEY)).toBe("off");
  });

  it.each(["default", "denied"])(
    "T2 stored opt-in is restored off (and rewritten 'off') when permission is %s",
    (perm) => {
      localStorage.setItem(NOTIFY_KEY, "on");
      setPermission(perm);
      mount(false);
      expect(latest.enabled).toBe(false);
      expect(localStorage.getItem(NOTIFY_KEY)).toBe("off");
    },
  );

  it("T2b stored opt-in is restored off when the origin is insecure", () => {
    localStorage.setItem(NOTIFY_KEY, "on");
    Object.defineProperty(window, "isSecureContext", { value: false, configurable: true });
    mount(false);
    expect(latest.enabled).toBe(false);
    expect(latest.permission).toBe("insecure");
  });

  it("T2c garbage in the stored value means off", () => {
    localStorage.setItem(NOTIFY_KEY, "yes please");
    mount(false);
    expect(latest.enabled).toBe(false);
  });
});

// ── the turn-end notification gate ───────────────────────────────────────

describe("useTaskNotifications — turn-end notification", () => {
  beforeEach(() => {
    localStorage.setItem(NOTIFY_KEY, "on"); // enabled (permission granted by default)
  });

  it("T3 fires when the tab is hidden", () => {
    hidden = true;
    focused = false;
    endTurn();
    expect(N).toHaveBeenCalledTimes(1);
    expect(N).toHaveBeenCalledWith(
      "Shifu",
      expect.objectContaining({ body: expect.stringContaining("Task complete") }),
    );
  });

  it("T4 fires when the tab is visible but the window is unfocused", () => {
    hidden = false;
    focused = false;
    endTurn();
    expect(turnEndPings()).toHaveLength(1);
  });

  it("T5 does not fire when visible and focused", () => {
    hidden = false;
    focused = true;
    endTurn();
    expect(N).not.toHaveBeenCalled();
  });

  it("T6 does not fire on idle→running or while still running", () => {
    hidden = true;
    focused = false;
    mount(false);
    rerender(true);
    rerender(true);
    expect(N).not.toHaveBeenCalled();
  });

  it("T6b does not fire when the user has not opted in", () => {
    localStorage.setItem(NOTIFY_KEY, "off");
    hidden = true;
    focused = false;
    endTurn();
    expect(N).not.toHaveBeenCalled();
  });

  it("T6c fires once per turn across two consecutive turns", () => {
    hidden = true;
    focused = false;
    mount(true);
    rerender(false);
    rerender(true);
    rerender(false);
    expect(turnEndPings()).toHaveLength(2);
  });
});

// ── sound ────────────────────────────────────────────────────────────────

describe("useTaskNotifications — sound", () => {
  it("T7 sound is off by default and nothing beeps on turn end", () => {
    hidden = true;
    focused = false;
    mount(true);
    expect(latest.sound).toBe(false);
    rerender(false);
    expect(oscStart).not.toHaveBeenCalled();
  });

  it("T7b stored garbage for the sound pref means off", () => {
    localStorage.setItem(SOUND_KEY, "1");
    mount(false);
    expect(latest.sound).toBe(false);
  });

  it("T8 sound plays on turn end only when the user looked away", () => {
    localStorage.setItem(SOUND_KEY, "on");
    hidden = true;
    focused = false;
    mount(true);
    expect(latest.sound).toBe(true);
    rerender(false);
    expect(oscStart).toHaveBeenCalledTimes(1);

    // Now watching: a second turn end stays silent.
    hidden = false;
    focused = true;
    rerender(true);
    rerender(false);
    expect(oscStart).toHaveBeenCalledTimes(1);
  });

  it("T8b sound also plays when visible but the window is unfocused", () => {
    localStorage.setItem(SOUND_KEY, "on");
    hidden = false;
    focused = false;
    endTurn();
    expect(oscStart).toHaveBeenCalledTimes(1);
  });

  it("T9 sound is independent of notification permission", () => {
    localStorage.setItem(SOUND_KEY, "on");
    setPermission("denied");
    hidden = true;
    focused = false;
    endTurn();
    expect(oscStart).toHaveBeenCalledTimes(1);
    expect(N).not.toHaveBeenCalled();
  });

  it("T9b sound works in a plain-HTTP (insecure) context", () => {
    localStorage.setItem(SOUND_KEY, "on");
    Object.defineProperty(window, "isSecureContext", { value: false, configurable: true });
    hidden = true;
    focused = false;
    endTurn();
    expect(oscStart).toHaveBeenCalledTimes(1);
    expect(N).not.toHaveBeenCalled();
  });

  it("T9c a missing AudioContext never throws on turn end", () => {
    vi.stubGlobal("AudioContext", undefined);
    localStorage.setItem(SOUND_KEY, "on");
    hidden = true;
    focused = false;
    expect(() => endTurn()).not.toThrow();
    expect(oscStart).not.toHaveBeenCalled();
  });

  it("T10 toggleSound persists and beeps once as confirmation", () => {
    mount(false);
    act(() => latest.toggleSound());
    expect(latest.sound).toBe(true);
    expect(localStorage.getItem(SOUND_KEY)).toBe("on");
    expect(oscStart).toHaveBeenCalledTimes(1);

    act(() => latest.toggleSound());
    expect(latest.sound).toBe(false);
    expect(localStorage.getItem(SOUND_KEY)).toBe("off");
    expect(oscStart).toHaveBeenCalledTimes(1);
  });

  it("T10b the sound pref survives a remount", () => {
    mount(false);
    act(() => latest.toggleSound());
    unmount();
    mount(false);
    expect(latest.sound).toBe(true);
  });
});

// ── toggle / permission prompt ───────────────────────────────────────────

describe("useTaskNotifications — bell toggle", () => {
  it("T11 toggle on with 'default' permission prompts once and confirms once (StrictMode)", async () => {
    setPermission("default");
    mount(false, { strict: true });
    expect(latest.permission).toBe("default");

    await act(async () => {
      latest.toggle();
    });

    expect(N.requestPermission).toHaveBeenCalledTimes(1);
    expect(latest.permission).toBe("granted");
    expect(latest.enabled).toBe(true);
    expect(N).toHaveBeenCalledTimes(1);
    expect(N).toHaveBeenCalledWith("Shifu notifications on", expect.anything());
  });

  it("T11b a denied prompt leaves permission 'denied' and sends no confirmation", async () => {
    setPermission("default");
    N.requestPermission.mockImplementation(async () => "denied");
    mount(false, { strict: true });

    await act(async () => {
      latest.toggle();
    });

    expect(N.requestPermission).toHaveBeenCalledTimes(1);
    expect(latest.permission).toBe("denied");
    expect(N).not.toHaveBeenCalled();
  });

  it("T11c toggle on with 'granted' permission confirms once without prompting (StrictMode)", () => {
    mount(false, { strict: true });
    act(() => latest.toggle());
    expect(N.requestPermission).not.toHaveBeenCalled();
    expect(N).toHaveBeenCalledTimes(1);
    expect(N).toHaveBeenCalledWith("Shifu notifications on", expect.anything());
  });

  it("T11d toggling off sends no confirmation", () => {
    localStorage.setItem(NOTIFY_KEY, "on");
    mount(false);
    act(() => latest.toggle());
    expect(latest.enabled).toBe(false);
    expect(N).not.toHaveBeenCalled();
  });
});

// ── shells ───────────────────────────────────────────────────────────────

describe("useTaskNotifications — Tauri shells", () => {
  it.each(["desktop", "mobile"] as const)(
    "T12 the bell is hidden inside the %s app",
    (shell) => {
      setShell(shell);
      localStorage.setItem(NOTIFY_KEY, "on");
      const c = mount(false, { withToggle: true });
      expect(c.querySelector("button")).toBeNull();
      expect(latest.permission).toBe("unsupported");
      expect(latest.enabled).toBe(false);
    },
  );

  it.each(["desktop", "mobile"] as const)(
    "T12b nothing fires inside the %s app even if a stored opt-in exists",
    (shell) => {
      setShell(shell);
      localStorage.setItem(NOTIFY_KEY, "on");
      hidden = true;
      focused = false;
      endTurn();
      expect(N).not.toHaveBeenCalled();
    },
  );

  it("T13 the bell and speaker show in a plain browser (control)", () => {
    const c = mount(false, { withToggle: true });
    const buttons = c.querySelectorAll("button");
    expect(buttons).toHaveLength(2);
    for (const b of buttons) expect(b.hasAttribute("aria-pressed")).toBe(true);
    expect(latest.permission).toBe("granted");
  });
});
