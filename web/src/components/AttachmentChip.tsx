// Composer chip for one pending attachment: a 64×64 thumbnail for images the
// browser can render, otherwise an icon chip (tinted glyph + ext + name + size).

import type { Pending } from "../useAttachments";
import {
  GROUP_STYLE,
  extLabel,
  fileGroup,
  formatSize,
  isThumbnailable,
  middleTruncate,
} from "../fileTypes";

const NO_VISION = "Model can't view images — sent as file";
const TOO_BIG_FOR_MODEL =
  "Too large or unsupported for the model to view — sent as a file";

function FileGlyph({ className }: { className: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      className={className}
    >
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <path d="M14 2v6h6" />
    </svg>
  );
}

// 2px bar pinned to the chip's bottom edge.
function ProgressBar({ fraction }: { fraction: number }) {
  return (
    <div className="absolute inset-x-0 bottom-0 h-0.5 bg-zinc-800">
      <div
        className="h-full bg-emerald-500 transition-[width]"
        style={{ width: `${Math.round(fraction * 100)}%` }}
      />
    </div>
  );
}

// Always visible (not hover-only): touch screens have no hover.
function RemoveButton({ name, onClick }: { name: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={`Remove ${name}`}
      aria-label={`Remove ${name}`}
      className="flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-zinc-950/80 text-xs leading-none text-zinc-300 hover:bg-red-900"
    >
      ×
    </button>
  );
}

/** The ⚠ explanation for an image the model will receive as a file, if any. */
function visionWarning(chip: Pending, supportsVision: boolean): string | null {
  if (chip.state !== "done" || !isThumbnailable(chip.attachment.mime)) return null;
  if (chip.attachment.kind === "file") return TOO_BIG_FOR_MODEL;
  return supportsVision ? null : NO_VISION;
}

export function AttachmentChip({
  chip,
  supportsVision,
  onRemove,
}: {
  chip: Pending;
  supportsVision: boolean;
  onRemove: () => void;
}) {
  const name = chip.file.name || "pasted file";
  const mime = chip.state === "done" ? chip.attachment.mime : chip.file.type;
  const warn = visionWarning(chip, supportsVision);
  const errored = chip.state === "error";
  // Done chips fall back to the served file if no local preview survived.
  const src =
    chip.preview ??
    (chip.state === "done"
      ? `/api/uploads/${chip.attachment.convo}/${chip.attachment.id}`
      : undefined);

  if (isThumbnailable(mime) && src && !errored) {
    return (
      <div className="relative h-16 w-16 overflow-hidden rounded-md border border-zinc-700">
        <img src={src} alt={name} className="h-full w-full object-cover" />
        <div className="absolute right-0.5 top-0.5">
          <RemoveButton name={name} onClick={onRemove} />
        </div>
        {warn && (
          <span
            title={warn}
            className="absolute bottom-1 left-0.5 rounded bg-zinc-950/80 px-0.5 text-[11px] leading-none text-amber-400"
          >
            ⚠
          </span>
        )}
        {chip.state === "uploading" && <ProgressBar fraction={chip.progress} />}
      </div>
    );
  }

  const group = fileGroup(name, mime);
  const size = chip.state === "done" ? chip.attachment.size : chip.file.size;
  return (
    <div
      className={`relative flex max-w-[16rem] flex-col overflow-hidden rounded-md border bg-zinc-900 px-2 py-1.5 ${
        errored ? "border-red-800" : "border-zinc-700"
      }`}
    >
      <div className="flex items-center gap-2">
        <FileGlyph className={`h-6 w-6 shrink-0 ${GROUP_STYLE[group].color}`} />
        <div className="min-w-0 flex-1 text-xs leading-tight">
          <div className="flex items-center gap-1.5">
            {extLabel(name) && (
              <span
                className={`text-[10px] font-semibold ${GROUP_STYLE[group].color}`}
              >
                {extLabel(name)}
              </span>
            )}
            <span className="truncate text-zinc-200" title={name}>
              {middleTruncate(name)}
            </span>
          </div>
          <div className="text-[11px] text-zinc-500">{formatSize(size)}</div>
        </div>
        {warn && (
          <span title={warn} className="text-amber-400">
            ⚠
          </span>
        )}
        <RemoveButton name={name} onClick={onRemove} />
      </div>
      {chip.state === "error" && (
        <div className="mt-1 text-[11px] text-red-400">{chip.message}</div>
      )}
      {chip.state === "uploading" && <ProgressBar fraction={chip.progress} />}
    </div>
  );
}
