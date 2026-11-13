// Header of the mobile Conversation screen (design §4.2): "‹ Chats" back
// button, centred title + a subtitle that IS the selection chip (`selector`),
// Panels button, Focus toggle and connection state.
import type { JSX, ReactNode } from "react";
import { FocusToggle } from "./FocusToggle";
import { PanelsButton, type PanelsButtonProps } from "./PanelsSheet";

interface ChatHeaderProps {
  title: string;
  /** `<SelectionChip sheet …/>`; renders nothing before the first state. */
  selector: ReactNode;
  onBack(): void;
  connected: boolean;
  isRunning: boolean;
  focusMode: boolean;
  onToggleFocus(): void;
  /** Absent ⇒ no button (≥1024px or no visible tabs). */
  panels?: PanelsButtonProps;
}

export function ChatHeader(props: ChatHeaderProps): JSX.Element {
  const {
    title,
    selector,
    onBack,
    connected,
    isRunning,
    focusMode,
    onToggleFocus,
    panels,
  } = props;

  return (
    <header className="relative z-30 flex min-h-14 items-center gap-0.5 border-b border-zinc-800 bg-zinc-950/80 px-2 pb-2.5 pt-[calc(env(safe-area-inset-top)+10px)] backdrop-blur">
      <button
        type="button"
        data-testid="chat-back"
        aria-label="Back to chats"
        onClick={onBack}
        // h-11 keeps the tap target at the 44px phone minimum.
        className="relative -ml-0.5 flex h-11 shrink-0 items-center gap-0.5 rounded px-1.5 text-base text-sky-400 transition-colors hover:text-sky-300"
      >
        <svg
          aria-hidden
          width="24"
          height="24"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M15 5l-7 7 7 7" />
        </svg>
        Chats
      </button>

      <div className="flex min-w-0 flex-1 flex-col items-center justify-center gap-[3px]">
        <h1
          data-testid="chat-title"
          className="w-full truncate text-center text-[15px] font-semibold tracking-[-0.01em] text-zinc-100"
        >
          {title}
        </h1>
        <div
          data-testid="chat-subtitle"
          className="flex w-full min-w-0 items-center justify-center gap-1.5 text-xs text-zinc-500"
        >
          {selector}
        </div>
      </div>

      {/* Mock order (proto:376-382): state, focus, panels. */}
      <div className="flex shrink-0 items-center gap-0.5">
        {!connected ? (
          // Offline is kept (user decision 1): the only offline signal in the
          // header — the mock's .mdot-offline dot plus 12px muted text.
          <span
            data-testid="chat-state"
            data-state="offline"
            className="flex h-11 shrink-0 items-center gap-1.5 px-1 text-xs text-zinc-500"
          >
            <span className="h-2 w-2 rounded-full bg-zinc-500" />
            offline
          </span>
        ) : isRunning ? (
          // Spinner only: the word lives in the inline status line, so the
          // text stays for assistive tech.
          <span
            data-testid="chat-state"
            data-state="working"
            className="grid h-11 w-7 shrink-0 place-items-center"
          >
            <span className="h-4 w-4 animate-spin rounded-full border-2 border-amber-500 border-t-transparent" />
            <span className="sr-only">working…</span>
          </span>
        ) : null /* connected and idle: the mock shows nothing here */}
        <FocusToggle phone enabled={focusMode} onToggle={onToggleFocus} />
        {panels && <PanelsButton {...panels} />}
      </div>
    </header>
  );
}
