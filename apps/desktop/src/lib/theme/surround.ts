/**
 * Surround luminance (featureset §7, D6): chrome stays dark; the image
 * surround (Grid canvas + Look backdrop) steps through five levels. Pure:
 * the level enum, cycle order, the persistence codec, and the per-level
 * contrast table that mirrors theme/tokens.css [data-surround] blocks
 * (tested for coverage so CSS and TS cannot drift silently).
 */

export type SurroundLevel = "black" | "dark" | "middle" | "light" | "white";

/** Cycle order, darkest → lightest (right-click menu order, D6). */
export const SURROUND_LEVELS: readonly SurroundLevel[] = [
  "black",
  "dark",
  "middle",
  "light",
  "white",
];

export const DEFAULT_SURROUND: SurroundLevel = "black";

export const SURROUND_LABELS: Record<SurroundLevel, string> = {
  black: "Black",
  dark: "Dark gray",
  middle: "Middle gray",
  light: "Light gray",
  white: "White",
};

/** Next level in cycle order, wrapping (delta -1 steps darker). */
export function cycleSurround(level: SurroundLevel, delta: 1 | -1 = 1): SurroundLevel {
  const i = SURROUND_LEVELS.indexOf(level);
  const n = SURROUND_LEVELS.length;
  return SURROUND_LEVELS[(i + delta + n) % n];
}

/** Persistence codec: stored string → level, anything else → null. */
export function parseSurround(v: string | null): SurroundLevel | null {
  return (SURROUND_LEVELS as readonly string[]).includes(v ?? "")
    ? (v as SurroundLevel)
    : null;
}

export interface SurroundContrast {
  /** The surround itself. */
  surround: string;
  /** Has-journal dot, retuned per level (a dulled red at every level). */
  journalDot: string;
  /** Selected-cell ring. */
  selection: string;
  /** Active-cell (focus) ring — distinct from selection (featureset §1). */
  focusCell: string;
  /** Marquee rubber-band fill. */
  marquee: string;
}

/**
 * Per-level contrast table — the single source the tokens.css
 * [data-surround] blocks transcribe. surround.test.ts asserts coverage,
 * distinctness, and that selection/focus flip dark on light surrounds.
 */
export const SURROUND_CONTRAST: Record<SurroundLevel, SurroundContrast> = {
  black: {
    surround: "#0e0e0e",
    journalDot: "#a33c3c",
    selection: "#cfcfcf",
    focusCell: "#8a8a8a",
    marquee: "rgba(207, 207, 207, 0.35)",
  },
  dark: {
    surround: "#2e2e2e",
    journalDot: "#b04545",
    selection: "#e0e0e0",
    focusCell: "#9a9a9a",
    marquee: "rgba(224, 224, 224, 0.35)",
  },
  middle: {
    surround: "#7f7f7f",
    journalDot: "#8c2f2f",
    selection: "#f5f5f5",
    focusCell: "#2a2a2a",
    marquee: "rgba(20, 20, 20, 0.3)",
  },
  light: {
    surround: "#c9c9c9",
    journalDot: "#872c2c",
    selection: "#1b1b1b",
    focusCell: "#4a4a4a",
    marquee: "rgba(20, 20, 20, 0.25)",
  },
  white: {
    surround: "#f4f4f4",
    journalDot: "#7e2828",
    selection: "#0e0e0e",
    focusCell: "#5a5a5a",
    marquee: "rgba(14, 14, 14, 0.22)",
  },
};
