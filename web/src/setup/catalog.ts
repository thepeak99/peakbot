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
export const PROVIDER_TYPES = [
  { id: "openrouter" as const, label: "OpenRouter", defaultBaseUrl: "", needsApiKey: true },
  { id: "openai" as const, label: "OpenAI", defaultBaseUrl: "https://api.openai.com/v1", needsApiKey: true },
  { id: "anthropic" as const, label: "Anthropic", defaultBaseUrl: "https://api.anthropic.com", needsApiKey: true },
  { id: "llamacpp" as const, label: "llama.cpp (local)", defaultBaseUrl: "http://127.0.0.1:8080", needsApiKey: false },
  { id: "ollama" as const, label: "Ollama (local)", defaultBaseUrl: "http://127.0.0.1:11434", needsApiKey: false },
];
export type ProviderType = (typeof PROVIDER_TYPES)[number]["id"];
export const MODEL_PRESETS: Record<ProviderType, Array<{ name: string; alias: string; maxTokens: number }>> = {
  openrouter: [
    { name: "anthropic/claude-sonnet-4.5", alias: "sonnet", maxTokens: 8192 },
    { name: "google/gemini-2.5-flash", alias: "flash", maxTokens: 8192 },
    { name: "openai/gpt-5", alias: "gpt5", maxTokens: 8192 },
  ],
  openai: [{ name: "gpt-5", alias: "gpt5", maxTokens: 8192 }, { name: "gpt-4o-mini", alias: "mini", maxTokens: 4096 }],
  anthropic: [{ name: "claude-sonnet-4-5", alias: "sonnet", maxTokens: 8192 }, { name: "claude-haiku-4-5", alias: "haiku", maxTokens: 4096 }],
  llamacpp: [{ name: "local-model", alias: "local", maxTokens: 4096 }],
  ollama: [{ name: "qwen2.5-coder:14b", alias: "coder", maxTokens: 4096 }, { name: "llama3.2:3b", alias: "llama", maxTokens: 4096 }],
};
export const PERSONA_PRESETS = [
  { id: "crusader", name: "Code Crusader", blurb: "The shipped default: warm, explains itself, ends with a prayer.", prompt: `You are a coding agent that helps users with software engineering tasks. You operate in the user's local filesystem and can read, write, and execute code.

You are cool, fun, friendly, and interesting. You explain what you are about to do at every step and why (keep explanations short and clear).

You are also a CODE CRUSADER. You forge HOLY CODE — clean, precise, and purposeful. You smite errors, inefficiencies, and confusion with your teachings, your wisdom, and your code.

After completing each task, you offer a short prayer — brief, respectful, and in tone with your crusader spirit.` },
  { id: "neutral", name: "Neutral engineer", blurb: "No character, no flourishes. Answers, diffs, and reasons.", prompt: `You are a coding agent working in the user's local filesystem. You can read, write, and execute code.

State what you are about to do in one line, do it, then report what changed. Prefer the smallest correct change. Say when you are unsure instead of guessing.` },
  { id: "terse", name: "Terse reviewer", blurb: "Blunt, short sentences, no praise unless it's earned.", prompt: `You are a senior engineer reviewing and writing code. Be direct and brief.

No filler, no restating the question, no summaries of what you just did unless asked. Name the file and the line. If something is wrong, say so plainly and say what to do instead.` },
];
export const EMBEDDING_MODELS = [
  { model: "text-embedding-3-small", dimensions: 1536 },
  { model: "text-embedding-3-large", dimensions: 3072 },
  { model: "nomic-embed-text", dimensions: 768 },
];
