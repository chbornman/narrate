/**
 * Grid sort (spec/UI.md §3.6): the complete v1 set. Sort persists per
 * folder; thumbnail size persists globally (persistence in prefs.ts).
 */
import type { GridItem } from "../types/dto";

export type SortMode =
  | "capture-desc" // default: capture date, newest first
  | "capture-asc"
  | "filename"
  | "added"
  // M3 search-as-scope (Phase 1): "preserve the backend's fused order". Only
  // meaningful — and only offered in the sort menu — while a query scopes
  // the grid; sortItems is a PASS-THROUGH for it (results arrive pre-sorted
  // by score). Committing a semantic search auto-selects it (app.svelte.ts).
  | "relevance"
  // Attention heatmap (DESIGN-ATTENTION-HEATMAP.md): sort the scope by
  // normalized engagement intensity, hottest first. Only meaningful when
  // intensity is LOADED (the heat tint is on) — sortItems takes the per-hash
  // intensity map; an image with no loaded score sorts as 0 (cold).
  | "attention";

export const SORT_MODES: { mode: SortMode; label: string }[] = [
  { mode: "capture-desc", label: "Capture date · newest first" },
  { mode: "capture-asc", label: "Capture date · oldest first" },
  { mode: "filename", label: "Filename A-Z" },
  { mode: "added", label: "Date added" },
];

/** The query-only sort row (UI §3.6 + M3): shown in the sort menu ONLY
 * while a query scopes the grid — relevance has no meaning over a folder or
 * collection. Kept separate from SORT_MODES so the always-available rows
 * never gain a row that would sit inert in folder/collection mode. */
export const RELEVANCE_SORT: { mode: SortMode; label: string } = {
  mode: "relevance",
  label: "Relevance",
};

/** The attention-only sort row (DESIGN-ATTENTION-HEATMAP.md): shown in the sort
 * menu ONLY while the heat tint is on (intensity loaded) — sorting by
 * engagement is meaningless with no scores. Kept separate from SORT_MODES like
 * RELEVANCE_SORT so the always-available rows never gain an inert row. */
export const ATTENTION_SORT: { mode: SortMode; label: string } = {
  mode: "attention",
  label: "Attention",
};

export const DEFAULT_SORT: SortMode = "capture-desc";

/** Undated images sort after dated ones, by filename for determinism.
 * `relevance` is a no-op pass-through: the query grid is fed result hashes
 * already in fused order, and re-sorting would destroy it (M3). `attention`
 * sorts by the per-hash intensity map (hottest first; a missing score is 0,
 * cold), stable by filename — only meaningful when `intensity` is supplied. */
export function sortItems(
  items: GridItem[],
  mode: SortMode,
  intensity?: ReadonlyMap<string, number>,
): GridItem[] {
  const v = [...items];
  const byName = (a: GridItem, b: GridItem) =>
    a.fileName.localeCompare(b.fileName) || a.hash.localeCompare(b.hash);
  switch (mode) {
    case "relevance":
      return v; // preserve the backend's fused order
    case "attention": {
      const heat = (i: GridItem) => intensity?.get(i.hash) ?? 0;
      return v.sort((a, b) => heat(b) - heat(a) || byName(a, b));
    }
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
export const THUMB_STEPS = [96, 160, 240, 320, 420, 512] as const;
export const DEFAULT_THUMB_STEP = 1;
