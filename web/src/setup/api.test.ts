import { describe, it, expect, vi, afterEach } from "vitest";
import { apiErrorMessage, getSetupInfo, getService, installBinary, installService, uninstallService, writeConfig } from "./api";

const fetchMock = vi.fn();

afterEach(() => fetchMock.mockReset());

describe("setup api", () => {
  it("getSetupInfo returns the parsed JSON on 200", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ os: "linux" }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try { const info = await getSetupInfo(); expect(info.os).toBe("linux"); }
    finally { globalThis.fetch = original; }
  });

  it("writeConfig POSTs { yaml } as JSON", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ path: "p", backup: null, restart_required: true }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const res = await writeConfig("providers: []\n");
      expect(res.path).toBe("p");
      const [url, init] = fetchMock.mock.calls[0];
      expect(url).toBe("/api/setup/config");
      expect(init.method).toBe("POST");
      expect(init.headers["Content-Type"]).toBe("application/json");
      expect(JSON.parse(init.body)).toEqual({ yaml: "providers: []\n" });
    } finally { globalThis.fetch = original; }
  });

  it("installBinary POSTs {} and returns the action", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ source: "s", target: "t", action: "installed", path: { status: "on_path" }, notes: [] }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const res = await installBinary();
      expect(res.action).toBe("installed");
      expect(fetchMock.mock.calls[0][1].body).toBe("{}");
    } finally { globalThis.fetch = original; }
  });

  it("getService sends GET", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ manager: "systemd-user", name: "peakbot", artifact: null, installed: false, exe: null, run_state: "unknown", survives_logout: false, commands: [], notes: [] }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const res = await getService();
      expect(res.manager).toBe("systemd-user");
      expect(fetchMock.mock.calls[0][1]).toBeUndefined();
    } finally { globalThis.fetch = original; }
  });

  it("installService POSTs { bind, token }", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ manager: "systemd-user", name: "peakbot", artifact: "u", installed: true, exe: "e", run_state: "running", survives_logout: true, commands: ["x"], notes: [] }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      const res = await installService({ bind: "0.0.0.0:7823", token: "t" });
      expect(res.installed).toBe(true);
      expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ bind: "0.0.0.0:7823", token: "t" });
    } finally { globalThis.fetch = original; }
  });

  it("uninstallService sends DELETE", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ manager: "systemd-user", name: "peakbot", artifact: null, installed: false, exe: null, run_state: "unknown", survives_logout: false, commands: [], notes: [] }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      await uninstallService();
      expect(fetchMock.mock.calls[0][1].method).toBe("DELETE");
    } finally { globalThis.fetch = original; }
  });

  it("preserves 422 problems in the thrown error", async () => {
    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ error: "config is not valid", problems: ["alias 'foo' duplicates 'foo'", "default_model 'bar' is not declared"] }), { status: 422, headers: { "Content-Type": "application/json" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      let caught: unknown;
      try { await writeConfig("x"); } catch (e) { caught = e; }
      expect(caught).toBeInstanceOf(Error);
      const lines = apiErrorMessage(caught);
      expect(lines).toEqual(["config is not valid", "alias 'foo' duplicates 'foo'", "default_model 'bar' is not declared"]);
    } finally { globalThis.fetch = original; }
  });

  it("falls back to a clean error envelope when the body is not JSON", async () => {
    fetchMock.mockResolvedValueOnce(new Response("nope", { status: 500, headers: { "Content-Type": "text/plain" } }));
    const original = globalThis.fetch;
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    try {
      let caught: unknown;
      try { await getSetupInfo(); } catch (e) { caught = e; }
      const lines = apiErrorMessage(caught);
      expect(lines[0]).toMatch(/Request failed/);
    } finally { globalThis.fetch = original; }
  });
});
