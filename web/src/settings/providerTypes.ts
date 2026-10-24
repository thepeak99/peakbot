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

/**
 * `+ Add model` preset chips per type (mockup MODEL_PRESETS), as
 * [name, alias, max_tokens]. The SINGLE source: the Providers pane's chips
 * and the first-run Provider step both render from here.
 */
export const MODEL_PRESETS: Record<ProviderType, [string, string, number][]> = {
  openrouter: [
    ["anthropic/claude-sonnet-4.5", "sonnet", 8192],
    ["google/gemini-2.5-flash", "flash", 8192],
    ["openai/gpt-5", "gpt5", 8192],
  ],
  openai: [
    ["gpt-5", "gpt5", 8192],
    ["gpt-4o-mini", "mini", 4096],
  ],
  anthropic: [
    ["claude-sonnet-4-5", "sonnet", 8192],
    ["claude-haiku-4-5", "haiku", 4096],
  ],
  llamacpp: [["local-model", "local", 4096]],
  ollama: [
    ["qwen2.5-coder:14b", "coder", 4096],
    ["llama3.2:3b", "llama", 4096],
  ],
};
