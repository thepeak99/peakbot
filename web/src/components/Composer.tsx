// The message composer. Sends `{"type":"send_message"}` on Enter (Shift+Enter
// inserts a newline) and `{"type":"stop"}` while the agent is running. Slash
// commands ride send_message — the backend classifies them. Disabled until
// the WebSocket connects.

import { useState } from "react";

export function Composer({
  isRunning,
  connected,
  onSend,
  onStop,
}: {
  isRunning: boolean;
  connected: boolean;
  onSend: (text: string) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState("");

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed || !connected) return;
    onSend(trimmed);
    setText("");
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };

  const placeholder = connected
    ? "Send a message…  (Enter to send, Shift+Enter for newline)"
    : "Connecting…";

  return (
    <div className="border-t border-zinc-800 bg-zinc-950 p-3">
      <div className="mx-auto max-w-3xl">
        <div className="flex items-end gap-2 rounded-xl border border-zinc-800 bg-zinc-900 p-2 focus-within:border-zinc-700">
          <textarea
            rows={1}
            disabled={!connected}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={placeholder}
            className="max-h-40 flex-1 resize-none bg-transparent px-2 py-1.5 text-sm text-zinc-100 placeholder-zinc-600 outline-none disabled:cursor-not-allowed"
          />
          {isRunning ? (
            <button
              onClick={onStop}
              className="flex items-center gap-1.5 rounded-lg bg-red-950/70 px-3 py-1.5 text-sm font-medium text-red-300 hover:bg-red-900/70"
            >
              <span className="h-2 w-2 rounded-sm bg-red-400" />
              Stop
            </button>
          ) : (
            <button
              onClick={submit}
              disabled={!connected || !text.trim()}
              className="rounded-lg bg-emerald-700 px-3.5 py-1.5 text-sm font-medium text-white hover:bg-emerald-600 disabled:opacity-40"
            >
              Send
            </button>
          )}
        </div>
        <div className="mt-1.5 flex items-center gap-3 px-1 text-[11px] text-zinc-600">
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">Enter</kbd> to send
          </span>
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">/</kbd> for commands
          </span>
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">Shift+Enter</kbd> newline
          </span>
        </div>
      </div>
    </div>
  );
}
