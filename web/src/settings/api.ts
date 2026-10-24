// Typed fetchers for the Settings API endpoints (spec §8.2). Plain `fetch`
// underneath, so tests stub `globalThis.fetch`. Every non-2xx response
// rejects with `ApiError` (403 pin/tenant, 409 stale, 422 validation
// envelope, 400/500).

import type { ConfigErr, ConfigText, PutOk, Scope, SettingsMeta, ValidateRes } from "./types";

/** The parsed error body of a non-2xx response. */
export interface ApiErrorBody {
  error?: string;
  errors?: ConfigErr[];
}

export class ApiError extends Error {
  readonly status: number;
  readonly body: ApiErrorBody;

  constructor(status: number, body: ApiErrorBody) {
    super(body.error ?? `HTTP ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (!res.ok) {
    let body: ApiErrorBody = {};
    try {
      body = (await res.json()) as ApiErrorBody;
    } catch {
      // Non-JSON error body — keep the status and the HTTP error text.
    }
    throw new ApiError(res.status, body);
  }
  return (await res.json()) as T;
}

/** GET /api/settings (…?convo=<uuid> when given). */
export function getSettings(convo?: string): Promise<SettingsMeta> {
  return request<SettingsMeta>(
    convo ? `/api/settings?convo=${encodeURIComponent(convo)}` : "/api/settings",
  );
}

/** GET /api/config?scope=…&convo=… */
export function getConfig(scope: Scope, convo?: string): Promise<ConfigText> {
  const params = new URLSearchParams({ scope });
  if (convo) params.set("convo", convo);
  return request<ConfigText>(`/api/config?${params.toString()}`);
}

/** POST /api/config/validate { scope, text, convo? } → 200 ValidateRes | 422. */
export function validateConfig(scope: Scope, text: string, convo?: string): Promise<ValidateRes> {
  const body: { scope: Scope; text: string; convo?: string } = { scope, text };
  if (convo) body.convo = convo;
  return request<ValidateRes>("/api/config/validate", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** PUT /api/config { scope, text, base_sha256, convo? } → 200 PutOk | 409 | 422. */
export function putConfig(
  scope: Scope,
  text: string,
  baseSha256: string | null,
  convo?: string,
): Promise<PutOk> {
  const body: { scope: Scope; text: string; base_sha256: string | null; convo?: string } = {
    scope,
    text,
    base_sha256: baseSha256,
  };
  if (convo) body.convo = convo;
  return request<PutOk>("/api/config", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}
