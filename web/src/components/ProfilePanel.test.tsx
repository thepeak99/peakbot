// Tests for the new `ProfilePanel` component (Layer E, E4) — the profile
// selector shown in the drawer's new "Profile" tab, modelled directly on the
// pipeline radio list in `AgentsPanel.tsx` (AgentsPanel.tsx:139-204: "None
// (single agent)" + one row per pipeline, `disabled` while `locked`, a
// `disabledReason`/hint paragraph explaining why).
//
// `ProfilePanel` does not exist yet (`web/src/components/ProfilePanel.tsx`
// is new production code). Expected RED reason: "Cannot find module
// './ProfilePanel'" at import time — the feature is absent, not a typo.
//
// ─── Assumed props (this is the interface this test locks in) ────────────
//   interface ProfilePanelProps {
//     /** Selectable profile names, sorted (state.ts `AppState.profiles`).
//      *  Empty when no profiles are defined OR a `--profile` pin is active
//      *  — App.tsx only mounts this tab when `visible_tabs` includes
//      *  "profile", so an empty list is a defensive/never-should-happen
//      *  case here, not the normal empty state. */
//     profiles: string[];
//     /** The profile this conversation is bound to, or null for
//      *  "None (base config)" (`AppState.active_profile`). */
//     active: string | null;
//     /** True once the conversation has a real turn — mirrors
//      *  AgentsPanel's `locked` (fed from the same `conversationStarted`
//      *  signal in App.tsx). Selection is then frozen. */
//     locked: boolean;
//     /** Bind the conversation to a profile; `null` selects
//      *  "None (base config)". No-op while `locked` (radios disabled). */
//     onSelect: (name: string | null) => void;
//   }
//
// Harness modeled on `Composer.test.tsx` / `TopBar.test.tsx`: plain
// `createRoot` + `act` (jsdom is auto-enabled for `src/components/**` by
// `vitest.config.ts`).

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { ProfilePanel } from "./ProfilePanel";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

// ─── fixture ────────────────────────────────────────────────────────────

interface ProfilePanelProps {
  profiles: string[];
  active: string | null;
  locked: boolean;
  onSelect: (name: string | null) => void;
}

const makeProps = (
  overrides: Partial<ProfilePanelProps> = {},
): ProfilePanelProps => ({
  profiles: ["web", "cli"],
  active: null,
  locked: false,
  onSelect: vi.fn(),
  ...overrides,
});

// ─── mount helper ───────────────────────────────────────────────────────

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(props: ProfilePanelProps): Promise<HTMLDivElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(<ProfilePanel {...props} />);
  });
  return container;
}

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
});

// ─── DOM query helpers ──────────────────────────────────────────────────

/** The radio `<input>` inside the `<label>` whose visible text contains
 *  `text` — mirrors how a user reads the AgentsPanel pipeline rows (label
 *  wraps the radio + text, exactly like AgentsPanel.tsx:142-158). */
function radioByLabel(el: HTMLElement, text: string): HTMLInputElement | null {
  const labels = Array.from(el.querySelectorAll("label"));
  const label = labels.find((l) => (l.textContent ?? "").includes(text));
  return label?.querySelector('input[type="radio"]') ?? null;
}

function allRadios(el: HTMLElement): HTMLInputElement[] {
  return Array.from(el.querySelectorAll('input[type="radio"]'));
}

// ─── tests ──────────────────────────────────────────────────────────────

describe("ProfilePanel — renders one radio per option", () => {
  it("renders a 'None (base config)' option plus one row per profile", async () => {
    const el = await mount(makeProps({ profiles: ["web", "cli"] }));

    expect(radioByLabel(el, "None (base config)")).not.toBeNull();
    expect(radioByLabel(el, "web")).not.toBeNull();
    expect(radioByLabel(el, "cli")).not.toBeNull();
    expect(allRadios(el)).toHaveLength(3); // None + web + cli
  });

  it("renders only the 'None (base config)' option when profiles is empty", async () => {
    const el = await mount(makeProps({ profiles: [] }));

    expect(radioByLabel(el, "None (base config)")).not.toBeNull();
    expect(allRadios(el)).toHaveLength(1);
  });

  it("renders a unicode profile name verbatim", async () => {
    const el = await mount(makeProps({ profiles: ["生産チーム"] }));

    expect(radioByLabel(el, "生産チーム")).not.toBeNull();
  });
});

