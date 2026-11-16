// G.2 + G.4 — the Settings → Cloud pane: one view per `CloudState` variant
// (design §A.1) with the exact copy (§D) and the confirm dialogs (§A.3).
//
// Harness: the raw `createRoot` + `act` + `querySelector` style (no
// testing-library). State is driven through the shared store (one
// `EventSource`), matching the real components.
//
// RED before implementation: `CloudPane` renders nothing and `store` is a
// skeleton, so every case fails for the right reason (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { CloudPane } from "./CloudPane";
import { store } from "./store";
import { COPY } from "./copy";
import {
  active,
  connecting,
  device,
  disabled,
  FakeEventSource,
  installFakeEventSource,
  json,
  loggedOut,
  mockFetch,
  pendingEmpty,
  pendingRejected,
  pendingRequesting,
  pendingWithSas,
  request,
  render,
  selfDevice,
  trust,
  uninstallFakeEventSource,
  type StateBody,
} from "./testkit";

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

let container: HTMLDivElement | null = null;
let openSpy: { mock: { calls: Array<Array<unknown>> } } | undefined;

beforeEach(() => {
  installFakeEventSource();
  FakeEventSource.reset();
  vi.restoreAllMocks();
  openSpy = vi.spyOn(window, "open").mockImplementation(() => null);
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

/** Flush microtasks (and React) so the (now async) fetch mock settles. */
async function flush(): Promise<void> {
  for (let i = 0; i < 4; i++) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/** Drive the store to `body`, then render the pane. */
async function withState(body: StateBody): Promise<HTMLDivElement> {
  mockFetch((url) => (url.endsWith("/api/cloud/state") ? Promise.resolve(json(200, body)) : Promise.resolve(json(200, {}))));
  store.reset();
  store.subscribe(() => {});
  await flush();
  FakeEventSource.last()?.emit("state", body);
  container = await render(<CloudPane />);
  return container;
}

const text = (el: Element): string => el.textContent ?? "";
const buttons = (el: Element): HTMLButtonElement[] =>
  [...el.querySelectorAll("button")] as HTMLButtonElement[];
const buttonByText = (el: Element, label: string): HTMLButtonElement | undefined =>
  buttons(el).find((b) => b.textContent?.trim() === label);

describe("no snapshot", () => {
  it("shows the loading line before the first SSE state", async () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? Promise.resolve(json(200, disabled())) : Promise.resolve(json(200, {}))));
    store.reset();
    store.subscribe(() => {});
    await flush();
    container = await render(<CloudPane />);
    expect(text(container!)).toContain(COPY.loading);
  });
});

