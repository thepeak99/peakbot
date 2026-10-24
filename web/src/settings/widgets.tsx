// Form building blocks for the Settings panes (contract.ts "./widgets (T24)").
//
// All widgets are DUMB: they take `value` + `onChange` and render red on
// `invalid` (the pane matches ConfigErr paths against the server's 422
// errors, spec §9.6). Every composite stamps `testid` on its container;
// inner elements use the fixed relative testids from the contract. Number
// fields write a NUMBER (never a string); an empty number (or text) field
// calls onChange(undefined) = key removed (spec §11.5).

import { useEffect, useState } from "react";
import { setMode, type FilterMode, type FilterState } from "./filter";

const INPUT_CLS =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-xs text-zinc-100 placeholder:text-zinc-600 focus:border-zinc-500 focus:outline-none disabled:cursor-default disabled:opacity-60";
const GHOST_BTN_CLS =
  "cursor-pointer rounded border border-zinc-700 px-2 py-0.5 text-[11px] text-zinc-300 hover:bg-zinc-800 disabled:cursor-default disabled:opacity-60";

function Label({ text }: { text: string }) {
  return <span className="mb-1 block text-[11px] text-zinc-400">{text}</span>;
}

function Hint({ text }: { text: string }) {
  return <span data-testid="hint" className="mt-1 block text-[10px] text-zinc-500">{text}</span>;
}

function ErrorText({ text }: { text: string }) {
  return <span data-testid="error" className="mt-1 block text-[11px] text-red-400">{text}</span>;
}

// ── composing edit state (list widgets) ─────────────────────────────────
// The list widgets (ChipRow / KvRows / FilterBlock) are driven fire-and-forget:
// neither the shell's tests nor a fast typing burst re-passes the `value`
// prop after every `onChange`, so consecutive edits compose on the widget's
// OWN state. When the prop arrives with DIFFERENT content (Discard, a Raw
// YAML edit parsed back, a reload), the widget adopts it.
function useComposing<T>(value: T, equal: (a: T, b: T) => boolean): [T, (next: T) => void] {
  const [state, setState] = useState(value);
  useEffect(() => {
    setState((cur) => (equal(cur, value) ? cur : value));
  }, [value, equal]);
  return [state, setState];
}

const sameStrings = (a: string[], b: string[]) =>
  a.length === b.length && a.every((x, i) => x === b[i]);
const sameRows = (a: KvRow[], b: KvRow[]) =>
  a.length === b.length && a.every((x, i) => x.key === b[i].key && x.value === b[i].value);
const sameFilterState = (a: FilterState, b: FilterState) =>
  a.mode === b.mode && sameStrings(a.names, b.names);

// ── Field ────────────────────────────────────────────────────────────────
export interface FieldProps {
  testid: string;
  label: string;
  value: string | number | undefined; // undefined = key absent
  onChange(value: string | number | undefined): void;
  type?: "text" | "number"; // default "text"
  hint?: string;
  disabled?: boolean;
  invalid?: string; // matched error message → red field + message below
}

export function Field({
  testid,
  label,
  value,
  onChange,
  type = "text",
  hint,
  disabled,
  invalid,
}: FieldProps) {
  return (
    <div data-testid={testid} className="min-w-0">
      <Label text={label} />
      <input
        data-testid="input"
        type={type === "number" ? "number" : "text"}
        value={value ?? ""}
        disabled={disabled}
        onChange={(e) => {
          const raw = e.target.value;
          if (type === "number") {
            // A number field writes a NUMBER — never a string; empty = key
            // removed (not 0, not null).
            onChange(raw.trim() === "" ? undefined : Number(raw));
          } else {
            onChange(raw === "" ? undefined : raw);
          }
        }}
        className={INPUT_CLS + (invalid ? " border-red-600" : "")}
      />
      {hint && <Hint text={hint} />}
      {invalid && <ErrorText text={invalid} />}
    </div>
  );
}

// ── Toggle ───────────────────────────────────────────────────────────────
export interface ToggleProps {
  testid: string;
  label: string;
  checked: boolean;
  onChange(checked: boolean): void;
  hint?: string;
  invalid?: string;
}

