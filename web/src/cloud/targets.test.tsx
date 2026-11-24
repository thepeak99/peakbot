// P5 — Task 2: toCloudRows turns devices + profiles into catalog rows.
// Pure function, no store, no hook — test the derivation directly.
// Plus: hook retry behavior for failed peer profile fetches.

import { describe, expect, it, vi, beforeAll } from "vitest";
import { act } from "react";
import { toCloudRows, useCloudTargets } from "./targets";
import { device, selfDevice, mockFetch, json } from "./testkit";

// Mock useCloudDevices — the hook test needs to control the device list
vi.mock("./useCloud", () => ({
  useCloudDevices: vi.fn(() => [device({ id: "d1", slug: "desk" })]),
}));

import { render as testkitRender, unmount as testkitUnmount } from "./testkit";

// Wrapper component that exposes hook state for testing
function CloudTargetsInspector() {
  const targets = useCloudTargets();
  return (
    <div data-testid="cloud-targets">
      <span data-testid="status">{targets.status}</span>
      <span data-testid="rows">{JSON.stringify(targets.status === "ready" ? targets.rows : [])}</span>
    </div>
  );
}

async function mountTargetsInspector() {
  return testkitRender(<CloudTargetsInspector />);
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

describe("useCloudTargets", () => {
  it("retries failed peer profiles fetch after 2 seconds", async () => {
    const fetchMock = mockFetch(async (url) => {
      if (url.includes("d1/profiles")) {
        throw new Error("network error");
      }
      return json(200, {});
    });

    const container = await mountTargetsInspector();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });

    // First fetch was attempted
    expect(fetchMock).toHaveBeenCalledWith("/api/cloud/devices/d1/profiles", expect.anything());

    // Wait past the 2-second retry interval (real timer)
    await act(async () => {
      await new Promise((r) => setTimeout(r, 2100));
    });

    // BUG: current code never retries — this assertion should FAIL (RED)
    expect(fetchMock).toHaveBeenCalledTimes(2);

    await testkitUnmount(container);
  });

  it("refetches failed peer immediately on visibilitychange to visible", async () => {
    const fetchMock = mockFetch(async (url) => {
      if (url.includes("d1/profiles")) {
        throw new Error("network error");
      }
      return json(200, {});
    });

    const container = await mountTargetsInspector();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Stub visibilityState and fire visibilitychange
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });

    // Should have retried immediately, not waiting for 2s backoff
    expect(fetchMock).toHaveBeenCalledTimes(2);

    await testkitUnmount(container);
  });

    it("stops retrying after unmount", async () => {
    const fetchMock = mockFetch(async (url) => {
      if (url.includes("d1/profiles")) {
        throw new Error("network error");
      }
      return json(200, {});
    });

    const container = await mountTargetsInspector();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);

    // Unmount
    await testkitUnmount(container);

    // Wait longer than retry interval (use shorter wait to avoid timeout)
    await act(async () => {
      await new Promise((r) => setTimeout(r, 3000));
    });

    // Should still be 1 call - no retry after unmount
    expect(fetchMock).toHaveBeenCalledTimes(1);
  }, 8000);

  it("does not refetch successful peer while another retries and caps backoff at 30s", async () => {
    // Override mock to return two devices: d1 (ok), d2 (failing)
    const { useCloudDevices } = await import("./useCloud");
    useCloudDevices.mockReturnValue([
      device({ id: "d1", slug: "desk1" }),
      device({ id: "d2", slug: "desk2" }),
    ]);

    const fetchMock = mockFetch(async (url) => {
      if (url.includes("d1/profiles")) {
        return json(200, { items: [{ name: "p1" }] });
      }
      if (url.includes("d2/profiles")) {
        throw new Error("network error");
      }
      return json(200, {});
    });

    const container = await mountTargetsInspector();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 100));
    });

    // Both peers attempted once
    expect(fetchMock).toHaveBeenCalledTimes(2);

    // Wait through multiple backoff periods: 2s, 4s, 8s, 16s, 30s, 30s = 90s total
    // d2 should have retried 6 times (count 7 total), d1 should still be 1
    await act(async () => {
      await new Promise((r) => setTimeout(r, 90000));
    });

    // d1 fetched once (successful, not retried)
    const d1Calls = fetchMock.mock.calls.filter(
      (c) => c[0].includes("d1/profiles")
    ).length;
    expect(d1Calls).toBe(1);

    // d2: 1 initial + 6 retries = 7 calls (backoff: 2+4+8+16+30+30 = 90s)
    const d2Calls = fetchMock.mock.calls.filter(
      (c) => c[0].includes("d2/profiles")
    ).length;
    expect(d2Calls).toBe(7);

    await testkitUnmount(container);

    // Reset mock for other tests
    useCloudDevices.mockReturnValue([device({ id: "d1", slug: "desk" })]);
  }, 100000);
});

