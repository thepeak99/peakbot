/**
 * fixtures.ts — every canned value in the /setup wizard.
 *
 * The wizard is a clickable dummy: it reads nothing, writes nothing, and makes
 * no network calls. Anything that would come from the backend lives here and
 * nowhere else, so the PR that adds `/api/setup/*` deletes exactly this file
 * and swaps each export for a fetch (plan §8.1).
 */

/** Provider types the binary supports (agents.md "Multi-model with /model").
 *  `defaultBaseUrl` prefills the field; `needsApiKey` drives the API-key row. */
export const PROVIDER_TYPES = [
  {
    id: "openrouter" as const,
    label: "OpenRouter",
    defaultBaseUrl: "",
    needsApiKey: true,
  },
  {
    id: "openai" as const,
    label: "OpenAI",
    defaultBaseUrl: "https://api.openai.com/v1",
    needsApiKey: true,
  },
  {
    id: "anthropic" as const,
    label: "Anthropic",
    defaultBaseUrl: "https://api.anthropic.com",
    needsApiKey: true,
  },
  {
    id: "llamacpp" as const,
    label: "llama.cpp (local)",
    defaultBaseUrl: "http://127.0.0.1:8080",
    needsApiKey: false,
  },
  {
    id: "ollama" as const,
    label: "Ollama (local)",
    defaultBaseUrl: "http://127.0.0.1:11434",
    needsApiKey: false,
  },
];

export type ProviderType = (typeof PROVIDER_TYPES)[number]["id"];

/** Model presets offered per provider type — one click fills a models row. */
export const MODEL_PRESETS: Record<
  ProviderType,
  Array<{ name: string; alias: string; maxTokens: number }>
> = {
  openrouter: [
    {
      name: "anthropic/claude-sonnet-4.5",
      alias: "sonnet",
      maxTokens: 8192,
    },
    { name: "google/gemini-2.5-flash", alias: "flash", maxTokens: 8192 },
    { name: "openai/gpt-5", alias: "gpt5", maxTokens: 8192 },
  ],
  openai: [
    { name: "gpt-5", alias: "gpt5", maxTokens: 8192 },
    { name: "gpt-4o-mini", alias: "mini", maxTokens: 4096 },
  ],
  anthropic: [
    { name: "claude-sonnet-4-5", alias: "sonnet", maxTokens: 8192 },
    { name: "claude-haiku-4-5", alias: "haiku", maxTokens: 4096 },
  ],
  llamacpp: [{ name: "local-model", alias: "local", maxTokens: 4096 }],
  ollama: [
    { name: "qwen2.5-coder:14b", alias: "coder", maxTokens: 4096 },
    { name: "llama3.2:3b", alias: "llama", maxTokens: 4096 },
  ],
};

/** Built-in tool wire names — mirrors `BUILTIN_TOOL_NAMES` in
 *  `src/config/mod.rs`, which is what the `tools:` filter validates against. */
export const BUILTIN_TOOL_NAMES = [
  "bash",
  "bash_bg",
  "delegate",
  "doc_index",
  "doc_search",
  "fetch_page",
  "fetch_url",
  "file_create",
  "file_insert",
  "file_read",
  "file_str_replace",
  "list_directory",
  "pdf_read",
  "powershell",
  "think",
  "todo",
  "view_image",
  "web_search",
];

/** Persona presets (plan §8.4 step 5). The prompts are the copy a real
 *  `persona:` key would ship as built-in presets (plan §8.B.5). */
export const PERSONA_PRESETS = [
  {
    id: "crusader",
    name: "Code Crusader",
    blurb: "The shipped default: warm, explains itself, ends with a prayer.",
    prompt: `You are a coding agent that helps users with software engineering tasks. You operate in the user's local filesystem and can read, write, and execute code.

You are cool, fun, friendly, and interesting. You explain what you are about to do at every step and why (keep explanations short and clear).

You are also a CODE CRUSADER. You forge HOLY CODE — clean, precise, and purposeful. You smite errors, inefficiencies, and confusion with your teachings, your wisdom, and your code.

After completing each task, you offer a short prayer — brief, respectful, and in tone with your crusader spirit.`,
  },
  {
    id: "neutral",
    name: "Neutral engineer",
    blurb: "No character, no flourishes. Answers, diffs, and reasons.",
    prompt: `You are a coding agent working in the user's local filesystem. You can read, write, and execute code.

State what you are about to do in one line, do it, then report what changed. Prefer the smallest correct change. Say when you are unsure instead of guessing.`,
  },
  {
    id: "terse",
    name: "Terse reviewer",
    blurb: "Blunt, short sentences, no praise unless it's earned.",
    prompt: `You are a senior engineer reviewing and writing code. Be direct and brief.

No filler, no restating the question, no summaries of what you just did unless asked. Name the file and the line. If something is wrong, say so plainly and say what to do instead.`,
  },
];

/** Platform facts the real wizard would read from the running binary
 *  (`std::env::current_exe()`, `dirs::*`). Placeholders here. */
export const PLATFORM = {
  os: "Linux",
  arch: "x86_64",
  exePath: "/home/you/.local/bin/peakbot",
  configDir: "~/.config/peakbot",
  dataDir: "~/.local/share/peakbot",
  cacheDir: "~/.cache/peakbot",
  skillsDir: "~/.agents/skills",
  /** Offered as the LAN bind on the Access step. */
  lanBind: "0.0.0.0:7823",
  /** Shown by the "put the binary on my PATH" checkbox. */
  installCommand: "cp /tmp/peakbot-build/peakbot ~/.local/bin/peakbot",
};

/** The start-on-boot file the wizard *would* write, plus the command that
 *  enables it. Per-OS in the real thing; Linux/systemd here (plan §8.B.3). */
export const BOOT_SERVICE = {
  path: "~/.config/systemd/user/peakbot.service",
  enableCommand: "systemctl --user enable --now peakbot",
  content: `[Unit]
Description=PeakBot agent (web UI)
After=network-online.target

[Service]
ExecStart=%h/.local/bin/peakbot --web
Restart=on-failure
Environment=PEAKBOT_WEB_TOKEN=

[Install]
WantedBy=default.target
`,
};

/** Embedding models offered on the Services step, with the dimensions that
 *  must match the model *and* any existing DB at `db_path`. */
export const EMBEDDING_MODELS = [
  { model: "text-embedding-3-small", dimensions: 1536 },
  { model: "text-embedding-3-large", dimensions: 3072 },
  { model: "nomic-embed-text", dimensions: 768 },
];

/** Canned result of the fake "Test connection" click (plan §8.4 step 3). */
export const TEST_CONNECTION_RESULT = "Reachable · 214 models";

/** Canned result of the fake "Import" click (plan §8.4 step 1). */
export const IMPORT_RESULT = "Parsed 1 provider, 3 models";

/** What Import prefills the draft with. Deliberately a full provider so the
 *  Models step has something to show. */
export const IMPORTED_PROVIDER = {
  name: "openrouter",
  type: "openrouter" as const,
  apiKey: "sk-or-v1-imported-key",
  models: [
    { name: "anthropic/claude-sonnet-4.5", alias: "sonnet", maxTokens: 8192 },
    { name: "google/gemini-2.5-flash", alias: "flash", maxTokens: 8192 },
    { name: "openai/gpt-5", alias: "gpt5", maxTokens: 8192 },
  ],
  defaultModel: "sonnet",
};
