import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import rehypeHighlight from "rehype-highlight";
import { memo, useState, type ComponentProps } from "react";
import type { AttachmentView, ChatMessage, MessageRole } from "../types";
import {
  GROUP_STYLE,
  extLabel,
  fileGroup,
  formatSize,
  isThumbnailable,
  middleTruncate,
} from "../fileTypes";
import { Lightbox } from "./Lightbox";

// Per-role visual treatment. Mirrors the TUI's role glyphs/colours so the
// web transcript reads the same way (src/ui/app_state.rs MessageRole +
// the REPL message renderer).
const ROLE_META: Record<
  MessageRole,
  { label: string; glyph: string; accent: string; bubble: string }
> = {
  user: {
    label: "You",
    glyph: "▷",
    accent: "text-sky-400",
    bubble: "bg-sky-950/40 border-sky-900/60",
  },
  agent: {
    label: "Shifu",
    glyph: "✦",
    accent: "text-emerald-400",
    bubble: "bg-zinc-900/60 border-zinc-800",
  },
  toolCall: {
    label: "Tool call",
    glyph: "🔧",
    accent: "text-amber-400",
    bubble: "bg-amber-950/20 border-amber-900/40",
  },
  toolResult: {
    label: "Tool result",
    glyph: "↳",
    accent: "text-zinc-400",
    bubble: "bg-zinc-900/40 border-zinc-800/70",
  },
  system: {
    label: "System",
    glyph: "•",
    accent: "text-violet-400",
    bubble: "bg-violet-950/20 border-violet-900/40",
  },
  summary: {
    label: "Summary",
    glyph: "≡",
    accent: "text-zinc-500",
    bubble: "bg-zinc-900/30 border-dashed border-zinc-800",
  },
};

function isMonospace(role: MessageRole): boolean {
  return role === "toolCall" || role === "toolResult";
}

// Agent replies, compaction summaries, system banners, and tool results are
// markdown text; user input and toolCall lines stay literal. remark-breaks
// makes single newlines render as line breaks, matching the TUI's SoftBreak
// handling — so multi-line banners like /stats and /context don't collapse
// onto one line. System banners carry deliberate markup (e.g. the backticked
// path in a `/cd` error is meant to render as inline code) and are our own
// strings, not user-controlled — safe because react-markdown renders no raw
// HTML by default (kept that way).
function isMarkdown(role: MessageRole): boolean {
  return (
    role === "agent" ||
    role === "summary" ||
    role === "toolResult" ||
    role === "system"
  );
}

// `sameMessage` is the value comparator for the memoised `Message` and is
// imported by `Message.test.ts` alongside the component itself, so it lives
// here for test ergonomics. No component tree imports it directly, so Fast
// Refresh's "non-component export in a component file" hazard is moot.
// eslint-disable-next-line react-refresh/only-export-components
export function sameMessage(a: ChatMessage, b: ChatMessage): boolean {
  return (
    a.role === b.role &&
    a.content === b.content &&
    a.timestamp === b.timestamp &&
    a.toolName === b.toolName &&
    a.fromBackground === b.fromBackground &&
    a.subAgentRole === b.subAgentRole &&
    sameThinking(a.thinking, b.thinking) &&
    sameAttachments(a.attachments, b.attachments)
  );
}

// Ids are uuids, so id equality is attachment equality (the rest is derived).
function sameAttachments(
  a: AttachmentView[] | undefined,
  b: AttachmentView[] | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i].id !== b[i].id) return false;
  return true;
}

