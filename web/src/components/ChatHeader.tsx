// Header of the mobile Conversation screen (design §4.2): "‹ Chats" back
// button (dot badge when another chat is running / unread), centred title +
// a subtitle that IS the selection chip (`selector`, behind a machine prefix
// on remote chats), Panels button, Focus toggle and connection state.
import type { JSX, ReactNode } from "react";
import { FocusToggle } from "./FocusToggle";
import { PanelsButton, type PanelsButtonProps } from "./PanelsSheet";

interface ChatHeaderProps {
  title: string;
  /** Remote machine display name; null = this device (no prefix). */
  machine: string | null;
  /** `<SelectionChip sheet …/>`; renders nothing before the first state. */
  selector: ReactNode;
  badge: boolean;
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
    machine,
    selector,
    badge,
    onBack,
    connected,
    isRunning,
    focusMode,
    onToggleFocus,
    panels,
  } = props;

  return (
    <header className="relative z-30 flex min-h-14 items-center gap-2 border-b border-zinc-800 bg-zinc-950/80 px-3 py-2 pt-[env(safe-area-inset-top)] backdrop-blur">
      <button
        type="button"
        data-testid="chat-back"
        aria-label="Back to chats"
        onClick={onBack}
        // h-11 keeps the tap target at the 44px phone minimum.
        className="relative -ml-1 flex h-11 shrink-0 items-center gap-1 rounded px-2 text-sm text-sky-400 transition-colors hover:text-sky-300"
      >
        <span aria-hidden="true" className="text-lg leading-none">
          ‹
        </span>
        Chats
        {badge && (
          <span
            data-testid="chat-back-badge"
            className="absolute right-1.5 top-2 h-2 w-2 rounded-full bg-sky-400"
          />
        )}
      </button>

      <div className="flex min-w-0 flex-1 flex-col items-center justify-center">
        <h1
          data-testid="chat-title"
          className="w-full truncate text-center text-sm font-semibold text-zinc-100"
        >
          {title}
        </h1>
        <div
          data-testid="chat-subtitle"
          className="flex w-full min-w-0 items-center justify-center gap-1 text-xs text-zinc-500"
        >
          {machine && (
            <span data-testid="chat-machine" className="shrink-0">
              {machine} ·
            </span>
          )}
          {selector}
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-3">
        {panels && <PanelsButton {...panels} />}
        <FocusToggle enabled={focusMode} onToggle={onToggleFocus} />
        {!connected ? (
          <span
            data-testid="chat-state"
            data-state="offline"
            className="flex items-center gap-1.5 text-xs text-zinc-500"
          >
            <span className="h-1.5 w-1.5 rounded-full bg-zinc-600" />
            offline
          </span>
        ) : isRunning ? (
          // Spinner only: the word lives in the inline status line, so the
          // text stays for assistive tech.
          <span
            data-testid="chat-state"
            data-state="working"
            className="flex items-center gap-1.5 text-xs text-amber-400"
          >
            <span className="h-3 w-3 animate-spin rounded-full border-2 border-amber-400 border-t-transparent shrink-0" />
            <span className="sr-only">working…</span>
          </span>
        ) : (
          <span
            data-testid="chat-state"
            data-state="connected"
            className="flex items-center gap-1.5 text-xs text-emerald-400"
          >
            <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
          </span>
        )}
      </div>
    </header>
  );
}
