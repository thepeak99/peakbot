// Full-panel drop target. Wraps <main>; shows a dashed "Drop files to attach"
// overlay while a drag carrying Files is over it.

import { useRef, useState, type ReactNode } from "react";

const carriesFiles = (e: React.DragEvent): boolean =>
  Array.from(e.dataTransfer?.types ?? []).includes("Files");

export function DropOverlay({
  children,
  onAdd,
}: {
  children: ReactNode;
  onAdd: (files: File[]) => void;
}) {
  const [active, setActive] = useState(false);
  // dragenter/dragleave also fire for every child crossed, so a boolean would
  // flicker; the depth counter hides the overlay only when the drag truly leaves.
  const depth = useRef(0);

  const onDragEnter = (e: React.DragEvent) => {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    depth.current += 1;
    setActive(true);
  };

  const onDragOver = (e: React.DragEvent) => {
    // Without preventDefault on dragover the browser refuses the drop.
    if (carriesFiles(e)) e.preventDefault();
  };

  const onDragLeave = () => {
    if (depth.current === 0) return;
    depth.current -= 1;
    if (depth.current === 0) setActive(false);
  };

  const onDrop = (e: React.DragEvent) => {
    if (!carriesFiles(e)) return;
    e.preventDefault();
    depth.current = 0;
    setActive(false);
    const files = Array.from(e.dataTransfer.files ?? []);
    if (files.length > 0) onAdd(files);
  };

  return (
    <main
      className="relative flex min-w-0 flex-1 flex-col"
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      {children}
      {active && (
        <div className="pointer-events-none absolute inset-2 z-30 flex items-center justify-center rounded-xl border-2 border-dashed border-emerald-500 bg-zinc-950/80 text-lg font-medium text-emerald-300">
          Drop files to attach
        </div>
      )}
    </main>
  );
}
