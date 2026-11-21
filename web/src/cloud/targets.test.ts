// P5 — Task 2: toCloudRows turns devices + profiles into catalog rows.
// Pure function, no store, no hook — test the derivation directly.

import { describe, expect, it } from "vitest";
import { toCloudRows } from "./targets";
import { device, selfDevice } from "./testkit";

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