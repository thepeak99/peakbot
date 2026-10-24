// Provider types and their preset base URLs (spec §9.8), shared by the
// Providers pane and the first-run Provider step. A plain module so both
// component files keep exporting components only.

export const PROVIDER_TYPES = ["openrouter", "openai", "anthropic", "llamacpp", "ollama"] as const;
export type ProviderType = (typeof PROVIDER_TYPES)[number];

/** Preset base_url per provider type. openrouter is fixed. */
export const BASE_URL_PRESETS: Record<ProviderType, string> = {
  openrouter: "https://openrouter.ai/api/v1",
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com",
  ollama: "http://127.0.0.1:11434",
  llamacpp: "http://127.0.0.1:8080",
};
