// P5 — Task 2: toCloudRows turns devices + profiles into catalog rows.
// Pure function, no store, no hook — test the derivation directly.
// Plus: hook retry behavior for failed peer profile fetches.

import { describe, expect, it, vi, beforeEach, afterEach, beforeAll } from "vitest";
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
});