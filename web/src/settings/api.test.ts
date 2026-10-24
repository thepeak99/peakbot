// RED (T20 D1a): ./api — typed fetchers for the §8.2 endpoints.
//
// Assumed API (see contract.ts):
//   getSettings(convo?): Promise<SettingsMeta>          GET /api/settings[?convo=]
//   getConfig(scope, convo?): Promise<ConfigText>       GET /api/config?scope=…&convo=…
//   validateConfig(scope, text, convo?): Promise<ValidateRes>
//                                                       POST /api/config/validate {scope,text,convo?}
//   putConfig(scope, text, baseSha256, convo?): Promise<PutOk>
//                                                       PUT /api/config {scope,text,base_sha256,convo?}
//   class ApiError extends Error { status: number; body: {error?, errors?} }
//   — every non-2xx rejects with ApiError (403 pin, 409 stale, 422 envelope).
//
// fetch is stubbed (no network). Expected RED: `./api` does not exist yet.

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  getConfig,
  getSettings,
  putConfig,
  validateConfig,
} from "./api";
import type { SettingsMeta } from "./types";

const fetchMock = vi.fn();

afterEach(() => {
  fetchMock.mockReset();
});

function installFetch() {
  vi.stubGlobal("fetch", fetchMock);
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const META: SettingsMeta = {
  needs_setup: false,
  legacy_layout: false,
  migrate_error: null,
  settings_available: true,
  global_path: "/home/u/.config/peakbot/config.yaml",
  repo_path: "/home/u/workz/peakbot/.peakbot/config.yaml",
  repo_exists: true,
  builtin_tools: ["bash", "web_search"],
  tab_names: ["session", "todo", "files", "tasks", "bash", "agents", "settings"],
  skills: ["github"],
  builtin_persona: "You are Code Crusader.",
  mcp_status: [
    { name: "playwright", state: "connected" },
    { name: "linear", state: "failed" },
  ],
  boot: { bind: "127.0.0.1:7823", token_source: "config", tls: false },
  facts: {
    os: "linux",
    arch: "x86_64",
    exe_path: "/home/u/.local/bin/peakbot",
    data_dir: "/home/u/.local/share/peakbot",
    version: "0.24.0",
    install: null,
  },
};

describe("api", () => {
  it("get_settings_calls_get_api_settings", async () => {
    installFetch();
    fetchMock.mockResolvedValue(jsonResponse(200, META));

    const meta = await getSettings("convo-1");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/settings?convo=convo-1");
    expect(init?.method).toBeUndefined(); // plain GET
    expect(meta).toEqual(META);

    // Without a conversation: bare /api/settings (server uses boot cwd).
    fetchMock.mockResolvedValue(jsonResponse(200, META));
    await getSettings();
    const [url2] = fetchMock.mock.calls[1] as [string];
    expect(url2).toBe("/api/settings");
  });

  it("get_config_calls_get_api_config_with_scope_and_convo", async () => {
    installFetch();
    const body = {
      scope: "repo",
      path: "/home/u/workz/peakbot/.peakbot/config.yaml",
      exists: false,
      text: "",
      sha256: null,
    };
    fetchMock.mockResolvedValue(jsonResponse(200, body));

    const res = await getConfig("repo", "convo-2");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url] = fetchMock.mock.calls[0] as [string];
    expect(url).toBe("/api/config?scope=repo&convo=convo-2");
    expect(res).toEqual(body);

    // Global scope, no convo.
    fetchMock.mockResolvedValue(
      jsonResponse(200, {
        scope: "global",
        path: "/home/u/.config/peakbot/config.yaml",
        exists: true,
        text: "providers: []\n",
        sha256: "abc123",
      }),
    );
    await getConfig("global");
    const [url2] = fetchMock.mock.calls[1] as [string];
    expect(url2).toBe("/api/config?scope=global");
  });

  it("validate_calls_post_api_config_validate", async () => {
    installFetch();
    fetchMock.mockResolvedValue(
      jsonResponse(200, { ok: true, restart_required: ["general.web"], warnings: [] }),
    );

    const res = await validateConfig("global", "providers: []\n", "convo-3");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/config/validate");
    expect(init.method).toBe("POST");
    expect(new Headers(init.headers).get("content-type")).toContain("application/json");
    expect(JSON.parse(String(init.body))).toEqual({
      scope: "global",
      text: "providers: []\n",
      convo: "convo-3",
    });
    expect(res).toEqual({ ok: true, restart_required: ["general.web"], warnings: [] });

    // Repo scope without a convo: convo key absent from the body.
    fetchMock.mockResolvedValue(jsonResponse(200, { ok: true, restart_required: [], warnings: [] }));
    await validateConfig("repo", "profiles: {}\n");
    const [, init2] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(JSON.parse(String(init2.body))).toEqual({ scope: "repo", text: "profiles: {}\n" });
  });

  it("validate_422_rejects_with_api_error_envelope", async () => {
    installFetch();
    fetchMock.mockResolvedValue(
      jsonResponse(422, {
        error: "config is not valid",
        errors: [
          {
            file: "global",
            path: ["profiles", "dev", "agents", "team", "model"],
            message: "Unknown model alias gpt5 — declared: sonnet, opus",
          },
        ],
      }),
    );

    const p = validateConfig("global", "bad: yaml: [");
    await expect(p).rejects.toBeInstanceOf(ApiError);
    await expect(p).rejects.toMatchObject({
      status: 422,
      body: {
        error: "config is not valid",
        errors: [
          {
            file: "global",
            path: ["profiles", "dev", "agents", "team", "model"],
            message: "Unknown model alias gpt5 — declared: sonnet, opus",
          },
        ],
      },
    });
  });

  it("put_calls_put_api_config_with_base_sha256", async () => {
    installFetch();
    fetchMock.mockResolvedValue(
      jsonResponse(200, {
        path: "/home/u/.config/peakbot/config.yaml",
        backup: null,
        sha256: "newsha",
        restart_required: ["general.web"],
        applies_on_next_new: true,
      }),
    );

    const res = await putConfig("global", "providers: []\n", "basesha", "convo-4");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/config");
    expect(init.method).toBe("PUT");
    expect(JSON.parse(String(init.body))).toEqual({
      scope: "global",
      text: "providers: []\n",
      base_sha256: "basesha",
      convo: "convo-4",
    });
    expect(res).toEqual({
      path: "/home/u/.config/peakbot/config.yaml",
      backup: null,
      sha256: "newsha",
      restart_required: ["general.web"],
      applies_on_next_new: true,
    });

    // No saved file yet (repo scope, first save): base_sha256 is null.
    fetchMock.mockResolvedValue(
      jsonResponse(200, {
        path: "/w/r/.peakbot/config.yaml",
        backup: null,
        sha256: "r1",
        restart_required: [],
        applies_on_next_new: true,
      }),
    );
    await putConfig("repo", "profiles: {}\n", null);
    const [, init2] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(JSON.parse(String(init2.body))).toEqual({
      scope: "repo",
      text: "profiles: {}\n",
      base_sha256: null,
    });
  });

  it("put_409_rejects_with_stale_api_error", async () => {
    installFetch();
    fetchMock.mockResolvedValue(
      jsonResponse(409, {
        error:
          "/home/u/.config/peakbot/config.yaml changed on disk since you opened Settings — reload to see the new version.",
      }),
    );

    const p = putConfig("global", "providers: []\n", "stale-sha");
    await expect(p).rejects.toBeInstanceOf(ApiError);
    await expect(p).rejects.toMatchObject({ status: 409 });
    const err = (await p.catch((e: unknown) => e)) as ApiError;
    expect(err.body?.error).toContain("changed on disk");
  });

  it("non_2xx_rejects_with_api_error", async () => {
    installFetch();
    fetchMock.mockResolvedValue(
      jsonResponse(403, {
        error: "Settings are disabled while PeakBot runs with --profile dev.",
      }),
    );

    const p = getSettings();
    await expect(p).rejects.toBeInstanceOf(ApiError);
    await expect(p).rejects.toMatchObject({
      status: 403,
      body: { error: "Settings are disabled while PeakBot runs with --profile dev." },
    });
  });
});
