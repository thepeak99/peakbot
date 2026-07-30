/**
 * ui.tsx — the handful of form primitives every step reuses.
 *
 * Not a form library and not a component kit: just the label/input/error
 * wrapper and the two dummy-specific widgets (the `preview` chip and the fake
 * backend button), so ten step components don't each carry a copy of the same
 * focus-ring classes. Everything speaks the SPA's zinc ramp — no colours are
 * hardcoded, so both themes come for free (index.css remaps the ramp).
 */

import { useState, type ReactNode } from "react";

/** Shared text-input / select / textarea skin. */
export const inputClass =
  "w-full rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-1.5 text-sm text-zinc-100 placeholder-zinc-600 focus:border-zinc-700 focus:outline-none focus:ring-1 focus:ring-zinc-600";

/** Primary action (Next, Generate, Write config). */
export const buttonClass =
  "cursor-pointer rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition-colors hover:bg-zinc-700 disabled:cursor-not-allowed disabled:opacity-40";

/** Secondary/ghost action (Back, Add model, Remove). */
export const ghostButtonClass =
  "cursor-pointer rounded-md border border-zinc-800 px-3 py-1.5 text-sm text-zinc-400 transition-colors hover:border-zinc-700 hover:text-zinc-200 disabled:cursor-not-allowed disabled:opacity-40";

/** One labelled row. `hint` explains the field; `error` is a hard error. */
export function Field({
  label,
  hint,
  error,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
}) {
  return (
    <label className="block space-y-1">
      <span className="text-xs font-medium text-zinc-400">{label}</span>
      {children}
      {hint && <span className="block text-[11px] text-zinc-500">{hint}</span>}
      {error && <span className="block text-[11px] text-red-400">{error}</span>}
    </label>
  );
}

/** Checkbox + label, sized to match Field's rows. */
export function Check({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint?: ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-start gap-2">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-0.5 h-3.5 w-3.5 cursor-pointer accent-zinc-500"
      />
      <span className="text-sm text-zinc-300">
        {label}
        {hint && <span className="block text-[11px] text-zinc-500">{hint}</span>}
      </span>
    </label>
  );
}

/** A group of radio cards. Used for the provider paths, persona presets,
 *  access mode and the tools filter mode. */
export function RadioCards<T extends string>({
  name,
  value,
  options,
  onChange,
}: {
  name: string;
  value: T | undefined;
  options: Array<{ value: T; label: string; hint?: ReactNode }>;
  onChange: (value: T) => void;
}) {
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      {options.map((o) => (
        <label
          key={o.value}
          className={`flex cursor-pointer items-start gap-2 rounded-md border px-3 py-2 transition-colors ${
            value === o.value
              ? "border-zinc-600 bg-zinc-800"
              : "border-zinc-800 hover:border-zinc-700"
          }`}
        >
          <input
            type="radio"
            name={name}
            checked={value === o.value}
            onChange={() => onChange(o.value)}
            className="mt-1 h-3.5 w-3.5 cursor-pointer accent-zinc-500"
          />
          <span>
            <span className="block text-sm text-zinc-200">{o.label}</span>
            {o.hint && (
              <span className="block text-[11px] text-zinc-500">{o.hint}</span>
            )}
          </span>
        </label>
      ))}
    </div>
  );
}

/** Heading for a sub-block inside a step (the Services step has three). */
export function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="space-y-3 rounded-lg border border-zinc-800 p-3">
      <div>
        <h3 className="text-sm font-medium text-zinc-200">{title}</h3>
        {hint && <p className="mt-0.5 text-[11px] text-zinc-500">{hint}</p>}
      </div>
      {children}
    </section>
  );
}

/** The badge that marks everything a real backend would do. Every fake action
 *  carries one, so a tester is never fooled into thinking work happened. */
export function PreviewChip() {
  return (
    <span className="rounded border border-amber-700/60 bg-amber-950/40 px-1.5 py-0.5 text-[10px] font-medium tracking-wide text-amber-400 uppercase">
      preview
    </span>
  );
}

/** Hard-error list, shown above the step body and under the offending field. */
export function Errors({ errors }: { errors: string[] }) {
  if (errors.length === 0) return null;
  return (
    <ul className="space-y-1 rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-xs text-red-300">
      {errors.map((e) => (
        <li key={e}>{e}</li>
      ))}
    </ul>
  );
}

/**
 * A button standing in for a backend call: `preview` chip, a brief spinner,
 * then a canned success line. It never touches the network — the delay only
 * exists so the flow reads like the real thing.
 */
export function FakeActionButton({
  label,
  result,
  disabled,
  onDone,
}: {
  label: string;
  /** The canned success line shown next to the button. */
  result: string;
  disabled?: boolean;
  /** Draft mutation the real response would have caused (e.g. Import). */
  onDone?: () => void;
}) {
  const [phase, setPhase] = useState<"idle" | "running" | "done">("idle");

  const run = () => {
    setPhase("running");
    window.setTimeout(() => {
      setPhase("done");
      onDone?.();
    }, 800);
  };

  return (
    <div className="flex flex-wrap items-center gap-2">
      <button
        type="button"
        onClick={run}
        disabled={disabled || phase === "running"}
        className={buttonClass}
      >
        {phase === "running" ? "Working…" : label}
      </button>
      <PreviewChip />
      {phase === "running" && (
        <span className="text-xs text-zinc-500">
          <Spinner /> contacting…
        </span>
      )}
      {phase === "done" && (
        <span className="text-xs text-emerald-400">✓ {result}</span>
      )}
    </div>
  );
}

/** Inline spinner (borrowed shape from the SPA's working indicator). */
export function Spinner() {
  return (
    <span className="mr-1 inline-block h-3 w-3 animate-spin rounded-full border border-zinc-600 border-t-transparent align-[-1px]" />
  );
}
