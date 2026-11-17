// P5 — cloud target rows (design §2).
//
// Turns the live cloud device list into catalog rows the chip can render.
// Pure function; the hook wires it to the cloud store.

import type { WireProfileInfo } from "../state";
import type { Device } from "./types";

export type CloudTargets =
  | { status: "loading" }
  | { status: "ready"; rows: WireProfileInfo[] };

/** Pure: devices + per-device profiles → catalog rows (design §3, §5). */
export function toCloudRows(
  devices: Device[],
  profilesByDevice: Record<string, { name: string; color?: string }[]>,
): WireProfileInfo[] {
  void devices;
  void profilesByDevice;
  throw new Error("not implemented");
}

/** Hook: derive cloud rows from the live cloud store. */
export function useCloudTargets(): CloudTargets {
  throw new Error("not implemented");
}