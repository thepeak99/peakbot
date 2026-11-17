// P5 — Task 3: NoMachines empty state (design §4).
// Zero local profiles + zero cloud targets → exact copy + button.

import { afterEach, describe, expect, it, vi } from "vitest";
import { NoMachines } from "./NoMachines";
import { COPY } from "./copy";
import { render, unmount } from "./testkit";

describe("NoMachines", () => {
  let container: HTMLDivElement;

  afterEach(async () => {
    if (container) await unmount(container);
    vi.restoreAllMocks();
  });

  it("renders the exact empty-state copy", async () => {
    container = await render(<NoMachines />);
    expect(container.textContent).toContain(COPY.empty.noMachines);
  });

  it("renders a button that opens cloud settings", async () => {
    const spy = vi.fn();
    window.addEventListener("peakbot:open-cloud-settings", spy);
    container = await render(<NoMachines />);
    const btn = container.querySelector("button");
    expect(btn).not.toBeNull();
    btn?.click();
    expect(spy).toHaveBeenCalledTimes(1);
    window.removeEventListener("peakbot:open-cloud-settings", spy);
  });
});