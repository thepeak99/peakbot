// Tests for `components/BottomBar.tsx` — mobile rail clearance.
//
// The fixed right-hand TabbedDrawer rail (40px handles) overlaps the bottom
// bar on phones. Contract: the <footer> carries `pr-12` so its pickers never
// sit under the rail. Class names ARE the layout contract (jsdom has no
// layout engine). Harness modeled on Composer.test.tsx.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { BottomBar } from "./BottomBar";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let originalMatchMedia: typeof window.matchMedia | undefined;
beforeEach(() => {
  originalMatchMedia = window.matchMedia;
  window.matchMedia = (media: string): MediaQueryList =>
    ({
      matches: false,
      media,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
});

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(cwd: string | null): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(
      <BottomBar
        conversations={[]}
        conversationsError={null}
        models={[]}
        activeAlias=""
        hasTranscript={false}
        cwd={cwd}
        dirListing={null}
        recentDirs={[]}
        send={vi.fn()}
        onSwitchModel={vi.fn()}
        onLoadConversation={vi.fn()}
      />,
    );
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  if (originalMatchMedia === undefined) {
    delete (window as unknown as { matchMedia?: unknown }).matchMedia;
  } else {
    window.matchMedia = originalMatchMedia;
  }
});

describe("BottomBar — rail clearance (mobile)", () => {
  it("footer has the pr-12 right clearance class", async () => {
    const el = await mount(null);
    const footer = el.querySelector("footer")!;
    expect(footer).not.toBeNull();
    expect(footer.classList.contains("pr-12")).toBe(true);
  });

  it("footer keeps pr-12 when the cwd picker is shown too", async () => {
    const el = await mount("/tmp/project");
    const footer = el.querySelector("footer")!;
    expect(footer.classList.contains("pr-12")).toBe(true);
  });

  it("footer stays mobile-only (lg:hidden) — regression", async () => {
    const el = await mount(null);
    expect(el.querySelector("footer")!.classList.contains("lg:hidden")).toBe(true);
  });
});
