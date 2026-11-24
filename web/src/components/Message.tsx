import ReactMarkdown, { type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import rehypeHighlight from "rehype-highlight";
import { memo, useRef, useState, type ComponentProps } from "react";
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
import { MTAG } from "./phoneUi";
import { firstLine } from "../transcriptRows";
import { formatClock, formatFull, localeClock } from "../dates";

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
    a.at === b.at &&
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

// Only /images/, /r/<profile>/images/ (server relay re-roots image URLs to
// the remote profile), http(s)://, and data: become <img>: a bare local
// path the backend left unresolved would hit the SPA catch-all route.
// Type guard: after this check `src` is a usable string.
const REMOTE_IMAGE_RE = /^\/r\/[^/]+\/images\//;
function isRenderableImageSrc(src: string | undefined): src is string {
  return (
    src !== undefined &&
    (src.startsWith("/images/") ||
      REMOTE_IMAGE_RE.test(src) ||
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
  const [open, setOpen] = useState(false);
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
    <>
      <img
        src={src}
        alt={alt ?? ""}
        loading="lazy"
        onClick={() => setOpen(true)}
        onError={() => setError(true)}
        className="max-h-96 cursor-zoom-in rounded border border-zinc-800/60"
      />
      {open && (
        <Lightbox
          images={[{ id: src, name: alt || "image", url: src }]}
          start={0}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

// Module scope: stable identity across renders — an inline literal would be
// a fresh reference every render and defeat memoization.
const MARKDOWN_COMPONENTS = { img: CachedImage } as const;

// ─── Phone (< lg) faces: mock `.u` / `.a` / `.tool` / `.code` (fix-plan 3.3–3.6) ───
// Kept apart from the desktop card so `phone` false renders exactly the old DOM.

const PHONE_MARKDOWN_COMPONENTS = { img: CachedImage, pre: CodeBlock } as const;

// Centralized chat markdown renderer. Encapsulates the plugin list and
// selects the correct components map (phone vs desktop) to keep the
// rendering pipeline consistent across call sites.
function ChatMarkdown({ phone = false, content }: { phone?: boolean; content: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkBreaks]}
      rehypePlugins={[rehypeHighlight]}
      components={phone ? PHONE_MARKDOWN_COMPONENTS : MARKDOWN_COMPONENTS}
    >
      {content}
    </ReactMarkdown>
  );
}

const COPY_ICON = (
  <svg aria-hidden width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15V6a2 2 0 0 1 2-2h9" />
  </svg>
);
const CHECK_ICON = (
  <svg aria-hidden width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M5 12l5 5 9-10" />
  </svg>
);

// Fence wrapper: language header + Copy button around the highlighted <pre>.
// Styled by the `.md-phone .code-*` rules in index.css.
function CodeBlock({ node, children, ...rest }: ComponentProps<"pre"> & ExtraProps) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  const codeClass = node?.children[0]?.type === "element" ? node.children[0].properties.className : undefined;
  const lang =
    (Array.isArray(codeClass) ? codeClass.map(String).find((c) => c.startsWith("language-")) : undefined)?.slice(9) ?? "code";
  const copy = () => {
    // Swallow clipboard errors (insecure context / denied), like FilesPanel.
    navigator.clipboard
      ?.writeText(ref.current?.textContent ?? "")
      ?.then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {});
  };
  return (
    <div className="code-block">
      <div className="code-hdr">
        <span>{lang}</span>
        <button type="button" aria-label="Copy code" onClick={copy}>
          {copied ? CHECK_ICON : COPY_ICON}
        </button>
      </div>
      <pre ref={ref} {...rest}>
        {children}
      </pre>
    </div>
  );
}

// Collapsible tool row (`.tool` + `.toolext`). Opening changes the row's
// height; the transcript's per-row ResizeObserver (measureElement) re-measures.
function ToolRow({ message }: { message: ChatMessage }) {
  const [open, setOpen] = useState(false);
  const call = message.role === "toolCall";
  return (
    <div className="flex flex-col">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex h-11 w-full cursor-pointer items-center gap-2 rounded-xl border border-zinc-800 bg-zinc-900 px-3 text-left text-[13px] text-zinc-400"
      >
        <span aria-hidden className="text-[15px]">
          {call ? "🔧" : "↳"}
        </span>
        <span className="shrink-0 font-mono text-[12.5px] text-zinc-300">
          {message.toolName ?? (call ? "tool" : "result")}
        </span>
        <span className="min-w-0 flex-1 truncate">{firstLine(message.content)}</span>
        <svg
          aria-hidden
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`shrink-0 text-zinc-500 transition-transform ${open ? "rotate-90" : ""}`}
        >
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
      {open && (
        <div
          data-testid="tool-ext"
          className="mt-1.5 whitespace-pre-wrap break-words rounded-[10px] border border-zinc-800 bg-zinc-950 px-3 py-2.5 font-mono text-[11.5px] leading-[1.5] text-zinc-300"
        >
          {isMarkdown(message.role) ? (
            <ChatMarkdown phone content={message.content} />
          ) : (
            message.content
          )}
        </div>
      )}
      <MessageTime at={message.at} end={false} />
    </div>
  );
}

// Tap toggles HH:MM <-> the full date; local state, so a row scrolled out of
// the virtualizer overscan resets to the short form.
function MessageTime({ at, end }: { at: number; end: boolean }) {
  const [full, setFull] = useState(false);
  return (
    <button
      type="button"
      aria-expanded={full}
      onClick={() => setFull((f) => !f)}
      className={`mt-1 rounded-[5px] px-1 text-[10px] tabular-nums text-zinc-500 hover:text-zinc-300 ${end ? "self-end" : "self-start"}`}
    >
      {full ? formatFull(at) : formatClock(at)}
    </button>
  );
}

function PhoneMessage({ message }: { message: ChatMessage }) {
  const { role } = message;
  if (role === "user") {
    return (
      <div className="flex flex-col items-end gap-1.5">
        {message.attachments && <Attachments attachments={message.attachments} />}
        <div className="max-w-[88%] whitespace-pre-wrap break-words rounded-[18px_18px_6px_18px] bg-zinc-800 px-3.5 py-2.5 text-[15px] leading-[1.4] text-zinc-100">
          {message.content}
        </div>
        <MessageTime at={message.at} end />
      </div>
    );
  }
  if (role === "toolCall" || role === "toolResult") return <ToolRow message={message} />;
  const prose = role === "agent" ? "text-[15px] leading-[1.5] text-zinc-100" : "text-[13.5px] text-zinc-400";
  return (
    <div className="flex flex-col">
      {(message.subAgentRole || message.fromBackground || role === "summary") && (
        <div className="mb-1 flex gap-1">
          {role === "summary" && <span className={MTAG}>summary</span>}
          {message.subAgentRole && <span className={MTAG}>🧩 {message.subAgentRole}</span>}
          {message.fromBackground && <span className={MTAG}>🛰 bg</span>}
        </div>
      )}
      {message.thinking && message.thinking.length > 0 && (
        <ThinkingBlocks blocks={message.thinking} />
      )}
      {message.content.trim() !== "" && (
        <div className={`markdown-body md-phone max-w-[96%] break-words ${prose}`}>
          <ChatMarkdown phone content={message.content} />
        </div>
      )}
      <MessageTime at={message.at} end={role === "system"} />
    </div>
  );
}

// Unexported renderer. Wrapped by `memo` below so that, with the value comparator
// (`sameMessage`), re-renders are skipped when the adapted message hasn't
// actually changed. Identity comparison alone wouldn't help: every websocket
// frame is a fresh `JSON.parse`, and `adaptMessage` allocates a new object per
// call at the render site, so the prop reference never survives a re-render —
// we need the structural comparison.
function MessageView({ message, phone = false }: { message: ChatMessage; phone?: boolean }) {
  if (phone) return <PhoneMessage message={message} />;
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
        <span className="ml-auto tabular-nums text-zinc-600">{localeClock(message.at)}</span>
      </div>
      {message.thinking && message.thinking.length > 0 && (
        <ThinkingBlocks blocks={message.thinking} />
      )}
      {message.attachments && <Attachments attachments={message.attachments} />}
      {isMarkdown(message.role) ? (
        <div className="markdown-body text-sm leading-relaxed text-zinc-200">
          <ChatMarkdown content={message.content} />
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

export const Message = memo(
  MessageView,
  (a, b) => a.phone === b.phone && sameMessage(a.message, b.message),
);
