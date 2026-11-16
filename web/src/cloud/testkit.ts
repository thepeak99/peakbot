// P4 — cloud test harness (design §G).
//
// `@testing-library/*` is NOT a dependency: component tests copy the existing
// `createRoot` + `act` + `querySelector` style. This file provides the shared
// fakes every cloud test installs in `beforeEach`:
//  • `FakeEventSource` (installed on `globalThis.EventSource`) with
//    `emit(type, data)`, `fail()`, `open()` and a static `instances` list;
//  • `mockFetch(handler)` — a `vi.fn` `fetch`;
//  • `snapshot(...)` / per-state builders.
//
// This is test infrastructure, not app behaviour.

import { vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { ReactNode } from "react";
import type { CloudState, StateBody } from "./types";

export type { CloudState, StateBody, Device, EnrollRequest } from "./types";

/** Copy the repo's `createRoot` + `act` harness (no testing-library).
 *  Returns the mounted container for `querySelector` assertions. */
export async function render(element: ReactNode): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.appendChild(container);
  let root: Root | undefined;
  await act(async () => {
    root = createRoot(container);
    root.render(element);
  });
  return container;
}

/** Unmount a container produced by `render`. */
export async function unmount(container: HTMLDivElement): Promise<void> {
  await act(async () => {
    // `createRoot` has no unmount on the node; the container's root is torn
    // down by React when the container leaves the DOM.
    container.remove();
  });
}

/** The one SSE message shape the store consumes. */
export type FakeMessage = { data: string; type: string };

export class FakeEventSource {
  static instances: FakeEventSource[] = [];

  static last(): FakeEventSource | undefined {
    return this.instances[this.instances.length - 1];
  }

  static reset(): void {
    this.instances = [];
  }

  readonly url: string;
  closed = false;
  onopen: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  onmessage: ((ev: FakeMessage) => void) | null = null;
  private listeners = new Map<string, ((ev: FakeMessage) => void)[]>();

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, h: (ev: FakeMessage) => void): void {
    const arr = this.listeners.get(type) ?? [];
    arr.push(h);
    this.listeners.set(type, arr);
  }

  removeEventListener(_type: string, _h: (ev: FakeMessage) => void): void {
    void _type;
    void _h;
  }

  close(): void {
    this.closed = true;
  }

  /** Fire the `open` handler (a (re)connect). */
  open(): void {
    this.onopen?.(new Event("open"));
  }

  /** Fire the `error` handler (the stream dropped). */
  fail(): void {
    this.onerror?.(new Event("error"));
  }

  /** Deliver one SSE event. `data` is JSON-stringified unless a string. */
  emit(type: string, data: unknown): void {
    const payload = typeof data === "string" ? data : JSON.stringify(data);
    const ev: FakeMessage = { type, data: payload };
    if (type === "message") this.onmessage?.(ev);
    for (const h of this.listeners.get(type) ?? []) h(ev);
  }
}

let originalEventSource: unknown;

/** Install `FakeEventSource` as the global `EventSource`. Call in `beforeEach`. */
export function installFakeEventSource(): void {
  if (originalEventSource === undefined) {
    originalEventSource = (globalThis as { EventSource?: unknown }).EventSource;
  }
  (globalThis as { EventSource?: unknown }).EventSource = FakeEventSource;
}

export function uninstallFakeEventSource(): void {
  if (originalEventSource !== undefined) {
    (globalThis as { EventSource?: unknown }).EventSource = originalEventSource;
  }
}

type FetchHandler = (url: string, init?: RequestInit) => Response | Promise<Response>;

/** Replace `globalThis.fetch` with a `vi.fn` that delegates to `handler`. */
export function mockFetch(handler: FetchHandler): ReturnType<typeof vi.fn> {
  const fn = vi.fn(handler) as unknown as (url: string, init?: RequestInit) => Response | Promise<Response>;
  (globalThis as { fetch?: unknown }).fetch = fn;
  return fn as ReturnType<typeof vi.fn>;
}

/** A JSON response with the given status. */
export function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** A full `StateBody` from a `CloudState` (the one extra field defaults null). */
export function snapshot(state: CloudState): StateBody {
  return { ...state, last_login_error: null };
}

export const sas = (words: string[] = ["apple", "river", "ocean", "tiger", "maple", "spark"]): string[] =>
  words;

// Per-state builders (design §A.1 rows).
export const disabled = (): StateBody => snapshot({ state: "disabled" });

export const loggedOut = (
  reason: "never" | "expired" | "signed_out" = "never",
): StateBody => snapshot({ state: "logged_out", reason });

export const connecting = (attempt = 2, last_error: string | null = null): StateBody =>
  snapshot({ state: "connecting", attempt, last_error });

export const pendingEmpty = (): StateBody =>
  snapshot({ state: "pending", empty_account: true, sas: null, rejected: false });

export const pendingWithSas = (words = sas()): StateBody =>
  snapshot({ state: "pending", empty_account: false, sas: words, rejected: false });

export const pendingRequesting = (): StateBody =>
  snapshot({ state: "pending", empty_account: false, sas: null, rejected: false });

export const pendingRejected = (): StateBody =>
  snapshot({ state: "pending", empty_account: false, sas: null, rejected: true });

export type TestDevice = {
  id: string;
  name: string;
  platform: string;
  online: boolean | null;
  slug: string;
  self: boolean;
};

export const device = (over: Partial<TestDevice> = {}): TestDevice => ({
  id: "aa".repeat(32),
  name: "Laptop",
  platform: "linux",
  online: true,
  slug: "laptop",
  self: false,
  ...over,
});

export const selfDevice = (over: Partial<TestDevice> = {}): TestDevice =>
  device({ self: true, ...over });

export const active = (
  devices: TestDevice[] = [selfDevice(), device({ name: "Desk", slug: "desk", platform: "macos" })],
  requests: TestEnrollRequest[] = [],
): StateBody => snapshot({ state: "active", devices, requests });

export type TestEnrollRequest = {
  letter_id: string;
  device_id: string;
  name: string;
  platform: string;
  sas: string[];
  expires_at: number;
};

export const request = (over: Partial<TestEnrollRequest> = {}): TestEnrollRequest => ({
  letter_id: "bb".repeat(16),
  device_id: "cc".repeat(32),
  name: "Phone",
  platform: "ios",
  sas: sas(),
  expires_at: 9_999_999_999,
  ...over,
});

export const trust = (
  trust_error: "genesis_mismatch" | "fork" | "revoked" | "chain_invalid" | "seed_store_error" = "fork",
): StateBody => snapshot({ state: "trust", trust_error });
