// useScreenSlide: three-state presence with a WAAPI exit hold (design §5.1,
// S1–S8). `Element.prototype.animate` is mocked with a controllable
// `finished` promise + `cancel()`, so the tests can assert: the overlay
// stays mounted while `shown` goes false, the exit keyframes run, and it
// unmounts exactly when `finished` settles. Off the mobile shell, without
// WAAPI, or under prefers-reduced-motion the overlay must swap INSTANTLY
// (no hold, no animate call). Mid-exit re-show must cancel the exit and
// never let a stale `finished` unmount a visible overlay.
//
// RED on origin/master: ./useScreenSlide does not exist, so the import
// below fails — the feature is absent, and that is the only failure.

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, StrictMode, useRef, useState, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useScreenSlide } from "./useScreenSlide";

// ─── controllable WAAPI mock ─────────────────────────────────────────────

interface FakeAnim {
  el: Element;
  keyframes: Keyframe[];
  options: KeyframeAnimationOptions;
  finished: Promise<void>;
  cancelled: boolean;
  cancel(): void;
  finish(): void;
}

let anims: FakeAnim[] = [];

function installWAAPI(): void {
  // A `function`, not an arrow: the hook animates the caller's element, so
  // the mock records `this` to prove WHICH element was animated.
  (
    Element.prototype as unknown as {
      animate: (kf: Keyframe[], opts: KeyframeAnimationOptions) => FakeAnim;
    }
  ).animate = function (
    this: Element,
    keyframes: Keyframe[],
    options: KeyframeAnimationOptions,
  ) {
    let resolve: () => void;
    let reject: (e: unknown) => void;
    const finished = new Promise<void>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    // Pre-mark handled: the enter path never attaches its own `.catch`, so a
    // `cancel()` before the hook wires up a handler must not surface as an
    // unhandled rejection (a browser tolerates an aborted animation).
    void finished.catch(() => {});
    const anim: FakeAnim = {
      el: this,
      keyframes,
      options,
      finished,
      cancelled: false,
      cancel() {
        if (anim.cancelled) return;
        anim.cancelled = true;
        reject(new Error("AbortError"));
      },
      finish() {
        resolve();
      },
    };
    anims.push(anim);
    return anim;
  };
}

function removeWAAPI(): void {
  delete (Element.prototype as { animate?: unknown }).animate;
}

// ─── shell + reduced motion ──────────────────────────────────────────────

function setShell(marker: "desktop" | "mobile" | "browser"): void {
  delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
  if (marker !== "browser") (globalThis as { peakbotShell?: unknown }).peakbotShell = marker;
}

let reduce = false;

// ─── harness ─────────────────────────────────────────────────────────────
// `shown` and `side` are module-level, flipped through a forced re-render
// under `act` (spike convention). Every test starts at `shown = false`.

let shown = false;
let side: "left" | "right" = "left";
let rerender: () => void = () => {};

function Harness() {
  const [, force] = useState(0);
  rerender = () => force((n) => n + 1);
  const ref = useRef<HTMLDivElement | null>(null);
  const s = useScreenSlide(shown, ref, side);
  return s === "gone" ? null : <div ref={ref} data-testid="overlay" data-state={s} />;
}

function setProps(nextShown: boolean, nextSide?: "left" | "right"): void {
  shown = nextShown;
  if (nextSide !== undefined) side = nextSide;
  act(() => rerender());
}

let root: Root | null = null;
let host: HTMLElement;

function mount(node: ReactNode = <Harness />): void {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
}

function unmount(): void {
  act(() => {
    root?.unmount();
  });
  root = null;
  host?.remove();
}

const overlay = (): Element | null => host.querySelector('[data-testid="overlay"]');