describe("state → view (one row per §A.1)", () => {
  it("disabled → 'Cloud mode is off on this machine.' and no actions", async () => {
    const el = await withState(disabled());
    expect(text(el)).toContain(COPY.disabled);
    expect(buttons(el)).toHaveLength(0);
  });

  it("logged_out (never) → sign-in copy + a Sign in button", async () => {
    const el = await withState(loggedOut("never"));
    expect(text(el)).toContain(COPY.loggedOut.never);
    expect(buttonByText(el, COPY.btn.signIn)).toBeDefined();
  });

  it("logged_out (expired) → the expired copy", async () => {
    const el = await withState(loggedOut("expired"));
    expect(text(el)).toContain(COPY.loggedOut.expired);
  });

  it("logged_out (signed_out) → the signed-out copy", async () => {
    const el = await withState(loggedOut("signed_out"));
    expect(text(el)).toContain(COPY.loggedOut.signed_out);
  });

  it("Sign in → POST /api/cloud/login, then window.open(auth_url)", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, loggedOut("never")));
      if (url.endsWith("/api/cloud/login")) return Promise.resolve(json(200, { auth_url: "https://idp/cb?x=1" }));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", loggedOut("never"));
    container = await render(<CloudPane />);
    const btn = buttonByText(container!, COPY.btn.signIn);
    expect(btn).toBeDefined();
    await act(async () => btn!.click());
    const fetchMock = vi.mocked(globalThis.fetch);
    const post = fetchMock.mock.calls.find(([u, i]) => String(u).endsWith("/api/cloud/login") && i?.method === "POST");
    expect(post).toBeDefined();
    expect(openSpy!.mock.calls[0]?.[0]).toBe("https://idp/cb?x=1");
  });

  it("connecting → 'Connecting… (attempt N)' + the muted last_error", async () => {
    const el = await withState(connecting(3, "relay down"));
    expect(text(el)).toContain(COPY.connecting(3));
    expect(text(el)).toContain("relay down");
  });

  it("pending empty_account → explainer + a 'Make this the first device' button", async () => {
    const el = await withState(pendingEmpty());
    expect(text(el)).toContain(COPY.pendingEmpty.title);
    expect(text(el)).toContain(COPY.pendingEmpty.body);
    expect(buttonByText(el, COPY.btn.makeFirst)).toBeDefined();
  });

  it("pending with SAS → the 6 words as a numbered <ol>, in order", async () => {
    const words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    const el = await withState(pendingWithSas(words));
    expect(text(el)).toContain(COPY.pending.title);
    const items = [...el.querySelectorAll("ol li")].map((li) => text(li).replace(/^\d+\s?/, "").trim());
    expect(items).toEqual(words);
  });

  it("pending with SAS → the 15-minute note", async () => {
    const el = await withState(pendingWithSas());
    expect(text(el)).toContain(COPY.pending.body);
  });

  it("pending with no SAS (request posted) → 'Requesting approval…'", async () => {
    const el = await withState(pendingRequesting());
    expect(text(el)).toContain(COPY.pending.requesting);
  });

  it("pending rejected → the rejection text + a 'Try again' that POSTs /enroll/retry", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, pendingRejected()));
      if (url.endsWith("/api/cloud/enroll/retry")) return Promise.resolve(json(200, {}));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", pendingRejected());
    container = await render(<CloudPane />);
    expect(text(container!)).toContain(COPY.rejected.title);
    expect(text(container!)).toContain(COPY.rejected.body);
    const btn = buttonByText(container!, COPY.btn.tryAgain);
    expect(btn).toBeDefined();
    await act(async () => btn!.click());
    const fetchMock = vi.mocked(globalThis.fetch);
    expect(
      fetchMock.mock.calls.filter(([u]) => String(u).endsWith("/api/cloud/enroll/retry")),
    ).toHaveLength(1);
  });

  it("active → one row per device, a dot per online state, and '(this device)' with no Revoke on self", async () => {
    const body = active([
      selfDevice({ name: "Me", slug: "me", online: true }),
      device({ name: "Desk", slug: "desk", online: false }),
      device({ name: "Mystery", slug: "mystery", online: null }),
    ]);
    const el = await withState(body);
    expect(text(el)).toContain(COPY.active.title);
    expect(text(el)).toContain(COPY.active.self);
    const dots = [...el.querySelectorAll("[aria-label]")].map((d) => d.getAttribute("aria-label"));
    expect(dots).toEqual(expect.arrayContaining([COPY.active.online, COPY.active.offline, COPY.active.unknown]));
    // No Revoke on the self row.
    const rows = [...el.querySelectorAll("li, tr, [role='row']")];
    const selfRow = rows.find((r) => text(r).includes(COPY.active.self));
    expect(selfRow).toBeDefined();
    expect(buttonByText(selfRow!, COPY.btn.revoke)).toBeUndefined();
    // A non-self row has a Revoke.
    const deskRow = rows.find((r) => text(r).includes("Desk") && !text(r).includes(COPY.active.self));
    expect(deskRow).toBeDefined();
    expect(buttonByText(deskRow!, COPY.btn.revoke)).toBeDefined();
  });

  it("active → a 'Waiting' list per pending request with a Review button", async () => {
    const el = await withState(active([selfDevice()], [request({ name: "Phone", platform: "ios" })]));
    expect(text(el)).toContain(COPY.active.waiting("Phone", "ios"));
    expect(buttonByText(el, COPY.btn.review)).toBeDefined();
  });

  it("trust → the pane title/body for each trust_error", async () => {
    for (const te of ["genesis_mismatch", "fork", "revoked", "chain_invalid", "seed_store_error"] as const) {
      const el = await withState(trust(te));
      expect(text(el)).toContain(COPY.trust.paneTitle);
      expect(text(el)).toContain(COPY.trust.paneBody);
      container = null;
    }
  });
});

describe("last_login_error", () => {
  it("is shown outside active", async () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? Promise.resolve(json(200, { state: "logged_out", reason: "expired", last_login_error: "bad token" })) : Promise.resolve(json(200, {}))));
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", { state: "logged_out", reason: "expired", last_login_error: "bad token" });
    container = await render(<CloudPane />);
    expect(text(container!)).toContain(COPY.lastLoginError("bad token"));
  });

  it("is hidden in active (it is stale there)", async () => {
    const el = await withState(active());
    expect(text(el)).not.toContain("Sign-in failed");
  });
});

describe("error mapping (design §D.5)", () => {
  it("a 409 last_device revoke shows the mapped message, not the raw code", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, active([selfDevice(), device({ name: "Desk", slug: "desk" })])));
      if (url.endsWith("/revoke")) return Promise.resolve(json(409, { error: "last_device", message: "you can't revoke the last device" }));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", active([selfDevice(), device({ name: "Desk", slug: "desk" })]));
    container = await render(<CloudPane />);
    const revoke = buttonByText(container!, COPY.btn.revoke);
    expect(revoke).toBeDefined();
    await act(async () => revoke!.click());
    // The confirm step (G.4) — click the confirm button.
    const confirm = buttonByText(container!, COPY.confirm.revoke.confirm);
    expect(confirm).toBeDefined();
    await act(async () => confirm!.click());
    expect(text(container!)).toContain(COPY.err.last_device);
  });

  it("an unknown code shows the server `message`", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, active([selfDevice(), device({ name: "Desk", slug: "desk" })])));
      if (url.endsWith("/revoke")) return Promise.resolve(json(418, { error: "mystery", message: "teapot" }));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", active([selfDevice(), device({ name: "Desk", slug: "desk" })]));
    container = await render(<CloudPane />);
    const revoke = buttonByText(container!, COPY.btn.revoke);
    expect(revoke).toBeDefined();
    await act(async () => revoke!.click());
    const confirm = buttonByText(container!, COPY.confirm.revoke.confirm);
    await act(async () => confirm!.click());
    expect(text(container!)).toContain("teapot");
  });
});

