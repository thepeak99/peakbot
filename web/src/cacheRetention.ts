// Pure retention/gate-vs-purge logic for the shared remote conversation cache
// (design "Shared remote conversation cache + delta sync — revised design" §7).

export interface RosterEntry {
  id: string;
  name: string;
}

/**
 * Determine which cloud origins should be purged based on roster diff.
 * - first roster (prior=null) never triggers purge
 * - device present in prior but absent in current → purge
 * - device renamed (same id, different name) → retain
 */
export function determinePurges(
  priorRoster: RosterEntry[] | null,
  currentRoster: RosterEntry[],
): string[] {
  if (priorRoster === null) return [];
  const currentIds = new Set(currentRoster.map((e) => e.id));
  const purges: string[] = [];
  for (const entry of priorRoster) {
    if (!currentIds.has(entry.id)) {
      purges.push(`cloud://${entry.id}`);
    }
  }
  return purges;
}