async function microtask(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

const EASING = "cubic-bezier(0.215, 0.61, 0.355, 1)";
const HOME = "translateX(0px)";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  shown = false;
  side = "left";
  reduce = false;
  anims = [];
  setShell("mobile");
  // `matches` only for the reduced-motion query, driven by `reduce`.
  vi.stubGlobal(
    "matchMedia",
    (media: string) =>
      ({
        get matches() {
          return media === "(prefers-reduced-motion: reduce)" && reduce;
        },
        media,
        onchange: null,
        addListener: () => {},
        removeListener: () => {},
        addEventListener: () => {},
        removeEventListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList,
  );
});

afterEach(() => {
  unmount();
  removeWAAPI();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setShell("browser");
});

// ─── tests ───────────────────────────────────────────────────────────────

describe("useScreenSlide (S1–S8)", () => {
  it("S1: enters from the parked left edge when shown", () => {
    installWAAPI();
    mount();
    expect(overlay(), "nothing mounted while shown is false").toBeNull();

    setProps(true);

    expect(overlay(), "the overlay mounts").not.toBeNull();
    expect(overlay()?.getAttribute("data-state")).toBe("shown");
    const enter = anims.at(-1);
    expect(enter, "an enter animation started").toBeDefined();
    expect(enter!.el, "it animates the caller's element").toBe(overlay());
    expect(enter!.keyframes).toEqual([
      { opacity: 0, transform: "translateX(-48px)" },
      { opacity: 1, transform: HOME },
    ]);
    // toEqual: an extra `fill` key (or any other) would fail.
    expect(enter!.options).toEqual({ duration: 180, easing: EASING });
  });

  it("S2: holds the overlay through the exit and drops it on finish", async () => {
    installWAAPI();
    mount();
    setProps(true);

    setProps(false);

    expect(overlay(), "the overlay is HELD while exiting").not.toBeNull();
    expect(overlay()?.getAttribute("data-state")).toBe("leaving");
    const exit = anims.at(-1);
    expect(exit!.el).toBe(overlay());
    expect(exit!.keyframes).toEqual([
      { opacity: 1, transform: HOME },
      { opacity: 0, transform: "translateX(-48px)" },
    ]);
    expect(exit!.options).toEqual({ duration: 180, easing: EASING, fill: "forwards" });

    await act(async () => {
      exit!.finish();
    });
    expect(overlay(), "the overlay unmounts when the exit finishes").toBeNull();
  });

  it('S3: side "right" parks at +48px (enter start, exit end)', () => {
    installWAAPI();
    side = "right";
    mount();

    setProps(true);
    expect(anims.at(-1)!.keyframes[0]).toEqual({ opacity: 0, transform: "translateX(48px)" });

    setProps(false);
    const exit = anims.at(-1)!;
    expect(exit.keyframes.at(-1)).toEqual({ opacity: 0, transform: "translateX(48px)" });
  });

  it("S4: no WAAPI: instant swap, no hold", () => {
    // No installWAAPI(): jsdom's default, `Element.prototype.animate` undefined.
    mount();

    setProps(true);
    expect(overlay(), "the overlay mounts").not.toBeNull();

    setProps(false);
    expect(overlay(), "no hold: gone in the same act").toBeNull();
    expect(anims).toEqual([]);
  });

  it.each(["desktop", "browser"] as const)(
    "S5: off the mobile shell (%s): instant, animate never called",
    (marker) => {
      installWAAPI();
      setShell(marker);
      mount();

      setProps(true);
      expect(overlay(), "the overlay mounts").not.toBeNull();

      setProps(false);
      expect(overlay(), "no hold: gone in the same act").toBeNull();
      expect(anims, "animate was never called").toEqual([]);
    },
  );

  it("S6: prefers-reduced-motion: instant, animate never called", () => {
    installWAAPI();
    reduce = true;
    mount();

    setProps(true);
    expect(overlay(), "the overlay mounts").not.toBeNull();

    setProps(false);
    expect(overlay(), "no hold: gone in the same act").toBeNull();
    expect(anims, "animate was never called").toEqual([]);
  });

  it("S7: re-shown mid-exit: cancels the exit, re-enters, no stale unmount", async () => {
    installWAAPI();
    mount();
    setProps(true);
    setProps(false);
    const exitA = anims.at(-1)!;
    expect(overlay(), "held during exit A").not.toBeNull();

    setProps(true);

    expect(exitA.cancelled, "exit A was cancelled").toBe(true);
    expect(overlay(), "the overlay is back").not.toBeNull();
    expect(overlay()?.getAttribute("data-state")).toBe("shown");
    expect(anims.at(-1)!.keyframes, "the enter plays again").toEqual([
      { opacity: 0, transform: "translateX(-48px)" },
      { opacity: 1, transform: HOME },
    ]);

    // Let any stale `finished` handler land: it must not unmount the overlay.
    await microtask();
    expect(overlay(), "no stale unmount").not.toBeNull();

    setProps(false);
    const exitB = anims.at(-1)!;
    expect(exitB).not.toBe(exitA);
    expect(exitB.options).toEqual({ duration: 180, easing: EASING, fill: "forwards" });
    await act(async () => {
      exitB.finish();
    });
    expect(overlay(), "exit B unmounts").toBeNull();
  });

  it("S8: StrictMode: a cancelled exit never unmounts, the live one does", async () => {
    installWAAPI();
    mount(
      <StrictMode>
        <Harness />
      </StrictMode>,
    );
    setProps(true);
    setProps(false);

    const exits = anims.filter((a) => a.options.fill === "forwards");
    expect(exits.length, "at least one exit started").toBeGreaterThan(0);
    const live = exits.at(-1)!;
    for (const a of exits.slice(0, -1)) {
      expect(a.cancelled, "every superseded exit is cancelled").toBe(true);
    }
    expect(live.cancelled, "the live exit is not cancelled").toBe(false);

    await microtask();
    expect(overlay(), "cancelled exits unmount nothing").not.toBeNull();

    await act(async () => {
      live.finish();
    });
    expect(overlay(), "the live exit unmounts").toBeNull();
  });
});
