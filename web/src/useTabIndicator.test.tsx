// @vitest-environment jsdom
//
// PR-A tests T14–T15 (/tmp/peakbot/design/notify-pr-a.md §7). `useTabIndicator`
// (web/src/useTabIndicator.ts, renamed from `useFavicon.ts`) does not exist yet,
// so this file is RED at import time: "Failed to resolve import
// ./useTabIndicator".

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useTabIndicator } from "./useTabIndicator";

const TITLE = "Shifu — x";

let root: Root | null = null;
let container: HTMLElement | null = null;

function Probe({ running }: { running: boolean }) {
  useTabIndicator(running);
  return null;
}

function mount(running: boolean) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(<Probe running={running} />);
  });
}

function rerender(running: boolean) {
  act(() => {
    root!.render(<Probe running={running} />);
  });
}

function unmount() {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
}

function icon(): HTMLLinkElement {
  return document.querySelector<HTMLLinkElement>("link[rel~='icon']")!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  document.head.innerHTML = "";
  document.title = TITLE;
  const link = document.createElement("link");
  link.rel = "icon";
  link.setAttribute("href", "/logo_shifu.png");
  document.head.appendChild(link);
});

afterEach(() => {
  if (root) unmount();
});

describe("useTabIndicator — title badge", () => {
  it("T14 title is badged while running and restored exactly", () => {
    mount(true);
    expect(document.title).toBe(`⟳ ${TITLE}`);
    rerender(false);
    expect(document.title).toBe(TITLE);
  });

  it("T14b the badge never stacks across running → idle → running cycles", () => {
    mount(true);
    rerender(false);
    rerender(true);
    expect(document.title).toBe(`⟳ ${TITLE}`);
    rerender(false);
    expect(document.title).toBe(TITLE);
  });

  it("T14c re-rendering while still running does not double the badge", () => {
    mount(true);
    rerender(true);
    rerender(true);
    expect(document.title).toBe(`⟳ ${TITLE}`);
  });

  it("T14d an idle mount leaves the title untouched", () => {
    mount(false);
    expect(document.title).toBe(TITLE);
  });

  it("T14e unmounting while running restores the exact title", () => {
    mount(true);
    expect(document.title).toBe(`⟳ ${TITLE}`);
    unmount();
    expect(document.title).toBe(TITLE);
  });

  it("T14f an empty title is restored to empty", () => {
    document.title = "";
    mount(true);
    rerender(false);
    expect(document.title).toBe("");
  });
});

describe("useTabIndicator — favicon", () => {
  it("T15 favicon swaps while running and restores on stop", () => {
    mount(true);
    expect(icon().href.endsWith("/logo_shifu_think.png")).toBe(true);
    rerender(false);
    expect(icon().href.endsWith("/logo_shifu.png")).toBe(true);
  });

  it("T15b unmounting while running restores both icon and title", () => {
    mount(true);
    unmount();
    expect(icon().href.endsWith("/logo_shifu.png")).toBe(true);
    expect(document.title).toBe(TITLE);
  });

  it("T15c the swap reuses the one icon link (count stays flat)", () => {
    mount(true);
    rerender(false);
    rerender(true);
    expect(document.querySelectorAll("link[rel~='icon']")).toHaveLength(1);
  });
});
