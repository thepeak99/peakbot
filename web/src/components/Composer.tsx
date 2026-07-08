// The message composer. Phase 0 is a visual mock: the textarea is disabled
// and the Send / Stop buttons are inert. Phase 1 wires this to
// `{"type":"send_message"}` / `{"type":"stop"}` over the WebSocket.
export function Composer({ isRunning }: { isRunning: boolean }) {
  return (
    <div className="border-t border-zinc-800 bg-zinc-950 p-3">
      <div className="mx-auto max-w-3xl">
        <div className="flex items-end gap-2 rounded-xl border border-zinc-800 bg-zinc-900 p-2 focus-within:border-zinc-700">
          <textarea
            rows={1}
            disabled
            placeholder="Send a message…  (mock — input lands in Phase 1)"
            className="max-h-40 flex-1 resize-none bg-transparent px-2 py-1.5 text-sm text-zinc-100 placeholder-zinc-600 outline-none disabled:cursor-not-allowed"
          />
          {isRunning ? (
            <button
              disabled
              className="flex items-center gap-1.5 rounded-lg bg-red-950/70 px-3 py-1.5 text-sm font-medium text-red-300"
            >
              <span className="h-2 w-2 rounded-sm bg-red-400" />
              Stop
            </button>
          ) : (
            <button
              disabled
              className="rounded-lg bg-emerald-700 px-3.5 py-1.5 text-sm font-medium text-white opacity-70"
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
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">[img:…]</kbd> to attach
          </span>
        </div>
      </div>
    </div>
  );
}
