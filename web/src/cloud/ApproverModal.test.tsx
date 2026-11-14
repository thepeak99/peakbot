// G.3 — the approver modal's safety contract (design §E). This is the
// security-critical part of P4: Approve is NEVER the default, Enter does
// nothing, Escape = "later", a double click sends exactly one request, and
// focus is trapped.
//
// Harness: raw `createRoot` + `act` + `querySelector` + `KeyboardEvent` /
// `MouseEvent`. State is driven through the shared store (one EventSource).
//
// RED before implementation: `ApproverModal` renders nothing, so `activeElement`
// stays the body and no dialog buttons exist — every case fails for the right
// reason (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { ApproverModal } from "./ApproverModal";
import { store } from "./store";
import { COPY } from "./copy";
import {
  active,
  FakeEventSource,
  installFakeEventSource,
  json,
  mockFetch,
  request,
  render,
  selfDevice,
  sas,
  uninstallFakeEventSource,
  type TestEnrollRequest,
} from "./testkit";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

let container: HTMLDivElement | null = null;

beforeEach(() => {
  installFakeEventSource();
  FakeEventSource.reset();
  vi.restoreAllMocks();
});

afterEach(() => {
  act(() => container?.remove());
  container = null;
  try {
    store.reset();
  } catch {
    /* skeleton: reset throws — RED on purpose */
  }
  uninstallFakeEventSource();
});

async function withRequests(reqs: TestEnrollRequest[]): Promise<HTMLDivElement> {
  mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, active([selfDevice()], reqs)) : json(200, {})));
  store.reset();
  store.subscribe(() => {});
  FakeEventSource.last()?.emit("state", active([selfDevice()], reqs));
  container = await render(<ApproverModal />);
  return container;
}

const dialog = (): HTMLElement | null => container?.querySelector('[role="dialog"], [role="alertdialog"]') ?? null;
const buttons = (): HTMLButtonElement[] =>
  [...(container?.querySelectorAll("button") ?? [])] as HTMLButtonElement[];
const buttonByText = (label: string): HTMLButtonElement | undefined =>
  buttons().find((b) => b.textContent?.trim() === label);

function press(target: Element, key: string, extra: KeyboardEventInit = {}): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...extra }));
}

describe("focus and the words", () => {
  it("opens with 'Not now' focused — never Approve", async () => {
    await withRequests([request({ name: "Phone" })]);
    expect(dialog()).not.toBeNull();
    expect(document.activeElement?.textContent?.trim()).toBe(COPY.btn.later);
  });

  it("shows the title, the body, and the 6 SAS words as a numbered list", async () => {
    const words = sas();
    await withRequests([request({ name: "Phone", sas: words })]);
    expect(dialog()).not.toBeNull();
    const el = container!;
    expect(el.textContent).toContain(COPY.approve.title("Phone"));
    expect(el.textContent).toContain(COPY.approve.body);
    const items = [...el.querySelectorAll("ol li")].map((li) => li.textContent?.replace(/^\d+\s?/, "").trim());
    expect(items).toEqual(words);
  });

  it("shows the platform and a ticking 'expires in m:ss' sub-line", async () => {
    const future = Math.floor(Date.now() / 1000) + 90;
    await withRequests([request({ name: "Phone", platform: "ios", expires_at: future })]);
    expect(container!.textContent).toContain("expires in");
    expect(container!.textContent).toContain("ios");
  });
});

describe("Enter does nothing", () => {
  it("Enter on the dialog sends nothing and stays open", async () => {
    const d = await withRequests([request({ name: "Phone" })]);
    press(d, "Enter");
    await act(async () => {});
    expect(vi.mocked(globalThis.fetch).mock.calls.filter(([u]) => String(u).includes("/enroll/"))).toHaveLength(0);
    expect(dialog()).not.toBeNull();
  });

  it("Enter on a FOCUSED Approve button sends nothing (no implicit click)", async () => {
    await withRequests([request({ name: "Phone" })]);
    const approve = buttonByText(COPY.btn.approve);
    expect(approve).toBeDefined();
    approve!.focus();
    press(approve!, "Enter");
    await act(async () => {});
    expect(vi.mocked(globalThis.fetch).mock.calls.filter(([u]) => String(u).includes("/enroll/approve"))).toHaveLength(0);
    expect(dialog()).not.toBeNull();
  });
});

