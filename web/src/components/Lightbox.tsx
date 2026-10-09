// Full-size image viewer for one message's images. Portals to <body> so the
// transcript's virtualised row transforms can't clip or offset it.

import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { AttachmentView } from "../types";

export function Lightbox({
  images,
  start,
  onClose,
}: {
  images: AttachmentView[];
  start: number;
  onClose: () => void;
}) {
  const [index, setIndex] = useState(start);
  const [broken, setBroken] = useState<Set<string>>(new Set());
  const count = images.length;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowRight") setIndex((i) => (i + 1) % count);
      else if (e.key === "ArrowLeft") setIndex((i) => (i - 1 + count) % count);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [count, onClose]);

  const current = images[index];
  if (!current) return null;

  return createPortal(
    <div
      role="dialog"
      aria-modal="true"
      aria-label={current.name}
      onClick={onClose}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/85 p-4"
    >
      {broken.has(current.id) ? (
        <a
          href={current.url}
          target="_blank"
          rel="noopener"
          onClick={(e) => e.stopPropagation()}
          className="text-sm text-zinc-200 underline"
        >
          Can't display {current.name} — open it
        </a>
      ) : (
        <img
          src={current.url}
          alt={current.name}
          onClick={(e) => e.stopPropagation()}
          onError={() => setBroken((b) => new Set(b).add(current.id))}
          className="max-h-full max-w-full object-contain"
        />
      )}
      <div className="absolute inset-x-0 bottom-3 text-center text-xs text-zinc-300">
        {current.name}
        {count > 1 && ` · ${index + 1} / ${count}`}
      </div>
      {count > 1 && (
        <>
          <button
            type="button"
            aria-label="Previous image"
            onClick={(e) => {
              e.stopPropagation();
              setIndex((i) => (i - 1 + count) % count);
            }}
            className="absolute left-3 top-1/2 -translate-y-1/2 rounded-full bg-zinc-900/80 px-3 py-2 text-lg text-zinc-200 hover:bg-zinc-800"
          >
            ←
          </button>
          <button
            type="button"
            aria-label="Next image"
            onClick={(e) => {
              e.stopPropagation();
              setIndex((i) => (i + 1) % count);
            }}
            className="absolute right-3 top-1/2 -translate-y-1/2 rounded-full bg-zinc-900/80 px-3 py-2 text-lg text-zinc-200 hover:bg-zinc-800"
          >
            →
          </button>
        </>
      )}
      <button
        type="button"
        aria-label="Close"
        onClick={onClose}
        className="absolute right-3 top-3 rounded-full bg-zinc-900/80 px-3 py-1.5 text-zinc-200 hover:bg-zinc-800"
      >
        ✕
      </button>
    </div>,
    document.body,
  );
}