describe("toCloudRows", () => {
  it("empty devices and profiles → empty rows", () => {
    expect(toCloudRows([], {})).toEqual([]);
  });

  it("device with profiles → one row per profile, name is profile@slug", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], {
      d1: [
        { name: "alice", color: "#ff0000" },
        { name: "bob" },
      ],
    });
    expect(rows).toHaveLength(2);
    expect(rows[0].name).toBe("alice@desk");
    expect(rows[1].name).toBe("bob@desk");
  });

  it("rows have origin remote", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice" }] });
    expect(rows[0].origin).toBe("remote");
  });

  it("row color comes from the profile color", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice", color: "#ff0000" }] });
    expect(rows[0].color).toBe("#ff0000");
  });

  it("row without profile color has no color field", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice" }] });
    expect(rows[0].color).toBeUndefined();
  });

  it("self device is excluded", () => {
    const me = selfDevice({ slug: "me", id: "d1" });
    const other = device({ slug: "desk", id: "d2" });
    const rows = toCloudRows([me, other], {
      d1: [{ name: "me-profile" }],
      d2: [{ name: "other-profile" }],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].name).toBe("other-profile@desk");
  });

  it("offline device is excluded", () => {
    const online = device({ slug: "desk", id: "d1", online: true });
    const offline = device({ slug: "away", id: "d2", online: false });
    const rows = toCloudRows([online, offline], {
      d1: [{ name: "p1" }],
      d2: [{ name: "p2" }],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].name).toBe("p1@desk");
  });

  it("device with null online (unknown) is included", () => {
    const d = device({ slug: "desk", id: "d1", online: null });
    const rows = toCloudRows([d], { d1: [{ name: "p1" }] });
    expect(rows).toHaveLength(1);
  });

  it("stable order: devices order then profile order", () => {
    const d1 = device({ slug: "a", id: "d1" });
    const d2 = device({ slug: "b", id: "d2" });
    const rows = toCloudRows([d1, d2], {
      d1: [{ name: "z" }, { name: "a" }],
      d2: [{ name: "m" }],
    });
    expect(rows.map((r) => r.name)).toEqual(["z@a", "a@a", "m@b"]);
  });

  it("device with no profiles contributes no rows", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], {});
    expect(rows).toEqual([]);
  });

  it("row has overrides_global false and empty agents", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice" }] });
    expect(rows[0].overrides_global).toBe(false);
    expect(rows[0].agents).toEqual([]);
  });

  // ── Regression: two hosts, both carrying profile `default` ───────────────
  //
  // The catalog row must carry the device slug as `machine` (the very value
  // mergeCatalog uses to look up the human device name). Without it every
  // cloud row's machine is undefined and two `default` profiles on different
  // hosts collapse into one indistinguishable entry.
  it("carries the device slug as machine on every row", () => {
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice" }, { name: "bob" }] });
    expect(rows.map((r) => r.machine)).toEqual(["desk", "desk"]);
  });

  it("uses each device's own slug when several peers report the same profile", () => {
    const desk = device({ slug: "desk", id: "d1", name: "Desktop" });
    const phone = device({ slug: "phone", id: "d2", name: "Pixel 10 Pro" });
    const rows = toCloudRows([desk, phone], {
      d1: [{ name: "default" }],
      d2: [{ name: "default" }],
    });
    expect(rows.map((r) => r.name)).toEqual(["default@desk", "default@phone"]);
    expect(rows.map((r) => r.machine)).toEqual(["desk", "phone"]);
  });

  it("preserves the profile name verbatim before appending @slug", () => {
    // A profile name may itself contain '@'. The route key is exactly
    // `${profile}@${slug}`; the last '@' is the machine separator.
    const d = device({ slug: "desk", id: "d1" });
    const rows = toCloudRows([d], { d1: [{ name: "alice@corp" }] });
    expect(rows[0].name).toBe("alice@corp@desk");
    expect(rows[0].machine).toBe("desk");
  });
});