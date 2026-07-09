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
//
// Images: paste, drag-drop, or click the 📎 clip button to attach image files.
// Attachments show as
// removable chips above the input and are kept OUT of the visible textarea
// (a multi-MB data URL there would be unreadable and would break the palette
// query). On submit each becomes a `[img:data:<mime>;base64,…]` token appended
// to the sent text — the backend's `vision.rs` resolves the data URI exactly
// like a `[img:/path]` token. Over-size files are rejected client-side before
// they hit the wire; a non-vision model is rejected server-side.

import { useRef, useState } from "react";
import type { SlashCommand } from "../state";

// Mirror of vision.rs MAX_IMAGE_BYTES — fail fast before shipping a doomed frame.
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const MAX_IMAGES = 8;

type PendingImage = { id: string; name: string; dataUrl: string };

const readAsDataUrl = (file: File) =>
  new Promise<string>((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(r.result as string);
    r.onerror = () => reject(r.error);
    r.readAsDataURL(file);
  });

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
  const [images, setImages] = useState<PendingImage[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const [attachError, setAttachError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Palette shows only while typing a bare command name (`/foo`, no space).
  const query = text.startsWith("/") && !text.includes(" ") ? text.slice(1) : null;
  const matches =
    query === null
      ? []
      : commands.filter((c) => c.name.startsWith(query.toLowerCase()));
  const showPalette = connected && !isRunning && !dismissed && matches.length > 0;
  const sel = Math.min(selected, matches.length - 1);

  const addFiles = async (files: File[]) => {
    const imgs = files.filter((f) => f.type.startsWith("image/"));
    if (imgs.length === 0) return;
    setAttachError(null);
    for (const f of imgs) {
      if (f.size > MAX_IMAGE_BYTES) {
        setAttachError(`"${f.name || "image"}" is too large (max 10 MB).`);
        continue;
      }
      let dataUrl: string;
      try {
        dataUrl = await readAsDataUrl(f);
      } catch {
        setAttachError(`Could not read "${f.name || "image"}".`);
        continue;
      }
      // Functional update so back-to-back adds see the live count, not the
      // stale render-time snapshot; drop silently past the cap.
      let capped = false;
      setImages((prev) => {
        if (prev.length >= MAX_IMAGES) {
          capped = true;
          return prev;
        }
        return [
          ...prev,
          { id: crypto.randomUUID(), name: f.name || "pasted image", dataUrl },
        ];
      });
      if (capped) {
        setAttachError(`At most ${MAX_IMAGES} images per message.`);
        break;
      }
    }
  };

  const removeImage = (id: string) =>
    setImages((prev) => prev.filter((i) => i.id !== id));

  const onPickFiles = (e: React.ChangeEvent<HTMLInputElement>) => {
    void addFiles(Array.from(e.target.files ?? []));
    e.target.value = ""; // reset so re-picking the same file fires onChange again
  };

  const submit = () => {
    const trimmed = text.trim();
    if ((!trimmed && images.length === 0) || !connected) return;
    const tokens = images.map((i) => `[img:${i.dataUrl}]`).join(" ");
    const payload = [trimmed, tokens].filter(Boolean).join(" ");
    onSend(payload);
    setText("");
    setImages([]);
    setDismissed(false);
    setAttachError(null);
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

  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(e.clipboardData.files);
    if (files.some((f) => f.type.startsWith("image/"))) {
      e.preventDefault();
      void addFiles(files);
    }
  };

  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDragOver(false);
    if (!connected) return;
    void addFiles(Array.from(e.dataTransfer.files));
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
    ? "Send a message…  (Enter to send, Shift+Enter for newline, 📎/paste/drag to attach an image)"
    : "Connecting…";

  const canSend = connected && (!!text.trim() || images.length > 0);

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

          <div
            onDragOver={(e) => {
              e.preventDefault();
              setDragOver(true);
            }}
            onDragLeave={() => setDragOver(false)}
            onDrop={onDrop}
            className={`flex flex-col gap-2 rounded-xl border bg-zinc-900 p-2 ${
              dragOver
                ? "border-emerald-600 bg-emerald-950/20"
                : "border-zinc-800 focus-within:border-zinc-700"
            }`}
          >
            {images.length > 0 && (
              <div className="flex flex-wrap gap-2 px-1 pt-1">
                {images.map((img) => (
                  <div
                    key={img.id}
                    className="group relative h-16 w-16 overflow-hidden rounded-md border border-zinc-700"
                  >
                    <img
                      src={img.dataUrl}
                      alt={img.name}
                      className="h-full w-full object-cover"
                    />
                    <button
                      onClick={() => removeImage(img.id)}
                      title={`Remove ${img.name}`}
                      className="absolute right-0.5 top-0.5 flex h-4 w-4 items-center justify-center rounded-full bg-zinc-950/80 text-xs leading-none text-zinc-300 opacity-0 group-hover:opacity-100 hover:bg-red-900"
                    >
                      ×
                    </button>
                  </div>
                ))}
              </div>
            )}

            <div className="flex items-end gap-2">
              <input
                ref={fileInputRef}
                type="file"
                accept="image/*"
                multiple
                onChange={onPickFiles}
                className="hidden"
              />
              <button
                type="button"
                disabled={!connected}
                onClick={() => fileInputRef.current?.click()}
                title="Attach images"
                aria-label="Attach images"
                className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-40"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  className="h-5 w-5"
                >
                  <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                </svg>
              </button>
              <textarea
                rows={1}
                disabled={!connected}
                value={text}
                onChange={(e) => onChange(e.target.value)}
                onKeyDown={onKeyDown}
                onPaste={onPaste}
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
                  disabled={!canSend}
                  className="rounded-lg bg-emerald-700 px-3.5 py-1.5 text-sm font-medium text-white hover:bg-emerald-600 disabled:opacity-40"
                >
                  Send
                </button>
              )}
            </div>
          </div>
        </div>
        {attachError && (
          <div className="mt-1.5 px-1 text-[11px] text-red-400">{attachError}</div>
        )}
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
          <span>
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">📎</kbd> or{" "}
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">paste</kbd> /{" "}
            <kbd className="rounded bg-zinc-800 px-1 text-zinc-400">drag</kbd> an image
          </span>
        </div>
      </div>
    </div>
  );
}
