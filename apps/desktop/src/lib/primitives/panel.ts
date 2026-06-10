/**
 * Pure side-panel math (paired with Panel.svelte): size clamping, resize
 * drag, and width persistence. Push panels participate in the shell flex
 * row — the grid re-snaps integer columns on every resize (featureset §1).
 */

export function clampSize(size: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, Math.round(size)));
}

/**
 * Resize drag: a left panel grows as the pointer moves right; a right
 * panel grows as it moves left.
 */
export function dragSize(
  start: { size: number; x: number },
  x: number,
  side: "left" | "right",
  min: number,
  max: number,
): number {
  const delta = x - start.x;
  return clampSize(start.size + (side === "left" ? delta : -delta), min, max);
}

export function loadSize(
  key: string,
  fallback: number,
  min: number,
  max: number,
): number {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(key);
  } catch {
    /* private mode etc. */
  }
  const v = Number(raw);
  return raw !== null && Number.isFinite(v) ? clampSize(v, min, max) : fallback;
}

export function saveSize(key: string, size: number): void {
  try {
    localStorage.setItem(key, String(size));
  } catch {
    /* prefs simply don't persist */
  }
}
