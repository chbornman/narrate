/**
 * T cell-info cycle (featureset §1): none → minimal → annotated-state.
 * Badges stay display-only and hover-quiet; these strings are the only
 * extra cell text and render dimmed. The default is "none" — the levels
 * are an explicit per-user opt-in (persisted, prefs.ts), which is how the
 * featureset amends UI §3.5's bare-thumbnail rule without breaking quiet.
 */
import type { CellInfoLevel } from "../state/prefs";

export const CELL_INFO_CYCLE: readonly CellInfoLevel[] = ["none", "minimal", "annotated"];

export function nextLevel(level: CellInfoLevel): CellInfoLevel {
  const i = CELL_INFO_CYCLE.indexOf(level);
  return CELL_INFO_CYCLE[(i + 1) % CELL_INFO_CYCLE.length];
}

/**
 * Info-strip height (px) for a level — the SINGLE SOURCE OF TRUTH the grid
 * layout and Thumb both read. The strip sits ABOVE the image so the cell
 * GROWS downward and the thumbnail is never overlaid (founder, June 2026).
 * Fixed px per level (not scaled with zoom) for v1: one text line at
 * minimal (filename), two at annotated (filename + state).
 */
export function infoStripHeight(level: CellInfoLevel): number {
  if (level === "minimal") return 18;
  if (level === "annotated") return 32;
  return 0;
}

export interface CellFacts {
  fileName: string;
  rating: number | null;
  hasJournal: boolean;
}

export interface CellInfoLine {
  /** Filename line (minimal + annotated). */
  name: string | null;
  /** Annotated-state line: rating fold + journal presence. */
  state: string | null;
}

export function infoLine(level: CellInfoLevel, facts: CellFacts): CellInfoLine {
  if (level === "none") return { name: null, state: null };
  if (level === "minimal") return { name: facts.fileName, state: null };
  const parts = [facts.rating === null ? "unrated" : `★ ${facts.rating}`];
  if (facts.hasJournal) parts.push("journaled");
  return { name: facts.fileName, state: parts.join(" · ") };
}
