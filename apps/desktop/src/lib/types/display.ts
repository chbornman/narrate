/**
 * The cross-stage seam types (frozen by FOUNDATIONS): Grid PRODUCES these,
 * Look CONSUMES them. RAW+JPEG stacking is display-level only (D1, K13:
 * the data model keeps two images).
 */
import type { GridItem } from "./dto";

/** One grid CELL: a lone image, or a collapsed pair (one cell, one
 * chevron, TWO write-scope targets). `primary` is the displayed member
 * (JPEG for a collapsed pair); `alt` is the hidden member (RAW). */
export interface DisplayUnit {
  primary: GridItem;
  alt: GridItem | null;
}

/** One Look navigation entry. `display` is shown; `alt` is the other pair
 * member `R` flips to (featureset §5). Hashes, not items — Look resolves
 * pixels via the photoproof:// protocol. */
export interface LookEntry {
  display: string;
  alt: string | null;
}
