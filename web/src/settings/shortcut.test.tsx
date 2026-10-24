// RED (T20 D1a): entry/exit of the Settings view. Spec §9.0 "Entry and
// exit": the ⚙ gear in the TopBar and Ctrl+, / ⌘+, toggle Settings; Esc
// closes in the priority order modal → popover → Settings; Settings is an
// in-app view (state in App.tsx), not a route.
//
// Assumed API (see contract.ts) — the testable seam is the hook in
// ./Settings:
//   useSettingsToggle(): { open: boolean; toggle(): void; close(): void }
//     window keydown while mounted:
//       - key "," with ctrlKey OR metaKey (no shift/alt) → preventDefault +
//         toggle
//       - key "Escape" while open → close, UNLESS event.defaultPrevented
//         (modals/popovers handle Esc first and win the priority order)
//   The TopBar gear (tooltip `Settings  Ctrl ,`) calls the same toggle; the
//   gear button itself is wired in App/TopBar (T13/T32 territory) — this
//   file pins the hook the gear will call.
//
// Harness: the project convention is a plain createRoot/act probe
// (ConversationsPicker.test.tsx style); no testing-library in this repo.
// jsdom is enabled for `src/settings/**/*.test.tsx` by vitest.config.ts.
//
// Expected RED: `./Settings` does not exist yet → import error.

import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useSettingsToggle } from "./Settings";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// The probe renders the toggle state and a stand-in gear button.
function Probe() {
  const t = useSettingsToggle();
  return (
    <div>
      <button data-testid="gear" onClick={() => t.toggle()}>
        ⚙
      </button>
      {t.open && <div data-testid="settings-view">Settings</div>}
    </div>
  );
}

let root: Root;
let host: HTMLElement;

function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(<Probe />));
}

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function key(type: string, init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent(type, { bubbles: true, cancelable: true, ...init });
}

describe("settings shortcut (Ctrl+, / ⌘+, and Esc)", () => {
  it("shortcut_ctrl_comma_toggles", () => {
    mount();
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();

    // Ctrl+, opens.
    act(() => {
      window.dispatchEvent(key("keydown", { key: ",", ctrlKey: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).not.toBeNull();

    // Ctrl+, again closes.
    act(() => {
      window.dispatchEvent(key("keydown", { key: ",", ctrlKey: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();

    // ⌘+, (macOS) opens too.
    act(() => {
      window.dispatchEvent(key("keydown", { key: ",", metaKey: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).not.toBeNull();
  });

  it("esc_closes_settings", () => {
    mount();

    // Open via the gear (the TopBar gear calls the same toggle).
    act(() => {
      host.querySelector('[data-testid="gear"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).not.toBeNull();

    // Esc closes.
    act(() => {
      window.dispatchEvent(key("keydown", { key: "Escape" }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();

    // Esc while closed is a no-op (nothing to close, no crash).
    act(() => {
      window.dispatchEvent(key("keydown", { key: "Escape" }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();
  });

  it("esc_priority_modal_wins_then_settings_closes", () => {
    mount();
    act(() => {
      host.querySelector('[data-testid="gear"]')?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).not.toBeNull();

    // A modal (or popover) handles Esc first: it calls preventDefault /
    // stopPropagation, so the Settings layer must NOT close on that event.
    const handled = key("keydown", { key: "Escape" });
    handled.preventDefault();
    act(() => {
      window.dispatchEvent(handled);
    });
    expect(host.querySelector('[data-testid="settings-view"]')).not.toBeNull();

    // The next unhandled Esc closes Settings.
    act(() => {
      window.dispatchEvent(key("keydown", { key: "Escape" }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();
  });

  it("plain_comma_and_unrelated_keys_do_not_toggle", () => {
    mount();

    act(() => {
      window.dispatchEvent(key("keydown", { key: "," })); // no modifier
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();

    act(() => {
      window.dispatchEvent(key("keydown", { key: ",", ctrlKey: true, shiftKey: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();

    act(() => {
      window.dispatchEvent(key("keydown", { key: "s", ctrlKey: true }));
    });
    expect(host.querySelector('[data-testid="settings-view"]')).toBeNull();
  });
});
