// P5 — cloud target rows (design §2).
//
// Turns the live cloud device list into catalog rows the chip can render.
// Pure function; the hook wires it to the cloud store.

import { useEffect, useMemo, useState } from "react";
import type { TabColor, WireProfileInfo } from "../state";
import type { Device } from "./types";
import { useCloudDevices } from "./useCloud";

export type CloudTargets =
  | { status: "loading" }
  | { status: "ready"; rows: WireProfileInfo[] };

/** The fields `toCloudRows` reads; full `Device`s satisfy it. */
type Peer = Pick<Device, "id" | "slug" | "self" | "online">;
type Profile = { name: string; color?: string };

/** Pure: devices + per-device profiles → catalog rows (design §3, §5). */
export function toCloudRows(
  devices: readonly Peer[],
  profilesByDevice: Record<string, Profile[]>,
): WireProfileInfo[] {
  const rows: WireProfileInfo[] = [];
  for (const d of devices) {
    // Exclude self and offline devices
    if (d.self) continue;
    if (d.online === false) continue;
    const profiles = profilesByDevice[d.id];
    if (!profiles) continue;
    for (const p of profiles) {
      const row: WireProfileInfo = {
        name: `${p.name}@${d.slug}`,
        origin: "remote",
        overrides_global: false,
        agents: [],
      };
      if (p.color !== undefined) {
        row.color = p.color as TabColor;
      }
      rows.push(row);
    }
  }
  return rows;
}

/** `GET /api/cloud/devices/{id}/profiles` answers `{ "profiles": [...] }`
 *  (src/ui/web/cloud_api.rs). Anything else contributes no rows. */
function parseProfiles(body: unknown): Profile[] {
  if (typeof body !== "object" || body === null) return [];
  const list = (body as { profiles?: unknown }).profiles;
  if (!Array.isArray(list)) return [];
  return list.filter(
    (p): p is Profile =>
      typeof p === "object" && p !== null && typeof (p as Profile).name === "string",
  );
}

function sameRows(a: WireProfileInfo[], b: WireProfileInfo[]): boolean {
  return a.length === b.length && a.every((r, i) => r.name === b[i].name && r.color === b[i].color);
}

const NO_ROWS: WireProfileInfo[] = [];

/** Hook: derive cloud rows from the live cloud store. */
export function useCloudTargets(): CloudTargets {
  const devices = useCloudDevices();
  const [rows, setRows] = useState<WireProfileInfo[]>(NO_ROWS);
  const [loading, setLoading] = useState(true);

  // A primitive key of the eligible peers, computed in render. Every SSE
  // event parses into a new `devices` array, so the effect must key on
  // content, not identity; the effect reads the peers back from the key so
  // it has no other dependency.
  const peersKey = JSON.stringify(
    devices
      .filter((d) => !d.self && d.online !== false)
      .map((d): Peer => ({ id: d.id, slug: d.slug, self: d.self, online: d.online })),
  );

  useEffect(() => {
    const peers = JSON.parse(peersKey) as Peer[];
    const commit = (next: WireProfileInfo[]): void => {
      setRows((prev) => (sameRows(prev, next) ? prev : next));
      setLoading(false);
    };
    if (peers.length === 0) {
      commit(NO_ROWS);
      return;
    }

    const ctrl = new AbortController();
    setLoading(true);
    const fetchOne = async (p: Peer): Promise<Profile[]> => {
      try {
        const res = await fetch(`/api/cloud/devices/${p.id}/profiles`, { signal: ctrl.signal });
        return res.ok ? parseProfiles(await res.json()) : [];
      } catch {
        return []; // unreachable device (or aborted) → 0 rows
      }
    };
    void Promise.all(peers.map(fetchOne)).then((lists) => {
      if (ctrl.signal.aborted) return;
      const byId: Record<string, Profile[]> = {};
      peers.forEach((p, i) => {
        byId[p.id] = lists[i];
      });
      commit(toCloudRows(peers, byId));
    });
    return () => ctrl.abort();
  }, [peersKey]);

  // Stable result identity: App's auto-select effect depends on this object.
  return useMemo<CloudTargets>(
    () => (loading ? { status: "loading" } : { status: "ready", rows }),
    [loading, rows],
  );
}
