// P5 — cloud target rows (design §2).
//
// Turns the live cloud device list into catalog rows the chip can render.
// Pure function; the hook wires it to the cloud store.

import { useEffect, useMemo, useRef, useState } from "react";
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
        machine: d.slug,
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

/** Hook: derive cloud rows from the live cloud store, retrying failed peers. */
export function useCloudTargets(): CloudTargets {
  const devices = useCloudDevices();
  const [rows, setRows] = useState<WireProfileInfo[]>(NO_ROWS);
  const [loading, setLoading] = useState(true);

  // Content key of eligible peers
  const peersKey = JSON.stringify(
    devices
      .filter((d) => !d.self && d.online !== false)
      .map((d): Peer => ({ id: d.id, slug: d.slug, self: d.self, online: d.online })),
  );

  // Retry state stored in refs so it persists across effect re-runs
  const failedPeers = useRef<Set<string>>(new Set());
  const backoffs = useRef<Map<string, number>>(new Map());
  const timers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const profilesRef = useRef<Record<string, Profile[]>>({});

  useEffect(() => {
    const peers = JSON.parse(peersKey) as Peer[];
    const ctrl = new AbortController();
    let active = true;

    failedPeers.current.clear();
    backoffs.current.clear();
    for (const t of timers.current.values()) clearTimeout(t);
    timers.current.clear();
    profilesRef.current = {};

    const commit = () => {
      if (!active || ctrl.signal.aborted) return;
      setLoading(false);
      setRows((prev) => {
        const next = toCloudRows(peers, profilesRef.current);
        return sameRows(prev, next) ? prev : next;
      });
    };

    const fetchOne = async (p: Peer) => {
      try {
        const res = await fetch(`/api/cloud/devices/${p.id}/profiles`, { signal: ctrl.signal });
        if (!res.ok) throw new Error("not ok");
        profilesRef.current[p.id] = parseProfiles(await res.json());
        failedPeers.current.delete(p.id);
        const t = timers.current.get(p.id);
        if (t) { clearTimeout(t); timers.current.delete(p.id); }
      } catch {
        if (ctrl.signal.aborted) return;
        failedPeers.current.add(p.id);
        const delay = backoffs.current.get(p.id) ?? 2000;
        const t = setTimeout(() => {
          if (failedPeers.current.has(p.id)) void fetchOne(p);
        }, delay);
        timers.current.set(p.id, t);
        backoffs.current.set(p.id, Math.min(delay * 2, 30000));
        return;
      }
      commit();
    };

    if (peers.length === 0) {
      setLoading(false);
      setRows(NO_ROWS);
      return;
    }

    setLoading(true);
    for (const p of peers) void fetchOne(p);

    const onVis = () => {
      if (document.visibilityState !== "visible") return;
      for (const id of failedPeers.current) {
        const t = timers.current.get(id);
        if (t) { clearTimeout(t); timers.current.delete(id); }
        backoffs.current.delete(id);
        const peer = peers.find((p) => p.id === id);
        if (peer) void fetchOne(peer);
      }
    };
    document.addEventListener("visibilitychange", onVis);

    return () => {
      active = false;
      ctrl.abort();
      document.removeEventListener("visibilitychange", onVis);
      for (const t of timers.current.values()) clearTimeout(t);
      timers.current.clear();
    };
  }, [peersKey]);

  return useMemo<CloudTargets>(
    () => (loading ? { status: "loading" } : { status: "ready", rows }),
    [loading, rows],
  );
}
