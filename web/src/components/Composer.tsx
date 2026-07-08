// The message composer. Sends `{"type":"send_message"}` on Enter (Shift+Enter
// inserts a newline) and `{"type":"stop"}` while the agent is running. Slash
// commands ride send_message — the backend classifies them. Disabled until
// the WebSocket connects.
//
// A slash palette (fed by `GET /commands`, the single source of truth) opens
// while the input is a bare `/name` prefix. It's a flat filtered list — pick
// with ↑/↓ + Enter/Tab, dismiss with Esc. Accepting fills the input (adding a
// trailing space for commands that take args); the user still presses Enter
// to send, so nothing auto-fires.

import { useState } from "react";
import type { SlashCommand } from "../state";

export function Composer({
  isRunning,
  connected,
  commands,
  onSend,
  onStop,
}: {
  isRunning: boolean;
  connected: boolean;
  commands: SlashCommand[];
  onSend: (text: string) => void;
  onStop: () => void;
}) {
  const [text, setText] = useState("");
  const [selected, setSelected] = useState(0);
  const [dismissed, setDismissed] = useState(false);

  // Palette shows only while typing a bare command name (`/foo`, no space).
  const query = text.startsWith("/") && !text.includes(" ") ? text.slice(1) : null;
  const matches =
    query === null
      ? []
      : commands.filter((c) => c.name.startsWith(query.toLowerCase()));
  const showPalette = connected && !isRunning && !dismissed && matches.length > 0;
  const sel = Math.min(selected, matches.length - 1);

  const submit = () => {
    const trimmed = text.trim();
    if (!trimmed || !connected) return;
    onSend(trimmed);
    setText("");
    setDismissed(false);
  };

  const accept = (cmd: SlashCommand) => {
    setText(`/${cmd.name}${cmd.takes_args ? " " : ""}`);
    setDismissed(true); // hide until the field is edited again
  };

  const onChange = (v: string) => {
    setText(v);
    setSelected(0);
    setDismissed(false);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (showPalette) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected((s) => (s + 1) % matches.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected((s) => (s - 1 + matches.length) % matches.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        accept(matches[sel]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setDismissed(true);
        return;
      }
    }
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
        <div className="relative">
          {showPalette && (
            <div className="absolute bottom-full left-0 z-20 mb-2 max-h-72 w-full overflow-y-auto rounded-lg border border-zinc-800 bg-zinc-900 py-1 shadow-xl">
              {matches.map((c, i) => (
                <button
                  key={c.name}
                  // Keep textarea focus so keyboard flow isn't broken.
                  onMouseDown={(e) => {
                    e.preventDefault();
                    accept(c);
                  }}
                  className={`flex w-full items-baseline gap-2 px-3 py-1.5 text-left text-xs ${
                    i === sel ? "bg-zinc-800" : "hover:bg-zinc-800/60"
                  }`}
                >
                  <span className="font-mono font-medium text-emerald-400">
                    /{c.name}
                    {c.takes_args && <span className="text-zinc-500"> &lt;args&gt;</span>}
                  </span>
                  <span className="truncate text-zinc-500">{c.description}</span>
                </button>
              ))}
            </div>
          )}

          <div className="flex items-end gap-2 rounded-xl border border-zinc-800 bg-zinc-900 p-2 focus-within:border-zinc-700">
            <textarea
              rows={1}
              disabled={!connected}
              value={text}
              onChange={(e) => onChange(e.target.value)}
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
