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
  /** The `error` code from the failing POST (design §D.5 mapping); null. */
  error_code: string | null;
  dismissed: ReadonlySet<string>;
};

export type CloudStore = {
  subscribe(cb: () => void): () => void;
  getSnapshot(): CloudStoreState;
  /** One POST; resolves with the parsed JSON body (null if non-JSON). */
  act(path: string, body?: unknown): Promise<unknown>;
  /** Hide a request's approver modal (Escape / Later). Sends nothing. */
  dismiss(letterId: string): void;
  /** Un-hide a dismissed request (the pane's Review button). Sends nothing. */
  undismiss(letterId: string): void;
  /** Test-only: tear down the live EventSource and reset all state. */
  reset(): void;
};

const initialState = (): CloudStoreState => ({
  snapshot: null,
  available: true,
  stale: false,
  busy: false,
  error: null,
  error_code: null,
  dismissed: new Set<string>(),
});

let state: CloudStoreState = initialState();
let es: EventSource | null = null;
const listeners = new Set<() => void>();
// Bumped by reset()/last-unsubscribe so a late GET /state can't open a stream.
let epoch = 0;
// The availability probe (GET /state, design §B) runs ONCE per app session:
// `cloud_disabled` is a property of the server, not of the stream. `reset()`
// (test-only) tears the stream down but keeps this knowledge.
let availabilityKnown = false;

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
  // jsdom (and other minimal envs) have no EventSource; the probe above
  // already told us the API is up, so the app works read-only there.
  if (typeof EventSource === "undefined") return;
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
  if (availabilityKnown) {
    if (state.available) openStream();
    return;
  }
  const mine = epoch;
  const onResponse = (res: Response): void => {
    if (mine !== epoch) return;
    availabilityKnown = true;
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
  let probe: Promise<Response>;
  try {
    probe = fetch("/api/cloud/state");
  } catch {
    return; // no fetch in this env (some test harnesses)
  }
  probe.then(onResponse, onFailure);
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
    set({ busy: true, error: null, error_code: null });
    try {
      const res = await fetch(path, {
        method: "POST",
        headers: body === undefined ? undefined : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      if (!res.ok) {
        let message = `HTTP ${res.status}`;
        let code: string | null = null;
        try {
          const j = (await res.json()) as Partial<CloudError>;
          if (j.message) message = j.message;
          if (typeof j.error === "string") code = j.error;
        } catch {
          /* non-JSON error body */
        }
        set({ error: message, error_code: code });
        throw new Error(message);
      }
      let data: unknown = null;
      try {
        data = await res.json();
      } catch {
        /* non-JSON success body */
      }
      return data;
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
  undismiss(letterId) {
    const next = new Set(state.dismissed);
    next.delete(letterId);
    set({ dismissed: next });
  },
  reset() {
    closeStream();
    listeners.clear();
    state = initialState();
  },
};