describe("Sign out availability", () => {
  it("is present in pending, active and trust", async () => {
    for (const body of [pendingWithSas(), active([selfDevice()]), trust()]) {
      const el = await withState(body);
      expect(buttonByText(el, COPY.btn.signOut), `Sign out in ${body.state}`).toBeDefined();
    }
  });

  it("is NOT present in disabled / logged_out / connecting", async () => {
    for (const body of [disabled(), loggedOut("never"), connecting(1)]) {
      const el = await withState(body);
      expect(buttonByText(el, COPY.btn.signOut), `no Sign out in ${body.state}`).toBeUndefined();
    }
  });
});

describe("confirms (G.4)", () => {
  it("first device: opening sends nothing; Cancel is focused; confirm → exactly one POST /genesis", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, pendingEmpty()));
      if (url.endsWith("/api/cloud/genesis")) return Promise.resolve(json(200, { device_id: "aa" }));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", pendingEmpty());
    container = await render(<CloudPane />);
    buttonByText(container!, COPY.btn.makeFirst)?.click();
    await act(async () => {});
    // Opening the dialog must not have POSTed anything yet.
    const fetchMock = vi.mocked(globalThis.fetch);
    expect(fetchMock.mock.calls.filter(([u]) => String(u).includes("/api/cloud/genesis"))).toHaveLength(0);
    expect(document.activeElement?.textContent?.trim()).toBe(COPY.btn.cancel);
    const confirm = buttonByText(container!, COPY.confirm.first.confirm);
    expect(confirm).toBeDefined();
    await act(async () => confirm!.click());
    expect(fetchMock.mock.calls.filter(([u]) => String(u).endsWith("/api/cloud/genesis"))).toHaveLength(1);
  });

  it("revoke: confirm → exactly one POST /devices/{id}/revoke", async () => {
    const id = "dd".repeat(32);
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, active([selfDevice(), device({ id, name: "Desk", slug: "desk" })])));
      if (url.endsWith("/revoke")) return Promise.resolve(json(200, {}));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", active([selfDevice(), device({ id, name: "Desk", slug: "desk" })]));
    container = await render(<CloudPane />);
    buttonByText(container!, COPY.btn.revoke)?.click();
    await act(async () => {});
    const confirm = buttonByText(container!, COPY.confirm.revoke.confirm);
    expect(confirm).toBeDefined();
    await act(async () => confirm!.click());
    const fetchMock = vi.mocked(globalThis.fetch);
    expect(fetchMock.mock.calls.filter(([u]) => String(u).includes(`/devices/${id}/revoke`))).toHaveLength(1);
  });

  it("sign out: confirm → exactly one POST /api/cloud/logout", async () => {
    mockFetch((url) => {
      if (url.endsWith("/api/cloud/state")) return Promise.resolve(json(200, active([selfDevice()])));
      if (url.endsWith("/api/cloud/logout")) return Promise.resolve(json(200, { revoked: true }));
      return Promise.resolve(json(200, {}));
    });
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", active([selfDevice()]));
    container = await render(<CloudPane />);
    buttonByText(container!, COPY.btn.signOut)?.click();
    await act(async () => {});
    const confirm = buttonByText(container!, COPY.confirm.signOut.confirm);
    expect(confirm).toBeDefined();
    await act(async () => confirm!.click());
    const fetchMock = vi.mocked(globalThis.fetch);
    expect(fetchMock.mock.calls.filter(([u]) => String(u).endsWith("/api/cloud/logout"))).toHaveLength(1);
  });

  it("a confirm Cancel closes with no fetch", async () => {
    mockFetch((url) => (url.endsWith("/api/cloud/state") ? Promise.resolve(json(200, pendingEmpty())) : Promise.resolve(json(200, {}))));
    store.reset();
    store.subscribe(() => {});
    await flush();
    FakeEventSource.last()?.emit("state", pendingEmpty());
    container = await render(<CloudPane />);
    buttonByText(container!, COPY.btn.makeFirst)?.click();
    await act(async () => {});
    const cancel = buttonByText(container!, COPY.btn.cancel);
    expect(cancel).toBeDefined();
    await act(async () => cancel!.click());
    const fetchMock = vi.mocked(globalThis.fetch);
    expect(fetchMock.mock.calls.filter(([u]) => String(u).includes("/api/cloud/genesis"))).toHaveLength(0);
  });
});
