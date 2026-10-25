// D7 "System" pane (spec §9.10, contract.ts "./system (T30)"): read-only
// locations, the PATH pill + "Install / Update binary", the "Start on boot"
// service card and the version. Talks to /api/system/* with plain `fetch`.
// "Check for updates" is deliberately not built (spec: omit in v1).

import { useCallback, useEffect, useState } from "react";
import { useSettingsCtx } from "./context";
import { Badge } from "./widgets";

type PathStatus =
  | { status: "on_path" }
  | { status: "shadowed"; by: string }
  | { status: "absent"; hint: string };

interface ServiceReport {
  manager: string;
  name: string;
  artifact: string | null;
  installed: boolean;
  exe: string | null;
  run_state: string;
  tls: boolean;
  survives_logout: boolean;
  commands: string[];
  notes: string[];
}

interface InstallReport {
  source: string;
  target: string;
  action: string;
  path: PathStatus;
  notes: string[];
}

/** facts.install is `unknown` in types.ts; read it defensively. */
function installInfo(raw: unknown): { target: string; path: PathStatus | null } | null {
  if (raw === null || typeof raw !== "object") return null;
  const o = raw as { target?: unknown; path?: unknown };
  if (typeof o.target !== "string") return null;
  return { target: o.target, path: pathStatus(o.path) };
}

function pathStatus(raw: unknown): PathStatus | null {
  if (raw === null || typeof raw !== "object") return null;
  const p = raw as { status?: unknown; by?: unknown; hint?: unknown };
  if (p.status === "on_path") return { status: "on_path" };
  if (p.status === "shadowed") return { status: "shadowed", by: String(p.by ?? "") };
  if (p.status === "absent") return { status: "absent", hint: String(p.hint ?? "") };
  return null;
}

async function call<T>(method: string, url: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method };
  if (body !== undefined) {
    init.headers = { "content-type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await fetch(url, init);
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = (await res.json()) as { error?: string };
      if (j.error) msg = j.error;
    } catch {
      // Non-JSON error body — keep the HTTP status text.
    }
    throw new Error(msg);
  }
  return (await res.json()) as T;
}

function PathPill({ path }: { path: PathStatus | null }) {
  if (path === null) return null;
  if (path.status === "on_path") {
    return (
      <Badge testid="system-path-pill" tone="green">
        on PATH
      </Badge>
    );
  }
  if (path.status === "shadowed") {
    return (
      <Badge testid="system-path-pill" tone="amber">
        shadowed by {path.by}
      </Badge>
    );
  }
  return (
    <Badge testid="system-path-pill" tone="amber">
      not on PATH{path.hint ? ` — ${path.hint}` : ""}
    </Badge>
  );
}

function Row({ testid, label, value }: { testid: string; label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-3 py-1 text-xs">
      <span className="w-28 shrink-0 text-zinc-500">{label}</span>
      <code data-testid={testid} className="min-w-0 break-all font-mono text-zinc-300">
        {value}
      </code>
    </div>
  );
}

const CARD = "max-w-2xl rounded-lg border border-zinc-800 bg-zinc-900/40 p-4";
const BTN =
  "cursor-pointer rounded border border-zinc-700 px-3 py-1 text-xs text-zinc-200 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60";

export function SystemPane() {
  const { meta } = useSettingsCtx();
  const facts = meta?.facts;
  const install = installInfo(facts?.install);

  const [service, setService] = useState<ServiceReport | null>(null);
  const [installRes, setInstallRes] = useState<InstallReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // One service/install call at a time; the report a call returns replaces
  // the shown state, a failure keeps it and shows the error.
  const run = useCallback(async <T,>(fn: () => Promise<T>, onOk: (r: T) => void) => {
    setBusy(true);
    setError(null);
    try {
      onOk(await fn());
    } catch (e) {
      setError(e instanceof Error ? e.message : "Request failed");
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    let live = true;
    call<ServiceReport>("GET", "/api/system/service")
      .then((r) => {
        if (live) setService(r);
      })
      .catch((e: unknown) => {
        if (live) setError(e instanceof Error ? e.message : "Request failed");
      });
    return () => {
      live = false;
    };
  }, []);

  const refresh = () =>
    run(() => call<ServiceReport>("GET", "/api/system/service"), setService);
  const enable = () =>
    run(() => call<ServiceReport>("POST", "/api/system/service", { op: "install" }), setService);
  const disable = () => run(() => call<ServiceReport>("DELETE", "/api/system/service"), setService);
  const installBinary = () =>
    run(() => call<InstallReport>("POST", "/api/system/install", {}), setInstallRes);

  return (
    <div data-testid="system-pane" className="space-y-4">
      <section className={CARD}>
        <h3 className="mb-2 text-xs font-semibold text-zinc-300">Locations</h3>
        <Row testid="system-path-config" label="Config" value={meta?.global_path ?? ""} />
        <Row testid="system-path-repo" label="Repo overlay" value={meta?.repo_path ?? ""} />
        <Row testid="system-path-data" label="Data" value={facts?.data_dir ?? ""} />
        <Row testid="system-path-binary" label="Binary" value={facts?.exe_path ?? ""} />
        {install && (
          <div className="flex flex-wrap items-baseline gap-3">
            <div className="min-w-0 flex-1">
              <Row testid="system-path-install" label="Install to" value={install.target} />
            </div>
            <PathPill path={install.path} />
          </div>
        )}
        <div className="mt-3 flex flex-wrap items-center gap-3">
          <button
            data-testid="system-install"
            onClick={installBinary}
            disabled={busy}
            className={BTN}
          >
            Install / Update binary
          </button>
          {installRes && (
            <span data-testid="system-install-result" className="text-xs text-zinc-400">
              {installRes.action.replace(/_/g, " ")} → <code>{installRes.target}</code>
              {installRes.notes.length > 0 && ` · ${installRes.notes.join(" · ")}`}
            </span>
          )}
        </div>
      </section>

      <section className={CARD}>
        <h3 className="mb-2 text-xs font-semibold text-zinc-300">Start on boot</h3>
        {service ? (
          <div className="space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <Badge testid="system-service-kind">{service.manager}</Badge>
              <Badge testid="system-service-installed" tone={service.installed ? "green" : "neutral"}>
                {service.installed ? "installed" : "not installed"}
              </Badge>
              <Badge testid="system-service-state" tone={service.run_state === "running" ? "green" : "neutral"}>
                {service.run_state}
              </Badge>
            </div>
            {service.notes.map((n, i) => (
              <p key={i} className="text-xs text-zinc-500">
                {n}
              </p>
            ))}
          </div>
        ) : (
          <p className="text-xs text-zinc-500">Loading…</p>
        )}
        <div className="mt-3 flex flex-wrap gap-2">
          <button data-testid="system-service-enable" onClick={enable} disabled={busy} className={BTN}>
            Enable at login
          </button>
          <button data-testid="system-service-disable" onClick={disable} disabled={busy} className={BTN}>
            Disable
          </button>
          <button data-testid="system-service-refresh" onClick={refresh} disabled={busy} className={BTN}>
            Refresh
          </button>
        </div>
        {error !== null && (
          <p
            data-testid="system-error"
            className="mt-3 rounded border border-red-900/70 bg-red-950/30 px-3 py-2 text-xs text-red-300"
          >
            {error}
          </p>
        )}
      </section>

      <section className={CARD}>
        <h3 className="mb-2 text-xs font-semibold text-zinc-300">Version</h3>
        <code data-testid="system-version" className="font-mono text-xs text-zinc-300">
          PeakBot {facts?.version ?? ""}
        </code>
      </section>
    </div>
  );
}