describe("ProfilePanel — checked state reflects `active`", () => {
  it("checks 'None (base config)' when active is null", async () => {
    const el = await mount(makeProps({ profiles: ["web"], active: null }));

    expect(radioByLabel(el, "None (base config)")!.checked).toBe(true);
    expect(radioByLabel(el, "web")!.checked).toBe(false);
  });

  it("checks the matching profile row when active names it", async () => {
    const el = await mount(makeProps({ profiles: ["web", "cli"], active: "cli" }));

    expect(radioByLabel(el, "cli")!.checked).toBe(true);
    expect(radioByLabel(el, "web")!.checked).toBe(false);
    expect(radioByLabel(el, "None (base config)")!.checked).toBe(false);
  });

  it("checks exactly one radio at all times (mutually exclusive)", async () => {
    const el = await mount(makeProps({ profiles: ["web", "cli"], active: "web" }));

    const checkedCount = allRadios(el).filter((r) => r.checked).length;
    expect(checkedCount).toBe(1);
  });
});

describe("ProfilePanel — selecting an option calls onSelect", () => {
  it("clicking a different profile row calls onSelect with that profile's name", async () => {
    const onSelect = vi.fn();
    const el = await mount(
      makeProps({ profiles: ["web", "cli"], active: null, onSelect }),
    );

    const web = radioByLabel(el, "web")!;
    await act(async () => {
      web.click();
    });

    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith("web");
  });

  it("clicking 'None (base config)' while a profile is active calls onSelect(null)", async () => {
    const onSelect = vi.fn();
    const el = await mount(
      makeProps({ profiles: ["web"], active: "web", onSelect }),
    );

    const none = radioByLabel(el, "None (base config)")!;
    await act(async () => {
      none.click();
    });

    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith(null);
  });

  it("clicking the already-active option does not call onSelect (no redundant frame)", async () => {
    // jsdom does not fire `change` (nor, by extension, React's onChange) when
    // clicking an already-checked radio — mirrors native browser behaviour.
    const onSelect = vi.fn();
    const el = await mount(
      makeProps({ profiles: ["web"], active: "web", onSelect }),
    );

    const web = radioByLabel(el, "web")!;
    await act(async () => {
      web.click();
    });

    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe("ProfilePanel — locked once the conversation has started", () => {
  it("disables every radio (None + all profiles) when locked is true", async () => {
    const el = await mount(
      makeProps({ profiles: ["web", "cli"], active: "web", locked: true }),
    );

    for (const radio of allRadios(el)) {
      expect(radio.disabled).toBe(true);
    }
  });

  it("leaves every radio enabled when locked is false", async () => {
    const el = await mount(
      makeProps({ profiles: ["web", "cli"], active: "web", locked: false }),
    );

    for (const radio of allRadios(el)) {
      expect(radio.disabled).toBe(false);
    }
  });

  it("clicking a locked, non-active option does not call onSelect", async () => {
    const onSelect = vi.fn();
    const el = await mount(
      makeProps({ profiles: ["web", "cli"], active: "web", locked: true, onSelect }),
    );

    const cli = radioByLabel(el, "cli")!;
    await act(async () => {
      cli.click();
    });

    expect(onSelect).not.toHaveBeenCalled();
    // Native disabled-input semantics: the click must not even flip `checked`.
    expect(cli.checked).toBe(false);
  });

  it("shows a hint explaining the selection is locked (mirrors AgentsPanel's locked message)", async () => {
    const el = await mount(
      makeProps({ profiles: ["web"], active: "web", locked: true }),
    );

    // AgentsPanel's exact locked copy is "Locked for this conversation —
    // start a new one to change pipeline." We only require the same idea
    // ("locked" + "new" conversation), not byte-identical copy.
    expect(el.textContent).toMatch(/locked/i);
    expect(el.textContent).toMatch(/new/i);
  });

  it("shows no locked hint while the conversation has not started", async () => {
    const el = await mount(
      makeProps({ profiles: ["web"], active: "web", locked: false }),
    );

    expect(el.textContent).not.toMatch(/locked/i);
  });
});
