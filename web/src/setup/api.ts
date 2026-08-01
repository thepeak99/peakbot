export type ApiError = { error: string; problems?: string[] };

export type PathState =
  | { status: "on_path" }
  | { status: "shadowed"; by: string }
  | { status: "absent"; hint: string };

export type InstallInfo = {
  target: string;
  state: "current" | "absent" | "other";
  path: PathState;
};

export type ExistingConfig =
  | { status: "absent" }
  | { status: "ok"; config: unknown }
  | { status: "error"; message: string };

export type SetupInfo = {
  os: string;
  arch: string;
  exe_path: string | null;
  config_path: string;
  data_dir: string | null;
  cache_dir: string | null;
  skills_dir: string | null;
  lan_bind_hint: string;
  needs_setup: boolean;
  builtin_tools: string[];
  install: InstallInfo;
  existing: ExistingConfig;
};

export type WriteResponse = { path: string; backup: string | null; restart_required: true };
export type InstallResponse = {
  source: string;
  target: string;
  action: "already_current" | "installed" | "replaced";
  path: PathState;
  notes: string[];
};
export type ServiceReport = {
  manager: "systemd-user" | "launchd-agent" | "windows-task";
  name: string;
  artifact: string | null;
  installed: boolean;
  exe: string | null;
  run_state: "running" | "stopped" | "unknown";
  survives_logout: boolean;
  commands: string[];
  notes: string[];
};
export type ServiceRequest = { bind?: string; token?: string };

class SetupApiError extends Error {
  constructor(public readonly status: number, public readonly body: ApiError) {
    super(body.error);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init);
  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = { error: `Request failed (${response.status})` };
  }
  if (!response.ok) {
    const error = body && typeof body === "object" && "error" in body
      ? body as ApiError
      : { error: `Request failed (${response.status})` };
    throw new SetupApiError(response.status, error);
  }
  return body as T;
}

const json = (method: string, body?: unknown): RequestInit => ({
  method,
  headers: { "Content-Type": "application/json" },
  ...(body === undefined ? {} : { body: JSON.stringify(body) }),
});

export function getSetupInfo(): Promise<SetupInfo> {
  return request<SetupInfo>("/api/setup");
}
export function writeConfig(yaml: string): Promise<WriteResponse> {
  return request<WriteResponse>("/api/setup/config", json("POST", { yaml }));
}
export function installBinary(): Promise<InstallResponse> {
  return request<InstallResponse>("/api/setup/install", json("POST", {}));
}
export function getService(): Promise<ServiceReport> {
  return request<ServiceReport>("/api/setup/service");
}
export function installService(requestBody: ServiceRequest): Promise<ServiceReport> {
  return request<ServiceReport>("/api/setup/service", json("POST", requestBody));
}
export function uninstallService(): Promise<ServiceReport> {
  return request<ServiceReport>("/api/setup/service", json("DELETE"));
}

export function apiErrorMessage(error: unknown): string[] {
  if (error instanceof SetupApiError) {
    return [error.body.error, ...(error.body.problems ?? [])];
  }
  return [error instanceof Error ? error.message : "Request failed"];
}
