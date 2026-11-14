// G.5 — the trust banner (design §A.4): a full-width red `role="alert"` bar
// shown only in `trust`, non-dismissible, with an "Open Cloud settings" button.
//
// Harness: raw `createRoot` + `act` + `querySelector`. State is driven through
// the shared store (one EventSource).
//
// RED before implementation: `TrustBanner` renders nothing, so the alerts are
// absent in every state — every case fails for the right reason (feature absent).

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { TrustBanner } from "./TrustBanner";
import { store } from "./store";
import { COPY } from "./copy";
import {
  active,
  connecting,
  disabled,
  FakeEventSource,
  installFakeEventSource,
  json,
  loggedOut,
  mockFetch,
  pendingWithSas,
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

async function withState(body: StateBody): Promise<HTMLDivElement> {
  mockFetch((url) => (url.endsWith("/api/cloud/state") ? json(200, body) : json(200, {})));
  store.reset();
  store.subscribe(() => {});
  FakeEventSource.last()?.emit("state", body);
  container = await render(<TrustBanner />);
  return container;
}

describe("visibility", () => {
  it("renders nothing in every non-trust state", async () => {
    for (const body of [
      disabled(),
      loggedOut("never"),
      connecting(1),
      pendingWithSas(),
      active([selfDevice()]),
    ]) {
      const el = await withState(body);
      expect(el.querySelector('[role="alert"]'), `no alert in ${body.state}`).toBeNull();
    }
  });

  it("renders a role=alert bar for each trust_error with the exact §A.4 text", async () => {
    const cases: Record<string, string> = {
      genesis_mismatch: COPY.banner.genesis_mismatch,
      fork: COPY.banner.fork,
      revoked: COPY.banner.revoked,
      chain_invalid: COPY.banner.chain_invalid,
    };
    for (const [te, copy] of Object.entries(cases)) {
      const el = await withState(trust(te as Parameters<typeof trust>[0]));
      const alert = el.querySelector('[role="alert"]');
      expect(alert, `alert for ${te}`).not.toBeNull();
      expect(alert?.textContent).toContain(copy);
    }
  });
});

describe("safety", () => {
  it("has NO close/dismiss button", async () => {
    const el = await withState(trust("fork"));
    const btns = [...el.querySelectorAll("button")];
    expect(btns.some((b) => /close|dismiss|×/i.test(b.textContent ?? ""))).toBe(false);
  });

  it("has an 'Open Cloud settings' <button> (a real button, not an anchor)", async () => {
    const el = await withState(trust("fork"));
    const open = [...el.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === COPY.btn.openCloudSettings,
    );
    expect(open, "the opener is a <button>").toBeDefined();
    expect(el.querySelector(`a`), "no anchor for the opener").toBeNull();
  });

  it("clicking 'Open Cloud settings' does not throw and sends no fetch", async () => {
    const el = await withState(trust("fork"));
    const open = [...el.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === COPY.btn.openCloudSettings,
    );
    expect(open).toBeDefined();
    await act(async () => open!.click());
    // Opening settings is navigation, not a mutation: no fetch.
    expect(vi.mocked(globalThis.fetch).mock.calls.filter(([u]) => String(u).includes("/api/cloud/"))).toHaveLength(0);
  });
});
