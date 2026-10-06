// File-type presentation helpers for attachment chips (design §5.7).
// Images never get a group — they render as thumbnails.

export type FileGroup =
  | "pdf"
  | "doc"
  | "sheet"
  | "code"
  | "archive"
  | "audio"
  | "video"
  | "other";

const EXT_GROUPS: Record<Exclude<FileGroup, "audio" | "video" | "other">, string[]> = {
  pdf: ["pdf"],
  doc: ["txt", "md", "markdown", "rst", "doc", "docx", "odt", "rtf", "tex", "log"],
  sheet: ["csv", "tsv", "xls", "xlsx", "ods"],
  code: [
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "hpp",
    "cs", "rb", "php", "swift", "kt", "sh", "bash", "zsh", "ps1", "sql", "json",
    "yaml", "yml", "toml", "xml", "html", "css", "scss", "lua", "r", "ipynb",
  ],
  archive: ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst"],
};

const GROUP_BY_EXT = new Map<string, FileGroup>(
  Object.entries(EXT_GROUPS).flatMap(([group, exts]) =>
    exts.map((e): [string, FileGroup] => [e, group as FileGroup]),
  ),
);

// Literal class strings (not built from the colour name) so Tailwind's
// scanner sees them.
export const GROUP_STYLE: Record<FileGroup, { color: string }> = {
  pdf: { color: "text-red-400" },
  doc: { color: "text-sky-400" },
  sheet: { color: "text-emerald-400" },
  code: { color: "text-violet-400" },
  archive: { color: "text-amber-400" },
  audio: { color: "text-pink-400" },
  video: { color: "text-orange-400" },
  other: { color: "text-zinc-400" },
};

// A leading dot (".bashrc") or trailing dot ("a.") is not an extension.
function ext(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot + 1) : "";
}

/** Extension first, then mime prefix (audio/video); everything else is "other". */
export function fileGroup(name: string, mime: string): FileGroup {
  const byExt = GROUP_BY_EXT.get(ext(name).toLowerCase());
  if (byExt) return byExt;
  if (mime.startsWith("audio/")) return "audio";
  if (mime.startsWith("video/")) return "video";
  return "other";
}

/** Uppercased last extension, capped at 4 chars; "" when there is none. */
export function extLabel(name: string): string {
  return ext(name).slice(0, 4).toUpperCase();
}

/** Shorten the middle of a long name, keeping the extension visible. */
export function middleTruncate(name: string, max = 24): string {
  if (name.length <= max) return name;
  const budget = max - 1; // one char for the ellipsis
  const e = ext(name);
  // An extension that would eat most of the budget is not worth preserving.
  const extLen = e && e.length + 1 <= budget / 2 ? e.length + 1 : 0;
  // Keep a few stem chars before the extension so "…name.pdf" stays readable.
  const tail = Math.min(extLen ? extLen + 4 : Math.floor(budget / 2), budget - 1);
  const head = Math.max(1, budget - tail);
  return `${name.slice(0, head)}…${name.slice(name.length - (budget - head))}`;
}

/** Mirrors Rust `attachments::human_size`: base 1024, one decimal. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

const THUMBNAILABLE = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

/** Formats every browser renders in an <img>; SVG is excluded on purpose. */
export function isThumbnailable(mime: string): boolean {
  return THUMBNAILABLE.has(mime);
}
