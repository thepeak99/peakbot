// RED (T21 D1b): ./SaveBar — the save bar + restart banner. Spec §9.6:
//   - bar only when the scope is dirty: "Unsaved changes" (no count), chips
//     "● applies on next /new" (green) / "● restart required" (amber, iff
//     validate.restart_required is non-empty), buttons Discard · View diff ·
//     Save (primary). The Save button element must stay stable across
//     re-renders (a re-rendered Save once swallowed a click).
//   - validate is debounced at 400 ms after edits (also on Save / View diff
//     / Raw blur — the shell funnels those through the same hook).
//   - 409 → stale banner with a Reload button.
//   - after a save with restart_required: bottom banner "⚠ Some changes are
//     restart-required (…) — they take effect after a restart." with Later;
//     "Restart now" ONLY in the desktop app (isDesktop()) → the caller
//     navigates to "peakbot-desktop:restart"; in a browser the text
//     "Restart PeakBot to apply." is shown instead.
//
// Assumed API (see contract.ts): <SaveBar …/> and <RestartBanner …/> with
// the props/data-testids documented there.
//
// Harness: plain createRoot/act probe (project convention, no
// testing-library). jsdom is enabled for `src/settings/**/*.test.tsx`.
//
// Expected RED: `./SaveBar` does not exist yet → import error.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { RestartBanner, SaveBar, type SaveBarProps } from "./SaveBar";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root;
let host: HTMLElement;

function mount(node: React.ReactNode) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(<div>{node}</div>));
}

function rerender(node: React.ReactNode) {
  act(() => root.render(<div>{node}</div>));
}

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.useRealTimers();
});

function baseProps(over: Partial<SaveBarProps> = {}): SaveBarProps {
  return {
    scope: "global",
    dirty: true,
    draftVersion: 1,
    restartRequired: [],
    saving: false,
    stale: false,
    staleMessage: null,
    onValidate: vi.fn(),
    onSave: vi.fn(),
    onDiscard: vi.fn(),
    onReload: vi.fn(),
    onViewDiff: vi.fn(),
    ...over,
  };
}

