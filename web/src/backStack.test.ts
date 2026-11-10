// @vitest-environment jsdom
//
// Tests for `backStack.ts` — the one arbiter of "Back closes the top layer"
// (design §3.4). `pushBackEntry(onBack) → release`: push a marker (URL
// unchanged), run `onBack` if the user's Back consumes it, and `release`
// consumes the marker when the layer closes any other way.
//
// The module owns a single `popstate` listener plus a LIFO stack and a
// `swallow` counter, all at module scope. Each test therefore imports a FRESH
// module instance (`vi.resetModules`) and is written to end with an empty
// stack and `swallow === 0`, so the previous instances' listeners stay inert.
//
// RED before implementation: `./backStack` does not exist yet, so each test's
// `fresh()` dynamic import throws — the right reason (feature absent).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type BackStackApi = { pushBackEntry: (onBack: () => void) => () => void };

/** Import a fresh module instance (the stack/swallow are module-scoped). */
async function fresh(): Promise<BackStackApi> {
  vi.resetModules();
  return (await import("./backStack")) as BackStackApi;
}

/** A user Back / forward-swipe: a synchronous popstate on the window. */
const pop = () => window.dispatchEvent(new PopStateEvent("popstate"));

/** Await the popstate that a `history.back()` produces (jsdom: one task). */
function waitForPopstate(): Promise<void> {
  return new Promise((resolve) => {
    const t = setTimeout(resolve, 250);
    window.addEventListener(
      "popstate",
      () => {
        clearTimeout(t);
        resolve();
      },
      { once: true },
    );
  });
}

beforeEach(() => {
  window.history.replaceState(null, "", "/");
});

afterEach(() => {
  vi.restoreAllMocks();
  window.history.replaceState(null, "", "/");
});

describe("backStack — pushBackEntry / release", () => {
  it("B1: pushing on an empty stack pushStates once, keeping the URL", async () => {
    const { pushBackEntry: push } = await fresh();
    const pushSpy = vi.spyOn(window.history, "pushState");
    const stateBefore = window.history.state;
    const hrefBefore = window.location.href;

    push(vi.fn());

    expect(pushSpy).toHaveBeenCalledTimes(1);
    expect(pushSpy).toHaveBeenCalledWith(stateBefore, "");
    expect(window.location.href).toBe(hrefBefore);

    pop(); // drain, so this instance's listener stays inert for later tests
  });

  it("B2: a popstate runs the top onBack; a later release is a no-op", async () => {
    const { pushBackEntry: push } = await fresh();
    const f = vi.fn();
    const release = push(f);

    pop();
    expect(f).toHaveBeenCalledTimes(1);

    // The entry is gone, so releasing now must not traverse history.
    const backSpy = vi.spyOn(window.history, "back");
    release();
    expect(backSpy).not.toHaveBeenCalled();
  });

  it("B3: release issues one back() whose popstate calls nothing", async () => {
    const { pushBackEntry: push } = await fresh();
    const f = vi.fn();
    const release = push(f);
    const backSpy = vi.spyOn(window.history, "back");

    release();
    expect(backSpy).toHaveBeenCalledTimes(1);

    const popped = waitForPopstate();
    await popped;
    expect(f, "the marker's onBack must not run for a self-issued back()").not.toHaveBeenCalled();
  });

  it("B4: popstate pops LIFO — the last pushed runs first", async () => {
    const { pushBackEntry: push } = await fresh();
    const a = vi.fn();
    const b = vi.fn();
    push(a);
    push(b);

    pop();
    expect(b).toHaveBeenCalledTimes(1);
    expect(a).not.toHaveBeenCalled();

    pop();
    expect(a).toHaveBeenCalledTimes(1);
  });

  it("B5: releasing the top swallows its back(), then a user Back pops the rest", async () => {
    const { pushBackEntry: push } = await fresh();
    const a = vi.fn();
    const b = vi.fn();
    push(a);
    const releaseB = push(b);

    releaseB();
    const firstPop = waitForPopstate();
    await firstPop;
    // The self-issued back() is swallowed: nothing runs.
    expect(a).not.toHaveBeenCalled();
    expect(b).not.toHaveBeenCalled();

    // A genuine user Back now pops the remaining layer.
    pop();
    expect(a).toHaveBeenCalledTimes(1);
  });

  it("B6: release is idempotent — one back() in total", async () => {
    const { pushBackEntry: push } = await fresh();
    const f = vi.fn();
    const release = push(f);
    const backSpy = vi.spyOn(window.history, "back");

    release();
    release();
    expect(backSpy).toHaveBeenCalledTimes(1);

    const popped = waitForPopstate();
    await popped;
    expect(f).not.toHaveBeenCalled();
  });

  it("B7: a popstate on an empty stack is ignored without throwing", async () => {
    await fresh();

    expect(() => pop()).not.toThrow();
  });
});