// Array comparison for the optional `thinking` field. Length first (cheap
// short-circuit), then per-index string equality. Reference compare wouldn't
// work — `adaptMessage` allocates a fresh array per call at every render, so
// two equal transcripts would always allocate different objects.
function sameThinking(a: string[] | undefined, b: string[] | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

// Only /images/, http(s)://, and data: become <img>: a bare local path the
// backend left unresolved would hit the SPA catch-all route and render broken.
function isRenderableImageSrc(src: string | undefined): boolean {
  return (
    src !== undefined &&
    (src.startsWith("/images/") ||
      src.startsWith("http://") ||
      src.startsWith("https://") ||
      src.startsWith("data:"))
  );
}

// Markdown image renderer: a real <img> for renderable srcs, a muted text
// stub when the src isn't renderable or the load fails (cache file swept).
function CachedImage({ src, alt }: ComponentProps<"img"> & { node?: unknown }) {
  const [error, setError] = useState(false);
  const [lastSrc, setLastSrc] = useState(src);
  if (src !== lastSrc) {
    // Reset during render (not in an effect) so a new src never shows the
    // stale fallback for a frame.
    setLastSrc(src);
    setError(false);
  }
  if (error || !isRenderableImageSrc(src)) {
    return (
      <span className="text-zinc-500" title={src ?? ""}>
        🖼 {alt || "image"}
      </span>
    );
  }
  return (
    <a href={src} target="_blank" rel="noreferrer">
      <img
        src={src}
        alt={alt ?? ""}
        loading="lazy"
        onError={() => setError(true)}
        className="max-h-96 rounded border border-zinc-800/60"
      />
    </a>
  );
}

// Module scope: stable identity across renders — an inline literal would be
// a fresh reference every render and defeat memoization.
const MARKDOWN_COMPONENTS = { img: CachedImage } as const;

// Unexported renderer. Wrapped by `memo` below so that, with the value comparator
// (`sameMessage`), re-renders are skipped when the adapted message hasn't
// actually changed. Identity comparison alone wouldn't help: every websocket
// frame is a fresh `JSON.parse`, and `adaptMessage` allocates a new object per
// call at the render site, so the prop reference never survives a re-render —
// we need the structural comparison.
function MessageView({ message }: { message: ChatMessage }) {
  const meta = ROLE_META[message.role];
  return (
    <div className={`rounded-lg border px-3.5 py-2.5 ${meta.bubble}`}>
      <div className="mb-1 flex items-center gap-2 text-xs">
        <span className={meta.accent}>{meta.glyph}</span>
        <span className={`font-medium ${meta.accent}`}>{meta.label}</span>
        {message.toolName && (
          <span className="rounded bg-zinc-800 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
            {message.toolName}
          </span>
        )}
        {message.fromBackground && (
          <span className="rounded bg-cyan-950/60 px-1.5 py-0.5 text-[10px] text-cyan-300">
            🛰 bg
          </span>
        )}
        {message.subAgentRole && (
          <span className="rounded bg-cyan-950/60 px-1.5 py-0.5 text-[10px] text-cyan-300">
            🧩 {message.subAgentRole}
          </span>
        )}
        <span className="ml-auto tabular-nums text-zinc-600">{message.timestamp}</span>
      </div>
      {message.thinking && message.thinking.length > 0 && (
        <ThinkingBlocks blocks={message.thinking} />
      )}
      {message.attachments && <Attachments attachments={message.attachments} />}
      {isMarkdown(message.role) ? (
        <div className="markdown-body text-sm leading-relaxed text-zinc-200">
          <ReactMarkdown
            remarkPlugins={[remarkGfm, remarkBreaks]}
            rehypePlugins={[rehypeHighlight]}
            components={MARKDOWN_COMPONENTS}
          >
            {message.content}
          </ReactMarkdown>
        </div>
      ) : (
        <div
          className={`whitespace-pre-wrap break-words text-sm leading-relaxed text-zinc-200 ${
            isMonospace(message.role) ? "font-mono text-[13px] text-zinc-300" : ""
          }`}
        >
          {message.content}
        </div>
      )}
    </div>
  );
}

// Link chip for a non-image attachment (or an image the browser can't render).
function FileChip({ attachment }: { attachment: AttachmentView }) {
  const group = fileGroup(attachment.name, attachment.mime);
  return (
    <a
      href={attachment.url}
      target="_blank"
      rel="noopener"
      title={attachment.name}
      className="inline-flex max-w-full items-center gap-1.5 rounded-md border border-zinc-700 bg-zinc-900/70 px-2 py-1 text-xs text-zinc-200 hover:bg-zinc-800"
    >
      <span aria-hidden className={GROUP_STYLE[group].color}>
        📄
      </span>
      {extLabel(attachment.name) && (
        <span className={`text-[10px] font-semibold ${GROUP_STYLE[group].color}`}>
          {extLabel(attachment.name)}
        </span>
      )}
      <span className="truncate">
        {middleTruncate(attachment.name)} · {formatSize(attachment.size)}
      </span>
    </a>
  );
}

// 96px lazy thumbnail; a failed load degrades to the file chip so the
// attachment is never just a broken-image icon.
function Thumbnail({
  attachment,
  onOpen,
}: {
  attachment: AttachmentView;
  onOpen: () => void;
}) {
  const [failed, setFailed] = useState(false);
  if (failed) return <FileChip attachment={attachment} />;
  return (
    <img
      src={attachment.url}
      alt={attachment.name}
      loading="lazy"
      onClick={onOpen}
      onError={() => setFailed(true)}
      className="h-24 w-24 cursor-zoom-in rounded-md border border-zinc-700 object-cover"
    />
  );
}

// Right-aligned above the text, like a chat app's own-message media.
function Attachments({ attachments }: { attachments: AttachmentView[] }) {
  const [open, setOpen] = useState<number | null>(null);
  // The lightbox cycles only this message's images, never its files.
  const images = attachments.filter((a) => isThumbnailable(a.mime));
  return (
    <div className="mb-2 flex flex-wrap justify-end gap-2">
      {attachments.map((a) => {
        const at = images.indexOf(a);
        return at === -1 ? (
          <FileChip key={a.id} attachment={a} />
        ) : (
          <Thumbnail key={a.id} attachment={a} onOpen={() => setOpen(at)} />
        );
      })}
      {open !== null && (
        <Lightbox images={images} start={open} onClose={() => setOpen(null)} />
      )}
    </div>
  );
}

// Anthropic thinking blocks, rendered as a collapsible group above the
// assistant's prose. Native `<details>` is collapsed-by-default when the `open`
// attribute is absent — no JS state, no dependency. One `<details>` per
// `Message` (not per block): collapsing the group is the common case, and
// splitting it into N toggles just hides each block behind a click. Inside the
// group each block is a separate `whitespace-pre-wrap` paragraph so individual
// blocks stay distinct visually if the user expands.
//
// The contents are rendering as raw text, not markdown — thinking is reasoning
// scratchpad, not authored prose, so code fences / links / headings would be
// noise at best and misleading at worst.
function ThinkingBlocks({ blocks }: { blocks: string[] }) {
  const summary =
    blocks.length === 1 ? "Thinking" : `Thinking (${blocks.length})`;
  return (
    <details className="mb-1.5 rounded border border-zinc-800/60 bg-zinc-900/30 px-2.5 py-1.5 text-[11px] leading-relaxed text-zinc-400 group">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 font-medium text-zinc-500 select-none marker:hidden">
        <span aria-hidden className="text-[9px] transition-transform group-open:rotate-90">▸</span>
        {summary}
      </summary>
      <div className="mt-1.5 space-y-1.5 border-t border-zinc-800/60 pt-1.5">
        {blocks.map((text, i) => (
          // Index is a stable identity within the message — append-only,
          // never reordered, and sameMessage skips re-renders when the array
          // content is unchanged.
          <div key={i} className="whitespace-pre-wrap break-words font-mono text-[11px] text-zinc-400">
            {text}
          </div>
        ))}
      </div>
    </details>
  );
}

export const Message = memo(MessageView, (a, b) => sameMessage(a.message, b.message));
