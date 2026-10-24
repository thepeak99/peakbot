// D9 "Raw YAML" pane (spec §9.11, contract.ts "./raw (T31)"). Controlled: the
// shell owns the text (store.setRaw parses it back into the draft) and the
// syntax error; this component only renders them.

import type { RawError } from "./contract";
import type { Scope } from "./types";

export interface RawPaneProps {
  scope: Scope;
  /** What the textarea shows — verbatim, no trimming. */
  text: string;
  error: RawError | null;
  /** Every keystroke. */
  onChange(text: string): void;
  /** Textarea blur → the shell validates (spec §9.6 "Raw YAML blur"). */
  onBlur?(): void;
}

export function RawPane({ scope, text, error, onChange, onBlur }: RawPaneProps) {
  return (
    <div data-testid="raw-pane" className="max-w-4xl space-y-2">
      <p data-testid="raw-banner" className="text-xs text-amber-400/90">
        ⚠ Advanced: unknown keys are rejected at boot. Edits here replace the generated file on
        save.
        {scope === "repo" && " The repo file may only contain profiles:."}
      </p>
      <textarea
        data-testid="raw-text"
        value={text}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => onBlur?.()}
        spellCheck={false}
        aria-label="Raw YAML"
        aria-invalid={error !== null}
        className={`block h-[60vh] w-full resize-y rounded-md border bg-zinc-950 p-3 font-mono text-xs leading-relaxed text-zinc-200 focus:outline-none ${
          error ? "border-red-700" : "border-zinc-700 focus:border-zinc-500"
        }`}
      />
      {error && (
        <p data-testid="raw-error" className="font-mono text-xs text-red-300">
          {`line ${error.line}, col ${error.col}: ${error.message}`}
        </p>
      )}
    </div>
  );
}
