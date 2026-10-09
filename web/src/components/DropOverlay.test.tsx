// RED (T12): `DropOverlay.tsx` does not exist yet. Contract (design T12):
// a drag-depth counter on the container; the full-panel dashed "Drop files
// to attach" overlay shows only while a drag carrying Files is inside
// (depth > 0); a drop hands the files to onAdd (the shared
// attachments.add from App).
//
// ASSUMPTION: DropOverlay({ children, onAdd }) renders its children inside
// a container element that owns the drag handlers, so events dispatched on
// a child (the marker below) bubble to it.

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { DropOverlay } from "./DropOverlay";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT = true;
});

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mount(onAdd: (files: File[]) => void): Promise<HTMLDivElement> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();

  container = document.createElement("div");
  document.body.appendChild(container);
  await act(async () => {
    root = createRoot(container!);
    root.render(
      <DropOverlay onAdd={onAdd}>
        <span data-testid="marker">content</span>
      </DropOverlay>,
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
});

// jsdom's DragEvent constructor does not accept a dataTransfer init we can
// rely on, so build a plain Event and attach the dataTransfer surface the
// handlers read (`types`, `files`).
function dragEvent(type: string, dataTransfer: Record<string, unknown>): Event {
  const ev = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(ev, "dataTransfer", { value: dataTransfer });
  return ev;
}

const marker = (el: HTMLElement): HTMLElement =>
  el.querySelector('[data-testid="marker"]')!;

const hasOverlay = (el: HTMLElement): boolean =>
  (el.textContent ?? "").includes("Drop files to attach");

describe("DropOverlay", () => {
  it("shows 'Drop files to attach' on dragenter when the drag carries Files", async () => {
    const el = await mount(vi.fn());
    expect(hasOverlay(el)).toBe(false);

    await act(async () => {
      marker(el).dispatchEvent(dragEvent("dragenter", { types: ["Files"] }));
    });

    expect(hasOverlay(el)).toBe(true);
  });

  it("hides the overlay when dragleave returns the depth to 0", async () => {
    const el = await mount(vi.fn());

    await act(async () => {
      marker(el).dispatchEvent(dragEvent("dragenter", { types: ["Files"] }));
    });
    expect(hasOverlay(el)).toBe(true);

    await act(async () => {
      marker(el).dispatchEvent(dragEvent("dragleave", { types: ["Files"] }));
    });
    expect(hasOverlay(el)).toBe(false);
  });

  it("does not show the overlay for a drag without Files", async () => {
    const el = await mount(vi.fn());

    await act(async () => {
      marker(el).dispatchEvent(dragEvent("dragenter", { types: [] }));
    });

    expect(hasOverlay(el)).toBe(false);
  });

  it("calls onAdd with the dropped files on drop", async () => {
    const onAdd = vi.fn();
    const el = await mount(onAdd);
    const pdf = new File(["x"], "spec.pdf", { type: "application/pdf" });
    const png = new File(["y"], "cat.png", { type: "image/png" });

    await act(async () => {
      marker(el).dispatchEvent(dragEvent("dragenter", { types: ["Files"] }));
    });
    await act(async () => {
      marker(el).dispatchEvent(
        dragEvent("drop", { types: ["Files"], files: [pdf, png] }),
      );
    });

    expect(onAdd).toHaveBeenCalledTimes(1);
    expect(onAdd).toHaveBeenCalledWith([pdf, png]);
  });
});
