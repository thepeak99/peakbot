// PR-A tests T16–T19 (/tmp/peakbot/design/notify-pr-a.md §7). The target
// interface is `<NotifyToggle notify={TaskNotifications} />`, rendering a bell
// plus a speaker button. RED today: the component still takes
// `enabled/permission/onToggle`, so `notify` is ignored, `permission` is
// undefined, and the expected strings/speaker button are absent. (Typecheck is
// also RED until the prop shape changes; vitest still runs.)
//
// jsdom is enabled for `src/components/**` by vitest.config.ts.

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { NotifyToggle } from "./NotifyToggle";
import type { TaskNotifications } from "../useTaskNotifications";

let root: Root | null = null;
let container: HTMLElement | null = null;

function render(over: Partial<TaskNotifications>) {
  const notify: TaskNotifications = {
    enabled: false,
    permission: "default",
    toggle: vi.fn(),
    sound: false,
    toggleSound: vi.fn(),
    ...over,
  };
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(<NotifyToggle notify={notify} />);
  });
  return { container, notify };
}

// Bell is the first button, speaker the second (design §5.4 markup order).
function buttons(c: HTMLElement) {
  const [bell, speaker] = Array.from(c.querySelectorAll("button"));
  return { bell, speaker };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

describe("NotifyToggle — tooltips and disabled states", () => {
  it("T16 insecure tooltip mentions HTTPS, --tls and the CA; bell disabled, speaker not", () => {
    const { container: c } = render({ permission: "insecure" });
    const { bell, speaker } = buttons(c);
    expect(bell).toBeDefined();
    expect(bell.title).toContain("HTTPS");
    expect(bell.title).toContain("--tls");
    expect(bell.title).toContain("/peakbot-ca.crt");
    expect(bell.disabled).toBe(true);
    expect(speaker).toBeDefined();
    expect(speaker.disabled).toBe(false);
  });

  it("T17 denied tooltip points to site settings; bell disabled, speaker not", () => {
    const { container: c } = render({ permission: "denied" });
    const { bell, speaker } = buttons(c);
    expect(bell.title).toContain("site settings");
    expect(bell.disabled).toBe(true);
    expect(speaker.disabled).toBe(false);
  });

  it("T17b the bell's aria-label mirrors its title", () => {
    const { container: c } = render({ permission: "denied" });
    const { bell } = buttons(c);
    expect(bell.getAttribute("aria-label")).toBe(bell.title);
  });

  it("T17c granted + enabled: bell is pressed, enabled, tooltip says (on)", () => {
    const { container: c } = render({ permission: "granted", enabled: true });
    const { bell } = buttons(c);
    expect(bell.getAttribute("aria-pressed")).toBe("true");
    expect(bell.disabled).toBe(false);
    expect(bell.title).toContain("(on)");
  });

  it("T17d granted + not enabled: bell is unpressed, tooltip says (off)", () => {
    const { container: c } = render({ permission: "granted", enabled: false });
    const { bell } = buttons(c);
    expect(bell.getAttribute("aria-pressed")).toBe("false");
    expect(bell.title).toContain("(off)");
  });

  it("T17e a stale enabled flag does not light the bell when permission is denied", () => {
    const { container: c } = render({ permission: "denied", enabled: true });
    expect(buttons(c).bell.getAttribute("aria-pressed")).toBe("false");
  });

  it("T17f clicking the bell calls toggle once (not toggleSound)", () => {
    const { container: c, notify } = render({ permission: "default" });
    act(() => buttons(c).bell.click());
    expect(notify.toggle).toHaveBeenCalledTimes(1);
    expect(notify.toggleSound).not.toHaveBeenCalled();
  });

  it("T18 unsupported renders nothing", () => {
    const { container: c } = render({ permission: "unsupported" });
    expect(c.innerHTML).toBe("");
  });
});

describe("NotifyToggle — speaker", () => {
  it("T19 speaker reflects sound off and a click flips it", () => {
    const { container: c, notify } = render({ permission: "granted", sound: false });
    const { speaker } = buttons(c);
    expect(speaker.getAttribute("aria-pressed")).toBe("false");
    expect(speaker.title).toContain("(off)");
    act(() => speaker.click());
    expect(notify.toggleSound).toHaveBeenCalledTimes(1);
    expect(notify.toggle).not.toHaveBeenCalled();
  });

  it("T19b speaker reflects sound on", () => {
    const { container: c } = render({ permission: "granted", sound: true });
    const { speaker } = buttons(c);
    expect(speaker.getAttribute("aria-pressed")).toBe("true");
    expect(speaker.title).toContain("(on)");
    expect(speaker.getAttribute("aria-label")).toBe(speaker.title);
  });

  it("T19c the speaker works on a denied origin (sound is independent of permission)", () => {
    const { container: c, notify } = render({ permission: "denied", sound: false });
    const { speaker } = buttons(c);
    expect(speaker.disabled).toBe(false);
    act(() => speaker.click());
    expect(notify.toggleSound).toHaveBeenCalledTimes(1);
  });
});