describe("Escape = later", () => {
  it("Escape sends nothing, closes the modal, and dismisses the letter", async () => {
    const d = await withRequests([request({ name: "Phone" })]);
    press(d, "Escape");
    await act(async () => {});
    expect(vi.mocked(globalThis.fetch).mock.calls.filter(([u]) => String(u).includes("/enroll/"))).toHaveLength(0);
    expect(dialog()).toBeNull();
    expect(store.getSnapshot().dismissed.has("bb".repeat(16))).toBe(true);
  });
});

describe("double click sends exactly once", () => {
  it("two synchronous clicks on Approve → exactly ONE POST /enroll/approve", async () => {
    let release: ((r: Response) => void) | undefined;
    const gate = new Promise<Response>((resolve) => { release = resolve; });
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return json(200, active([selfDevice()], [request({ name: "Phone" })]));
      if (url.endsWith("/enroll/approve")) return gate;
      return json(200, {});
    });
    store.reset();
    store.subscribe(() => {});
    FakeEventSource.last()?.emit("state", active([selfDevice()], [request({ name: "Phone" })]));
    container = await render(<ApproverModal />);
    const approve = buttonByText(COPY.btn.approve);
    expect(approve).toBeDefined();
    await act(async () => {
      approve!.click();
      approve!.click(); // same tick, before the first resolves
    });
    release!(json(200, { device_id: "cc" }));
    await act(async () => {});
    const posts = vi.mocked(globalThis.fetch).mock.calls.filter(([u]) => String(u).endsWith("/enroll/approve"));
    expect(posts).toHaveLength(1);
    const body = JSON.parse(String(posts[0]?.[1]?.body ?? "{}"));
    expect(body.letter_id).toBe("bb".repeat(16));
  });

  it("while the POST is pending, Approve/Reject are disabled but 'Not now' is enabled", async () => {
    let release: ((r: Response) => void) | undefined;
    const gate = new Promise<Response>((resolve) => { release = resolve; });
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return json(200, active([selfDevice()], [request({ name: "Phone" })]));
      if (url.endsWith("/enroll/approve")) return gate;
      return json(200, {});
    });
    store.reset();
    store.subscribe(() => {});
    FakeEventSource.last()?.emit("state", active([selfDevice()], [request({ name: "Phone" })]));
    container = await render(<ApproverModal />);
    const approve = buttonByText(COPY.btn.approve);
    expect(approve).toBeDefined();
    const p = act(async () => approve!.click());
    // Pending now.
    expect(buttonByText(COPY.btn.approve)?.disabled).toBe(true);
    expect(buttonByText(COPY.btn.reject)?.disabled).toBe(true);
    expect(buttonByText(COPY.btn.later)?.disabled).toBe(false);
    release!(json(200, { device_id: "cc" }));
    await p;
  });
});

describe("focus trap", () => {
  it("Tab from the last button wraps to the first; Shift+Tab from the first wraps to the last", async () => {
    const d = await withRequests([request({ name: "Phone" })]);
    const btns = buttons();
    expect(btns.length).toBeGreaterThanOrEqual(3);
    const last = btns[btns.length - 1];
    last.focus();
    press(d, "Tab");
    expect(document.activeElement).toBe(btns[0]);
    const first = btns[0];
    first.focus();
    press(d, "Tab", { shiftKey: true });
    expect(document.activeElement).toBe(btns[btns.length - 1]);
  });

  it("focus returns to the opener on close", async () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    const d = await withRequests([request({ name: "Phone" })]);
    press(d, "Escape");
    await act(async () => {});
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});

describe("lifecycle", () => {
  it("unmounts when the request disappears from the next snapshot", async () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, active([selfDevice()], [request({ name: "Phone" })])) : json(200, {})));
    store.reset();
    store.subscribe(() => {});
    FakeEventSource.last()?.emit("state", active([selfDevice()], [request({ name: "Phone" })]));
    container = await render(<ApproverModal />);
    expect(dialog()).not.toBeNull();
    await act(async () => FakeEventSource.last()?.emit("state", active([selfDevice()], [])));
    expect(dialog()).toBeNull();
  });

  it("two requests → the earlier expires_at is shown first", async () => {
    const t = Math.floor(Date.now() / 1000);
    const later = request({ name: "Later", expires_at: t + 600 });
    const sooner = request({ name: "Sooner", letter_id: "11".repeat(16), expires_at: t + 120 });
    await withRequests([later, sooner]);
    expect(container!.textContent).toContain(COPY.approve.title("Sooner"));
    expect(container!.textContent).not.toContain(COPY.approve.title("Later"));
  });

  it("an expired request is not shown", async () => {
    const past = Math.floor(Date.now() / 1000) - 5;
    await withRequests([request({ name: "Gone", expires_at: past })]);
    expect(dialog()).toBeNull();
  });
});
