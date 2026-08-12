import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import rehypeHighlight from "rehype-highlight";
import { memo } from "react";
import type { ChatMessage, MessageRole } from "../types";

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
    a.subAgentRole === b.subAgentRole
  );
}

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
      {isMarkdown(message.role) ? (
        <div className="markdown-body text-sm leading-relaxed text-zinc-200">
          <ReactMarkdown remarkPlugins={[remarkGfm, remarkBreaks]} rehypePlugins={[rehypeHighlight]}>
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

export const Message = memo(MessageView, (a, b) => sameMessage(a.message, b.message));
