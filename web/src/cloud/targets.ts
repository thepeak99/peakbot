// P5 — cloud target rows (design §2).
//
// Turns the live cloud device list into catalog rows the chip can render.
// Pure function; the hook wires it to the cloud store.

import { useEffect, useState } from "react";
import type { WireProfileInfo } from "../state";
import type { Device } from "./types";
import { useCloudDevices } from "./useCloud";

export type CloudTargets =
  | { status: "loading" }
  | { status: "ready"; rows: WireProfileInfo[] };

/** Pure: devices + per-device profiles → catalog rows (design §3, §5). */
export function toCloudRows(
  devices: Device[],
  profilesByDevice: Record<string, { name: string; color?: string }[]>,
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
        row.color = p.color;
      }
      rows.push(row);
    }
  }
  return rows;
}

/** Hook: derive cloud rows from the live cloud store. */
export function useCloudTargets(): CloudTargets {
  const devices = useCloudDevices();
  const [rows, setRows] = useState<WireProfileInfo[]>([]);
  const [loading, setLoading] = useState(true);

  // Signature of eligible devices: ids + online status
  const eligible = devices.filter((d) => !d.self && d.online !== false);
  const sig = eligible.map((d) => `${d.id}:${d.online}`).join(",");

  useEffect(() => {
    if (eligible.length === 0) {
      setRows([]);
      setLoading(false);
      return;
    }

    let cancelled = false;
    setLoading(true);

    const fetchProfiles = async () => {
      const profilesByDevice: Record<string, { name: string; color?: string }[]> = {};
      await Promise.all(
        eligible.map(async (d) => {
          try {
            const res = await fetch(`/api/cloud/devices/${d.id}/profiles`);
            if (res.ok) {
              const data = await res.json();
              if (Array.isArray(data)) {
                profilesByDevice[d.id] = data;
              }
            }
          } catch {
            // Device unreachable; contributes 0 rows
          }
        }),
      );
      if (!cancelled) {
        setRows(toCloudRows(eligible, profilesByDevice));
        setLoading(false);
      }
    };

    fetchProfiles();
    return () => {
      cancelled = true;
    };
  }, [sig]);

  if (loading) {
    return { status: "loading" };
  }
  return { status: "ready", rows };
}