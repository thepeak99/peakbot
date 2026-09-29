// The "Profile" side panel — binds this conversation to a named config
// profile. Modelled directly on AgentsPanel's pipeline radio list
// (AgentsPanel.tsx:139-204): "None (base config)" + one row per profile,
// disabled while `locked`, with a hint explaining why.

export function ProfilePanel({
  profiles,
  active,
  locked,
  onSelect,
}: {
  /** Selectable profile names, sorted (state.ts `AppState.profiles`). */
  profiles: string[];
  /** The profile this conversation is bound to, or null for base config. */
  active: string | null;
  /** True once the conversation has a real turn — the selection is then
   * frozen, same signal AgentsPanel uses for pipelines. */
  locked: boolean;
  /** Bind the conversation to a profile; `null` selects base config. No-op
   * while `locked` (radios disabled). */
  onSelect: (name: string | null) => void;
}) {
  const rowClass = (isSelected: boolean) =>
    `flex items-center gap-2 rounded-md border px-2.5 py-1.5 text-xs transition-colors ${
      isSelected
        ? "border-sky-700 bg-sky-950/40 text-sky-200"
        : "border-transparent text-zinc-300"
    } ${locked ? "cursor-not-allowed text-zinc-500" : "cursor-pointer hover:border-zinc-800 hover:bg-zinc-900/70"}`;

  return (
    <section>
      <div className="mb-3 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Profile
        </h3>
      </div>

      <ul className="mb-2 space-y-1">
        <li>
          <label
            className={rowClass(active === null)}
            title={
              locked
                ? "Locked — this conversation has already started. Start a new one to pick a different profile."
                : "Run this conversation on the base config — no profile overlay."
            }
          >
            <input
              type="radio"
              name="profile"
              checked={active === null}
              disabled={locked}
              onChange={() => onSelect(null)}
              className="h-3.5 w-3.5 accent-sky-500"
            />
            <span className="flex-1">None (base config)</span>
          </label>
        </li>
        {profiles.map((p) => (
          <li key={p}>
            <label
              className={rowClass(active === p)}
              title={
                locked
                  ? "Locked — this conversation has already started. Start a new one to pick a different profile."
                  : `Run this conversation under the '${p}' profile.`
              }
            >
              <input
                type="radio"
                name="profile"
                checked={active === p}
                disabled={locked}
                onChange={() => onSelect(p)}
                className="h-3.5 w-3.5 accent-sky-500"
              />
              <span className="flex-1 break-words font-medium">{p}</span>
            </label>
          </li>
        ))}
      </ul>

      {locked && (
        <p className="mb-3 text-[11px] text-zinc-600">
          Locked for this conversation — start a new one to change profile.
        </p>
      )}
    </section>
  );
}