export function Toggle({ testid, label, checked, onChange, hint, invalid }: ToggleProps) {
  return (
    <div data-testid={testid} className="flex min-w-0 items-start justify-between gap-3">
      <span className="min-w-0">
        <span className="block text-xs text-zinc-300">{label}</span>
        {hint && <span className="mt-0.5 block text-[10px] text-zinc-500">{hint}</span>}
        {invalid && <span data-testid="error" className="mt-0.5 block text-[11px] text-red-400">{invalid}</span>}
      </span>
      <input
        data-testid="input"
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-0.5 h-4 w-4 shrink-0 cursor-pointer accent-emerald-600"
      />
    </div>
  );
}

// ── KvRows ───────────────────────────────────────────────────────────────
export interface KvRow {
  key: string;
  value: string;
}

export interface KvRowsProps {
  testid: string;
  label: string;
  rows: KvRow[];
  onChange(rows: KvRow[]): void; // full new array on every edit
}

export function KvRows({ testid, label, rows: rowsProp, onChange }: KvRowsProps) {
  const [rows, setRows] = useComposing(rowsProp, sameRows);

  function update(next: KvRow[]) {
    setRows(next);
    onChange(next);
  }

  return (
    <div data-testid={testid} className="min-w-0">
      <div className="mb-1 flex items-center justify-between gap-2">
        <span className="text-[11px] text-zinc-400">{label}</span>
        <button
          data-testid="add"
          type="button"
          onClick={() => update([...rows, { key: "", value: "" }])}
          className={GHOST_BTN_CLS}
        >
          + add row
        </button>
      </div>
      <div className="space-y-1.5">
        {rows.map((r, i) => (
          <div key={i} data-testid={`row-${i}`} className="flex items-center gap-1.5">
            <input
              data-testid={`row-${i}-key`}
              value={r.key}
              placeholder="KEY"
              onChange={(e) =>
                update(rows.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))
              }
              className={INPUT_CLS + " flex-1"}
            />
            <input
              data-testid={`row-${i}-value`}
              value={r.value}
              placeholder="value"
              onChange={(e) =>
                update(rows.map((x, j) => (j === i ? { ...x, value: e.target.value } : x)))
              }
              className={INPUT_CLS + " flex-1"}
            />
            <button
              data-testid={`row-${i}-remove`}
              type="button"
              title="Remove row"
              onClick={() => update(rows.filter((_, j) => j !== i))}
              className="cursor-pointer rounded px-1 text-xs text-zinc-500 hover:bg-zinc-800 hover:text-red-400"
            >
              ✕
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

// ── ChipRow ──────────────────────────────────────────────────────────────
export interface ChipRowProps {
  testid: string;
  label: string;
  items: string[];
  onChange(items: string[]): void; // full new array on every edit
  placeholder?: string;
}

export function ChipRow({
  testid,
  label,
  items: itemsProp,
  onChange,
  placeholder,
}: ChipRowProps) {
  const [items, setItems] = useComposing(itemsProp, sameStrings);
  const [draft, setDraft] = useState("");

  // Enter in the input or clicking + appends — trimmed, empty ignored,
  // duplicates ignored (no emit).
  function commit() {
    const name = draft.trim();
    setDraft("");
    if (name === "" || items.includes(name)) return;
    const next = [...items, name];
    setItems(next);
    onChange(next);
  }

  return (
    <div data-testid={testid} className="min-w-0">
      <span className="mb-1 block text-[11px] text-zinc-400">{label}</span>
      <div className="flex flex-wrap items-center gap-1.5">
        {items.map((name) => (
          <span
            key={name}
            data-testid={`chip-${name}`}
            className="inline-flex items-center gap-1 rounded-full border border-zinc-700 bg-zinc-900 px-2 py-0.5 font-mono text-[11px] text-zinc-200"
          >
            {name}
            <button
              data-testid={`chip-${name}-remove`}
              type="button"
              title={`Remove ${name}`}
              onClick={() => {
                const next = items.filter((x) => x !== name);
                setItems(next);
                onChange(next);
              }}
              className="cursor-pointer text-zinc-500 hover:text-red-400"
            >
              ✕
            </button>
          </span>
        ))}
        <input
          data-testid="add-input"
          value={draft}
          placeholder={placeholder ?? "add"}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
          }}
          className={INPUT_CLS + " w-32"}
        />
        <button data-testid="add" type="button" title="Add" onClick={commit} className={GHOST_BTN_CLS}>
          +
        </button>
      </div>
    </div>
  );
}

// ── Tri ──────────────────────────────────────────────────────────────────
export type VisionTri = "auto" | "on" | "off"; // → vision: omit / true / false

export interface TriProps {
  testid: string;
  label: string;
  value: VisionTri;
  onChange(value: VisionTri): void;
}

export function Tri({ testid, label, value, onChange }: TriProps) {
  return (
    <div data-testid={testid} className="flex flex-wrap items-center gap-1.5">
      <span className="text-[11px] text-zinc-400">{label}</span>
      {(["auto", "on", "off"] as const).map((v) => (
        <label
          key={v}
          className={`inline-flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] ${
            value === v
              ? "border-emerald-700/70 bg-emerald-950/40 text-emerald-300"
              : "border-zinc-700 bg-zinc-900 text-zinc-400 hover:text-zinc-200"
          }`}
        >
          <input
            data-testid={v}
            type="radio"
            name={`tri-${testid}`}
            checked={value === v}
            onChange={() => onChange(v)}
            className="h-3 w-3 accent-emerald-600"
          />
          {v}
        </label>
      ))}
    </div>
  );
}

// ── FilterBlock ──────────────────────────────────────────────────────────
export interface FilterBlockProps {
  testid: string;
  label: string;
  universe: string[]; // names from SettingsMeta (builtin_tools / mcps names /
  //                    tab_names / skills) — NEVER hard-coded
  value: FilterState; // from ./filter
  onChange(value: FilterState): void;
  emptyHint?: string; // shown for an allowlist with zero checked
}

const MODES: { id: FilterMode; name: string; desc: string }[] = [
  { id: "all", name: "all", desc: "Every tool — No filter." },
  { id: "only", name: "allowlist", desc: "Allowlist — Only the checked tools." },
  { id: "disabled", name: "blocklist", desc: "Blocklist — Remove the checked tools." },
];

export function FilterBlock({
  testid,
  label,
  universe,
  value,
  onChange,
  emptyHint,
}: FilterBlockProps) {
  const [state, setState] = useComposing(value, sameFilterState);
  const isChecked = (n: string) => state.names.includes(n);

  function emit(next: FilterState) {
    setState(next);
    onChange(next);
  }

  return (
    <div data-testid={testid} className="min-w-0 space-y-2">
      <span className="block text-[11px] text-zinc-400">{label}</span>
      <div className="flex flex-wrap gap-1">
        {MODES.map((m) => (
          <button
            key={m.id}
            data-testid={`mode-${m.id}`}
            type="button"
            title={m.desc}
            onClick={() => emit(setMode(state, m.id))}
            className={`cursor-pointer rounded border px-2 py-0.5 text-[11px] ${
              state.mode === m.id
                ? "border-emerald-700/70 bg-emerald-950/40 text-emerald-300"
                : "border-zinc-700 bg-zinc-900 text-zinc-400 hover:text-zinc-200"
            }`}
          >
            {m.name}
          </button>
        ))}
      </div>
      <div className="flex flex-wrap gap-1">
        {universe.map((n) => (
          <button
            key={n}
            data-testid={`chip-${n}`}
            type="button"
            onClick={() =>
              emit({
                mode: state.mode,
                names: isChecked(n)
                  ? state.names.filter((x) => x !== n)
                  : [...state.names, n],
              })
            }
            className={`cursor-pointer rounded-full border px-2 py-0.5 font-mono text-[11px] ${
              isChecked(n)
                ? "border-emerald-700/70 bg-emerald-950/40 text-emerald-300"
                : "border-zinc-700 bg-zinc-900 text-zinc-400 hover:text-zinc-200"
            }`}
          >
            {n}
          </button>
        ))}
      </div>
      {state.mode === "only" && state.names.length === 0 && (
        <span className="block text-[10px] text-amber-400/80">
          {emptyHint ?? "Nothing checked — serializes as { enabled: false }."}
        </span>
      )}
    </div>
  );
}

// ── SecretInput ──────────────────────────────────────────────────────────
export interface SecretInputProps {
  testid: string;
  label: string;
  value: string | undefined;
  onChange(value: string | undefined): void; // undefined = key removed
  hint?: string;
  invalid?: string;
}

export function SecretInput({ testid, label, value, onChange, hint, invalid }: SecretInputProps) {
  const [revealed, setRevealed] = useState(false);
  return (
    <div data-testid={testid} className="min-w-0">
      <Label text={label} />
      <div className="flex items-center gap-1.5">
        {revealed ? (
          <input
            data-testid="value"
            type="text"
            value={value ?? ""}
            onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
            className={INPUT_CLS + " flex-1 font-mono"}
          />
        ) : (
          // Masked by default: dots only — the secret text is NOT in the DOM.
          <span data-testid="value" className={INPUT_CLS + " flex-1 font-mono text-zinc-500"}>
            {value ? "••••••••" : ""}
          </span>
        )}
        <button
          data-testid="reveal"
          type="button"
          onClick={() => setRevealed((r) => !r)}
          className={GHOST_BTN_CLS}
        >
          {revealed ? "Hide" : "Reveal"}
        </button>
      </div>
      {hint && <Hint text={hint} />}
      {invalid && <ErrorText text={invalid} />}
    </div>
  );
}

// ── ModelSelect ──────────────────────────────────────────────────────────
export interface ModelSelectProps {
  testid: string;
  label: string;
  options: { alias: string; qualified: string }[];
  value: string;
  onChange(alias: string): void;
}

export function ModelSelect({ testid, label, options, value, onChange }: ModelSelectProps) {
  return (
    <div data-testid={testid} className="min-w-0">
      <Label text={label} />
      <select
        data-testid="select"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={INPUT_CLS + " cursor-pointer"}
      >
        {options.map((o) => (
          <option key={o.alias} value={o.alias}>
            {o.alias} — {o.qualified}
          </option>
        ))}
      </select>
    </div>
  );
}

// ── Crumbs ───────────────────────────────────────────────────────────────
export interface CrumbsProps {
  testid: string;
  items: string[]; // e.g. ["Profiles", "dev"] — "Profiles › dev"
  onNavigate(index: number): void;
}

export function Crumbs({ testid, items, onNavigate }: CrumbsProps) {
  return (
    <nav data-testid={testid} aria-label="Breadcrumb" className="flex flex-wrap items-center gap-1 text-xs">
      {items.map((item, i) => (
        <span key={i} className="flex items-center gap-1">
          {i > 0 && <span className="text-zinc-600">›</span>}
          {i < items.length - 1 ? (
            <button
              data-testid={`crumb-${i}`}
              type="button"
              onClick={() => onNavigate(i)}
              className="cursor-pointer text-sky-400 hover:underline"
            >
              {item}
            </button>
          ) : (
            <span data-testid={`crumb-${i}`} aria-current="page" className="text-zinc-200">
              {item}
            </span>
          )}
        </span>
      ))}
    </nav>
  );
}

// ── Badge ────────────────────────────────────────────────────────────────
export interface BadgeProps {
  testid?: string;
  tone?: "green" | "amber" | "red" | "neutral";
  children: React.ReactNode;
}

const BADGE_TONES = {
  green: "border-emerald-700/60 bg-emerald-950/40 text-emerald-400",
  amber: "border-amber-700/60 bg-amber-950/40 text-amber-400",
  red: "border-red-800/60 bg-red-950/40 text-red-300",
  neutral: "border-zinc-700 bg-zinc-900 text-zinc-400",
};

export function Badge({ testid, tone = "neutral", children }: BadgeProps) {
  return (
    <span
      data-testid={testid}
      className={`inline-flex items-center rounded-full border px-2 py-0.5 text-[10px] ${BADGE_TONES[tone]}`}
    >
      {children}
    </span>
  );
}
