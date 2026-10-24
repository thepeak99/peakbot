// Pure ordering/naming helpers for the profiles map (contract.ts
// "./profileOps (T27)"). The panes route ALL ordering/naming logic through
// here — unit-testable, no doc mutation. The profiles map is a plain JS
// object: insertion order = YAML key order, so "move first" = rebuild the
// object with the key first.

/**
 * A NEW map with `name` moved to the front; the relative order of the rest
 * is unchanged; the input is not mutated. Unknown `name` (or an empty map)
 * throws `Error("no such profile: <name>")`.
 */
export function makeDefault(
  profiles: Record<string, unknown>,
  name: string,
): Record<string, unknown> {
  if (!(name in profiles)) throw new Error(`no such profile: ${name}`);
  const out: Record<string, unknown> = {};
  out[name] = profiles[name];
  for (const key of Object.keys(profiles)) {
    if (key !== name) out[key] = profiles[key];
  }
  return out;
}

/**
 * `<name>-copy`; if that is in `taken`, `<name>-copy-2`, `<name>-copy-3`, …
 * — the first one not in `taken` (spec §9.3 "`<name>-copy[-N]`").
 */
export function duplicateName(name: string, taken: string[]): string {
  const base = `${name}-copy`;
  if (!taken.includes(base)) return base;
  let n = 2;
  while (taken.includes(`${base}-${n}`)) n += 1;
  return `${base}-${n}`;
}

/**
 * `name` if the name is free in the repo draft, else `name + "-copy"`
 * (further collisions are the server's validation problem, not this
 * helper's — spec §9.3 "(-copy suffix if the name is taken there)").
 */
export function copyToRepoName(name: string, repoProfileNames: string[]): string {
  return repoProfileNames.includes(name) ? `${name}-copy` : name;
}

/**
 * Where a duplicate of `source` goes in the key order (contract: "inserts a
 * copy DIRECTLY AFTER the source"): after the source's existing copy chain
 * (`<source>-copy`, `<source>-copy-N`), so repeated duplicates cluster
 * together — `dev, dev-copy` → `dev, dev-copy, dev-copy-2`.
 */
export function duplicateInsertIndex(names: string[], source: string): number {
  let index = names.indexOf(source);
  if (index === -1) index = names.length;
  const esc = source.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const copyRe = new RegExp(`^${esc}-copy(-\\d+)?$`);
  for (let i = names.length - 1; i >= 0; i -= 1) {
    if (copyRe.test(names[i])) {
      index = i;
      break;
    }
  }
  return index + 1;
}
