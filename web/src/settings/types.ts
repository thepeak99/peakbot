// Shared TS types for the Settings SPA — verbatim from the 0.24 config plan
// (M5 "shared TS types" block). These are the wire contract with the Settings
// API (spec §8.2) and are NOT to be re-shaped by the M5 builders.

export type Scope = "global" | "repo";
export type Seg = string | number;

export interface ConfigErr {
  file: Scope;
  path: Seg[];
  message: string;
}

export interface SettingsMeta {
  needs_setup: boolean;
  legacy_layout: boolean;
  migrate_error: string | null;
  settings_available: boolean;
  global_path: string;
  repo_path: string;
  repo_exists: boolean;
  builtin_tools: string[];
  tab_names: string[];
  skills: string[];
  builtin_persona: string;
  mcp_status: { name: string; state: "connected" | "failed" | "disabled" }[];
  boot: { bind: string; token_source: "config" | "env" | "cli" | "none"; tls: boolean };
  facts: {
    os: string;
    arch: string;
    exe_path: string;
    data_dir: string;
    version: string;
    install: unknown;
  };
}

export interface ConfigText {
  scope: Scope;
  path: string;
  exists: boolean;
  text: string;
  sha256: string | null;
}

export type ValidateRes =
  | { ok: true; restart_required: string[]; warnings: string[] }
  | { error: string; errors: ConfigErr[] };

export interface PutOk {
  path: string;
  backup: string | null;
  sha256: string;
  restart_required: string[];
  applies_on_next_new: true;
}