describe("SaveBar", () => {
  it("savebar_hidden_when_clean", () => {
    const props = baseProps({ dirty: false });
    mount(<SaveBar {...props} />);
    expect(host.querySelector('[data-testid="savebar"]')).toBeNull();
    expect(host.querySelector('[data-testid="savebar-save"]')).toBeNull();
  });

  it("savebar_chips_from_restart_required", () => {
    const props = baseProps({ restartRequired: ["general.web", "mcps"] });
    mount(<SaveBar {...props} />);

    expect(host.querySelector('[data-testid="savebar"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="savebar-unsaved"]')?.textContent).toContain(
      "Unsaved changes",
    );

    // restart_required non-empty → the amber chip, and no "next /new" chip.
    const restart = host.querySelector('[data-testid="savebar-chip-restart"]');
    expect(restart?.textContent).toContain("restart required");
    expect(host.querySelector('[data-testid="savebar-chip-nextnew"]')).toBeNull();

    // Buttons.
    expect(host.querySelector('[data-testid="savebar-discard"]')?.textContent).toBe("Discard");
    expect(host.querySelector('[data-testid="savebar-diff"]')?.textContent).toBe("View diff");
    expect(host.querySelector('[data-testid="savebar-save"]')?.textContent).toBe("Save");

    act(() => {
      host.querySelector('[data-testid="savebar-discard"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      host.querySelector('[data-testid="savebar-diff"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      host.querySelector('[data-testid="savebar-save"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(props.onDiscard).toHaveBeenCalledTimes(1);
    expect(props.onViewDiff).toHaveBeenCalledTimes(1);
    expect(props.onSave).toHaveBeenCalledTimes(1);

    // restart_required empty → the green chip, no amber chip.
    const clean = baseProps({ restartRequired: [] });
    rerender(<SaveBar {...clean} />);
    const next = host.querySelector('[data-testid="savebar-chip-nextnew"]');
    expect(next?.textContent).toContain("applies on next /new");
    expect(host.querySelector('[data-testid="savebar-chip-restart"]')).toBeNull();
  });

  it("put_409_shows_stale", () => {
    const message =
      "/home/u/.config/peakbot/config.yaml changed on disk since you opened Settings — reload to see the new version.";
    const props = baseProps({ stale: true, staleMessage: message });
    mount(<SaveBar {...props} />);

    const banner = host.querySelector('[data-testid="savebar-stale"]');
    expect(banner?.textContent).toContain("changed on disk");
    const reload = host.querySelector('[data-testid="savebar-stale-reload"]');
    expect(reload?.textContent).toBe("Reload");

    act(() => {
      reload?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(props.onReload).toHaveBeenCalledTimes(1);

    // No stale banner while not stale.
    const clean = baseProps({ stale: false, staleMessage: null });
    rerender(<SaveBar {...clean} />);
    expect(host.querySelector('[data-testid="savebar-stale"]')).toBeNull();
  });

  it("validate_debounced_400ms", () => {
    vi.useFakeTimers();
    const props = baseProps({ draftVersion: 1 });
    mount(<SaveBar {...props} />);

    // Nothing before 400 ms.
    act(() => {
      vi.advanceTimersByTime(399);
    });
    expect(props.onValidate).not.toHaveBeenCalled();

    // Trailing call at 400 ms.
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(props.onValidate).toHaveBeenCalledTimes(1);

    // A new edit (draftVersion bump) re-arms the debounce: no call within
    // the next 400 ms, then one.
    act(() => {
      root.render(<SaveBar {...props} draftVersion={2} />);
    });
    act(() => {
      vi.advanceTimersByTime(399);
    });
    expect(props.onValidate).toHaveBeenCalledTimes(1);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(props.onValidate).toHaveBeenCalledTimes(2);

    // A clean scope never validates.
    act(() => {
      root.render(<SaveBar {...props} dirty={false} draftVersion={3} />);
    });
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(props.onValidate).toHaveBeenCalledTimes(2);
  });

  it("savebar_save_button_element_stable", () => {
    const props = baseProps();
    mount(<SaveBar {...props} />);
    const first = host.querySelector('[data-testid="savebar-save"]');
    expect(first).not.toBeNull();

    // Re-render with unchanged props: the Save button must be the SAME DOM
    // node (spec: never re-render identical markup under a pending click).
    rerender(<SaveBar {...props} />);
    expect(host.querySelector('[data-testid="savebar-save"]')).toBe(first);

    // …and even after unrelated state changes while the bar stays dirty.
    const changed = baseProps({ restartRequired: ["mcps"] });
    rerender(<SaveBar {...changed} />);
    expect(host.querySelector('[data-testid="savebar-save"]')).toBe(first);
  });
});

describe("RestartBanner", () => {
  it("restart_now_desktop_only", () => {
    const desktopProps = {
      keys: ["general.web", "mcps"],
      desktop: true,
      onLater: vi.fn(),
      onRestartNow: vi.fn(),
    };
    mount(<RestartBanner {...desktopProps} />);

    const banner = host.querySelector('[data-testid="restart-banner"]');
    expect(banner?.textContent).toContain("restart-required (general.web, mcps)");
    expect(banner?.textContent).toContain("they take effect after a restart");

    const restart = host.querySelector('[data-testid="restart-banner-restart"]');
    expect(restart?.textContent).toBe("Restart now");
    const later = host.querySelector('[data-testid="restart-banner-later"]');
    expect(later?.textContent).toBe("Later");

    act(() => {
      restart?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      later?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(desktopProps.onRestartNow).toHaveBeenCalledTimes(1);
    expect(desktopProps.onLater).toHaveBeenCalledTimes(1);

    // Browser: no Restart now button, the plain restart hint instead.
    const browserProps = { ...desktopProps, desktop: false };
    rerender(<RestartBanner {...browserProps} />);
    expect(host.querySelector('[data-testid="restart-banner-restart"]')).toBeNull();
    expect(host.querySelector('[data-testid="restart-banner"]')?.textContent).toContain(
      "Restart PeakBot to apply.",
    );
    expect(host.querySelector('[data-testid="restart-banner-later"]')).not.toBeNull();
  });
});
