// P4 — the cloud store (design §B).
//
// Module singleton. Owns ONE `EventSource('/api/cloud/events')`, opened
// lazily on first subscriber and closed when the last one leaves. Holds the
// last `StateBody` plus UI flags. `act(path, body?)` is the single POST
// helper (re-entrancy-guarded). Never mutates `snapshot` — only SSE does.

import { parseStateBody, type CloudError, type StateBody } from "./types";

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
  /** Hide a request's approver modal (Escape / Later). Sends nothing. */
  dismiss(letterId: string): void;
  /** Test-only: tear down the live EventSource and reset all state. */
  reset(): void;
};

const initialState = (): CloudStoreState => ({
  snapshot: null,
  available: true,
  stale: false,
  busy: false,
  error: null,
  dismissed: new Set<string>(),
});

let state: CloudStoreState = initialState();
let es: EventSource | null = null;
const listeners = new Set<() => void>();
// Bumped by reset()/last-unsubscribe so a late GET /state can't open a stream.
let epoch = 0;

function set(patch: Partial<CloudStoreState>): void {
  state = { ...state, ...patch };
  for (const l of [...listeners]) l();
}

function closeStream(): void {
  epoch++;
  es?.close();
  es = null;
}

function openStream(): void {
  const src = new EventSource("/api/cloud/events");
  es = src;
  src.onopen = () => set({ stale: false });
  src.onerror = () => set({ stale: true });
  src.addEventListener("state", (ev) => {
    let parsed: StateBody | null = null;
    try {
      parsed = parseStateBody(JSON.parse((ev as MessageEvent).data as string));
    } catch {
      /* malformed → ignored */
    }
    if (parsed) set({ snapshot: parsed, stale: false });
  });
}

/** Detect `cloud_disabled` (404) once, then open the single stream. */
function start(): void {
  const mine = epoch;
  const onResponse = (res: Response): void => {
    if (mine !== epoch) return;
    if (res.status === 404) {
      set({ available: false });
      return;
    }
    set({ available: true });
    openStream();
  };
  const onFailure = (): void => {
    // Network hiccup: open the stream anyway; EventSource retries natively.
    if (mine === epoch) openStream();
  };
  const probe: Response | Promise<Response> = fetch("/api/cloud/state");
  // Real fetch always returns a Promise; a synchronous Response (test
  // doubles) is handled in the same tick.
  if (probe instanceof Promise) probe.then(onResponse, onFailure);
  else onResponse(probe);
}

export const store: CloudStore = {
  subscribe(cb) {
    listeners.add(cb);
    if (listeners.size === 1) start();
    return () => {
      if (!listeners.delete(cb)) return;
      if (listeners.size === 0) closeStream();
    };
  },
  getSnapshot: () => state,
  async act(path, body) {
    if (state.busy) return;
    set({ busy: true, error: null });
    try {
      const res = await fetch(path, {
        method: "POST",
        headers: body === undefined ? undefined : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (!res.ok) {
        let message = `HTTP ${res.status}`;
        try {
          const j = (await res.json()) as Partial<CloudError>;
          if (j.message) message = j.message;
        } catch {
          /* non-JSON error body */
        }
        set({ error: message });
        throw new Error(message);
      }
    } catch (e) {
      if (state.error === null) set({ error: e instanceof Error ? e.message : String(e) });
      throw e;
    } finally {
      set({ busy: false });
    }
  },
  dismiss(letterId) {
    set({ dismissed: new Set(state.dismissed).add(letterId) });
  },
  reset() {
    closeStream();
    listeners.clear();
    state = initialState();
  },
};
