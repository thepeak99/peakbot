// Step 8 — Start on boot. Shows the unit file it *would* write and the command
// that would enable it.
//
// Deliberately inert, even in the real implementation: a config wizard that
// silently registers a boot service is astonishing. Write the file, show the
// command, let the user run it.

import type { StartOnBootDraft } from "../draft";
import type { StepProps } from "../steps";
import { BOOT_SERVICE, PLATFORM } from "../fixtures";
import { Check } from "../ui";

export function StartOnBootStep({ draft, patch }: StepProps) {
  const set = (partial: Partial<StartOnBootDraft>) =>
    patch({ startOnBoot: { ...draft.startOnBoot, ...partial } });

  return (
    <div className="space-y-4">
      <p className="text-sm text-zinc-400">
        On {PLATFORM.os} this is a systemd user unit. It starts at login; add{" "}
        <code className="text-zinc-300">loginctl enable-linger</code> if it
        should survive logout.
      </p>

      <Check
        label="Start on boot"
        hint={`Would write ${BOOT_SERVICE.path}. Nothing is registered in this preview.`}
        checked={draft.startOnBoot.enabled ?? false}
        onChange={(enabled) =>
          set({
            enabled,
            serviceName: BOOT_SERVICE.path,
            command: BOOT_SERVICE.enableCommand,
          })
        }
      />

      <div className="space-y-1">
        <p className="text-xs font-medium text-zinc-400">{BOOT_SERVICE.path}</p>
        <pre className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-2 text-xs text-zinc-300">
          {BOOT_SERVICE.content}
        </pre>
      </div>

      <div className="space-y-1">
        <p className="text-xs font-medium text-zinc-400">Then enable it</p>
        <pre className="overflow-x-auto rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1.5 text-xs text-zinc-300">
          {BOOT_SERVICE.enableCommand}
        </pre>
      </div>
    </div>
  );
}
