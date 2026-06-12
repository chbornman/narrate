/**
 * Look navigation logic (featureset §2/§5): the navigation set IS the
 * entry selection, and R member flips over LookEntry pairs. The Space
 * tap-vs-hold machine that used to live here (hold-to-pan, clean-tap
 * close) was deleted on June 12 2026: Space is 100% the microphone key
 * now (defs/global.ts mic-press), drag-pan is the only pan, and Esc is
 * the only keyboard close (escape ladder "leave-look").
 */
import type { DisplayUnit, LookEntry } from "../types/display";

/** Grid cell → Look entry (the frozen cross-stage seam, types/display.ts). */
export function toEntry(unit: DisplayUnit): LookEntry {
  return { display: unit.primary.hash, alt: unit.alt?.hash ?? null };
}

/**
 * Navigation set = entry selection (featureset §2): entering Look with a
 * multi-selection (≥2 units, entered unit included) cycles within it;
 * single-image entry — or an entry outside the selection, which narrows
 * scope to the viewed image anyway (CAPTURE §3) — cycles the folder.
 * Order is GRID order: ←/→ walk the wall the way it reads; selection
 * ORDER stays a write-scope concern (CAPTURE event_targets.position),
 * never a navigation one. Returns null when the entry hash is unknown.
 */
export function navigationSet(
  units: readonly DisplayUnit[],
  selected: readonly string[],
  entryHash: string,
): { order: LookEntry[]; index: number } | null {
  const sel = new Set(selected);
  const scoped =
    sel.size >= 2 && sel.has(entryHash)
      ? units.filter((u) => sel.has(u.primary.hash))
      : units;
  const index = scoped.findIndex((u) => u.primary.hash === entryHash);
  if (index < 0) return null;
  return { order: scoped.map(toEntry), index };
}

/** Hash an entry displays, honoring an R flip (featureset §5). */
export function displayedHash(entry: LookEntry, flips: ReadonlySet<string>): string {
  return flips.has(entry.display) && entry.alt !== null ? entry.alt : entry.display;
}

/** R: toggle the flip for a pair entry; lone entries no-op (FRV keeps the
 * key inert rather than erroring — quiet). */
export function toggleFlip(
  flips: ReadonlySet<string>,
  entry: LookEntry,
): ReadonlySet<string> {
  if (entry.alt === null) return flips;
  const next = new Set(flips);
  if (next.has(entry.display)) next.delete(entry.display);
  else next.add(entry.display);
  return next;
}
