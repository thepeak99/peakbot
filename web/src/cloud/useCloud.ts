// P4 — cloud hooks (design §B).
//
// `useCloud()` is the one hook every consumer uses (no component touches
// `EventSource` directly). `useCloudDevices()` is the `active.devices`
// selector. Both are thin wrappers over the module store; the real work is in
// `store.ts`. With the store a skeleton, these throw at render time, so the
// component tests are RED for the right reason.

import { useSyncExternalStore } from "react";
import { store, type CloudStoreState } from "./store";
import type { Device } from "./types";

export type CloudView = CloudStoreState;

/** The cloud snapshot + UI flags, subscribed through the shared store. */
export function useCloud(): CloudView {
  return useSyncExternalStore(
    store.subscribe,
    store.getSnapshot,
    store.getSnapshot,
  );
}

// One shared empty list: a fresh `[]` per render would hand consumers a new
// identity every time and re-fire any effect/memo keyed on it.
const NO_DEVICES: readonly Device[] = Object.freeze([]);

/** `active.devices`, else `[]` (design §F.2 — the machine-name lookup list).
 *  Referentially stable: same snapshot → same array. */
export function useCloudDevices(): readonly Device[] {
  const view = useCloud();
  if (view.snapshot && view.snapshot.state === "active") {
    return view.snapshot.devices;
  }
  return NO_DEVICES;
}
