/**
 * Grid sort (spec/UI.md §3.6): the complete v1 set. Sort persists per
 * folder; thumbnail size persists globally (persistence in prefs.ts).
 */
import type { GridItem } from "../types/dto";

export type SortMode =
  | "capture-desc" // default: capture date, newest first
  | "capture-asc"
  | "filename"
  | "added";

export const SORT_MODES: { mode: SortMode; label: string }[] = [
  { mode: "capture-desc", label: "Capture date · newest first" },
  { mode: "capture-asc", label: "Capture date · oldest first" },
  { mode: "filename", label: "Filename A–Z" },
  { mode: "added", label: "Date added" },
];

export const DEFAULT_SORT: SortMode = "capture-desc";

/** Undated images sort after dated ones, by filename for determinism. */
export function sortItems(items: GridItem[], mode: SortMode): GridItem[] {
  const v = [...items];
  const byName = (a: GridItem, b: GridItem) =>
    a.fileName.localeCompare(b.fileName) || a.hash.localeCompare(b.hash);
  switch (mode) {
    case "filename":
      return v.sort(byName);
    case "added":
      return v.sort(
        (a, b) => b.addedTs.localeCompare(a.addedTs) || byName(a, b),
      );
    case "capture-asc":
      return v.sort((a, b) => {
        if (a.captureTs === null && b.captureTs === null) return byName(a, b);
        if (a.captureTs === null) return 1;
        if (b.captureTs === null) return -1;
        return a.captureTs.localeCompare(b.captureTs) || byName(a, b);
      });
    case "capture-desc":
      return v.sort((a, b) => {
        if (a.captureTs === null && b.captureTs === null) return byName(a, b);
        if (a.captureTs === null) return 1;
        if (b.captureTs === null) return -1;
        return b.captureTs.localeCompare(a.captureTs) || byName(a, b);
      });
  }
}

/** Thumbnail size: 4 steps, ~96–320 px cells (§3.6). */
export const THUMB_STEPS = [96, 160, 240, 320] as const;
export const DEFAULT_THUMB_STEP = 1;
