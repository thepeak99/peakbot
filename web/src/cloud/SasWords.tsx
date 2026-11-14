// P4 — the 6 numbered SAS words (design §E, §A.1/§A.2).
//
// `<ol>` of numbered `<li>` (screen readers say "1 apple, 2 river…"),
// monospace, ≥1.25rem. Used by BOTH the pending pane and the approver modal —
// identical rendering is the security property.

export function SasWords({ words }: { words: readonly string[] }) {
  return (
    <ol className="my-3 space-y-1 font-mono text-xl">
      {words.map((w, i) => (
        <li key={i} className="flex items-baseline gap-3">
          <span aria-hidden className="w-4 shrink-0 text-right text-zinc-400">
            {i + 1}
          </span>
          <span className="break-all">{w}</span>
        </li>
      ))}
    </ol>
  );
}
