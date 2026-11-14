// P4 — the cloud store (design §B).
//
// Module singleton. Owns ONE `EventSource('/api/cloud/events')`, opened
// lazily on first subscriber and closed when the last one leaves. Holds the
// last `StateBody` plus UI flags. `act(path, body?)` is the single POST
// helper (re-entrancy-guarded). Never mutates `snapshot` — only SSE does.
//
// SKELETON: nothing is implemented yet; every entry point throws, so the
// `store.test.ts` / component tests fail for the right reason (feature absent).

import type { StateBody } from "./types";

export type CloudStoreState = {
  snapshot: StateBody | null;
  available: boolean;
  stale: boolean;
  busy: boolean;
  error: string | null;
  dismissed: ReadonlySet<string>;
};

export type CloudStore = {
  subscribe(cb: () => void): () => void;
  getSnapshot(): CloudStoreState;
  act(path: string, body?: unknown): Promise<void>;
  /** Test-only: tear down the live EventSource and reset all state. */
  reset(): void;
};

const initialState: CloudStoreState = {
  snapshot: null,
  available: true,
  stale: false,
  busy: false,
  error: null,
  dismissed: new Set<string>(),
};

void initialState; // referenced by the real implementation; unused in the skeleton.

export const store: CloudStore = {
  subscribe() {
    throw new Error("not implemented");
  },
  getSnapshot(): CloudStoreState {
    throw new Error("not implemented");
  },
  act() {
    return Promise.reject(new Error("not implemented"));
  },
  reset() {
    throw new Error("not implemented");
  },
};
